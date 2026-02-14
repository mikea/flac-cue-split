use libflac_sys as flac;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::flac::FlacMetadata;
use crate::types::InputMetadata;

pub(crate) fn add_external_picture(
    meta: &mut InputMetadata,
    picture_names: &mut Vec<String>,
    search_dir: &Path,
    explicit_path: Option<&Path>,
) -> Result<()> {
    let picture_path = match explicit_path {
        Some(path) => path.to_path_buf(),
        None => match find_picture_file(search_dir)? {
            Some(path) => path,
            None => return Ok(()),
        },
    };

    let picture = load_picture_metadata(&picture_path)?;
    meta.pictures.push(picture);
    if let Some(name) = picture_path.file_name() {
        picture_names.push(name.to_string_lossy().into_owned());
    }
    Ok(())
}

pub(crate) fn build_picture_metadata_from_data(
    data: &[u8],
    filename_hint: Option<&str>,
) -> Result<FlacMetadata> {
    if data.is_empty() {
        return Err("embedded picture is empty".to_string());
    }

    let mime =
        picture_mime_type_from_name(filename_hint).or_else(|| picture_mime_type_from_data(data));
    let mime = mime.ok_or_else(|| "unsupported embedded picture type".to_string())?;
    create_picture_metadata(data, mime)
}

fn find_picture_file(dir: &Path) -> Result<Option<PathBuf>> {
    let mut matches = Vec::new();
    let read_dir = fs::read_dir(dir)
        .map_err(|err| format!("failed to read directory {}: {}", dir.display(), err))?;
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
        if matches_picture_extension(&ext) {
            matches.push(path);
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0))),
        _ => Err(format!(
            "multiple picture files found in {}, use --no-picture or keep one",
            dir.display()
        )),
    }
}

fn matches_picture_extension(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "tif" | "tiff"
    )
}

fn load_picture_metadata(path: &Path) -> Result<FlacMetadata> {
    let data = fs::read(path)
        .map_err(|err| format!("failed to read picture {}: {}", path.display(), err))?;
    let mime = picture_mime_type(path).or_else(|| picture_mime_type_from_data(&data));
    let mime = mime.ok_or_else(|| format!("unsupported picture type: {}", path.display()))?;
    create_picture_metadata(&data, mime)
}

fn create_picture_metadata(data: &[u8], mime: &str) -> Result<FlacMetadata> {
    let mut object = FlacMetadata::new(flac::FLAC__METADATA_TYPE_PICTURE)?;

    let mime_c =
        CString::new(mime).map_err(|_| format!("picture mime type contains NUL: {}", mime))?;
    let desc_c = CString::new("").map_err(|_| "picture description contains NUL".to_string())?;

    {
        let picture = unsafe { &mut object.as_mut().data.picture };
        picture.type_ = flac::FLAC__STREAM_METADATA_PICTURE_TYPE_FRONT_COVER;
        picture.width = 0;
        picture.height = 0;
        picture.depth = 0;
        picture.colors = 0;
    }

    let ok = unsafe {
        flac::FLAC__metadata_object_picture_set_mime_type(
            object.as_mut_ptr(),
            mime_c.as_ptr() as *mut _,
            1,
        ) != 0
    };
    if !ok {
        return Err("failed to set picture mime type".to_string());
    }

    let ok = unsafe {
        flac::FLAC__metadata_object_picture_set_description(
            object.as_mut_ptr(),
            desc_c.as_ptr() as *mut flac::FLAC__byte,
            1,
        ) != 0
    };
    if !ok {
        return Err("failed to set picture description".to_string());
    }

    let ok = unsafe {
        flac::FLAC__metadata_object_picture_set_data(
            object.as_mut_ptr(),
            data.as_ptr() as *mut flac::FLAC__byte,
            data.len() as u32,
            1,
        ) != 0
    };
    if !ok {
        return Err("failed to set picture data".to_string());
    }

    let mut violation: *const i8 = std::ptr::null();
    let ok = unsafe {
        flac::FLAC__metadata_object_picture_is_legal(object.as_mut_ptr(), &mut violation) != 0
    };
    if !ok {
        let msg = if violation.is_null() {
            "picture metadata is invalid".to_string()
        } else {
            unsafe { CStr::from_ptr(violation).to_string_lossy().into_owned() }
        };
        return Err(msg);
    }

    Ok(object)
}

fn picture_mime_type(path: &Path) -> Option<&'static str> {
    picture_mime_type_from_name(path.file_name().and_then(|name| name.to_str()))
}

fn picture_mime_type_from_name(name: Option<&str>) -> Option<&'static str> {
    let name = name?;
    let (_, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        "webp" => Some("image/webp"),
        "tif" | "tiff" => Some("image/tiff"),
        _ => None,
    }
}

fn picture_mime_type_from_data(data: &[u8]) -> Option<&'static str> {
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return Some("image/jpeg");
    }
    if data.len() >= 8 && data[0..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("image/png");
    }
    if data.len() >= 6 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        return Some("image/gif");
    }
    if data.len() >= 4 && &data[0..4] == b"RIFF" && data.len() >= 12 && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if data.len() >= 2
        && ((data[0] == b'I' && data[1] == b'I') || (data[0] == b'M' && data[1] == b'M'))
    {
        return Some("image/tiff");
    }
    if data.len() >= 2 && data[0] == b'B' && data[1] == b'M' {
        return Some("image/bmp");
    }

    None
}
