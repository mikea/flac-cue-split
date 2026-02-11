use encoding_rs::Encoding;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use std::io::{self, Write};
use std::path::Path;

use crate::Result;
use crate::cli::display_path;
use crate::flac::DecodeContext;
use crate::metadata::{compute_common_metadata, compute_unique_metadata_pairs};

pub(crate) fn print_plan(
    context: &DecodeContext,
    flac_path: &Path,
    cue_path: &Path,
    cue_encoding: &'static Encoding,
    cue_encoding_autodetected: bool,
) -> Result<()> {
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

    println!("{}", "Plan".bold());
    println!("  {} {}", "FLAC:".cyan(), flac_path.display());
    println!("  {} {}", "CUE:".cyan(), cue_path.display());
    let encoding_label = if cue_encoding_autodetected {
        format!("{} {}", cue_encoding.name(), "(autodetected)".dimmed())
    } else {
        cue_encoding.name().to_string()
    };
    println!("  {} {}", "CUE encoding:".cyan(), encoding_label.green());
    println!(
        "  {} {} ({} Hz, {} ch, {} bits, compression {})",
        "Tracks:".cyan(),
        context.tracks.len(),
        meta.sample_rate,
        meta.channels,
        meta.bits_per_sample,
        context.compression_level
    );

    let common_metadata = compute_common_metadata(meta, &context.cue, &context.tracks);
    let picture_count = meta.pictures.len();
    print_shared_metadata(&common_metadata, picture_count, &context.picture_names);

    for track in &context.tracks {
        let start_frames = track.start / samples_per_frame;
        let end_frames = track.end / samples_per_frame;
        let length_frames = end_frames.saturating_sub(start_frames);

        let output_display = display_path(context.display_base_abs.as_deref(), &track.output_path);
        let file_name = output_display
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| output_display.display().to_string());
        let length = format_msf(length_frames);
        let range = format!("({}-{})", format_msf(start_frames), format_msf(end_frames));
        let unique_metadata = compute_unique_metadata_pairs(
            meta,
            &context.cue,
            &context.tracks,
            track,
            &common_metadata,
        );
        let tags = format_tag_pairs(&unique_metadata);
        if tags.is_empty() {
            println!(
                "{} {}",
                file_name.bold(),
                format!("{} {}", length, range).dimmed()
            );
        } else {
            println!(
                "{} {} {}",
                file_name.bold(),
                format!("{} {}", length, range).dimmed(),
                tags
            );
        }
    }

    Ok(())
}

fn print_shared_metadata(common: &[(String, String)], pictures: usize, picture_names: &[String]) {
    println!("{}", "Shared tags".bold());
    let line = format_metadata_line(common, pictures, picture_names);
    if line.is_empty() {
        println!("  {}", "(none)".dimmed());
    } else {
        println!("  {}", line);
    }
}

fn format_metadata_line(
    common: &[(String, String)],
    pictures: usize,
    picture_names: &[String],
) -> String {
    let mut parts = Vec::new();
    for (key, value) in common {
        parts.push(format!("{}={}", key.cyan(), value.yellow()));
    }
    if pictures > 0 {
        let picture_value = if picture_names.is_empty() {
            pictures.to_string()
        } else {
            let mut value = picture_names.join(", ");
            if pictures > picture_names.len() {
                value.push_str(&format!(" (+{} embedded)", pictures - picture_names.len()));
            }
            value
        };
        parts.push(format!("{}={}", "PICTURES".cyan(), picture_value.yellow()));
    }
    parts.join("; ")
}

pub(crate) fn format_tag_pairs(pairs: &[(String, String)]) -> String {
    let mut parts = Vec::new();
    for (key, value) in pairs {
        parts.push(format!("{}={}", key.cyan(), value.yellow()));
    }
    parts.join("; ")
}

pub(crate) fn make_progress_bar(total_samples: u64) -> ProgressBar {
    if total_samples > 0 {
        let pb = ProgressBar::with_draw_target(
            Some(total_samples),
            ProgressDrawTarget::stderr_with_hz(10),
        );
        let style = ProgressStyle::with_template(
            "{bar:60.cyan/blue} {percent:>3}% {pos:>10}/{len:<10} {msg}",
        )
        .unwrap()
        .progress_chars("=>-");
        pb.set_style(style);
        pb.set_message("decoding");
        pb
    } else {
        let pb = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr_with_hz(10));
        pb.set_message("decoding");
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        pb
    }
}

pub(crate) fn finish_progress(context: &mut DecodeContext, message: &str) {
    if let Some(pb) = context.progress.take() {
        pb.finish_with_message(message.to_string());
    }
}

pub(crate) fn confirm_or_exit(yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }

    print!("Proceed? [y/N]: ");
    io::stdout()
        .flush()
        .map_err(|err| format!("failed to flush stdout: {}", err))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|err| format!("failed to read confirmation: {}", err))?;

    let answer = input.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

pub(crate) fn format_msf(frames: u64) -> String {
    let total_seconds = frames / 75;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let frames = frames % 75;
    format!("{:02}:{:02}:{:02}", minutes, seconds, frames)
}
