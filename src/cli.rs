use clap::Parser;
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
    #[arg(long, default_value_t = true)]
    pub(crate) picture: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "picture")]
    pub(crate) no_picture: bool,
}

pub(crate) struct InputPath {
    pub(crate) abs: PathBuf,
    pub(crate) display: PathBuf,
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
    if let Some(base) = base {
        if let Ok(rel) = path.strip_prefix(base) {
            if rel.as_os_str().is_empty() {
                return PathBuf::from(".");
            }
            return rel.to_path_buf();
        }
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
