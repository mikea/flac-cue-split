use crate::cue::parse_cue_from_str;
use crate::flac::{compute_track_spans, frames_to_samples, sanitize_filename};

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
