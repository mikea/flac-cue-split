use libflac_sys as flac;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::types::InputMetadata;

pub(crate) fn add_external_picture(
    meta: &mut InputMetadata,
    picture_names: &mut Vec<String>,
    search_dir: &Path,
) -> Result<()> {
    let picture_path = match find_picture_file(search_dir)? {
        Some(path) => path,
        None => return Ok(()),
    };

    let picture = load_picture_metadata(&picture_path)?;
    meta.pictures.push(picture);
    if let Some(name) = picture_path.file_name() {
        picture_names.push(name.to_string_lossy().into_owned());
    }
    Ok(())
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

fn load_picture_metadata(path: &Path) -> Result<*mut flac::FLAC__StreamMetadata> {
    let data = fs::read(path)
        .map_err(|err| format!("failed to read picture {}: {}", path.display(), err))?;
    if data.is_empty() {
        return Err(format!("picture {} is empty", path.display()));
    }

    let mime = picture_mime_type(path)
        .ok_or_else(|| format!("unsupported picture type: {}", path.display()))?;

    let object = unsafe { flac::FLAC__metadata_object_new(flac::FLAC__METADATA_TYPE_PICTURE) };
    if object.is_null() {
        return Err("failed to allocate picture metadata".to_string());
    }

    let mime_c = CString::new(mime)
        .map_err(|_| format!("picture mime type contains NUL: {}", mime))?;
    let desc_c = CString::new("").map_err(|_| "picture description contains NUL".to_string())?;

    unsafe {
        let picture = &mut (*object).data.picture;
        picture.type_ = flac::FLAC__STREAM_METADATA_PICTURE_TYPE_FRONT_COVER;
        picture.width = 0;
        picture.height = 0;
        picture.depth = 0;
        picture.colors = 0;
    }

    let ok = unsafe {
        flac::FLAC__metadata_object_picture_set_mime_type(object, mime_c.as_ptr() as *mut _, 1)
            != 0
    };
    if !ok {
        unsafe {
            flac::FLAC__metadata_object_delete(object);
        }
        return Err("failed to set picture mime type".to_string());
    }

    let ok = unsafe {
        flac::FLAC__metadata_object_picture_set_description(
            object,
            desc_c.as_ptr() as *mut flac::FLAC__byte,
            1,
        ) != 0
    };
    if !ok {
        unsafe {
            flac::FLAC__metadata_object_delete(object);
        }
        return Err("failed to set picture description".to_string());
    }

    let ok = unsafe {
        flac::FLAC__metadata_object_picture_set_data(
            object,
            data.as_ptr() as *mut flac::FLAC__byte,
            data.len() as u32,
            1,
        ) != 0
    };
    if !ok {
        unsafe {
            flac::FLAC__metadata_object_delete(object);
        }
        return Err("failed to set picture data".to_string());
    }

    let mut violation: *const i8 = std::ptr::null();
    let ok = unsafe { flac::FLAC__metadata_object_picture_is_legal(object, &mut violation) != 0 };
    if !ok {
        let msg = if violation.is_null() {
            "picture metadata is invalid".to_string()
        } else {
            unsafe { CStr::from_ptr(violation).to_string_lossy().into_owned() }
        };
        unsafe {
            flac::FLAC__metadata_object_delete(object);
        }
        return Err(msg);
    }

    Ok(object)
}

fn picture_mime_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
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
