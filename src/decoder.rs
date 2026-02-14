use std::path::{Path, PathBuf};

use crate::Result;
use crate::flac::FlacDecoder;
use crate::types::InputMetadata;
use crate::wavpack::WavPackDecoder;

pub(crate) struct DecoderMetadata {
    pub(crate) input_meta: InputMetadata,
    pub(crate) picture_names: Vec<String>,
}

pub(crate) struct AudioBlock {
    pub(crate) sample_index: u64,
    pub(crate) channels: u32,
    pub(crate) interleaved: Vec<i32>,
}

impl AudioBlock {
    pub(crate) fn sample_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.interleaved.len() / self.channels as usize
        }
    }
}

pub(crate) trait Decoder {
    fn read_metadata(&mut self) -> Result<DecoderMetadata>;
    fn into_blocks(self: Box<Self>) -> Result<Box<dyn Iterator<Item = Result<AudioBlock>>>>;
}

pub(crate) fn create_decoder(path: &Path) -> Result<Box<dyn Decoder>> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();

    let path = PathBuf::from(path);
    match ext.as_str() {
        "flac" => Ok(Box::new(FlacDecoder::new(path))),
        "wv" => Ok(Box::new(WavPackDecoder::new(path))),
        _ => Err(format!(
            "unsupported input format {} (expected .flac or .wv)",
            path.display()
        )),
    }
}
