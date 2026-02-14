use clap::Parser;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::Result;
use crate::cli::{Args, InputPair, resolve_input_path, resolve_matching_pairs};
use crate::cue::resolve_encoding;
use crate::flac::{SplitOptions, sanitize_filename, split_flac};

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

    let pairs = if args.flac.is_some() || args.cue.is_some() {
        vec![InputPair {
            flac: resolve_input_path(
                &base_dir_abs,
                display_base_abs.as_deref(),
                args.flac.as_ref(),
                "flac",
            )?,
            cue: resolve_input_path(
                &base_dir_abs,
                display_base_abs.as_deref(),
                args.cue.as_ref(),
                "cue",
            )?,
        }]
    } else {
        resolve_matching_pairs(&base_dir_abs, display_base_abs.as_deref())?
    };

    let output_subdirs = derive_output_subdirs(&pairs)?;

    for (pair, output_subdir) in pairs.into_iter().zip(output_subdirs.into_iter()) {
        let options = SplitOptions {
            flac_input: pair.flac,
            cue_input: pair.cue,
            display_base_abs: display_base_abs.clone(),
            cue_encoding: encoding,
            yes: args.yes,
            overwrite: args.overwrite,
            compression_level: args.compression_level,
            search_dir: base_dir_abs.clone(),
            picture_enabled,
            picture_path: picture_path.clone(),
            delete_original: args.delete_original,
            rename_original: args.rename_original,
            output_subdir,
        };

        split_flac(options)?;
    }

    Ok(())
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
                        "failed to derive basename from FLAC path: {}",
                        pair.flac.abs.display()
                    )
                })
        })
        .collect::<Result<Vec<String>>>()?;
    let stem_refs: Vec<&str> = stems.iter().map(String::as_str).collect();
    let prefix_len = longest_common_prefix_len(&stem_refs);
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
    use super::{derive_output_subdirs, longest_common_prefix_len, longest_common_suffix_len};
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
}
