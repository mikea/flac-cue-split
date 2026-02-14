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

        let stem = pairing_stem_for_extension(&path, &ext)?;

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

pub(crate) fn resolve_input_pairs(
    base_dir_abs: &Path,
    display_base_abs: Option<&Path>,
    flac: Option<&PathBuf>,
    cue: Option<&PathBuf>,
) -> Result<Vec<InputPair>> {
    if flac.is_some() || cue.is_some() {
        return Ok(vec![InputPair {
            flac: resolve_input_path(base_dir_abs, display_base_abs, flac, "flac")?,
            cue: resolve_input_path(base_dir_abs, display_base_abs, cue, "cue")?,
        }]);
    }

    let flacs = find_files_with_extension(base_dir_abs, "flac")?;
    let cues = find_files_with_extension(base_dir_abs, "cue")?;
    if flacs.len() == 1 && cues.len() == 1 {
        let flac_abs = flacs[0].clone();
        let cue_abs = cues[0].clone();
        return Ok(vec![InputPair {
            flac: InputPath {
                abs: flac_abs.clone(),
                display: display_path(display_base_abs, &flac_abs),
            },
            cue: InputPath {
                abs: cue_abs.clone(),
                display: display_path(display_base_abs, &cue_abs),
            },
        }]);
    }

    resolve_matching_pairs(base_dir_abs, display_base_abs)
}

fn find_files_with_extension(base_dir_abs: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    let read_dir = std::fs::read_dir(base_dir_abs).map_err(|err| {
        format!(
            "failed to read directory {}: {}",
            base_dir_abs.display(),
            err
        )
    })?;

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

    Ok(matches)
}

fn pairing_stem_for_extension(path: &Path, extension: &str) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("invalid unicode filename: {}", path.display()))?;
    if extension != "cue" {
        return Ok(stem.to_string());
    }

    Ok(strip_known_audio_suffix(stem).to_string())
}

fn strip_known_audio_suffix(stem: &str) -> &str {
    const KNOWN_AUDIO_EXTS: &[&str] = &[
        "flac", "wv", "ape", "wav", "tta", "alac", "aiff", "aif", "m4a", "mp3", "ogg",
    ];
    let Some((base, suffix)) = stem.rsplit_once('.') else {
        return stem;
    };
    if KNOWN_AUDIO_EXTS.contains(&suffix.to_ascii_lowercase().as_str()) {
        base
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_input_pairs, strip_known_audio_suffix};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn strip_known_audio_suffix_for_cue_basename() {
        assert_eq!(strip_known_audio_suffix("Album"), "Album");
        assert_eq!(strip_known_audio_suffix("Album.flac"), "Album");
        assert_eq!(strip_known_audio_suffix("Album.wv"), "Album");
        assert_eq!(strip_known_audio_suffix("Album.APE"), "Album");
        assert_eq!(strip_known_audio_suffix("Album.demo"), "Album.demo");
    }

    #[test]
    fn resolve_input_pairs_ignores_names_for_single_flac_and_cue() {
        let dir = unique_test_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("One Name.flac"), b"").unwrap();
        fs::write(dir.join("Different Name.wv.cue"), b"").unwrap();

        let pairs = resolve_input_pairs(&dir, Some(&dir), None, None).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs[0].flac.abs.file_name().unwrap().to_string_lossy(),
            "One Name.flac"
        );
        assert_eq!(
            pairs[0].cue.abs.file_name().unwrap().to_string_lossy(),
            "Different Name.wv.cue"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    fn unique_test_dir() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "flac-cue-split-test-{}-{}",
            std::process::id(),
            stamp
        ))
    }
}
