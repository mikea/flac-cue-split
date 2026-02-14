use indicatif::ProgressBar;
use libflac_sys as flac;
use owo_colors::OwoColorize;
use std::collections::VecDeque;
use std::ffi::{CString, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use crate::Result;
use crate::cli::display_path;
use crate::decoder::{AudioBlock, Decoder, DecoderMetadata};
use crate::metadata::{build_track_metadata, parse_vorbis_comment};
use crate::types::{CueDisc, InputMetadata, TrackSpan};

#[derive(Debug)]
pub(crate) struct FlacMetadata {
    ptr: NonNull<flac::FLAC__StreamMetadata>,
}

impl FlacMetadata {
    pub(crate) fn new(kind: flac::FLAC__MetadataType) -> Result<Self> {
        let ptr = unsafe { flac::FLAC__metadata_object_new(kind) };
        Self::from_raw(ptr, "failed to allocate FLAC metadata")
    }

    pub(crate) fn clone_from_raw(raw: *const flac::FLAC__StreamMetadata) -> Option<Self> {
        let ptr = unsafe { flac::FLAC__metadata_object_clone(raw) };
        NonNull::new(ptr).map(|ptr| Self { ptr })
    }

    pub(crate) fn try_clone(&self) -> Option<Self> {
        Self::clone_from_raw(self.as_ptr())
    }

    pub(crate) fn as_ptr(&self) -> *const flac::FLAC__StreamMetadata {
        self.ptr.as_ptr() as *const flac::FLAC__StreamMetadata
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut flac::FLAC__StreamMetadata {
        self.ptr.as_ptr()
    }

    pub(crate) fn collect_raw_ptrs(
        blocks: &mut [FlacMetadata],
    ) -> Vec<*mut flac::FLAC__StreamMetadata> {
        blocks.iter_mut().map(Self::as_mut_ptr).collect()
    }

    pub(crate) fn as_mut(&mut self) -> &mut flac::FLAC__StreamMetadata {
        unsafe { self.ptr.as_mut() }
    }

    fn from_raw(ptr: *mut flac::FLAC__StreamMetadata, err: &str) -> Result<Self> {
        match NonNull::new(ptr) {
            Some(ptr) => Ok(Self { ptr }),
            None => Err(err.to_string()),
        }
    }
}

impl Drop for FlacMetadata {
    fn drop(&mut self) {
        unsafe {
            flac::FLAC__metadata_object_delete(self.ptr.as_ptr());
        }
    }
}

pub(crate) struct FlacDecoder {
    path: PathBuf,
}

impl FlacDecoder {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn read_metadata_internal(&self) -> Result<DecoderMetadata> {
        let mut decoder = FlacStreamDecoder::new()?;
        let mut state = Box::new(FlacMetadataState::new());

        decoder.init_file(
            &self.path,
            None,
            Some(flac_metadata_callback),
            Some(flac_metadata_error_callback),
            state.as_mut() as *mut _ as *mut c_void,
        )?;

        let ok = decoder.process_until_end_of_metadata();
        if ok == 0 {
            return Err(state
                .error
                .take()
                .unwrap_or_else(|| "failed to read FLAC metadata".to_string()));
        }
        if let Some(err) = state.error.take() {
            return Err(err);
        }

        let input_meta = std::mem::replace(&mut state.meta, InputMetadata::new());
        Ok(DecoderMetadata {
            input_meta,
            picture_names: Vec::new(),
        })
    }

    fn block_iter(&self) -> Result<FlacBlockIter> {
        FlacBlockIter::new(&self.path)
    }
}

impl Decoder for FlacDecoder {
    fn read_metadata(&mut self) -> Result<DecoderMetadata> {
        self.read_metadata_internal()
    }

    fn into_blocks(self: Box<Self>) -> Result<Box<dyn Iterator<Item = Result<AudioBlock>>>> {
        Ok(Box::new(self.block_iter()?))
    }
}

struct FlacStreamDecoder {
    decoder: *mut flac::FLAC__StreamDecoder,
}

impl FlacStreamDecoder {
    fn new() -> Result<Self> {
        let decoder = unsafe { flac::FLAC__stream_decoder_new() };
        if decoder.is_null() {
            return Err("failed to create FLAC decoder".to_string());
        }
        Ok(Self { decoder })
    }

    fn init_file(
        &mut self,
        path: &Path,
        write_cb: Option<
            unsafe extern "C" fn(
                *const flac::FLAC__StreamDecoder,
                *const flac::FLAC__Frame,
                *const *const i32,
                *mut c_void,
            ) -> flac::FLAC__StreamDecoderWriteStatus,
        >,
        metadata_cb: Option<
            unsafe extern "C" fn(
                *const flac::FLAC__StreamDecoder,
                *const flac::FLAC__StreamMetadata,
                *mut c_void,
            ),
        >,
        error_cb: Option<
            unsafe extern "C" fn(
                *const flac::FLAC__StreamDecoder,
                flac::FLAC__StreamDecoderErrorStatus,
                *mut c_void,
            ),
        >,
        client_data: *mut c_void,
    ) -> Result<()> {
        let path_c = path_to_cstring(path)?;
        let init_status = unsafe {
            flac::FLAC__stream_decoder_set_metadata_respond_all(self.decoder);
            flac::FLAC__stream_decoder_init_file(
                self.decoder,
                path_c.as_ptr(),
                write_cb,
                metadata_cb,
                error_cb,
                client_data,
            )
        };
        if init_status != flac::FLAC__STREAM_DECODER_INIT_STATUS_OK {
            return Err(format!(
                "failed to init FLAC decoder (status {})",
                init_status
            ));
        }
        Ok(())
    }

    fn process_until_end_of_metadata(&mut self) -> i32 {
        unsafe { flac::FLAC__stream_decoder_process_until_end_of_metadata(self.decoder) }
    }

    fn process_single(&mut self) -> i32 {
        unsafe { flac::FLAC__stream_decoder_process_single(self.decoder) }
    }

    fn state(&self) -> flac::FLAC__StreamDecoderState {
        unsafe { flac::FLAC__stream_decoder_get_state(self.decoder) }
    }
}

impl Drop for FlacStreamDecoder {
    fn drop(&mut self) {
        if !self.decoder.is_null() {
            unsafe {
                flac::FLAC__stream_decoder_finish(self.decoder);
                flac::FLAC__stream_decoder_delete(self.decoder);
            }
            self.decoder = std::ptr::null_mut();
        }
    }
}

struct FlacMetadataState {
    meta: InputMetadata,
    error: Option<String>,
}

impl FlacMetadataState {
    fn new() -> Self {
        Self {
            meta: InputMetadata::new(),
            error: None,
        }
    }
}

unsafe extern "C" fn flac_metadata_callback(
    _decoder: *const flac::FLAC__StreamDecoder,
    metadata: *const flac::FLAC__StreamMetadata,
    client_data: *mut c_void,
) {
    if client_data.is_null() || metadata.is_null() {
        return;
    }

    let state = unsafe { &mut *(client_data as *mut FlacMetadataState) };
    let metadata_ref = unsafe { &*metadata };

    match metadata_ref.type_ {
        flac::FLAC__METADATA_TYPE_STREAMINFO => {
            let info = unsafe { metadata_ref.data.stream_info };
            state.meta.sample_rate = info.sample_rate;
            state.meta.channels = info.channels;
            state.meta.bits_per_sample = info.bits_per_sample;
            state.meta.total_samples = info.total_samples;
        }
        flac::FLAC__METADATA_TYPE_VORBIS_COMMENT => {
            let (vendor, comments) = parse_vorbis_comment(metadata_ref);
            state.meta.vendor = vendor;
            state.meta.comments = comments;
        }
        flac::FLAC__METADATA_TYPE_PICTURE => {
            if let Some(clone) = FlacMetadata::clone_from_raw(metadata) {
                state.meta.pictures.push(clone);
            }
        }
        _ => {}
    }
}

unsafe extern "C" fn flac_metadata_error_callback(
    _decoder: *const flac::FLAC__StreamDecoder,
    status: flac::FLAC__StreamDecoderErrorStatus,
    client_data: *mut c_void,
) {
    if client_data.is_null() {
        return;
    }
    let state = unsafe { &mut *(client_data as *mut FlacMetadataState) };
    state.error = Some(format!("FLAC decoder error status {}", status));
}

struct FlacBlockState {
    blocks: VecDeque<AudioBlock>,
    error: Option<String>,
    next_sample_number: u64,
}

impl FlacBlockState {
    fn new() -> Self {
        Self {
            blocks: VecDeque::new(),
            error: None,
            next_sample_number: 0,
        }
    }
}

struct FlacBlockIter {
    decoder: FlacStreamDecoder,
    state: Box<FlacBlockState>,
    done: bool,
}

impl FlacBlockIter {
    fn new(path: &Path) -> Result<Self> {
        let mut decoder = FlacStreamDecoder::new()?;
        let mut state = Box::new(FlacBlockState::new());

        decoder.init_file(
            path,
            Some(flac_write_callback),
            None,
            Some(flac_stream_error_callback),
            state.as_mut() as *mut _ as *mut c_void,
        )?;

        Ok(Self {
            decoder,
            state,
            done: false,
        })
    }
}

impl Iterator for FlacBlockIter {
    type Item = Result<AudioBlock>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(block) = self.state.blocks.pop_front() {
            return Some(Ok(block));
        }
        if self.done {
            return None;
        }

        loop {
            let ok = self.decoder.process_single();
            if ok == 0 {
                self.done = true;
                let err = self
                    .state
                    .error
                    .take()
                    .unwrap_or_else(|| "FLAC decoding failed".to_string());
                return Some(Err(err));
            }

            if let Some(err) = self.state.error.take() {
                self.done = true;
                return Some(Err(err));
            }

            if let Some(block) = self.state.blocks.pop_front() {
                return Some(Ok(block));
            }

            if self.decoder.state() == flac::FLAC__STREAM_DECODER_END_OF_STREAM {
                self.done = true;
                return None;
            }
        }
    }
}

unsafe extern "C" fn flac_stream_error_callback(
    _decoder: *const flac::FLAC__StreamDecoder,
    status: flac::FLAC__StreamDecoderErrorStatus,
    client_data: *mut c_void,
) {
    if client_data.is_null() {
        return;
    }
    let state = unsafe { &mut *(client_data as *mut FlacBlockState) };
    state.error = Some(format!("FLAC decoder error status {}", status));
}

unsafe extern "C" fn flac_write_callback(
    _decoder: *const flac::FLAC__StreamDecoder,
    frame: *const flac::FLAC__Frame,
    buffer: *const *const i32,
    client_data: *mut c_void,
) -> flac::FLAC__StreamDecoderWriteStatus {
    if frame.is_null() || buffer.is_null() || client_data.is_null() {
        return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
    }

    let state = unsafe { &mut *(client_data as *mut FlacBlockState) };
    if state.error.is_some() {
        return flac::FLAC__STREAM_DECODER_WRITE_STATUS_ABORT;
    }

    let frame_ref = unsafe { &*frame };
    let channels = frame_ref.header.channels as usize;
    let block_samples = frame_ref.header.blocksize as usize;
    if channels == 0 || block_samples == 0 {
        return flac::FLAC__STREAM_DECODER_WRITE_STATUS_CONTINUE;
    }

    let sample_index =
        if frame_ref.header.number_type == flac::FLAC__FRAME_NUMBER_TYPE_SAMPLE_NUMBER {
            unsafe { frame_ref.header.number.sample_number }
        } else {
            state.next_sample_number
        };
    state.next_sample_number = sample_index + block_samples as u64;

    let mut interleaved = Vec::with_capacity(block_samples * channels);
    for i in 0..block_samples {
        for ch in 0..channels {
            unsafe {
                let chan_ptr = *buffer.add(ch);
                interleaved.push(*chan_ptr.add(i));
            }
        }
    }

    state.blocks.push_back(AudioBlock {
        sample_index,
        channels: channels as u32,
        interleaved,
    });

    flac::FLAC__STREAM_DECODER_WRITE_STATUS_CONTINUE
}

pub(crate) struct TrackEncoder {
    encoder: *mut flac::FLAC__StreamEncoder,
}

impl TrackEncoder {
    pub(crate) fn write_interleaved(&mut self, interleaved: &[i32], samples: u32) -> Result<()> {
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

    pub(crate) fn finish(&mut self) -> Result<()> {
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

pub(crate) fn start_track_encoder(
    meta: &InputMetadata,
    cue: &CueDisc,
    tracks: &[TrackSpan],
    track: &TrackSpan,
    compression_level: u8,
    display_base_abs: Option<&Path>,
    progress: Option<&ProgressBar>,
) -> Result<TrackEncoder> {
    let encoder = unsafe { flac::FLAC__stream_encoder_new() };
    if encoder.is_null() {
        return Err("failed to create FLAC encoder".to_string());
    }

    let ok = unsafe {
        flac::FLAC__stream_encoder_set_channels(encoder, meta.channels) != 0
            && flac::FLAC__stream_encoder_set_bits_per_sample(encoder, meta.bits_per_sample) != 0
            && flac::FLAC__stream_encoder_set_sample_rate(encoder, meta.sample_rate) != 0
            && flac::FLAC__stream_encoder_set_compression_level(encoder, compression_level as u32)
                != 0
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

    let mut metadata_blocks = build_track_metadata(meta, cue, tracks, track)?;
    if !metadata_blocks.is_empty() {
        let mut metadata_ptrs = FlacMetadata::collect_raw_ptrs(&mut metadata_blocks);
        let ok = unsafe {
            flac::FLAC__stream_encoder_set_metadata(
                encoder,
                metadata_ptrs.as_mut_ptr(),
                metadata_ptrs.len() as u32,
            ) != 0
        };
        if !ok {
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

    if init_status != flac::FLAC__STREAM_ENCODER_INIT_STATUS_OK {
        unsafe {
            flac::FLAC__stream_encoder_delete(encoder);
        }
        return Err(format!(
            "failed to init encoder for {}",
            track.output_path.display()
        ));
    }

    announce_track_start(display_base_abs, progress, track);

    Ok(TrackEncoder { encoder })
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    let path_str = path.to_string_lossy();
    CString::new(path_str.as_bytes())
        .map_err(|_| format!("path contains NUL byte: {}", path.display()))
}

fn announce_track_start(
    display_base_abs: Option<&Path>,
    progress: Option<&ProgressBar>,
    track: &TrackSpan,
) {
    let output_display = display_path(display_base_abs, &track.output_path);
    let line = format!(
        "{} {}",
        "Creating".green().bold(),
        output_display.display().to_string().bold()
    );
    if let Some(progress) = progress {
        progress.println(line);
    } else {
        println!("{}", line);
    }
}
