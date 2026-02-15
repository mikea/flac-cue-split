use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::decoder::{AudioBlock, Decoder, DecoderMetadata};
use crate::flac::FlacMetadata;
use crate::picture::build_picture_metadata_from_data;
use crate::types::InputMetadata;

mod wavpack_bindings {
    #![allow(
        dead_code,
        non_camel_case_types,
        non_upper_case_globals,
        non_snake_case
    )]
    include!(concat!(env!("OUT_DIR"), "/wavpack_bindings.rs"));
}

pub(crate) struct WavPackDecoder {
    path: PathBuf,
}

impl WavPackDecoder {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn read_metadata_internal(&self) -> Result<DecoderMetadata> {
        let handle = WavPackHandle::open(&self.path, true)?;
        let mut input_meta = InputMetadata::new();
        self.fill_stream_info(&handle, &mut input_meta)?;
        self.fill_text_tags(&handle, &mut input_meta);

        let mut picture_names = Vec::new();
        self.fill_pictures(&handle, &mut input_meta, &mut picture_names)?;

        Ok(DecoderMetadata {
            input_meta,
            picture_names,
        })
    }

    fn fill_stream_info(
        &self,
        handle: &WavPackHandle,
        input_meta: &mut InputMetadata,
    ) -> Result<()> {
        let sample_rate = handle.sample_rate();
        let channels = handle.channels();
        let bits_per_sample = handle.bits_per_sample();
        if sample_rate == 0 {
            return Err("WavPack sample rate is zero".to_string());
        }
        if channels == 0 {
            return Err("WavPack channel count is zero".to_string());
        }
        if bits_per_sample == 0 {
            return Err("WavPack bits per sample is zero".to_string());
        }

        input_meta.sample_rate = sample_rate;
        input_meta.channels = channels;
        input_meta.bits_per_sample = bits_per_sample;
        input_meta.total_samples = handle.total_samples();
        Ok(())
    }

    fn fill_text_tags(&self, handle: &WavPackHandle, input_meta: &mut InputMetadata) {
        for (key, value) in handle.read_text_tags() {
            input_meta.comments.push((key, value));
        }
    }

    fn fill_pictures(
        &self,
        handle: &WavPackHandle,
        input_meta: &mut InputMetadata,
        picture_names: &mut Vec<String>,
    ) -> Result<()> {
        let pictures = handle.read_picture_tags()?;
        for picture in pictures {
            if let Some(name) = picture.name.as_ref() {
                picture_names.push(name.clone());
            }
            input_meta.pictures.push(picture.picture);
        }
        Ok(())
    }
}

impl Decoder for WavPackDecoder {
    fn read_metadata(&mut self) -> Result<DecoderMetadata> {
        self.read_metadata_internal()
    }

    fn into_blocks(self: Box<Self>) -> Result<Box<dyn Iterator<Item = Result<AudioBlock>>>> {
        Ok(Box::new(WavPackBlockIter::new(&self.path)?))
    }
}

struct EmbeddedPicture {
    name: Option<String>,
    picture: FlacMetadata,
}

struct WavPackHandle {
    context: *mut wavpack_bindings::WavpackContext,
}

impl WavPackHandle {
    fn open(path: &Path, with_tags: bool) -> Result<Self> {
        let path_c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| format!("path contains NUL byte: {}", path.display()))?;

        let mut error = [0i8; 81];
        let mut flags = 0i32;
        if with_tags {
            flags |= wavpack_bindings::OPEN_TAGS as i32;
        }

        let context = unsafe {
            wavpack_bindings::WavpackOpenFileInput(path_c.as_ptr(), error.as_mut_ptr(), flags, 0)
        };
        if context.is_null() {
            let err = unsafe { CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .trim()
                .to_string();
            if err.is_empty() {
                return Err(format!("failed to open WavPack file {}", path.display()));
            }
            return Err(format!(
                "failed to open WavPack file {}: {}",
                path.display(),
                err
            ));
        }

        Ok(Self { context })
    }

    fn sample_rate(&self) -> u32 {
        unsafe { wavpack_bindings::WavpackGetSampleRate(self.context) as u32 }
    }

    fn channels(&self) -> u32 {
        unsafe { wavpack_bindings::WavpackGetNumChannels(self.context) as u32 }
    }

    fn bits_per_sample(&self) -> u32 {
        unsafe { wavpack_bindings::WavpackGetBitsPerSample(self.context) as u32 }
    }

    fn total_samples(&self) -> u64 {
        unsafe { wavpack_bindings::WavpackGetNumSamples64(self.context) as u64 }
    }

    fn sample_index(&self) -> u64 {
        unsafe { wavpack_bindings::WavpackGetSampleIndex64(self.context) as u64 }
    }

    fn unpack_samples(&self, interleaved: &mut [i32], channels: usize) -> Result<usize> {
        if channels == 0 {
            return Err("invalid channel count".to_string());
        }
        let max_samples = interleaved.len() / channels;
        if max_samples == 0 {
            return Ok(0);
        }

        let samples = unsafe {
            wavpack_bindings::WavpackUnpackSamples(
                self.context,
                interleaved.as_mut_ptr(),
                max_samples as u32,
            )
        };
        Ok(samples as usize)
    }

    fn read_text_tags(&self) -> Vec<(String, String)> {
        let mut tags = Vec::new();
        let count = unsafe { wavpack_bindings::WavpackGetNumTagItems(self.context) };
        if count <= 0 {
            return tags;
        }

        for idx in 0..count {
            let Some(key) = self.tag_item_name(idx) else {
                continue;
            };
            let Some(value) = self.tag_item_value(&key) else {
                continue;
            };
            tags.push((key.to_ascii_uppercase(), value));
        }

        tags
    }

    fn read_picture_tags(&self) -> Result<Vec<EmbeddedPicture>> {
        let mut pictures = Vec::new();
        let count = unsafe { wavpack_bindings::WavpackGetNumBinaryTagItems(self.context) };
        if count <= 0 {
            return Ok(pictures);
        }

        for idx in 0..count {
            let Some(key) = self.binary_tag_name(idx) else {
                continue;
            };
            if !key.to_ascii_lowercase().starts_with("cover art") {
                continue;
            }
            let Some(value) = self.binary_tag_value(&key) else {
                continue;
            };

            let (name, data) = split_picture_blob(&value);
            if data.is_empty() {
                continue;
            }

            let picture = build_picture_metadata_from_data(data, name.as_deref())?;
            pictures.push(EmbeddedPicture { name, picture });
        }

        Ok(pictures)
    }

    fn tag_item_name(&self, index: i32) -> Option<String> {
        let len = unsafe {
            wavpack_bindings::WavpackGetTagItemIndexed(self.context, index, std::ptr::null_mut(), 0)
        };
        if len <= 0 {
            return None;
        }

        let mut key = vec![0u8; len as usize + 1];
        let written = unsafe {
            wavpack_bindings::WavpackGetTagItemIndexed(
                self.context,
                index,
                key.as_mut_ptr().cast::<c_char>(),
                key.len() as i32,
            )
        };
        if written <= 0 {
            return None;
        }

        decode_lossy_bytes(&key[..len as usize])
    }

    fn tag_item_value(&self, key: &str) -> Option<String> {
        let key_c = CString::new(key.as_bytes()).ok()?;
        let len = unsafe {
            wavpack_bindings::WavpackGetTagItem(
                self.context,
                key_c.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        if len <= 0 {
            return None;
        }

        let mut value = vec![0u8; len as usize + 1];
        let written = unsafe {
            wavpack_bindings::WavpackGetTagItem(
                self.context,
                key_c.as_ptr(),
                value.as_mut_ptr().cast::<c_char>(),
                value.len() as i32,
            )
        };
        if written <= 0 {
            return None;
        }

        decode_lossy_bytes(&value[..len as usize])
    }

    fn binary_tag_name(&self, index: i32) -> Option<String> {
        let len = unsafe {
            wavpack_bindings::WavpackGetBinaryTagItemIndexed(
                self.context,
                index,
                std::ptr::null_mut(),
                0,
            )
        };
        if len <= 0 {
            return None;
        }

        let mut key = vec![0u8; len as usize + 1];
        let written = unsafe {
            wavpack_bindings::WavpackGetBinaryTagItemIndexed(
                self.context,
                index,
                key.as_mut_ptr().cast::<c_char>(),
                key.len() as i32,
            )
        };
        if written <= 0 {
            return None;
        }

        decode_lossy_bytes(&key[..len as usize])
    }

    fn binary_tag_value(&self, key: &str) -> Option<Vec<u8>> {
        let key_c = CString::new(key.as_bytes()).ok()?;
        let len = unsafe {
            wavpack_bindings::WavpackGetBinaryTagItem(
                self.context,
                key_c.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        if len <= 0 {
            return None;
        }

        let mut value = vec![0u8; len as usize];
        let written = unsafe {
            wavpack_bindings::WavpackGetBinaryTagItem(
                self.context,
                key_c.as_ptr(),
                value.as_mut_ptr().cast::<c_char>(),
                len,
            )
        };
        if written <= 0 {
            return None;
        }

        Some(value)
    }
}

impl Drop for WavPackHandle {
    fn drop(&mut self) {
        unsafe {
            wavpack_bindings::WavpackCloseFile(self.context);
        }
    }
}

struct WavPackBlockIter {
    handle: WavPackHandle,
    channels: usize,
    buffer: Vec<i32>,
    done: bool,
}

impl WavPackBlockIter {
    fn new(path: &Path) -> Result<Self> {
        let handle = WavPackHandle::open(path, false)?;
        let channels = handle.channels() as usize;
        if channels == 0 {
            return Err("WavPack channel count is zero".to_string());
        }

        Ok(Self {
            handle,
            channels,
            buffer: vec![0i32; 4096 * channels],
            done: false,
        })
    }
}

impl Iterator for WavPackBlockIter {
    type Item = Result<AudioBlock>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        let sample_index = self.handle.sample_index();
        let samples = match self.handle.unpack_samples(&mut self.buffer, self.channels) {
            Ok(samples) => samples,
            Err(err) => {
                self.done = true;
                return Some(Err(err));
            }
        };

        if samples == 0 {
            self.done = true;
            return None;
        }

        let used = samples * self.channels;
        Some(Ok(AudioBlock {
            sample_index,
            channels: self.channels as u32,
            interleaved: self.buffer[..used].to_vec(),
        }))
    }
}

fn split_picture_blob(bytes: &[u8]) -> (Option<String>, &[u8]) {
    if let Some(pos) = bytes.iter().position(|byte| *byte == 0) {
        let name = decode_lossy_bytes(&bytes[..pos]);
        let data = if pos + 1 < bytes.len() {
            &bytes[pos + 1..]
        } else {
            &[]
        };
        return (name, data);
    }

    (None, bytes)
}

fn decode_lossy_bytes(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    if end == 0 {
        return None;
    }
    let decoded = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}
