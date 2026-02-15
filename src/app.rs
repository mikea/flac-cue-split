use clap::Parser;
use dialoguer::Input;
use owo_colors::OwoColorize;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::Result;
use crate::cli::{Args, InputPair, resolve_input_pairs};
use crate::cue::report_cue_warnings;
use crate::cue::resolve_encoding;
use crate::split::{SplitOptions, prepare_split, sanitize_filename};
use crate::ui::{ConfirmAction, confirm_or_exit, print_plan};

pub fn run() -> Result<()> {
    let args = Args::parse();
    let encoding = match args.cue_encoding {
        Some(label) => Some(resolve_encoding(&label)?),
        None => None,
    };

    let cwd = std::env::current_dir()
        .map_err(|err| format!("failed to get current directory: {}", err))?;
    let (base_dir_abs, display_base_abs) = match args.dir.as_ref() {
        Some(dir) if dir.is_absolute() => (dir.clone(), None),
        Some(dir) => (cwd.join(dir), Some(cwd.clone())),
        None => (cwd.clone(), Some(cwd)),
    };

    let picture_enabled = !args.no_picture;
    let picture_path = if let Some(path) = args.picture.as_ref() {
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            base_dir_abs.join(path)
        };
        if !abs.is_file() {
            return Err(format!("picture file not found: {}", abs.display()));
        }
        Some(abs)
    } else {
        None
    };

    let pairs = resolve_input_pairs(
        &base_dir_abs,
        display_base_abs.as_deref(),
        args.flac.as_ref(),
        args.cue.as_ref(),
    )?;

    let mut output_subdirs = derive_output_subdirs(&pairs)?;
    let total = pairs.len();
    let enforce_cue_filename_match = total > 1;
    loop {
        let mut prepared_jobs = Vec::with_capacity(total);
        for (pair, output_subdir) in pairs.iter().cloned().zip(output_subdirs.iter().cloned()) {
            let prepared = prepare_split(SplitOptions {
                flac_input: pair.flac,
                cue_input: pair.cue,
                display_base_abs: display_base_abs.clone(),
                cue_encoding: encoding,
                overwrite: args.overwrite,
                compression_level: args.compression_level,
                search_dir: base_dir_abs.clone(),
                picture_enabled,
                picture_path: picture_path.clone(),
                delete_original: args.delete_original,
                rename_original: args.rename_original,
                output_subdir,
                enforce_cue_filename_match,
            })?;
            prepared_jobs.push(prepared);
        }

        for (index, prepared) in prepared_jobs.iter().enumerate() {
            if total > 1 {
                if index > 0 {
                    println!();
                }
                println!("{}", format!("Pair {}/{}", index + 1, total).bold().blue());
            }
            report_cue_warnings(prepared.warnings());
            print_plan(prepared)?;
        }

        match confirm_or_exit(args.yes, total > 1)? {
            ConfirmAction::Proceed => {
                for prepared in prepared_jobs {
                    prepared.execute()?;
                }
                return Ok(());
            }
            ConfirmAction::Cancel => return Err("aborted by user".to_string()),
            ConfirmAction::EditSubdirs => {
                output_subdirs = prompt_output_subdirs(&pairs, &output_subdirs)?;
            }
        }
    }
}

fn prompt_output_subdirs(
    pairs: &[InputPair],
    current_subdirs: &[Option<PathBuf>],
) -> Result<Vec<Option<PathBuf>>> {
    println!();
    println!("{}", "Configure output subdirectories".bold());

    let mut seen = HashSet::new();
    let mut edited = Vec::with_capacity(pairs.len());

    for (index, pair) in pairs.iter().enumerate() {
        let default_subdir = current_subdirs
            .get(index)
            .and_then(|value| value.as_ref())
            .map(|value| value.to_string_lossy().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("missing derived output subdirectory for pair {}", index + 1))?;

        println!("{} {}", "Pair:".cyan(), pair_name(pair).bold());

        let edited_subdir: String = Input::new()
            .with_prompt("Subdir")
            .with_initial_text(default_subdir)
            .interact_text()
            .map_err(|err| format!("failed to read subdir name: {}", err))?;

        let normalized = sanitize_filename(edited_subdir.trim());
        if normalized.is_empty() {
            return Err("subdir name cannot be empty".to_string());
        }
        if !seen.insert(normalized.clone()) {
            return Err(format!(
                "duplicate output subdirectory name: {}",
                normalized
            ));
        }
        edited.push(Some(PathBuf::from(normalized)));
    }

    Ok(edited)
}

fn pair_name(pair: &InputPair) -> String {
    pair.flac
        .display
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| pair.flac.display.display().to_string())
}

fn derive_output_subdirs(pairs: &[InputPair]) -> Result<Vec<Option<PathBuf>>> {
    if pairs.len() <= 1 {
        return Ok(vec![None; pairs.len()]);
    }

    let stems: Vec<String> = pairs
        .iter()
        .map(|pair| {
            pair.flac
                .abs
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_string())
                .ok_or_else(|| {
                    format!(
                        "failed to derive basename from input path: {}",
                        pair.flac.abs.display()
                    )
                })
        })
        .collect::<Result<Vec<String>>>()?;
    let stem_refs: Vec<&str> = stems.iter().map(String::as_str).collect();
    let raw_prefix_len = longest_common_prefix_len(&stem_refs);
    let prefix_len = adjust_common_prefix_len(&stem_refs, raw_prefix_len);
    let mut suffix_len = longest_common_suffix_len(&stem_refs);
    let max_suffix = stem_refs
        .iter()
        .map(|stem| stem.len().saturating_sub(prefix_len))
        .min()
        .unwrap_or(0);
    if suffix_len > max_suffix {
        suffix_len = max_suffix;
    }

    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(stems.len());
    for stem in stem_refs {
        let start = std::cmp::min(prefix_len, stem.len());
        let end = stem.len().saturating_sub(suffix_len);
        let trimmed = if start < end { &stem[start..end] } else { "" };

        let candidate = sanitize_filename(trimmed);
        let fallback = sanitize_filename(stem);
        let name = if candidate.is_empty() {
            fallback
        } else {
            candidate
        };
        if name.is_empty() {
            return Err("failed to derive output subdirectory name".to_string());
        }
        if !seen.insert(name.clone()) {
            return Err(format!(
                "derived duplicate output subdirectory name: {}",
                name
            ));
        }

        out.push(Some(PathBuf::from(name)));
    }

    Ok(out)
}

fn longest_common_prefix_len(values: &[&str]) -> usize {
    if values.is_empty() {
        return 0;
    }

    let mut prefix_len = values[0].len();
    for value in &values[1..] {
        prefix_len = common_prefix_len(&values[0][..prefix_len], value);
        if prefix_len == 0 {
            break;
        }
    }
    prefix_len
}

fn adjust_common_prefix_len(values: &[&str], prefix_len: usize) -> usize {
    if values.is_empty() || prefix_len == 0 {
        return prefix_len;
    }

    let prefix = &values[0][..prefix_len];
    let mut best = None;
    for keyword in ["cd", "disk", "volume"] {
        if let Some(start) = keyword_start_in_prefix(prefix, keyword) {
            best = Some(best.map_or(start, |current: usize| current.max(start)));
        }
    }

    best.unwrap_or(prefix_len)
}

fn keyword_start_in_prefix(prefix: &str, keyword: &str) -> Option<usize> {
    let lowered = prefix.to_ascii_lowercase();
    let mut offset = 0usize;
    let mut found = None;

    while offset < lowered.len() {
        let rel = match lowered[offset..].find(keyword) {
            Some(rel) => rel,
            None => break,
        };
        let start = offset + rel;
        let end = start + keyword.len();

        let before_ok = if start == 0 {
            true
        } else {
            prefix[..start]
                .chars()
                .next_back()
                .map(|ch| !ch.is_alphanumeric())
                .unwrap_or(false)
        };
        let after_ok = if end >= prefix.len() {
            true
        } else {
            prefix[end..]
                .chars()
                .next()
                .map(|ch| ch.is_whitespace())
                .unwrap_or(false)
        };

        if before_ok && after_ok {
            found = Some(start);
        }
        offset = start + 1;
    }

    found
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut len = 0usize;
    for (left, right) in a.chars().zip(b.chars()) {
        if left != right {
            break;
        }
        len += left.len_utf8();
    }
    len
}

fn longest_common_suffix_len(values: &[&str]) -> usize {
    if values.is_empty() {
        return 0;
    }

    let mut suffix_len = values[0].len();
    for value in &values[1..] {
        suffix_len = common_suffix_len(
            &values[0][values[0].len().saturating_sub(suffix_len)..],
            value,
        );
        if suffix_len == 0 {
            break;
        }
    }
    suffix_len
}

fn common_suffix_len(a: &str, b: &str) -> usize {
    let mut len = 0usize;
    for (left, right) in a.chars().rev().zip(b.chars().rev()) {
        if left != right {
            break;
        }
        len += left.len_utf8();
    }
    len
}

#[cfg(test)]
mod tests {
    use super::{
        derive_output_subdirs, keyword_start_in_prefix, longest_common_prefix_len,
        longest_common_suffix_len,
    };
    use crate::cli::{InputPair, InputPath};
    use std::path::PathBuf;

    fn pair(stem: &str) -> InputPair {
        InputPair {
            flac: InputPath {
                abs: PathBuf::from(format!("{}.flac", stem)),
                display: PathBuf::from(format!("{}.flac", stem)),
            },
            cue: InputPath {
                abs: PathBuf::from(format!("{}.cue", stem)),
                display: PathBuf::from(format!("{}.cue", stem)),
            },
        }
    }

    #[test]
    fn common_affixes_lengths() {
        let stems = vec!["Album CD1", "Album CD2", "Album CD3"];
        assert_eq!(longest_common_prefix_len(&stems), 8);
        assert_eq!(longest_common_suffix_len(&stems), 0);
    }

    #[test]
    fn derive_subdirs_from_common_affixes() {
        let pairs = vec![
            pair("Artist - Album [Disc 1]"),
            pair("Artist - Album [Disc 2]"),
            pair("Artist - Album [Disc 3]"),
        ];
        let subdirs = derive_output_subdirs(&pairs).unwrap();
        assert_eq!(
            subdirs,
            vec![
                Some(PathBuf::from("1")),
                Some(PathBuf::from("2")),
                Some(PathBuf::from("3")),
            ]
        );
    }

    #[test]
    fn derive_subdirs_keeps_cd_token() {
        let pairs = vec![
            pair("Artist - Album CD 1"),
            pair("Artist - Album CD 2"),
            pair("Artist - Album CD 3"),
        ];
        let subdirs = derive_output_subdirs(&pairs).unwrap();
        assert_eq!(
            subdirs,
            vec![
                Some(PathBuf::from("CD 1")),
                Some(PathBuf::from("CD 2")),
                Some(PathBuf::from("CD 3")),
            ]
        );
    }

    #[test]
    fn derive_subdirs_keeps_disk_token() {
        let pairs = vec![pair("Artist - Disk 1"), pair("Artist - Disk 2")];
        let subdirs = derive_output_subdirs(&pairs).unwrap();
        assert_eq!(
            subdirs,
            vec![Some(PathBuf::from("Disk 1")), Some(PathBuf::from("Disk 2"))]
        );
    }

    #[test]
    fn derive_subdirs_keeps_volume_token() {
        let pairs = vec![pair("Artist - Volume 1"), pair("Artist - Volume 2")];
        let subdirs = derive_output_subdirs(&pairs).unwrap();
        assert_eq!(
            subdirs,
            vec![
                Some(PathBuf::from("Volume 1")),
                Some(PathBuf::from("Volume 2")),
            ]
        );
    }

    #[test]
    fn keyword_detection_requires_boundary_and_whitespace() {
        assert_eq!(keyword_start_in_prefix("Artist Scd ", "cd"), None);
        assert_eq!(keyword_start_in_prefix("Artist - CD  ", "cd"), Some(9));
        assert_eq!(keyword_start_in_prefix("Artist - CD1 ", "cd"), None);
        assert_eq!(keyword_start_in_prefix("Artist - Disk 1", "disk"), Some(9));
        assert_eq!(
            keyword_start_in_prefix("Artist - Volume 1", "volume"),
            Some(9)
        );
    }
}
