use clap::Parser;

use crate::Result;
use crate::cli::{Args, resolve_input_path};
use crate::cue::resolve_encoding;
use crate::flac::{SplitOptions, split_flac};

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

    let flac_input = resolve_input_path(
        &base_dir_abs,
        display_base_abs.as_deref(),
        args.flac.as_ref(),
        "flac",
    )?;
    let cue_input = resolve_input_path(
        &base_dir_abs,
        display_base_abs.as_deref(),
        args.cue.as_ref(),
        "cue",
    )?;

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

    let options = SplitOptions {
        flac_input,
        cue_input,
        display_base_abs,
        cue_encoding: encoding,
        yes: args.yes,
        overwrite: args.overwrite,
        compression_level: args.compression_level,
        search_dir: base_dir_abs,
        picture_enabled,
        picture_path,
    };

    split_flac(options)
}
