use libflac_sys as flac;
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub(crate) struct CueRem {
    pub(crate) date: Option<String>,
    pub(crate) replaygain_album_gain: Option<String>,
    pub(crate) replaygain_album_peak: Option<String>,
    pub(crate) replaygain_track_gain: Option<String>,
    pub(crate) replaygain_track_peak: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CueDisc {
    pub(crate) title: Option<String>,
    pub(crate) performer: Option<String>,
    pub(crate) songwriter: Option<String>,
    pub(crate) composer: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) disc_id: Option<String>,
    pub(crate) rem: CueRem,
    pub(crate) tracks: Vec<CueTrack>,
}

#[derive(Debug, Clone)]
pub(crate) struct CueTrack {
    pub(crate) number: u32,
    pub(crate) title: Option<String>,
    pub(crate) performer: Option<String>,
    pub(crate) songwriter: Option<String>,
    pub(crate) composer: Option<String>,
    pub(crate) isrc: Option<String>,
    pub(crate) start_frames: i64,
    pub(crate) length_frames: Option<i64>,
    pub(crate) filename: Option<String>,
    pub(crate) rem: CueRem,
}

#[derive(Debug, Clone)]
pub(crate) struct InputMetadata {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    pub(crate) bits_per_sample: u32,
    pub(crate) total_samples: u64,
    pub(crate) vendor: Option<String>,
    pub(crate) comments: Vec<(String, String)>,
    pub(crate) pictures: Vec<*mut flac::FLAC__StreamMetadata>,
}

impl InputMetadata {
    pub(crate) fn new() -> Self {
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
pub(crate) struct TrackSpan {
    pub(crate) number: u32,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) title: Option<String>,
    pub(crate) performer: Option<String>,
    pub(crate) songwriter: Option<String>,
    pub(crate) composer: Option<String>,
    pub(crate) isrc: Option<String>,
    pub(crate) rem: CueRem,
    pub(crate) output_path: PathBuf,
}
