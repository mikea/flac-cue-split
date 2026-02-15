use libflac_sys as flac;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn splits_generated_silent_flac_with_generated_cue() {
    let dir = unique_test_dir("generated-silence-split");
    fs::create_dir_all(&dir).expect("failed to create test directory");

    let input_flac = dir.join("album.flac");
    let input_cue = dir.join("album.cue");

    write_silent_flac(&input_flac, 44_100, 2, 44_100 * 3).expect("failed to generate FLAC");
    write_cue(&input_cue, "album.flac");

    let output = Command::new(env!("CARGO_BIN_EXE_flac-cue-split"))
        .current_dir(&dir)
        .arg("-y")
        .arg("--flac")
        .arg("album.flac")
        .arg("--cue")
        .arg("album.cue")
        .output()
        .expect("failed to run flac-cue-split");

    assert!(
        output.status.success(),
        "split command failed\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(dir.join("1 - One.flac").is_file());
    assert!(dir.join("2 - Two.flac").is_file());
    assert!(dir.join("3 - Three.flac").is_file());

    fs::remove_dir_all(&dir).expect("failed to remove test directory");
}

fn write_cue(path: &Path, flac_name: &str) {
    let cue = format!(
        r#"PERFORMER "Test Artist"
TITLE "Test Album"
FILE "{}" WAVE
  TRACK 01 AUDIO
    TITLE "One"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Two"
    INDEX 01 00:01:00
  TRACK 03 AUDIO
    TITLE "Three"
    INDEX 01 00:02:00
"#,
        flac_name
    );
    fs::write(path, cue).expect("failed to write cue file");
}

fn write_silent_flac(path: &Path, sample_rate: u32, channels: u32, samples_per_channel: u32) -> Result<(), String> {
    let encoder = unsafe { flac::FLAC__stream_encoder_new() };
    if encoder.is_null() {
        return Err("failed to allocate FLAC encoder".to_string());
    }

    let configured = unsafe {
        flac::FLAC__stream_encoder_set_channels(encoder, channels) != 0
            && flac::FLAC__stream_encoder_set_bits_per_sample(encoder, 16) != 0
            && flac::FLAC__stream_encoder_set_sample_rate(encoder, sample_rate) != 0
            && flac::FLAC__stream_encoder_set_compression_level(encoder, 5) != 0
            && flac::FLAC__stream_encoder_set_total_samples_estimate(
                encoder,
                samples_per_channel as u64,
            ) != 0
    };
    if !configured {
        unsafe {
            flac::FLAC__stream_encoder_delete(encoder);
        }
        return Err("failed to configure FLAC encoder".to_string());
    }

    let path_c = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| format!("path contains NUL byte: {}", path.display()))?;
    let init_status = unsafe {
        flac::FLAC__stream_encoder_init_file(encoder, path_c.as_ptr(), None, std::ptr::null_mut())
    };
    if init_status != flac::FLAC__STREAM_ENCODER_INIT_STATUS_OK {
        unsafe {
            flac::FLAC__stream_encoder_delete(encoder);
        }
        return Err(format!(
            "failed to init FLAC encoder for {}: status {}",
            path.display(),
            init_status
        ));
    }

    let interleaved_len = samples_per_channel as usize * channels as usize;
    let interleaved = vec![0i32; interleaved_len];
    let processed = unsafe {
        flac::FLAC__stream_encoder_process_interleaved(
            encoder,
            interleaved.as_ptr(),
            samples_per_channel,
        )
    };

    if processed == 0 {
        let state = unsafe { flac::FLAC__stream_encoder_get_state(encoder) };
        let state_msg = unsafe {
            let ptr = flac::FLAC__stream_encoder_get_resolved_state_string(encoder);
            if ptr.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        };
        unsafe {
            flac::FLAC__stream_encoder_finish(encoder);
            flac::FLAC__stream_encoder_delete(encoder);
        }
        return Err(format!(
            "failed to write FLAC samples (state {}: {})",
            state, state_msg
        ));
    }

    let finished = unsafe { flac::FLAC__stream_encoder_finish(encoder) };
    unsafe {
        flac::FLAC__stream_encoder_delete(encoder);
    }
    if finished == 0 {
        return Err("failed to finish FLAC encoder".to_string());
    }

    Ok(())
}

fn unique_test_dir(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "flac-cue-split-{}-{}-{}",
        label,
        std::process::id(),
        stamp
    ))
}
