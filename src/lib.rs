use clap::Parser;
use cue_sys as cue;
use encoding_rs::{Encoding, UTF_8, WINDOWS_1251};
use libc::{c_int, c_void as libc_void};
use libflac_sys as flac;
use std::collections::HashSet;
use std::ffi::{c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, String>;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[arg(long)]
    flac: PathBuf,
    #[arg(long)]
    cue: PathBuf,
    #[arg(long, value_name = "ENCODING")]
    cue_encoding: Option<String>,
    #[arg(long)]
    dry_run: bool,
}

pub fn run() -> Result<()> {
    let args = Args::parse();
    let encoding = match args.cue_encoding {
        Some(label) => Some(resolve_encoding(&label)?),
        None => None,
    };
    split_flac(&args.flac, &args.cue, encoding, args.dry_run)
}

fn split_flac(
    flac_path: &Path,
    cue_path: &Path,
    cue_encoding: Option<&'static Encoding>,
    dry_run: bool,
) -> Result<()> {
    let (cue, warnings) = parse_cue_file(cue_path, cue_encoding)?;
    report_cue_warnings(&warnings);
    validate_cue_files(&cue, flac_path)?;

    let output_dir = flac_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut context = DecodeContext::new(cue, output_dir);

    let decoder = unsafe { flac::FLAC__stream_decoder_new() };
    if decoder.is_null() {
        return Err("failed to create FLAC decoder".to_string());
    }

    let flac_path_c = path_to_cstring(flac_path)?;
    let init_status = unsafe {
        flac::FLAC__stream_decoder_set_metadata_respond_all(decoder);
        flac::FLAC__stream_decoder_init_file(
            decoder,
            flac_path_c.as_ptr(),
            Some(decoder_write_callback),
            Some(decoder_metadata_callback),
            Some(decoder_error_callback),
            &mut context as *mut _ as *mut c_void,
        )
    };

    if init_status != flac::FLAC__STREAM_DECODER_INIT_STATUS_OK {
        unsafe {
            flac::FLAC__stream_decoder_delete(decoder);
        }
        return Err(format!(
            "failed to init FLAC decoder (status {})",
            init_status
        ));
    }

    let ok = unsafe { flac::FLAC__stream_decoder_process_until_end_of_metadata(decoder) };
    if ok == 0 {
        let error = context
            .error
            .take()
            .unwrap_or_else(|| "failed to read FLAC metadata".to_string());
        unsafe {
            flac::FLAC__stream_decoder_finish(decoder);
            flac::FLAC__stream_decoder_delete(decoder);
        }
        return Err(error);
    }

    let (sample_rate, total_samples) = {
        let meta = context
            .input_meta
            .as_ref()
            .ok_or_else(|| "missing FLAC stream info".to_string())?;
        (meta.sample_rate, meta.total_samples)
    };

    context.prepare_tracks(sample_rate, total_samples, !dry_run)?;

    if dry_run {
        print_dry_run(&context, flac_path, cue_path)?;
        unsafe {
            flac::FLAC__stream_decoder_finish(decoder);
            flac::FLAC__stream_decoder_delete(decoder);
        }
        context.cleanup();
        return Ok(());
    }

    let ok = unsafe { flac::FLAC__stream_decoder_process_until_end_of_stream(decoder) };
    if ok == 0 {
        let error = context
            .error
            .take()
            .unwrap_or_else(|| "FLAC decoding failed".to_string());
        unsafe {
            flac::FLAC__stream_decoder_finish(decoder);
            flac::FLAC__stream_decoder_delete(decoder);
        }
        return Err(error);
    }

    if let Err(error) = context.finish_encoder() {
        unsafe {
            flac::FLAC__stream_decoder_finish(decoder);
            flac::FLAC__stream_decoder_delete(decoder);
        }
        return Err(error);
    }

    unsafe {
        flac::FLAC__stream_decoder_finish(decoder);
        flac::FLAC__stream_decoder_delete(decoder);
    }

    context.cleanup();
    Ok(())
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    let path_str = path.to_string_lossy();
    CString::new(path_str.as_bytes())
        .map_err(|_| format!("path contains NUL byte: {}", path.display()))
}

#[derive(Debug, Clone, Default)]
struct CueRem {
    date: Option<String>,
    replaygain_album_gain: Option<String>,
    replaygain_album_peak: Option<String>,
    replaygain_track_gain: Option<String>,
    replaygain_track_peak: Option<String>,
}

#[derive(Debug, Clone)]
struct CueDisc {
    title: Option<String>,
    performer: Option<String>,
    songwriter: Option<String>,
    composer: Option<String>,
    genre: Option<String>,
    message: Option<String>,
    disc_id: Option<String>,
    rem: CueRem,
    tracks: Vec<CueTrack>,
}

#[derive(Debug, Clone)]
struct CueTrack {
    number: u32,
    title: Option<String>,
    performer: Option<String>,
    songwriter: Option<String>,
    composer: Option<String>,
    isrc: Option<String>,
    start_frames: i64,
    length_frames: Option<i64>,
    filename: Option<String>,
    rem: CueRem,
}

fn parse_cue_file(
    path: &Path,
    encoding: Option<&'static Encoding>,
) -> Result<(CueDisc, Vec<CueParseWarning>)> {
    let contents = fs::read(path)
        .map_err(|err| format!("failed to read cue file {}: {}", path.display(), err))?;
    let encoding = encoding.unwrap_or_else(|| detect_cue_encoding(&contents));
    parse_cue_from_bytes(&contents, encoding)
}

#[cfg(test)]
fn parse_cue_from_str(contents: &str) -> Result<CueDisc> {
    let (disc, _) = parse_cue_from_bytes(contents.as_bytes(), UTF_8)?;
    Ok(disc)
}

fn parse_cue_from_bytes(
    contents: &[u8],
    encoding: &'static Encoding,
) -> Result<(CueDisc, Vec<CueParseWarning>)> {
    let cue_cstr =
        CString::new(contents).map_err(|_| "cue file contains NUL byte".to_string())?;
    let capture = StderrCapture::start()?;
    let cd = unsafe { cue::cue_parse_string(cue_cstr.as_ptr()) };
    let stderr = capture.finish()?;
    let warnings = parse_cue_warnings(&stderr, contents, encoding);
    if cd.is_null() {
        let mut message = "failed to parse cue file".to_string();
        let warning_text = format_cue_warnings(&warnings);
        if !warning_text.is_empty() {
            message.push('\n');
            message.push_str(&warning_text);
        }
        return Err(message);
    }

    let result = unsafe { parse_cd(cd, encoding) };
    unsafe {
        cue::cd_delete(cd);
    }
    result.map(|disc| (disc, warnings))
}

unsafe fn parse_cd(cd: *mut cue::CdPointer, encoding: &'static Encoding) -> Result<CueDisc> {
    if cd.is_null() {
        return Err("cue parser returned null CD".to_string());
    }

    let disc_mode = unsafe { cue::cd_get_mode(cd) };
    if !matches!(disc_mode, cue::DiscMode::CD_DA) {
        return Err("cue sheet is not audio (CD_DA)".to_string());
    }

    let cdtext = unsafe { cue::cd_get_cdtext(cd) };
    let rem = cue_rem_from_ptr(unsafe { cue::cd_get_rem(cd) }, encoding);

    let title = cdtext_string(cdtext, cue::PTI::Title, encoding);
    let performer = cdtext_string(cdtext, cue::PTI::Performer, encoding);
    let songwriter = cdtext_string(cdtext, cue::PTI::Songwriter, encoding);
    let composer = cdtext_string(cdtext, cue::PTI::Composer, encoding);
    let genre = cdtext_string(cdtext, cue::PTI::Genre, encoding);
    let message = cdtext_string(cdtext, cue::PTI::Message, encoding);
    let disc_id = cdtext_string(cdtext, cue::PTI::DiscID, encoding);

    let ntrack = unsafe { cue::cd_get_ntrack(cd) };
    if ntrack <= 0 {
        return Err("cue sheet has no tracks".to_string());
    }

    let mut tracks = Vec::with_capacity(ntrack as usize);
    for index in 1..=ntrack {
        let track_ptr = unsafe { cue::cd_get_track(cd, index) };
        if track_ptr.is_null() {
            return Err(format!("failed to read track {}", index));
        }

        if !matches!(unsafe { cue::track_get_mode(track_ptr) }, cue::TrackMode::Audio) {
            return Err(format!("track {} is not audio", index));
        }

        let track_cdtext = unsafe { cue::track_get_cdtext(track_ptr) };
        let track_rem = cue_rem_from_ptr(unsafe { cue::track_get_rem(track_ptr) }, encoding);
        let filename =
            opt_cstr_with_encoding(unsafe { cue::track_get_filename(track_ptr) }, encoding);

        let start = unsafe { cue::track_get_start(track_ptr) };
        if start < 0 {
            return Err(format!("track {} has invalid start", index));
        }

        let length = unsafe { cue::track_get_length(track_ptr) };
        let length_frames = if length < 0 { None } else { Some(length) };

        let track = CueTrack {
            number: index as u32,
            title: cdtext_string(track_cdtext, cue::PTI::Title, encoding),
            performer: cdtext_string(track_cdtext, cue::PTI::Performer, encoding),
            songwriter: cdtext_string(track_cdtext, cue::PTI::Songwriter, encoding),
            composer: cdtext_string(track_cdtext, cue::PTI::Composer, encoding),
            isrc: opt_cstr_with_encoding(unsafe { cue::track_get_isrc(track_ptr) }, encoding),
            start_frames: start,
            length_frames,
            filename,
            rem: track_rem,
        };
        tracks.push(track);
    }

    Ok(CueDisc {
        title,
        performer,
        songwriter,
        composer,
        genre,
        message,
        disc_id,
        rem,
        tracks,
    })
}

fn cdtext_string(
    cdtext: *mut cue::CdtextPointer,
    pti: cue::PTI,
    encoding: &'static Encoding,
) -> Option<String> {
    if cdtext.is_null() {
        return None;
    }
    let ptr = unsafe { cue::cdtext_get(pti, cdtext) };
    opt_cstr_with_encoding(ptr, encoding)
}

fn opt_cstr_with_encoding(
    ptr: *mut std::os::raw::c_char,
    encoding: &'static Encoding,
) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let bytes = unsafe { CStr::from_ptr(ptr).to_bytes() };
    let (decoded, _, _) = encoding.decode(bytes);
    Some(decoded.into_owned())
}

const REM_DATE: u32 = 0;
const REM_REPLAYGAIN_ALBUM_GAIN: u32 = 1;
const REM_REPLAYGAIN_ALBUM_PEAK: u32 = 2;
const REM_REPLAYGAIN_TRACK_GAIN: u32 = 3;
const REM_REPLAYGAIN_TRACK_PEAK: u32 = 4;

fn cue_rem_from_ptr(rem: *mut cue::RemPointer, encoding: &'static Encoding) -> CueRem {
    if rem.is_null() {
        return CueRem::default();
    }

    CueRem {
        date: rem_get_string(rem, REM_DATE, encoding),
        replaygain_album_gain: rem_get_string(rem, REM_REPLAYGAIN_ALBUM_GAIN, encoding),
        replaygain_album_peak: rem_get_string(rem, REM_REPLAYGAIN_ALBUM_PEAK, encoding),
        replaygain_track_gain: rem_get_string(rem, REM_REPLAYGAIN_TRACK_GAIN, encoding),
        replaygain_track_peak: rem_get_string(rem, REM_REPLAYGAIN_TRACK_PEAK, encoding),
    }
}

fn rem_get_string(
    rem: *mut cue::RemPointer,
    key: u32,
    encoding: &'static Encoding,
) -> Option<String> {
    if rem.is_null() {
        return None;
    }
    let ptr = unsafe { cue::rem_get(key, rem) };
    opt_cstr_with_encoding(ptr, encoding)
}

fn resolve_encoding(label: &str) -> Result<&'static Encoding> {
    Encoding::for_label(label.as_bytes())
        .ok_or_else(|| format!("unsupported cue encoding: {}", label))
}

fn detect_cue_encoding(bytes: &[u8]) -> &'static Encoding {
    if std::str::from_utf8(bytes).is_ok() {
        UTF_8
    } else {
        WINDOWS_1251
    }
}

fn validate_cue_files(cue: &CueDisc, flac_path: &Path) -> Result<()> {
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
struct InputMetadata {
    sample_rate: u32,
    channels: u32,
    bits_per_sample: u32,
    total_samples: u64,
    vendor: Option<String>,
    comments: Vec<(String, String)>,
    pictures: Vec<*mut flac::FLAC__StreamMetadata>,
}

impl InputMetadata {
    fn new() -> Self {
        Self {
            sample_rate: 0,
            channels: 0,
            bits_per_sample: 0,
            total_samples: 0,
            vendor: None,
            comments: Vec::new(),
            pictures: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct TrackSpan {
    number: u32,
    start: u64,
    end: u64,
    title: Option<String>,
    performer: Option<String>,
    songwriter: Option<String>,
    composer: Option<String>,
    isrc: Option<String>,
    rem: CueRem,
    output_path: PathBuf,
}

struct DecodeContext {
    cue: CueDisc,
    output_dir: PathBuf,
    input_meta: Option<InputMetadata>,
    tracks: Vec<TrackSpan>,
    track_index: usize,
    encoder: Option<TrackEncoder>,
    interleaved: Vec<i32>,
    error: Option<String>,
    next_sample_number: u64,
}

impl DecodeContext {
    fn new(cue: CueDisc, output_dir: PathBuf) -> Self {
        Self {
            cue,
            output_dir,
            input_meta: None,
            tracks: Vec::new(),
            track_index: 0,
            encoder: None,
            interleaved: Vec::new(),
            error: None,
            next_sample_number: 0,
        }
    }

    fn prepare_tracks(
        &mut self,
        sample_rate: u32,
        total_samples: u64,
        check_exists: bool,
    ) -> Result<()> {
        let tracks = compute_track_spans(&self.cue, sample_rate, total_samples)?;
        let output_paths = compute_output_paths(&tracks, &self.output_dir, check_exists)?;
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
        self.tracks = spans;
        Ok(())
    }

    fn finish_encoder(&mut self) -> Result<()> {
        if let Some(mut encoder) = self.encoder.take() {
            encoder.finish()?;
        }
        Ok(())
    }

    fn cleanup(&mut self) {
        if let Some(meta) = self.input_meta.take() {
            for picture in meta.pictures {
                unsafe {
                    if !picture.is_null() {
                        flac::FLAC__metadata_object_delete(picture);
                    }
                }
            }
        }
    }
}

struct TrackEncoder {
    encoder: *mut flac::FLAC__StreamEncoder,
}

impl TrackEncoder {
    fn write_interleaved(&mut self, interleaved: &[i32], samples: u32) -> Result<()> {
        if self.encoder.is_null() {
            return Err("encoder not initialized".to_string());
        }
        let ok = unsafe {
            flac::FLAC__stream_encoder_process_interleaved(
                self.encoder,
                interleaved.as_ptr(),
                samples,
            )
        };
        if ok == 0 {
            return Err("failed to encode FLAC frame".to_string());
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.encoder.is_null() {
            return Ok(());
        }
        let ok = unsafe { flac::FLAC__stream_encoder_finish(self.encoder) };
        unsafe {
            flac::FLAC__stream_encoder_delete(self.encoder);
        }
        self.encoder = std::ptr::null_mut();
        if ok == 0 {
            return Err("failed to finalize FLAC encoder".to_string());
        }
        Ok(())
    }
}

impl Drop for TrackEncoder {
    fn drop(&mut self) {
        if !self.encoder.is_null() {
            unsafe {
                flac::FLAC__stream_encoder_finish(self.encoder);
                flac::FLAC__stream_encoder_delete(self.encoder);
            }
            self.encoder = std::ptr::null_mut();
        }
    }
}

struct ComputedTrack {
    number: u32,
    start: u64,
    end: u64,
    title: Option<String>,
    performer: Option<String>,
    songwriter: Option<String>,
    composer: Option<String>,
    isrc: Option<String>,
    rem: CueRem,
}

fn compute_track_spans(cue: &CueDisc, sample_rate: u32, total_samples: u64) -> Result<Vec<ComputedTrack>> {
    if sample_rate == 0 {
        return Err("FLAC sample rate is zero".to_string());
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
                return Err("FLAC total samples unavailable for final track".to_string());
            }
            total_samples
        };

        if end <= start {
            return Err(format!("track {} has invalid length", track.number));
        }
        if total_samples > 0 && end > total_samples {
            return Err(format!(
                "track {} exceeds FLAC total samples",
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

fn frames_to_samples(frames: i64, sample_rate: u32) -> Result<u64> {
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

fn sanitize_filename(value: &str) -> String {
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

fn print_dry_run(context: &DecodeContext, flac_path: &Path, cue_path: &Path) -> Result<()> {
    let meta = context
        .input_meta
        .as_ref()
        .ok_or_else(|| "missing input metadata".to_string())?;
    if meta.sample_rate == 0 {
        return Err("invalid sample rate in metadata".to_string());
    }
    if meta.sample_rate % 75 != 0 {
        return Err(format!(
            "sample rate {} is not divisible by 75 (CUE frames)",
            meta.sample_rate
        ));
    }

    let samples_per_frame = (meta.sample_rate / 75) as u64;

    println!("Dry run");
    println!("  FLAC: {}", flac_path.display());
    println!("  CUE:  {}", cue_path.display());
    println!(
        "  Tracks: {} ({} Hz, {} ch, {} bits)",
        context.tracks.len(),
        meta.sample_rate,
        meta.channels,
        meta.bits_per_sample
    );

    for track in &context.tracks {
        let start_frames = track.start / samples_per_frame;
        let end_frames = track.end / samples_per_frame;
        let length_frames = end_frames.saturating_sub(start_frames);
        let duration_secs = (track.end - track.start) as f64 / meta.sample_rate as f64;

        let title = track
            .title
            .clone()
            .unwrap_or_else(|| format!("Track {}", track.number));
        let exists = track.output_path.exists();

        println!(
            "{:02}. {} -> {}{}",
            track.number,
            title,
            track.output_path.display(),
            if exists { " (exists)" } else { "" }
        );
        println!(
            "    start {} end {} length {} ({:.3}s)",
            format_msf(start_frames),
            format_msf(end_frames),
            format_msf(length_frames),
            duration_secs
        );
    }

    Ok(())
}

fn format_msf(frames: u64) -> String {
    let total_seconds = frames / 75;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let frames = frames % 75;
    format!("{:02}:{:02}:{:02}", minutes, seconds, frames)
}

unsafe extern "C" fn decoder_metadata_callback(
    _decoder: *const flac::FLAC__StreamDecoder,
    metadata: *const flac::FLAC__StreamMetadata,
    client_data: *mut c_void,
) {
    if client_data.is_null() || metadata.is_null() {
        return;
    }
    let ctx = unsafe { &mut *(client_data as *mut DecodeContext) };
    let meta = ctx.input_meta.get_or_insert_with(InputMetadata::new);

    let metadata_ref = unsafe { &*metadata };
    match metadata_ref.type_ {
        flac::FLAC__METADATA_TYPE_STREAMINFO => {
            let info = unsafe { metadata_ref.data.stream_info };
            meta.sample_rate = info.sample_rate;
            meta.channels = info.channels;
            meta.bits_per_sample = info.bits_per_sample;
            meta.total_samples = info.total_samples;
        }
        flac::FLAC__METADATA_TYPE_VORBIS_COMMENT => {
            let (vendor, comments) = parse_vorbis_comment(metadata_ref);
            meta.vendor = vendor;
            meta.comments = comments;
        }
        flac::FLAC__METADATA_TYPE_PICTURE => {
            let clone = unsafe { flac::FLAC__metadata_object_clone(metadata as *const _) };
            if !clone.is_null() {
                meta.pictures.push(clone);
            }
        }
        _ => {}
    }
}

unsafe extern "C" fn decoder_error_callback(
    _decoder: *const flac::FLAC__StreamDecoder,
    status: flac::FLAC__StreamDecoderErrorStatus,
    client_data: *mut c_void,
) {
    if client_data.is_null() {
        return;
    }
    let ctx = unsafe { &mut *(client_data as *mut DecodeContext) };
    ctx.error = Some(format!("FLAC decoder error status {}", status));
}

unsafe extern "C" fn decoder_write_callback(
    _decoder: *const flac::FLAC__StreamDecoder,
    frame: *const flac::FLAC__Frame,
    buffer: *const *const i32,
    client_data: *mut c_void,
) -> flac::FLAC__StreamDecoderWriteStatus {
    if client_data.is_null() || frame.is_null() || buffer.is_null() {
        return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
    }
    let ctx = unsafe { &mut *(client_data as *mut DecodeContext) };
    if ctx.error.is_some() {
        return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
    }
    if ctx.input_meta.is_none() {
        ctx.error = Some("missing FLAC metadata before audio data".to_string());
        return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
    }

    let frame_ref = unsafe { &*frame };
    let block_samples = frame_ref.header.blocksize as usize;
    if block_samples == 0 {
        return flac::FLAC__STREAM_DECODER_WRITE_STATUS_CONTINUE;
    }

    let mut block_start = if frame_ref.header.number_type
        == flac::FLAC__FRAME_NUMBER_TYPE_SAMPLE_NUMBER
    {
        unsafe { frame_ref.header.number.sample_number }
    } else {
        ctx.next_sample_number
    };
    ctx.next_sample_number = block_start + block_samples as u64;

    let mut local_offset = 0usize;
    let mut remaining = block_samples;

    while remaining > 0 {
        if ctx.track_index >= ctx.tracks.len() {
            break;
        }

        let track = &ctx.tracks[ctx.track_index];

        if block_start < track.start {
            let skip = std::cmp::min(remaining, (track.start - block_start) as usize);
            block_start += skip as u64;
            local_offset += skip;
            remaining -= skip;
            if remaining == 0 {
                break;
            }
        }

        if block_start >= track.end {
            if let Err(err) = ctx.finish_encoder() {
                ctx.error = Some(err);
                return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
            }
            ctx.track_index += 1;
            continue;
        }

        let take = std::cmp::min(remaining, (track.end - block_start) as usize);
        if take == 0 {
            break;
        }

        if ctx.encoder.is_none() {
            match start_track_encoder(ctx, track) {
                Ok(enc) => ctx.encoder = Some(enc),
                Err(err) => {
                    ctx.error = Some(err);
                    return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
                }
            }
        }

        let channels = match ctx.input_meta.as_ref() {
            Some(meta) if meta.channels > 0 => meta.channels as usize,
            _ => {
                ctx.error = Some("invalid channel count in metadata".to_string());
                return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
            }
        };

        interleave_samples(buffer, local_offset, take, &mut ctx.interleaved, channels);
        if let Some(encoder) = ctx.encoder.as_mut()
            && let Err(err) = encoder.write_interleaved(&ctx.interleaved, take as u32) {
                ctx.error = Some(err);
                return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
            }

        block_start += take as u64;
        local_offset += take;
        remaining -= take;

        if block_start >= track.end {
            if let Err(err) = ctx.finish_encoder() {
                ctx.error = Some(err);
                return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
            }
            ctx.track_index += 1;
        }
    }

    flac::FLAC__STREAM_DECODER_WRITE_STATUS_CONTINUE
}

fn interleave_samples(
    buffer: *const *const i32,
    offset: usize,
    samples: usize,
    out: &mut Vec<i32>,
    channels: usize,
) {
    if channels == 0 {
        return;
    }

    out.clear();
    out.reserve(samples * channels);

    for i in 0..samples {
        for ch in 0..channels {
            unsafe {
                let chan_ptr = *buffer.add(ch);
                out.push(*chan_ptr.add(offset + i));
            }
        }
    }
}

fn start_track_encoder(ctx: &DecodeContext, track: &TrackSpan) -> Result<TrackEncoder> {
    let meta = ctx
        .input_meta
        .as_ref()
        .ok_or_else(|| "missing input metadata".to_string())?;

    let encoder = unsafe { flac::FLAC__stream_encoder_new() };
    if encoder.is_null() {
        return Err("failed to create FLAC encoder".to_string());
    }

    let ok = unsafe {
        flac::FLAC__stream_encoder_set_channels(encoder, meta.channels) != 0
            && flac::FLAC__stream_encoder_set_bits_per_sample(encoder, meta.bits_per_sample) != 0
            && flac::FLAC__stream_encoder_set_sample_rate(encoder, meta.sample_rate) != 0
            && flac::FLAC__stream_encoder_set_compression_level(encoder, 5) != 0
    };
    if !ok {
        unsafe {
            flac::FLAC__stream_encoder_delete(encoder);
        }
        return Err("failed to configure FLAC encoder".to_string());
    }

    let track_samples = track.end - track.start;
    unsafe {
        flac::FLAC__stream_encoder_set_total_samples_estimate(encoder, track_samples);
    }

    let mut metadata_blocks = build_track_metadata(ctx, track)?;
    if !metadata_blocks.is_empty() {
        let ok = unsafe {
            flac::FLAC__stream_encoder_set_metadata(
                encoder,
                metadata_blocks.as_mut_ptr(),
                metadata_blocks.len() as u32,
            ) != 0
        };
        if !ok {
            cleanup_metadata_blocks(&mut metadata_blocks);
            unsafe {
                flac::FLAC__stream_encoder_delete(encoder);
            }
            return Err("failed to set FLAC metadata".to_string());
        }
    }

    let path_c = path_to_cstring(&track.output_path)?;
    let init_status = unsafe {
        flac::FLAC__stream_encoder_init_file(
            encoder,
            path_c.as_ptr(),
            None,
            std::ptr::null_mut(),
        )
    };

    cleanup_metadata_blocks(&mut metadata_blocks);

    if init_status != flac::FLAC__STREAM_ENCODER_INIT_STATUS_OK {
        unsafe {
            flac::FLAC__stream_encoder_delete(encoder);
        }
        return Err(format!(
            "failed to init encoder for {}",
            track.output_path.display()
        ));
    }

    Ok(TrackEncoder { encoder })
}

fn cleanup_metadata_blocks(blocks: &mut Vec<*mut flac::FLAC__StreamMetadata>) {
    for block in blocks.drain(..) {
        if !block.is_null() {
            unsafe {
                flac::FLAC__metadata_object_delete(block);
            }
        }
    }
}

fn build_track_metadata(
    ctx: &DecodeContext,
    track: &TrackSpan,
) -> Result<Vec<*mut flac::FLAC__StreamMetadata>> {
    let meta = ctx
        .input_meta
        .as_ref()
        .ok_or_else(|| "missing input metadata".to_string())?;

    let mut blocks = Vec::new();

    let comment = build_vorbis_comment(meta, ctx, track)?;
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
    ctx: &DecodeContext,
    track: &TrackSpan,
) -> Result<*mut flac::FLAC__StreamMetadata> {
    let object =
        unsafe { flac::FLAC__metadata_object_new(flac::FLAC__METADATA_TYPE_VORBIS_COMMENT) };
    if object.is_null() {
        return Err("failed to allocate Vorbis comment metadata".to_string());
    }

    let vendor = meta
        .vendor
        .as_deref()
        .unwrap_or("flac-cue-split");
    if let Err(err) = set_vendor_string(object, vendor) {
        unsafe {
            flac::FLAC__metadata_object_delete(object);
        }
        return Err(err);
    }

    let overrides = build_override_tags(ctx, track);
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

fn set_vendor_string(
    object: *mut flac::FLAC__StreamMetadata,
    vendor: &str,
) -> Result<()> {
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

fn append_comment(
    object: *mut flac::FLAC__StreamMetadata,
    key: &str,
    value: &str,
) -> Result<()> {
    let comment = format!("{}={}", key, value);
    let bytes = comment.as_bytes();
    let entry = flac::FLAC__StreamMetadata_VorbisComment_Entry {
        length: bytes.len() as u32,
        entry: bytes.as_ptr() as *mut flac::FLAC__byte,
    };

    let ok = unsafe {
        flac::FLAC__metadata_object_vorbiscomment_append_comment(object, entry, 1) != 0
    };
    if !ok {
        return Err(format!("failed to append Vorbis comment {}", key));
    }
    Ok(())
}

fn build_override_tags(ctx: &DecodeContext, track: &TrackSpan) -> Vec<(String, String)> {
    let mut tags = Vec::new();
    let total_tracks = ctx.tracks.len();

    let title = track
        .title
        .clone()
        .unwrap_or_else(|| format!("Track {}", track.number));
    tags.push(("TITLE".to_string(), title));

    let performer = track
        .performer
        .clone()
        .or_else(|| ctx.cue.performer.clone());
    if let Some(artist) = performer {
        tags.push(("ARTIST".to_string(), artist));
    }

    if let Some(album) = &ctx.cue.title {
        tags.push(("ALBUM".to_string(), album.clone()));
    }

    if let Some(album_artist) = &ctx.cue.performer {
        tags.push(("ALBUMARTIST".to_string(), album_artist.clone()));
    }

    if let Some(genre) = &ctx.cue.genre {
        tags.push(("GENRE".to_string(), genre.clone()));
    }

    if let Some(message) = &ctx.cue.message {
        tags.push(("COMMENT".to_string(), message.clone()));
    }

    if let Some(disc_id) = &ctx.cue.disc_id {
        tags.push(("DISCID".to_string(), disc_id.clone()));
    }

    let composer = track
        .composer
        .clone()
        .or_else(|| track.songwriter.clone())
        .or_else(|| ctx.cue.composer.clone())
        .or_else(|| ctx.cue.songwriter.clone());
    if let Some(comp) = composer {
        tags.push(("COMPOSER".to_string(), comp));
    }

    if let Some(isrc) = &track.isrc {
        tags.push(("ISRC".to_string(), isrc.clone()));
    }

    tags.push(("TRACKNUMBER".to_string(), track.number.to_string()));
    tags.push(("TRACKTOTAL".to_string(), total_tracks.to_string()));
    tags.push(("TOTALTRACKS".to_string(), total_tracks.to_string()));

    if let Some(date) = track.rem.date.clone().or_else(|| ctx.cue.rem.date.clone()) {
        tags.push(("DATE".to_string(), date));
    }

    if let Some(gain) = &ctx.cue.rem.replaygain_album_gain {
        tags.push(("REPLAYGAIN_ALBUM_GAIN".to_string(), gain.clone()));
    }
    if let Some(peak) = &ctx.cue.rem.replaygain_album_peak {
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

fn merge_tags(base: &[(String, String)], overrides: &[(String, String)]) -> Vec<(String, String)> {
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

fn parse_vorbis_comment(
    metadata: &flac::FLAC__StreamMetadata,
) -> (Option<String>, Vec<(String, String)>) {
    let mut vendor = None;
    let mut comments = Vec::new();

    if metadata.type_ != flac::FLAC__METADATA_TYPE_VORBIS_COMMENT {
        return (vendor, comments);
    }

    let vc = unsafe { metadata.data.vorbis_comment };

    vendor = parse_vorbis_entry(&vc.vendor_string);

    let entries = unsafe {
        std::slice::from_raw_parts(vc.comments, vc.num_comments as usize)
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_to_samples_44100() {
        assert_eq!(frames_to_samples(75, 44100).unwrap(), 44100);
        assert_eq!(frames_to_samples(0, 44100).unwrap(), 0);
    }

    #[test]
    fn frames_to_samples_invalid_rate() {
        assert!(frames_to_samples(1, 44101).is_err());
    }

    #[test]
    fn parse_cue_and_compute_spans() {
        let cue = r#"
REM DATE 2020
PERFORMER "Artist"
TITLE "Album"
FILE "test.flac" WAVE
  TRACK 01 AUDIO
    TITLE "One"
    PERFORMER "Artist"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Two"
    INDEX 01 00:01:00
"#;

        let disc = parse_cue_from_str(cue).unwrap();
        assert_eq!(disc.tracks.len(), 2);
        assert_eq!(disc.tracks[0].start_frames, 0);
        assert_eq!(disc.tracks[1].start_frames, 75);

        let spans = compute_track_spans(&disc, 44100, 88200).unwrap();
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 44100);
        assert_eq!(spans[1].start, 44100);
        assert_eq!(spans[1].end, 88200);
    }

    #[test]
    fn sanitize_filename_removes_separators() {
        assert_eq!(sanitize_filename("Track/01"), "Track_01");
        assert_eq!(sanitize_filename("Track\\02"), "Track_02");
    }
}
