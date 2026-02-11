use cue_sys as cue;
use encoding_rs::{Encoding, UTF_8, WINDOWS_1251};
use libc::{c_int, c_void as libc_void};
use owo_colors::OwoColorize;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::Path;

use crate::Result;
use crate::types::{CueDisc, CueRem, CueTrack};

const REM_DATE: u32 = 0;
const REM_REPLAYGAIN_ALBUM_GAIN: u32 = 1;
const REM_REPLAYGAIN_ALBUM_PEAK: u32 = 2;
const REM_REPLAYGAIN_TRACK_GAIN: u32 = 3;
const REM_REPLAYGAIN_TRACK_PEAK: u32 = 4;

pub(crate) fn resolve_encoding(label: &str) -> Result<&'static Encoding> {
    Encoding::for_label(label.as_bytes())
        .ok_or_else(|| format!("unsupported cue encoding: {}", label))
}

pub(crate) fn parse_cue_file(
    path: &Path,
    encoding: Option<&'static Encoding>,
) -> Result<(CueDisc, Vec<String>, &'static Encoding, bool)> {
    let contents = fs::read(path)
        .map_err(|err| format!("failed to read cue file {}: {}", path.display(), err))?;
    let (encoding, autodetected) = match encoding {
        Some(enc) => (enc, false),
        None => (detect_cue_encoding(&contents), true),
    };
    parse_cue_from_bytes(&contents, encoding)
        .map(|(disc, warnings, used)| (disc, warnings, used, autodetected))
}

#[cfg(test)]
pub(crate) fn parse_cue_from_str(contents: &str) -> Result<CueDisc> {
    let (disc, _, _) = parse_cue_from_bytes(contents.as_bytes(), UTF_8)?;
    Ok(disc)
}

fn parse_cue_from_bytes(
    contents: &[u8],
    encoding: &'static Encoding,
) -> Result<(CueDisc, Vec<String>, &'static Encoding)> {
    let cue_cstr =
        CString::new(contents).map_err(|_| "cue file contains NUL byte".to_string())?;
    let capture = StderrCapture::start()?;
    let cd = unsafe { cue::cue_parse_string(cue_cstr.as_ptr()) };
    let stderr = capture.finish()?;
    let warnings = parse_cue_warnings(&stderr, contents, encoding);
    if cd.is_null() {
        let mut message = "failed to parse cue file".to_string();
        let warning_text = format_cue_warnings(&warnings);
        if !warning_text.is_empty() {
            message.push('\n');
            message.push_str(&warning_text);
        }
        return Err(message);
    }

    let result = unsafe { parse_cd(cd, encoding) };
    unsafe {
        cue::cd_delete(cd);
    }
    result.map(|disc| (disc, warnings, encoding))
}

pub(crate) fn report_cue_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("{}", warning.yellow());
    }
}

fn format_cue_warnings(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    warnings.join("\n")
}

fn parse_cue_warnings(
    stderr: &str,
    contents: &[u8],
    encoding: &'static Encoding,
) -> Vec<String> {
    let (decoded, _, _) = encoding.decode(contents);
    let cue_lines: Vec<String> = decoded
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();
    let total_lines = if contents.is_empty() {
        0usize
    } else {
        contents.iter().filter(|byte| **byte == b'\n').count() + 1
    };

    let mut warnings = Vec::new();
    for raw in stderr.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        if let Some((num, message)) = parse_cue_warning_line(line) {
            let mut warning = format!("cue parse: line {}: {}", num, message);
            if let Some(source) = cue_lines.get(num.saturating_sub(1) as usize) {
                if !source.trim().is_empty() {
                    warning.push('\n');
                    warning.push_str("    ");
                    warning.push_str(source);
                }
            } else if total_lines > 0 {
                warning.push_str(&format!(
                    " (line out of range; file has {} lines)",
                    total_lines
                ));
            }
            warnings.push(warning);
        } else {
            warnings.push(format!("cue parse: {}", line));
        }
    }

    warnings
}

fn parse_cue_warning_line(line: &str) -> Option<(u32, String)> {
    let mut parts = line.splitn(2, ':');
    let num_part = parts.next()?.trim();
    let message = parts.next()?.trim();
    let num: u32 = num_part.parse().ok()?;
    Some((num, message.to_string()))
}

fn detect_cue_encoding(bytes: &[u8]) -> &'static Encoding {
    if std::str::from_utf8(bytes).is_ok() {
        UTF_8
    } else {
        WINDOWS_1251
    }
}

unsafe fn parse_cd(cd: *mut cue::CdPointer, encoding: &'static Encoding) -> Result<CueDisc> {
    if cd.is_null() {
        return Err("cue parser returned null CD".to_string());
    }

    let disc_mode = unsafe { cue::cd_get_mode(cd) };
    if !matches!(disc_mode, cue::DiscMode::CD_DA) {
        return Err("cue sheet is not audio (CD_DA)".to_string());
    }

    let cdtext = unsafe { cue::cd_get_cdtext(cd) };
    let rem = cue_rem_from_ptr(unsafe { cue::cd_get_rem(cd) }, encoding);

    let title = cdtext_string(cdtext, cue::PTI::Title, encoding);
    let performer = cdtext_string(cdtext, cue::PTI::Performer, encoding);
    let songwriter = cdtext_string(cdtext, cue::PTI::Songwriter, encoding);
    let composer = cdtext_string(cdtext, cue::PTI::Composer, encoding);
    let genre = cdtext_string(cdtext, cue::PTI::Genre, encoding);
    let message = cdtext_string(cdtext, cue::PTI::Message, encoding);
    let disc_id = cdtext_string(cdtext, cue::PTI::DiscID, encoding);

    let ntrack = unsafe { cue::cd_get_ntrack(cd) };
    if ntrack <= 0 {
        return Err("cue sheet has no tracks".to_string());
    }

    let mut tracks = Vec::with_capacity(ntrack as usize);
    for index in 1..=ntrack {
        let track_ptr = unsafe { cue::cd_get_track(cd, index) };
        if track_ptr.is_null() {
            return Err(format!("failed to read track {}", index));
        }

        if !matches!(unsafe { cue::track_get_mode(track_ptr) }, cue::TrackMode::Audio) {
            return Err(format!("track {} is not audio", index));
        }

        let track_cdtext = unsafe { cue::track_get_cdtext(track_ptr) };
        let track_rem = cue_rem_from_ptr(unsafe { cue::track_get_rem(track_ptr) }, encoding);
        let filename =
            opt_cstr_with_encoding(unsafe { cue::track_get_filename(track_ptr) }, encoding);

        let start = unsafe { cue::track_get_start(track_ptr) };
        if start < 0 {
            return Err(format!("track {} has invalid start", index));
        }

        let length = unsafe { cue::track_get_length(track_ptr) };
        let length_frames = if length < 0 { None } else { Some(length) };

        let track = CueTrack {
            number: index as u32,
            title: cdtext_string(track_cdtext, cue::PTI::Title, encoding),
            performer: cdtext_string(track_cdtext, cue::PTI::Performer, encoding),
            songwriter: cdtext_string(track_cdtext, cue::PTI::Songwriter, encoding),
            composer: cdtext_string(track_cdtext, cue::PTI::Composer, encoding),
            isrc: opt_cstr_with_encoding(unsafe { cue::track_get_isrc(track_ptr) }, encoding),
            start_frames: start,
            length_frames,
            filename,
            rem: track_rem,
        };
        tracks.push(track);
    }

    Ok(CueDisc {
        title,
        performer,
        songwriter,
        composer,
        genre,
        message,
        disc_id,
        rem,
        tracks,
    })
}

fn cdtext_string(
    cdtext: *mut cue::CdtextPointer,
    pti: cue::PTI,
    encoding: &'static Encoding,
) -> Option<String> {
    if cdtext.is_null() {
        return None;
    }
    let ptr = unsafe { cue::cdtext_get(pti, cdtext) };
    opt_cstr_with_encoding(ptr, encoding)
}

fn opt_cstr_with_encoding(
    ptr: *mut std::os::raw::c_char,
    encoding: &'static Encoding,
) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let bytes = unsafe { CStr::from_ptr(ptr).to_bytes() };
    let (decoded, _, _) = encoding.decode(bytes);
    Some(decoded.into_owned())
}

fn cue_rem_from_ptr(rem: *mut cue::RemPointer, encoding: &'static Encoding) -> CueRem {
    if rem.is_null() {
        return CueRem::default();
    }

    CueRem {
        date: rem_get_string(rem, REM_DATE, encoding),
        replaygain_album_gain: rem_get_string(rem, REM_REPLAYGAIN_ALBUM_GAIN, encoding),
        replaygain_album_peak: rem_get_string(rem, REM_REPLAYGAIN_ALBUM_PEAK, encoding),
        replaygain_track_gain: rem_get_string(rem, REM_REPLAYGAIN_TRACK_GAIN, encoding),
        replaygain_track_peak: rem_get_string(rem, REM_REPLAYGAIN_TRACK_PEAK, encoding),
    }
}

fn rem_get_string(rem: *mut cue::RemPointer, key: u32, encoding: &'static Encoding) -> Option<String> {
    if rem.is_null() {
        return None;
    }
    let ptr = unsafe { cue::rem_get(key, rem) };
    opt_cstr_with_encoding(ptr, encoding)
}

struct StderrCapture {
    read_fd: c_int,
    old_fd: c_int,
}

impl StderrCapture {
    fn start() -> Result<Self> {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err("failed to create pipe for stderr capture".to_string());
        }

        let old_fd = unsafe { libc::dup(libc::STDERR_FILENO) };
        if old_fd == -1 {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err("failed to dup stderr".to_string());
        }

        let rc = unsafe { libc::dup2(fds[1], libc::STDERR_FILENO) };
        unsafe {
            libc::close(fds[1]);
        }
        if rc == -1 {
            unsafe {
                libc::close(fds[0]);
                libc::close(old_fd);
            }
            return Err("failed to redirect stderr".to_string());
        }

        Ok(Self {
            read_fd: fds[0],
            old_fd,
        })
    }

    fn finish(self) -> Result<String> {
        unsafe {
            libc::dup2(self.old_fd, libc::STDERR_FILENO);
            libc::close(self.old_fd);
        }

        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = unsafe {
                libc::read(
                    self.read_fd,
                    chunk.as_mut_ptr() as *mut libc_void,
                    chunk.len(),
                )
            };
            if read <= 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read as usize]);
        }
        unsafe {
            libc::close(self.read_fd);
        }

        Ok(String::from_utf8_lossy(&buffer).into_owned())
    }
}
