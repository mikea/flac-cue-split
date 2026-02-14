# flac-cue-split

Split a single FLAC into per-track FLAC files using a CUE sheet.

## Installation

Install dependencies:

```
sudo apt install cmake bison flex
```

Installation:

```bash
cargo install --git https://github.com/mikea/flac-cue-split
```

## Usage

Run (auto-detects `.flac` and `.cue` in current directory):

```bash
flac-cue-split
```

Run with explicit files:

```bash
flac-cue-split --flac "Album.flac" --cue "Album.cue"
```

Run in a different directory (positional `DIR`):

```bash
flac-cue-split /path/to/album
```

Skip confirmation:

```bash
flac-cue-split -y
```

Overwrite existing outputs:

```bash
flac-cue-split -o
```

Set compression level (0-8 or `max`):

```bash
flac-cue-split -c 8
flac-cue-split --compression-level max
```

Pick a specific picture file:

```bash
flac-cue-split --picture cover.jpg
```

Disable picture auto-detect:

```bash
flac-cue-split --no-picture
```

Delete original FLAC after successful split:

```bash
flac-cue-split --delete-original
```

Rename original FLAC after successful split:

```bash
flac-cue-split -r
```

Force cue encoding:

```bash
flac-cue-split --cue-encoding windows-1251
```

## Behavior

- If `--flac` or `--cue` is not provided, the tool searches the chosen directory for exactly one `.flac` and one `.cue` file. It fails if none or multiple are found.
- Output files are written next to the input FLAC using the pattern `NN - Title.flac`.
- The tool prints a plan, shared tags, and per-track unique tags, then asks for confirmation.
- A progress bar is shown during encoding.
- If `--picture <FILE>` is provided, that file is embedded as the cover image.
- Otherwise, if there is exactly one image file in the chosen directory (jpg/jpeg/png/gif/bmp/webp/tif/tiff), it is embedded as a cover picture in all output files (unless `--no-picture` is used).
- Cue encoding is auto-detected (UTF-8, otherwise Windows-1251) and shown in the plan. You can override it with `--cue-encoding`.
- `--delete-original` removes the input FLAC after a successful split.
- `--rename-original` (or `-r`) renames the input FLAC to `*.flac.processed` after a successful split.

## Options

- `--flac <FILE>`: Path to input FLAC
- `--cue <FILE>`: Path to input CUE
- `--cue-encoding <ENCODING>`: Force cue text encoding (example: `windows-1251`)
- `-y, --yes`: Skip confirmation
- `-o, --overwrite`: Overwrite existing output files
- `-c, --compression-level <LEVEL>`: FLAC compression level (0-8 or `max`)
- `--picture <FILE>`: Use a specific picture file
- `--no-picture`: Disable picture auto-detection
- `--delete-original`: Delete input FLAC after successful split
- `-r, --rename-original`: Rename input FLAC to `*.flac.processed` after successful split
- `DIR`: Optional directory to scan for input files
