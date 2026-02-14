use encoding_rs::Encoding;
use indicatif::ProgressBar;
use owo_colors::OwoColorize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::cli::{InputPath, display_path};
use crate::cue::parse_cue_file;
use crate::decoder::{AudioBlock, create_decoder};
use crate::flac::{TrackEncoder, start_track_encoder};
use crate::output::{finish_progress, make_progress_bar};
use crate::picture::add_external_picture;
use crate::types::{CueDisc, CueRem, InputMetadata, TrackSpan};

pub(crate) struct SplitOptions {
    pub(crate) flac_input: InputPath,
    pub(crate) cue_input: InputPath,
    pub(crate) display_base_abs: Option<PathBuf>,
    pub(crate) cue_encoding: Option<&'static Encoding>,
    pub(crate) overwrite: bool,
    pub(crate) compression_level: u8,
    pub(crate) search_dir: PathBuf,
    pub(crate) picture_enabled: bool,
    pub(crate) picture_path: Option<PathBuf>,
    pub(crate) delete_original: bool,
    pub(crate) rename_original: bool,
    pub(crate) output_subdir: Option<PathBuf>,
    pub(crate) enforce_cue_filename_match: bool,
}

pub(crate) struct Plan {
    cue: CueDisc,
    input_meta: InputMetadata,
    tracks: Vec<TrackSpan>,
    compression_level: u8,
    display_base_abs: Option<PathBuf>,
    picture_names: Vec<String>,
    total_samples: u64,
    warnings: Vec<String>,
    flac_display: PathBuf,
    cue_display: PathBuf,
    flac_abs: PathBuf,
    overwrite: bool,
    delete_original: bool,
    rename_original: bool,
    encoding_used: &'static Encoding,
    encoding_autodetected: bool,
}

impl Plan {
    pub(crate) fn cue(&self) -> &CueDisc {
        &self.cue
    }

    pub(crate) fn input_meta(&self) -> &InputMetadata {
        &self.input_meta
    }

    pub(crate) fn tracks(&self) -> &[TrackSpan] {
        &self.tracks
    }

    pub(crate) fn compression_level(&self) -> u8 {
        self.compression_level
    }

    pub(crate) fn display_base_abs(&self) -> Option<&Path> {
        self.display_base_abs.as_deref()
    }

    pub(crate) fn picture_names(&self) -> &[String] {
        &self.picture_names
    }

    pub(crate) fn flac_display(&self) -> &Path {
        &self.flac_display
    }

    pub(crate) fn cue_display(&self) -> &Path {
        &self.cue_display
    }

    pub(crate) fn cue_encoding(&self) -> (&'static Encoding, bool) {
        (self.encoding_used, self.encoding_autodetected)
    }

    pub(crate) fn source_actions(&self) -> (bool, bool) {
        (self.delete_original, self.rename_original)
    }

    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn execute(self) -> Result<()> {
        ensure_output_paths_available(&self.tracks, self.overwrite)?;

        let mut progress = Some(make_progress_bar(self.total_samples));

        let result = (|| {
            let decoder = create_decoder(&self.flac_abs)?;
            let blocks = decoder.into_blocks()?;

            let mut state = SplitState::new();
            for block in blocks {
                process_audio_block(&self, &mut state, progress.as_ref(), block?)?;
            }

            if let Some(mut encoder) = state.encoder.take() {
                encoder.finish()?;
            }

            Ok(())
        })();

        match result {
            Ok(()) => {
                finish_progress(&mut progress, "done");
                handle_original_flac(
                    self.display_base_abs.as_deref(),
                    &self.flac_abs,
                    self.delete_original,
                    self.rename_original,
                )
            }
            Err(err) => {
                finish_progress(&mut progress, "aborted");
                Err(err)
            }
        }
    }
}

pub(crate) fn prepare_split(options: SplitOptions) -> Result<Plan> {
    let (cue, warnings, encoding_used, encoding_autodetected) =
        parse_cue_file(&options.cue_input.abs, options.cue_encoding)?;
    validate_cue_files(
        &cue,
        &options.flac_input.abs,
        options.enforce_cue_filename_match,
    )?;

    let mut output_dir = options
        .flac_input
        .abs
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if let Some(subdir) = options.output_subdir.as_ref() {
        output_dir = output_dir.join(subdir);
    }
    fs::create_dir_all(&output_dir).map_err(|err| {
        format!(
            "failed to create output directory {}: {}",
            output_dir.display(),
            err
        )
    })?;

    let mut decoder = create_decoder(&options.flac_input.abs)?;
    let mut decoded = decoder.read_metadata()?;

    if options.picture_enabled {
        add_external_picture(
            &mut decoded.input_meta,
            &mut decoded.picture_names,
            &options.search_dir,
            options.picture_path.as_deref(),
        )?;
    }

    let sample_rate = decoded.input_meta.sample_rate;
    let total_samples = decoded.input_meta.total_samples;
    let tracks = build_output_tracks(&cue, &output_dir, sample_rate, total_samples, false)?;

    Ok(Plan {
        cue,
        input_meta: decoded.input_meta,
        tracks,
        compression_level: options.compression_level,
        display_base_abs: options.display_base_abs,
        picture_names: decoded.picture_names,
        total_samples,
        warnings,
        flac_display: options.flac_input.display,
        cue_display: options.cue_input.display,
        flac_abs: options.flac_input.abs,
        overwrite: options.overwrite,
        delete_original: options.delete_original,
        rename_original: options.rename_original,
        encoding_used,
        encoding_autodetected,
    })
}

struct SplitState {
    track_index: usize,
    encoder: Option<TrackEncoder>,
}

impl SplitState {
    fn new() -> Self {
        Self {
            track_index: 0,
            encoder: None,
        }
    }

    fn finish_encoder(&mut self) -> Result<()> {
        if let Some(mut encoder) = self.encoder.take() {
            encoder.finish()?;
        }
        Ok(())
    }
}

fn process_audio_block(
    prepared: &Plan,
    state: &mut SplitState,
    progress: Option<&ProgressBar>,
    block: AudioBlock,
) -> Result<()> {
    let channels = block.channels as usize;
    if channels == 0 {
        return Err("decoder produced zero channels".to_string());
    }
    if block.channels != prepared.input_meta.channels {
        return Err(format!(
            "decoder channel count {} does not match metadata {}",
            block.channels, prepared.input_meta.channels
        ));
    }

    let block_samples = block.sample_count();
    if block_samples == 0 {
        return Ok(());
    }
    if block.interleaved.len() != block_samples * channels {
        return Err("decoder produced invalid interleaved block size".to_string());
    }

    if let Some(pb) = progress {
        pb.inc(block_samples as u64);
    }

    let mut sample = block.sample_index;
    let mut local_offset = 0usize;
    let mut remaining = block_samples;

    while remaining > 0 {
        if state.track_index >= prepared.tracks.len() {
            break;
        }

        let track = prepared.tracks[state.track_index].clone();

        if sample < track.start {
            let skip = std::cmp::min(remaining, (track.start - sample) as usize);
            sample += skip as u64;
            local_offset += skip;
            remaining -= skip;
            if remaining == 0 {
                break;
            }
        }

        if sample >= track.end {
            state.finish_encoder()?;
            state.track_index += 1;
            continue;
        }

        let take = std::cmp::min(remaining, (track.end - sample) as usize);
        if take == 0 {
            break;
        }

        if state.encoder.is_none() {
            let encoder = start_track_encoder(
                &prepared.input_meta,
                &prepared.cue,
                &prepared.tracks,
                &track,
                prepared.compression_level,
                prepared.display_base_abs.as_deref(),
                progress,
            )?;
            state.encoder = Some(encoder);
        }

        let begin = local_offset * channels;
        let end = (local_offset + take) * channels;
        if let Some(encoder) = state.encoder.as_mut() {
            encoder.write_interleaved(&block.interleaved[begin..end], take as u32)?;
        }

        sample += take as u64;
        local_offset += take;
        remaining -= take;

        if sample >= track.end {
            state.finish_encoder()?;
            state.track_index += 1;
        }
    }

    Ok(())
}

fn build_output_tracks(
    cue: &CueDisc,
    output_dir: &Path,
    sample_rate: u32,
    total_samples: u64,
    check_exists: bool,
) -> Result<Vec<TrackSpan>> {
    let tracks = compute_track_spans(cue, sample_rate, total_samples)?;
    let output_paths = compute_output_paths(&tracks, output_dir, check_exists)?;
    let mut spans = Vec::with_capacity(tracks.len());
    for (track, output_path) in tracks.into_iter().zip(output_paths.into_iter()) {
        spans.push(TrackSpan {
            number: track.number,
            start: track.start,
            end: track.end,
            title: track.title,
            performer: track.performer,
            songwriter: track.songwriter,
            composer: track.composer,
            isrc: track.isrc,
            rem: track.rem,
            output_path,
        });
    }
    Ok(spans)
}

fn validate_cue_files(cue: &CueDisc, flac_path: &Path, enforce_filename_match: bool) -> Result<()> {
    let flac_name = flac_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| flac_path.to_string_lossy().to_string());

    let flac_stem = flac_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| flac_name.clone());

    let mut files = HashSet::new();
    for track in &cue.tracks {
        if let Some(name) = &track.filename {
            files.insert(name.clone());
        }
    }

    if files.len() > 1 {
        return Err("cue sheet references multiple audio files".to_string());
    }

    if !enforce_filename_match {
        return Ok(());
    }

    if let Some(name) = files.iter().next() {
        let cue_name = Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| name.clone());
        let cue_stem = Path::new(name)
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| cue_name.clone());

        if cue_name != flac_name && cue_stem != flac_stem {
            return Err(format!(
                "cue sheet references {}, but --flac is {}",
                cue_name, flac_name
            ));
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ComputedTrack {
    pub(crate) number: u32,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) title: Option<String>,
    pub(crate) performer: Option<String>,
    pub(crate) songwriter: Option<String>,
    pub(crate) composer: Option<String>,
    pub(crate) isrc: Option<String>,
    pub(crate) rem: CueRem,
}

pub(crate) fn compute_track_spans(
    cue: &CueDisc,
    sample_rate: u32,
    total_samples: u64,
) -> Result<Vec<ComputedTrack>> {
    if sample_rate == 0 {
        return Err("input sample rate is zero".to_string());
    }
    if !sample_rate.is_multiple_of(75) {
        return Err(format!(
            "sample rate {} is not divisible by 75 (CUE frames)",
            sample_rate
        ));
    }

    let mut tracks = Vec::with_capacity(cue.tracks.len());
    for (idx, track) in cue.tracks.iter().enumerate() {
        let start = frames_to_samples(track.start_frames, sample_rate)?;
        let length_frames = match track.length_frames {
            Some(length) if length >= 0 => Some(length),
            _ => {
                if idx + 1 < cue.tracks.len() {
                    let next_start = cue.tracks[idx + 1].start_frames;
                    Some(next_start - track.start_frames)
                } else {
                    None
                }
            }
        };

        let end = if let Some(length) = length_frames {
            start + frames_to_samples(length, sample_rate)?
        } else {
            if total_samples == 0 {
                return Err("input total samples unavailable for final track".to_string());
            }
            total_samples
        };

        if end <= start {
            return Err(format!("track {} has invalid length", track.number));
        }
        if total_samples > 0 && end > total_samples {
            return Err(format!(
                "track {} exceeds input total samples",
                track.number
            ));
        }

        tracks.push(ComputedTrack {
            number: track.number,
            start,
            end,
            title: track.title.clone(),
            performer: track.performer.clone(),
            songwriter: track.songwriter.clone(),
            composer: track.composer.clone(),
            isrc: track.isrc.clone(),
            rem: track.rem.clone(),
        });
    }

    Ok(tracks)
}

pub(crate) fn frames_to_samples(frames: i64, sample_rate: u32) -> Result<u64> {
    if frames < 0 {
        return Err("negative frame count in cue sheet".to_string());
    }
    if !sample_rate.is_multiple_of(75) {
        return Err(format!(
            "sample rate {} is not divisible by 75",
            sample_rate
        ));
    }
    let samples_per_frame = (sample_rate / 75) as u64;
    Ok(frames as u64 * samples_per_frame)
}

fn compute_output_paths(
    tracks: &[ComputedTrack],
    output_dir: &Path,
    check_exists: bool,
) -> Result<Vec<PathBuf>> {
    let width = tracks.len().to_string().len();
    let mut seen = HashSet::new();
    let mut paths = Vec::with_capacity(tracks.len());
    for track in tracks {
        let name = track
            .title
            .as_deref()
            .map(sanitize_filename)
            .unwrap_or_else(String::new);

        let base = if name.is_empty() {
            format!("{:0width$}", track.number, width = width)
        } else {
            format!("{:0width$} - {}", track.number, name, width = width)
        };

        let filename = format!("{}.flac", base);
        let path = output_dir.join(filename);

        if check_exists && path.exists() {
            return Err(format!("output file already exists: {}", path.display()));
        }
        if !seen.insert(path.clone()) {
            return Err(format!(
                "duplicate output filename for track {}",
                track.number
            ));
        }

        paths.push(path);
    }

    Ok(paths)
}

pub(crate) fn sanitize_filename(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch == '/' || ch == '\\' || ch == '\0' {
            out.push('_');
            continue;
        }
        if ch.is_control() {
            continue;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

fn ensure_output_paths_available(tracks: &[TrackSpan], overwrite: bool) -> Result<()> {
    for track in tracks {
        if track.output_path.exists() {
            if overwrite {
                fs::remove_file(&track.output_path).map_err(|err| {
                    format!(
                        "failed to remove existing file {}: {}",
                        track.output_path.display(),
                        err
                    )
                })?;
            } else {
                return Err(format!(
                    "output file already exists: {}",
                    track.output_path.display()
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn processed_flac_path(flac_path: &Path) -> Option<PathBuf> {
    let file_name = flac_path.file_name()?.to_str()?;
    Some(flac_path.with_file_name(format!("{}.processed", file_name)))
}

fn handle_original_flac(
    display_base_abs: Option<&Path>,
    flac_path: &Path,
    delete_original: bool,
    rename_original: bool,
) -> Result<()> {
    if delete_original {
        fs::remove_file(flac_path).map_err(|err| {
            format!(
                "split succeeded, but failed to delete original file {}: {}",
                flac_path.display(),
                err
            )
        })?;
        let display = display_path(display_base_abs, flac_path);
        println!(
            "{} {}",
            "Deleted".red().bold(),
            display.display().to_string().red()
        );
        return Ok(());
    }

    if rename_original {
        let renamed = processed_flac_path(flac_path)
            .ok_or_else(|| format!("failed to rename original file: {}", flac_path.display()))?;
        fs::rename(flac_path, &renamed).map_err(|err| {
            format!(
                "split succeeded, but failed to rename original file {} -> {}: {}",
                flac_path.display(),
                renamed.display(),
                err
            )
        })?;
        let from_display = display_path(display_base_abs, flac_path);
        let to_display = display_path(display_base_abs, &renamed);
        println!(
            "{} {} -> {}",
            "Renamed".yellow().bold(),
            from_display.display().to_string().yellow(),
            to_display.display().to_string().yellow()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_cue_files;
    use crate::types::{CueDisc, CueRem, CueTrack};
    use std::path::Path;

    fn cue_with_filenames(names: &[&str]) -> CueDisc {
        let tracks = names
            .iter()
            .enumerate()
            .map(|(idx, name)| CueTrack {
                number: (idx + 1) as u32,
                title: None,
                performer: None,
                songwriter: None,
                composer: None,
                isrc: None,
                start_frames: 0,
                length_frames: None,
                filename: Some((*name).to_string()),
                rem: CueRem::default(),
            })
            .collect();

        CueDisc {
            title: None,
            performer: None,
            songwriter: None,
            composer: None,
            genre: None,
            message: None,
            disc_id: None,
            rem: CueRem::default(),
            tracks,
        }
    }

    #[test]
    fn validate_cue_files_allows_mismatch_for_single_pair_mode() {
        let cue = cue_with_filenames(&["Different Name.flac"]);
        let flac_path = Path::new("Album.flac");
        assert!(validate_cue_files(&cue, flac_path, false).is_ok());
    }

    #[test]
    fn validate_cue_files_enforces_match_for_multi_pair_mode() {
        let cue = cue_with_filenames(&["Different Name.flac"]);
        let flac_path = Path::new("Album.flac");
        assert!(validate_cue_files(&cue, flac_path, true).is_err());
    }

    #[test]
    fn validate_cue_files_rejects_multiple_audio_files_always() {
        let cue = cue_with_filenames(&["Disc A.flac", "Disc B.flac"]);
        let flac_path = Path::new("Disc A.flac");
        assert!(validate_cue_files(&cue, flac_path, false).is_err());
        assert!(validate_cue_files(&cue, flac_path, true).is_err());
    }
}
