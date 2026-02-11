use clap::Parser;

use crate::cli::{resolve_input_path, Args};
use crate::cue::resolve_encoding;
use crate::flac::{split_flac, SplitOptions};
use crate::Result;

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

    let flac_input =
        resolve_input_path(&base_dir_abs, display_base_abs.as_deref(), args.flac.as_ref(), "flac")?;
    let cue_input =
        resolve_input_path(&base_dir_abs, display_base_abs.as_deref(), args.cue.as_ref(), "cue")?;

    let picture_enabled = if args.no_picture { false } else { args.picture };

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
    };

    split_flac(options)
}
