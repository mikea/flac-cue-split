use clap::Parser;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::Result;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub(crate) struct Args {
    #[arg(long)]
    pub(crate) flac: Option<PathBuf>,
    #[arg(long)]
    pub(crate) cue: Option<PathBuf>,
    #[arg(long, value_name = "ENCODING")]
    pub(crate) cue_encoding: Option<String>,
    #[arg(short = 'y', long)]
    pub(crate) yes: bool,
    #[arg(short = 'o', long)]
    pub(crate) overwrite: bool,
    #[arg(short = 'c', long, default_value_t = 5, value_parser = parse_compression_level)]
    pub(crate) compression_level: u8,
    #[arg(value_name = "DIR")]
    pub(crate) dir: Option<PathBuf>,
    #[arg(long, value_name = "FILE")]
    pub(crate) picture: Option<PathBuf>,
    #[arg(long, conflicts_with = "picture")]
    pub(crate) no_picture: bool,
    #[arg(long, conflicts_with = "rename_original")]
    pub(crate) delete_original: bool,
    #[arg(short = 'r', long, conflicts_with = "delete_original")]
    pub(crate) rename_original: bool,
}

pub(crate) struct InputPath {
    pub(crate) abs: PathBuf,
    pub(crate) display: PathBuf,
}

pub(crate) struct InputPair {
    pub(crate) flac: InputPath,
    pub(crate) cue: InputPath,
}

pub(crate) fn parse_compression_level(value: &str) -> Result<u8> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("max") {
        return Ok(8);
    }
    let level: u8 = trimmed
        .parse()
        .map_err(|_| "compression level must be 0-8 or 'max'".to_string())?;
    if level > 8 {
        return Err("compression level must be 0-8 or 'max'".to_string());
    }
    Ok(level)
}

pub(crate) fn resolve_input_path(
    base_dir_abs: &Path,
    display_base_abs: Option<&Path>,
    provided: Option<&PathBuf>,
    extension: &str,
) -> Result<InputPath> {
    if let Some(path) = provided {
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            base_dir_abs.join(path)
        };
        if !abs.exists() {
            return Err(format!("file not found: {}", abs.display()));
        }
        let display = display_path(display_base_abs, &abs);
        return Ok(InputPath { abs, display });
    }

    let abs = resolve_or_find_file(base_dir_abs, None, extension)?;
    let display = display_path(display_base_abs, &abs);
    Ok(InputPath { abs, display })
}

pub(crate) fn display_path(base: Option<&Path>, path: &Path) -> PathBuf {
    if let Some(base) = base
        && let Ok(rel) = path.strip_prefix(base)
    {
        if rel.as_os_str().is_empty() {
            return PathBuf::from(".");
        }
        return rel.to_path_buf();
    }
    path.to_path_buf()
}

fn resolve_or_find_file(
    base_dir: &Path,
    provided: Option<&PathBuf>,
    extension: &str,
) -> Result<PathBuf> {
    if let Some(path) = provided {
        let resolved = if path.is_absolute() {
            path.clone()
        } else {
            base_dir.join(path)
        };
        if !resolved.exists() {
            return Err(format!("file not found: {}", resolved.display()));
        }
        return Ok(resolved);
    }

    let mut matches = Vec::new();
    let read_dir = std::fs::read_dir(base_dir)
        .map_err(|err| format!("failed to read directory {}: {}", base_dir.display(), err))?;
    for entry in read_dir {
        let entry = entry.map_err(|err| format!("failed to read directory entry: {}", err))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => ext.to_ascii_lowercase(),
            None => continue,
        };
        if ext == extension {
            matches.push(path);
        }
    }

    match matches.len() {
        0 => Err(format!(
            "no .{} file found in {}",
            extension,
            base_dir.display()
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "multiple .{} files found in {}, please specify --{}",
            extension,
            base_dir.display(),
            extension
        )),
    }
}

pub(crate) fn resolve_matching_pairs(
    base_dir_abs: &Path,
    display_base_abs: Option<&Path>,
) -> Result<Vec<InputPair>> {
    let read_dir = std::fs::read_dir(base_dir_abs).map_err(|err| {
        format!(
            "failed to read directory {}: {}",
            base_dir_abs.display(),
            err
        )
    })?;

    let mut flac_by_stem = BTreeMap::<String, PathBuf>::new();
    let mut cue_by_stem = BTreeMap::<String, PathBuf>::new();

    for entry in read_dir {
        let entry = entry.map_err(|err| format!("failed to read directory entry: {}", err))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => ext.to_ascii_lowercase(),
            None => continue,
        };
        if ext != "flac" && ext != "cue" {
            continue;
        }

        let stem = match path.file_stem().and_then(|stem| stem.to_str()) {
            Some(stem) => stem.to_string(),
            None => {
                return Err(format!("invalid unicode filename: {}", path.display()));
            }
        };

        let target = if ext == "flac" {
            &mut flac_by_stem
        } else {
            &mut cue_by_stem
        };
        if let Some(existing) = target.insert(stem.clone(), path.clone()) {
            return Err(format!(
                "multiple .{} files with basename {:?}: {} and {}",
                ext,
                stem,
                existing.display(),
                path.display()
            ));
        }
    }

    if flac_by_stem.is_empty() {
        return Err(format!("no .flac file found in {}", base_dir_abs.display()));
    }
    if cue_by_stem.is_empty() {
        return Err(format!("no .cue file found in {}", base_dir_abs.display()));
    }
    if flac_by_stem.len() != cue_by_stem.len() {
        return Err(format!(
            "found {} .flac files but {} .cue files in {}; counts must match",
            flac_by_stem.len(),
            cue_by_stem.len(),
            base_dir_abs.display()
        ));
    }

    let missing_cue: Vec<&str> = flac_by_stem
        .keys()
        .filter(|stem| !cue_by_stem.contains_key(*stem))
        .map(String::as_str)
        .collect();
    if !missing_cue.is_empty() {
        return Err(format!(
            "missing .cue file(s) for basename(s): {}",
            missing_cue.join(", ")
        ));
    }

    let missing_flac: Vec<&str> = cue_by_stem
        .keys()
        .filter(|stem| !flac_by_stem.contains_key(*stem))
        .map(String::as_str)
        .collect();
    if !missing_flac.is_empty() {
        return Err(format!(
            "missing .flac file(s) for basename(s): {}",
            missing_flac.join(", ")
        ));
    }

    let mut pairs = Vec::with_capacity(flac_by_stem.len());
    for (stem, flac_abs) in flac_by_stem {
        let cue_abs = cue_by_stem
            .get(&stem)
            .ok_or_else(|| format!("missing .cue file for basename {}", stem))?
            .clone();
        pairs.push(InputPair {
            flac: InputPath {
                display: display_path(display_base_abs, &flac_abs),
                abs: flac_abs,
            },
            cue: InputPath {
                display: display_path(display_base_abs, &cue_abs),
                abs: cue_abs,
            },
        });
    }

    Ok(pairs)
}
