use libflac_sys as flac;
use std::collections::{HashMap, HashSet};

use crate::Result;
use crate::types::{CueDisc, InputMetadata, TrackSpan};

pub(crate) fn build_track_metadata(
    meta: &InputMetadata,
    cue: &CueDisc,
    tracks: &[TrackSpan],
    track: &TrackSpan,
) -> Result<Vec<*mut flac::FLAC__StreamMetadata>> {
    let mut blocks = Vec::new();

    let comment = build_vorbis_comment(meta, cue, tracks, track)?;
    blocks.push(comment);

    for picture in &meta.pictures {
        let clone = unsafe { flac::FLAC__metadata_object_clone(*picture as *const _) };
        if !clone.is_null() {
            blocks.push(clone);
        }
    }

    Ok(blocks)
}

fn build_vorbis_comment(
    meta: &InputMetadata,
    cue: &CueDisc,
    tracks: &[TrackSpan],
    track: &TrackSpan,
) -> Result<*mut flac::FLAC__StreamMetadata> {
    let object =
        unsafe { flac::FLAC__metadata_object_new(flac::FLAC__METADATA_TYPE_VORBIS_COMMENT) };
    if object.is_null() {
        return Err("failed to allocate Vorbis comment metadata".to_string());
    }

    let vendor = meta.vendor.as_deref().unwrap_or("flac-cue-split");
    if let Err(err) = set_vendor_string(object, vendor) {
        unsafe {
            flac::FLAC__metadata_object_delete(object);
        }
        return Err(err);
    }

    let overrides = build_override_tags(cue, tracks.len(), track);
    let merged = merge_tags(&meta.comments, &overrides);

    for (key, value) in merged {
        if let Err(err) = append_comment(object, &key, &value) {
            unsafe {
                flac::FLAC__metadata_object_delete(object);
            }
            return Err(err);
        }
    }

    Ok(object)
}

fn set_vendor_string(object: *mut flac::FLAC__StreamMetadata, vendor: &str) -> Result<()> {
    let bytes = vendor.as_bytes();
    let entry = flac::FLAC__StreamMetadata_VorbisComment_Entry {
        length: bytes.len() as u32,
        entry: bytes.as_ptr() as *mut flac::FLAC__byte,
    };

    let ok = unsafe {
        flac::FLAC__metadata_object_vorbiscomment_set_vendor_string(object, entry, 1) != 0
    };
    if !ok {
        return Err("failed to set Vorbis vendor string".to_string());
    }
    Ok(())
}

fn append_comment(object: *mut flac::FLAC__StreamMetadata, key: &str, value: &str) -> Result<()> {
    let comment = format!("{}={}", key, value);
    let bytes = comment.as_bytes();
    let entry = flac::FLAC__StreamMetadata_VorbisComment_Entry {
        length: bytes.len() as u32,
        entry: bytes.as_ptr() as *mut flac::FLAC__byte,
    };

    let ok =
        unsafe { flac::FLAC__metadata_object_vorbiscomment_append_comment(object, entry, 1) != 0 };
    if !ok {
        return Err(format!("failed to append Vorbis comment {}", key));
    }
    Ok(())
}

pub(crate) fn build_override_tags(
    cue: &CueDisc,
    total_tracks: usize,
    track: &TrackSpan,
) -> Vec<(String, String)> {
    let mut tags = Vec::new();

    let title = track
        .title
        .clone()
        .unwrap_or_else(|| format!("Track {}", track.number));
    tags.push(("TITLE".to_string(), title));

    let performer = track.performer.clone().or_else(|| cue.performer.clone());
    if let Some(artist) = performer {
        tags.push(("ARTIST".to_string(), artist));
    }

    if let Some(album) = &cue.title {
        tags.push(("ALBUM".to_string(), album.clone()));
    }

    if let Some(album_artist) = &cue.performer {
        tags.push(("ALBUMARTIST".to_string(), album_artist.clone()));
    }

    if let Some(genre) = &cue.genre {
        tags.push(("GENRE".to_string(), genre.clone()));
    }

    if let Some(message) = &cue.message {
        tags.push(("COMMENT".to_string(), message.clone()));
    }

    if let Some(disc_id) = &cue.disc_id {
        tags.push(("DISCID".to_string(), disc_id.clone()));
    }

    let composer = track
        .composer
        .clone()
        .or_else(|| track.songwriter.clone())
        .or_else(|| cue.composer.clone())
        .or_else(|| cue.songwriter.clone());
    if let Some(comp) = composer {
        tags.push(("COMPOSER".to_string(), comp));
    }

    if let Some(isrc) = &track.isrc {
        tags.push(("ISRC".to_string(), isrc.clone()));
    }

    tags.push(("TRACKNUMBER".to_string(), track.number.to_string()));
    tags.push(("TRACKTOTAL".to_string(), total_tracks.to_string()));
    tags.push(("TOTALTRACKS".to_string(), total_tracks.to_string()));

    if let Some(date) = track.rem.date.clone().or_else(|| cue.rem.date.clone()) {
        tags.push(("DATE".to_string(), date));
    }

    if let Some(gain) = &cue.rem.replaygain_album_gain {
        tags.push(("REPLAYGAIN_ALBUM_GAIN".to_string(), gain.clone()));
    }
    if let Some(peak) = &cue.rem.replaygain_album_peak {
        tags.push(("REPLAYGAIN_ALBUM_PEAK".to_string(), peak.clone()));
    }
    if let Some(gain) = &track.rem.replaygain_track_gain {
        tags.push(("REPLAYGAIN_TRACK_GAIN".to_string(), gain.clone()));
    }
    if let Some(peak) = &track.rem.replaygain_track_peak {
        tags.push(("REPLAYGAIN_TRACK_PEAK".to_string(), peak.clone()));
    }

    tags
}

pub(crate) fn merge_tags(
    base: &[(String, String)],
    overrides: &[(String, String)],
) -> Vec<(String, String)> {
    let mut override_keys = HashSet::new();
    for (key, _) in overrides {
        override_keys.insert(key.to_ascii_uppercase());
    }

    let mut merged = Vec::new();
    for (key, value) in base {
        if !override_keys.contains(&key.to_ascii_uppercase()) {
            merged.push((key.clone(), value.clone()));
        }
    }

    merged.extend(overrides.iter().cloned());
    merged
}

pub(crate) fn compute_common_metadata(
    meta: &InputMetadata,
    cue: &CueDisc,
    tracks: &[TrackSpan],
) -> Vec<(String, String)> {
    if tracks.is_empty() {
        return Vec::new();
    }

    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    let track_count = tracks.len();

    for track in tracks {
        let overrides = build_override_tags(cue, track_count, track);
        let merged = merge_tags(&meta.comments, &overrides);
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for pair in merged {
            seen.insert(pair);
        }
        for pair in seen {
            *counts.entry(pair).or_insert(0) += 1;
        }
    }

    let mut common: Vec<(String, String)> = counts
        .into_iter()
        .filter_map(|(pair, count)| {
            if count == track_count {
                Some(pair)
            } else {
                None
            }
        })
        .collect();
    common.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    common
}

pub(crate) fn compute_unique_metadata_pairs(
    meta: &InputMetadata,
    cue: &CueDisc,
    tracks: &[TrackSpan],
    track: &TrackSpan,
    common: &[(String, String)],
) -> Vec<(String, String)> {
    let overrides = build_override_tags(cue, tracks.len(), track);
    let merged = merge_tags(&meta.comments, &overrides);
    let mut unique: Vec<(String, String)> = Vec::new();
    let common_set: HashSet<(String, String)> = common.iter().cloned().collect();
    for pair in merged {
        if !common_set.contains(&pair) {
            unique.push(pair);
        }
    }

    unique.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    unique
}

pub(crate) fn parse_vorbis_comment(
    metadata: &flac::FLAC__StreamMetadata,
) -> (Option<String>, Vec<(String, String)>) {
    let mut vendor = None;
    let mut comments = Vec::new();

    if metadata.type_ != flac::FLAC__METADATA_TYPE_VORBIS_COMMENT {
        return (vendor, comments);
    }

    let vc = unsafe { metadata.data.vorbis_comment };

    vendor = parse_vorbis_entry(&vc.vendor_string);

    let entries = unsafe { std::slice::from_raw_parts(vc.comments, vc.num_comments as usize) };
    for entry in entries {
        if let Some((key, value)) = parse_vorbis_kv(entry) {
            comments.push((key, value));
        }
    }

    (vendor, comments)
}

fn parse_vorbis_entry(entry: &flac::FLAC__StreamMetadata_VorbisComment_Entry) -> Option<String> {
    if entry.entry.is_null() || entry.length == 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(entry.entry, entry.length as usize) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn parse_vorbis_kv(
    entry: &flac::FLAC__StreamMetadata_VorbisComment_Entry,
) -> Option<(String, String)> {
    let raw = parse_vorbis_entry(entry)?;
    let mut parts = raw.splitn(2, '=');
    let key = parts.next()?.trim();
    let value = parts.next()?.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_ascii_uppercase(), value.to_string()))
}
