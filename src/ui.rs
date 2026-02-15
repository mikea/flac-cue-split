use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use std::io::{self, Write};
use std::path::Path;

use crate::Result;
use crate::cli::display_path;
use crate::metadata::{compute_common_metadata, compute_unique_metadata_pairs};
use crate::split::{Plan, processed_flac_path};
use crate::types::{CueDisc, InputMetadata, TrackSpan};

pub(crate) enum ConfirmAction {
    Proceed,
    Cancel,
    EditSubdirs,
}

pub(crate) fn print_plan(plan: &Plan) -> Result<()> {
    let cue: &CueDisc = plan.cue();
    let meta: &InputMetadata = plan.input_meta();
    let tracks: &[TrackSpan] = plan.tracks();
    let compression_level = plan.compression_level();
    let display_base_abs = plan.display_base_abs();
    let picture_names = plan.picture_names();
    let input_path = plan.flac_display();
    let cue_path = plan.cue_display();
    let (cue_encoding, cue_encoding_autodetected) = plan.cue_encoding();
    let (delete_original, rename_original) = plan.source_actions();
    if meta.sample_rate == 0 {
        return Err("invalid sample rate in metadata".to_string());
    }
    if !meta.sample_rate.is_multiple_of(75) {
        return Err(format!(
            "sample rate {} is not divisible by 75 (CUE frames)",
            meta.sample_rate
        ));
    }

    let samples_per_frame = (meta.sample_rate / 75) as u64;

    println!("{}", "Plan".bold());
    println!("  {} {}", "Input:".cyan(), input_path.display());
    if delete_original {
        println!(
            "  {} {}",
            "Source action:".cyan(),
            "will be deleted after successful split".red().bold()
        );
    } else if rename_original {
        let rename_note = match processed_flac_path(input_path) {
            Some(renamed) => format!("will be renamed to {}", renamed.display()),
            None => "will be renamed after successful split".to_string(),
        };
        println!("  {} {}", "Source action:".cyan(), rename_note.yellow());
    }
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
        tracks.len(),
        meta.sample_rate,
        meta.channels,
        meta.bits_per_sample,
        compression_level
    );

    let common_metadata = compute_common_metadata(meta, cue, tracks);
    let picture_count = meta.pictures.len();
    print_shared_metadata(&common_metadata, picture_count, picture_names);

    for track in tracks {
        let start_frames = track.start / samples_per_frame;
        let end_frames = track.end / samples_per_frame;
        let length_frames = end_frames.saturating_sub(start_frames);

        let output_display = display_path(display_base_abs, &track.output_path);
        let output_target = format_output_target(&output_display);
        let length = format_msf(length_frames);
        let range = format!("({}-{})", format_msf(start_frames), format_msf(end_frames));
        let unique_metadata =
            compute_unique_metadata_pairs(meta, cue, tracks, track, &common_metadata);
        let tags = format_tag_pairs(&unique_metadata);
        if tags.is_empty() {
            println!(
                "{} {}",
                output_target,
                format!("{} {}", length, range).dimmed()
            );
        } else {
            println!(
                "{} {} {}",
                output_target,
                format!("{} {}", length, range).dimmed(),
                tags
            );
        }
    }

    Ok(())
}

fn format_output_target(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent
        && parent != Path::new(".")
    {
        let separator = if parent == Path::new(std::path::MAIN_SEPARATOR_STR) {
            ""
        } else {
            std::path::MAIN_SEPARATOR_STR
        };
        return format!(
            "{}{}{}",
            parent.display().to_string().blue(),
            separator,
            file_name.bold()
        );
    }
    file_name.bold().to_string()
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

pub(crate) fn finish_progress(progress: &mut Option<ProgressBar>, message: &str) {
    if let Some(pb) = progress.take() {
        pb.finish_with_message(message.to_string());
    }
}

pub(crate) fn confirm_or_exit(yes: bool, allow_subdirs_edit: bool) -> Result<ConfirmAction> {
    if yes {
        return Ok(ConfirmAction::Proceed);
    }

    if allow_subdirs_edit {
        print!("Proceed? [y/N/S]: ");
    } else {
        print!("Proceed? [y/N]: ");
    }
    io::stdout()
        .flush()
        .map_err(|err| format!("failed to flush stdout: {}", err))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|err| format!("failed to read confirmation: {}", err))?;

    Ok(parse_confirm_action(&input, allow_subdirs_edit))
}

fn parse_confirm_action(input: &str, allow_subdirs_edit: bool) -> ConfirmAction {
    let answer = input.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        return ConfirmAction::Proceed;
    }
    if allow_subdirs_edit && (answer == "s" || answer == "subdirs") {
        return ConfirmAction::EditSubdirs;
    }
    ConfirmAction::Cancel
}

pub(crate) fn format_msf(frames: u64) -> String {
    let total_seconds = frames / 75;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let frames = frames % 75;
    format!("{:02}:{:02}:{:02}", minutes, seconds, frames)
}

#[cfg(test)]
mod tests {
    use super::{ConfirmAction, parse_confirm_action};

    #[test]
    fn parse_confirm_action_accepts_yes() {
        assert!(matches!(
            parse_confirm_action("y", false),
            ConfirmAction::Proceed
        ));
        assert!(matches!(
            parse_confirm_action("YES", true),
            ConfirmAction::Proceed
        ));
    }

    #[test]
    fn parse_confirm_action_handles_subdirs_option() {
        assert!(matches!(
            parse_confirm_action("s", true),
            ConfirmAction::EditSubdirs
        ));
        assert!(matches!(
            parse_confirm_action("subdirs", true),
            ConfirmAction::EditSubdirs
        ));
        assert!(matches!(
            parse_confirm_action("s", false),
            ConfirmAction::Cancel
        ));
    }

    #[test]
    fn parse_confirm_action_defaults_to_cancel() {
        assert!(matches!(
            parse_confirm_action("", true),
            ConfirmAction::Cancel
        ));
        assert!(matches!(
            parse_confirm_action("n", false),
            ConfirmAction::Cancel
        ));
    }
}
