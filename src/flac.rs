use encoding_rs::Encoding;
use indicatif::ProgressBar;
use libflac_sys as flac;
use owo_colors::OwoColorize;
use std::collections::HashSet;
use std::ffi::{CString, c_void};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::cli::InputPath;
use crate::cue::parse_cue_file;
use crate::metadata::{build_track_metadata, parse_vorbis_comment};
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
}

pub(crate) struct PreparedSplit {
    context: DecodeContext,
    decoder: *mut flac::FLAC__StreamDecoder,
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

impl PreparedSplit {
    pub(crate) fn context(&self) -> &DecodeContext {
        &self.context
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

    pub(crate) fn execute(mut self) -> Result<()> {
        let progress = make_progress_bar(self.total_samples);
        self.context.progress = Some(progress.clone());

        ensure_output_paths_available(&self.context.tracks, self.overwrite)?;

        let ok = unsafe { flac::FLAC__stream_decoder_process_until_end_of_stream(self.decoder) };
        if ok == 0 {
            let error = self
                .context
                .error
                .take()
                .unwrap_or_else(|| "FLAC decoding failed".to_string());
            finish_progress(&mut self.context, "aborted");
            return Err(error);
        }

        if let Err(error) = self.context.finish_encoder() {
            finish_progress(&mut self.context, "aborted");
            return Err(error);
        }

        finish_progress(&mut self.context, "done");
        self.finish_decoder();
        self.context.cleanup();
        handle_original_flac(&self.flac_abs, self.delete_original, self.rename_original)
    }

    fn finish_decoder(&mut self) {
        if !self.decoder.is_null() {
            unsafe {
                flac::FLAC__stream_decoder_finish(self.decoder);
                flac::FLAC__stream_decoder_delete(self.decoder);
            }
            self.decoder = std::ptr::null_mut();
        }
    }
}

impl Drop for PreparedSplit {
    fn drop(&mut self) {
        self.finish_decoder();
        self.context.cleanup();
    }
}

pub(crate) fn prepare_split(options: SplitOptions) -> Result<PreparedSplit> {
    let (cue, warnings, encoding_used, encoding_autodetected) =
        parse_cue_file(&options.cue_input.abs, options.cue_encoding)?;
    validate_cue_files(&cue, &options.flac_input.abs)?;

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

    let mut context = DecodeContext::new(
        cue,
        output_dir,
        options.compression_level,
        options.display_base_abs.clone(),
    );

    let decoder = unsafe { flac::FLAC__stream_decoder_new() };
    if decoder.is_null() {
        return Err("failed to create FLAC decoder".to_string());
    }

    let flac_path_c = path_to_cstring(&options.flac_input.abs)?;
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

    if options.picture_enabled {
        let meta = context
            .input_meta
            .as_mut()
            .ok_or_else(|| "missing input metadata".to_string())?;
        add_external_picture(
            meta,
            &mut context.picture_names,
            &options.search_dir,
            options.picture_path.as_deref(),
        )?;
    }

    context.prepare_tracks(sample_rate, total_samples, false)?;
    Ok(PreparedSplit {
        context,
        decoder,
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

fn path_to_cstring(path: &Path) -> Result<CString> {
    let path_str = path.to_string_lossy();
    CString::new(path_str.as_bytes())
        .map_err(|_| format!("path contains NUL byte: {}", path.display()))
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

pub(crate) struct DecodeContext {
    pub(crate) cue: CueDisc,
    pub(crate) output_dir: PathBuf,
    pub(crate) input_meta: Option<InputMetadata>,
    pub(crate) tracks: Vec<TrackSpan>,
    track_index: usize,
    encoder: Option<TrackEncoder>,
    interleaved: Vec<i32>,
    pub(crate) error: Option<String>,
    next_sample_number: u64,
    pub(crate) progress: Option<ProgressBar>,
    pub(crate) compression_level: u8,
    pub(crate) display_base_abs: Option<PathBuf>,
    pub(crate) picture_names: Vec<String>,
}

impl DecodeContext {
    fn new(
        cue: CueDisc,
        output_dir: PathBuf,
        compression_level: u8,
        display_base_abs: Option<PathBuf>,
    ) -> Self {
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
            progress: None,
            compression_level,
            display_base_abs,
            picture_names: Vec::new(),
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
            return Err(format!("track {} exceeds FLAC total samples", track.number));
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
    }

    Ok(())
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
    if let Some(progress) = ctx.progress.as_ref() {
        progress.inc(block_samples as u64);
    }

    let mut block_start =
        if frame_ref.header.number_type == flac::FLAC__FRAME_NUMBER_TYPE_SAMPLE_NUMBER {
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
            && let Err(err) = encoder.write_interleaved(&ctx.interleaved, take as u32)
        {
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
            && flac::FLAC__stream_encoder_set_compression_level(
                encoder,
                ctx.compression_level as u32,
            ) != 0
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

    let mut metadata_blocks = build_track_metadata(meta, &ctx.cue, &ctx.tracks, track)?;
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
        flac::FLAC__stream_encoder_init_file(encoder, path_c.as_ptr(), None, std::ptr::null_mut())
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

    announce_track_start(ctx, track);

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

fn announce_track_start(ctx: &DecodeContext, track: &TrackSpan) {
    let title = track
        .title
        .clone()
        .unwrap_or_else(|| format!("Track {}", track.number));
    let line = format!(
        "{} {:02} - {} -> {}",
        "Creating".green().bold(),
        track.number,
        title,
        crate::cli::display_path(ctx.display_base_abs.as_deref(), &track.output_path).display()
    );
    if let Some(progress) = ctx.progress.as_ref() {
        progress.println(line);
    } else {
        println!("{}", line);
    }
}
