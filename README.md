# flac-cue-split

Split a single FLAC into per-track FLAC files using a CUE sheet.

## Usage

Build:

```bash
cargo build
```

Run (auto-detects `.flac` and `.cue` in current directory):

```bash
./target/debug/flac-cue-split
```

Run with explicit files:

```bash
./target/debug/flac-cue-split --flac "Album.flac" --cue "Album.cue"
```

Run in a different directory (positional `DIR`):

```bash
./target/debug/flac-cue-split /path/to/album
```

Skip confirmation:

```bash
./target/debug/flac-cue-split -y
```

Overwrite existing outputs:

```bash
./target/debug/flac-cue-split -o
```

Set compression level (0-8 or `max`):

```bash
./target/debug/flac-cue-split -c 8
./target/debug/flac-cue-split --compression-level max
```

Disable picture auto-detect:

```bash
./target/debug/flac-cue-split --no-picture
```

Force cue encoding:

```bash
./target/debug/flac-cue-split --cue-encoding windows-1251
```

## Behavior

- If `--flac` or `--cue` is not provided, the tool searches the chosen directory for exactly one `.flac` and one `.cue` file. It fails if none or multiple are found.
- Output files are written next to the input FLAC using the pattern `NN - Title.flac`.
- The tool prints a plan, shared tags, and per-track unique tags, then asks for confirmation.
- A progress bar is shown during encoding.
- If there is exactly one image file in the chosen directory (jpg/jpeg/png/gif/bmp/webp/tif/tiff), it is embedded as a cover picture in all output files (unless `--no-picture` is used).
- Cue encoding is auto-detected (UTF-8, otherwise Windows-1251) and shown in the plan. You can override it with `--cue-encoding`.

## Options

- `--flac <FILE>`: Path to input FLAC
- `--cue <FILE>`: Path to input CUE
- `--cue-encoding <ENCODING>`: Force cue text encoding (example: `windows-1251`)
- `-y, --yes`: Skip confirmation
- `-o, --overwrite`: Overwrite existing output files
- `-c, --compression-level <LEVEL>`: FLAC compression level (0-8 or `max`)
- `--picture` / `--no-picture`: Enable/disable picture auto-detection (default: enabled)
- `DIR`: Optional directory to scan for input files

