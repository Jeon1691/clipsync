#![cfg(target_os = "macos")]

use std::path::PathBuf;

use async_trait::async_trait;
use objc2::runtime::ProtocolObject;
use objc2::ClassType;
#[allow(deprecated)]
use objc2_app_kit::NSFilenamesPboardType;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    NSPasteboardURLReadingFileURLsOnlyKey,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber, NSString, NSURL};

use crate::detect::{hash_item, Capabilities};
use crate::item::{tiff_to_png, ClipboardItem, FileRef, ImageMime};
use crate::{ClipboardBackend, ClipboardError, Result};

pub struct MacClipboard;

impl MacClipboard {
    pub fn new() -> Result<Self> {
        let _ = NSPasteboard::generalPasteboard();
        Ok(Self)
    }

    fn read_blocking(&self) -> Result<Option<ClipboardItem>> {
        read_pasteboard(&NSPasteboard::generalPasteboard())
    }

    fn write_blocking(&self, item: &ClipboardItem) -> Result<()> {
        let pb = NSPasteboard::generalPasteboard();
        match item {
            ClipboardItem::Text { text } => {
                pb.clearContents();
                let ty = unsafe { NSPasteboardTypeString };
                if !pb.setString_forType(&NSString::from_str(text), ty) {
                    return Err(ClipboardError::Message("failed to set text".into()));
                }
            }
            ClipboardItem::Image { bytes, mime, .. } => write_image(&pb, *mime, bytes)?,
            ClipboardItem::Files { files } => {
                let paths: Vec<PathBuf> =
                    files.iter().filter_map(|f| f.staged_path.clone()).collect();
                if paths.len() != files.len() {
                    return Err(ClipboardError::Message(
                        "file items need staged inbox paths to land on the pasteboard".into(),
                    ));
                }
                publish_files(&pb, &paths, None)?;
            }
        }
        Ok(())
    }
}

fn read_pasteboard(pb: &NSPasteboard) -> Result<Option<ClipboardItem>> {
    let paths = file_paths(pb)?;
    // Image sidecars exist only so the Desktop can paste a file. Reading them
    // back as files would change the item hash and echo the image to peers.
    if crate::stage::paths_are_image_sidecars(&paths) {
        if let Some(item) = image_from_sidecar(&paths)? {
            return Ok(Some(item));
        }
    }
    if let Some(files) = load_file_refs(&paths)? {
        return Ok(Some(ClipboardItem::Files { files }));
    }
    if let Some(item) = read_image_data(pb)? {
        return Ok(Some(item));
    }
    let ty = unsafe { NSPasteboardTypeString };
    if let Some(s) = pb.stringForType(ty) {
        let text = s.to_string();
        if !text.is_empty() {
            return Ok(Some(ClipboardItem::Text { text }));
        }
    }
    Ok(None)
}

fn read_image_data(pb: &NSPasteboard) -> Result<Option<ClipboardItem>> {
    let png_ty = unsafe { NSPasteboardTypePNG };
    if let Some(data) = pb.dataForType(png_ty) {
        let bytes = data.to_vec();
        if let Some(mime) = ImageMime::detect(&bytes) {
            return Ok(Some(ClipboardItem::Image {
                mime,
                bytes,
                width: None,
                height: None,
            }));
        }
    }
    let tiff_ty = unsafe { NSPasteboardTypeTIFF };
    if let Some(data) = pb.dataForType(tiff_ty) {
        let raw = data.to_vec();
        let bytes = tiff_to_png(&raw).unwrap_or(raw);
        if let Some(mime) = ImageMime::detect(&bytes) {
            return Ok(Some(ClipboardItem::Image {
                mime,
                bytes,
                width: None,
                height: None,
            }));
        }
    }
    Ok(None)
}

fn image_from_sidecar(paths: &[PathBuf]) -> Result<Option<ClipboardItem>> {
    if paths.len() != 1 {
        return Ok(None);
    }
    let Some(files) = load_file_refs(paths)? else {
        return Ok(None);
    };
    let Some(file) = files.first() else {
        return Ok(None);
    };
    let Some(mime) = ImageMime::detect(&file.bytes) else {
        return Ok(None);
    };
    Ok(Some(ClipboardItem::Image {
        mime,
        bytes: file.bytes.clone(),
        width: None,
        height: None,
    }))
}

fn write_image(pb: &NSPasteboard, mime: ImageMime, bytes: &[u8]) -> Result<()> {
    match crate::stage::stage_image(mime, bytes) {
        Ok(path) => {
            if publish_files(pb, std::slice::from_ref(&path), Some(bytes)).is_ok() {
                if let Some(dir) = path.parent() {
                    crate::stage::sweep_other_clipboard_files(dir, &path);
                }
                return Ok(());
            }
            tracing::warn!("desktop file pasteboard write failed; falling back to image bytes");
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not stage an image file for Desktop paste");
        }
    }
    write_image_bytes_only(pb, bytes)
}

fn file_paths(pb: &NSPasteboard) -> Result<Vec<PathBuf>> {
    let from_urls = paths_from_file_urls(pb)?;
    let from_names = paths_from_filenames(pb);
    if from_names.len() > from_urls.len() {
        Ok(from_names)
    } else {
        Ok(from_urls)
    }
}

fn paths_from_file_urls(pb: &NSPasteboard) -> Result<Vec<PathBuf>> {
    let class_array = NSArray::from_slice(&[NSURL::class()]);
    let options = NSDictionary::from_slices(
        &[unsafe { NSPasteboardURLReadingFileURLsOnlyKey }],
        &[NSNumber::new_bool(true).as_ref()],
    );
    let objects = unsafe { pb.readObjectsForClasses_options(&class_array, Some(options.as_ref())) };
    let Some(array) = objects else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::new();
    for obj in array.iter() {
        let Ok(url) = obj.downcast::<NSURL>() else {
            continue;
        };
        let Some(path) = url.path() else {
            continue;
        };
        paths.push(PathBuf::from(path.to_string()));
    }
    Ok(paths)
}

#[allow(deprecated)]
fn paths_from_filenames(pb: &NSPasteboard) -> Vec<PathBuf> {
    let ty = unsafe { NSFilenamesPboardType };
    let Some(obj) = pb.propertyListForType(ty) else {
        return Vec::new();
    };
    let Some(array) = obj.downcast_ref::<NSArray>() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|item| {
            item.downcast::<NSString>()
                .ok()
                .map(|s| PathBuf::from(s.to_string()))
        })
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

fn load_file_refs(paths: &[PathBuf]) -> Result<Option<Vec<FileRef>>> {
    let mut files = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if meta.file_type().is_symlink() || meta.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let bytes = std::fs::read(path)?;
        files.push(FileRef {
            mime: infer::get(&bytes).map(|k| k.mime_type().to_string()),
            name: name.to_string(),
            bytes,
            staged_path: Some(path.clone()),
        });
    }
    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(files))
    }
}

/// File URLs let Finder copy into whichever folder has focus when the user
/// pastes. A preset filename list makes Finder move that file instead.
fn publish_files(pb: &NSPasteboard, paths: &[PathBuf], image: Option<&[u8]>) -> Result<()> {
    let abs = canonical_paths(paths)?;
    let urls = file_url_objects(&abs)?;
    pb.clearContents();
    if !pb.writeObjects(&NSArray::from_retained_slice(&urls)) {
        return Err(ClipboardError::Message(
            "NSPasteboard writeObjects failed for files".into(),
        ));
    }
    let png = image.and_then(|bytes| transcode_png(bytes).ok());
    let tiff = image.and_then(|bytes| png_or_raw_to_tiff(bytes).ok());
    let png_ty = unsafe { NSPasteboardTypePNG };
    let tiff_ty = unsafe { NSPasteboardTypeTIFF };
    let mut extra: Vec<&NSString> = Vec::new();
    if png.is_some() {
        extra.push(png_ty);
    }
    if tiff.is_some() {
        extra.push(tiff_ty);
    }
    if extra.is_empty() {
        return Ok(());
    }
    let _ = unsafe { pb.addTypes_owner(&NSArray::from_slice(&extra), None) };
    if let Some(png) = &png {
        let data = NSData::with_bytes(png);
        let _ = pb.setData_forType(Some(&data), png_ty);
    }
    if let Some(tiff) = &tiff {
        let data = NSData::with_bytes(tiff);
        let _ = pb.setData_forType(Some(&data), tiff_ty);
    }
    Ok(())
}

fn file_url_objects(
    paths: &[PathBuf],
) -> Result<Vec<objc2::rc::Retained<ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>>>> {
    let mut urls = Vec::with_capacity(paths.len());
    for path in paths {
        let text = path
            .to_str()
            .ok_or_else(|| ClipboardError::Message("clipboard path is not utf-8".into()))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        urls.push(ProtocolObject::from_retained(url));
    }
    Ok(urls)
}

fn write_image_bytes_only(pb: &NSPasteboard, bytes: &[u8]) -> Result<()> {
    pb.clearContents();
    let png = transcode_png(bytes).unwrap_or_else(|_| bytes.to_vec());
    let png_ty = unsafe { NSPasteboardTypePNG };
    let data = NSData::with_bytes(&png);
    if !pb.setData_forType(Some(&data), png_ty) {
        return Err(ClipboardError::Message("failed to set PNG".into()));
    }
    if let Ok(tiff) = png_or_raw_to_tiff(&png) {
        let tiff_ty = unsafe { NSPasteboardTypeTIFF };
        let tiff_data = NSData::with_bytes(&tiff);
        let _ = pb.setData_forType(Some(&tiff_data), tiff_ty);
    }
    Ok(())
}

pub(crate) fn transcode_png(bytes: &[u8]) -> Result<Vec<u8>> {
    if ImageMime::detect(bytes) == Some(ImageMime::Png) {
        return Ok(bytes.to_vec());
    }
    let img = crate::item::decode_image_limited(bytes)?;
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| ClipboardError::Message(e.to_string()))?;
    Ok(buf)
}

fn canonical_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if paths.is_empty() {
        return Err(ClipboardError::Message("no files to copy".into()));
    }
    let mut abs = Vec::with_capacity(paths.len());
    for path in paths {
        let canon = path.canonicalize().map_err(|e| {
            ClipboardError::Message(format!(
                "clipboard file {} is not readable: {e}",
                path.display()
            ))
        })?;
        if canon.to_str().is_none() {
            return Err(ClipboardError::Message(format!(
                "clipboard path is not utf-8: {}",
                canon.display()
            )));
        }
        abs.push(canon);
    }
    Ok(abs)
}

pub(crate) fn png_or_raw_to_tiff(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = crate::item::decode_image_limited(bytes)?;
    let mut buf = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut buf),
        image::ImageFormat::Tiff,
    )
    .map_err(|e| ClipboardError::Message(e.to_string()))?;
    Ok(buf)
}

#[async_trait]
impl ClipboardBackend for MacClipboard {
    fn name(&self) -> &'static str {
        "macos-nspasteboard"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }

    async fn read(&self) -> Result<Option<ClipboardItem>> {
        tokio::task::spawn_blocking(|| MacClipboard.read_blocking())
            .await
            .map_err(|e| ClipboardError::Message(e.to_string()))?
    }

    async fn write(&self, item: &ClipboardItem) -> Result<()> {
        let item = item.clone();
        tokio::task::spawn_blocking(move || MacClipboard.write_blocking(&item))
            .await
            .map_err(|e| ClipboardError::Message(e.to_string()))?
    }

    async fn signature(&self) -> Result<String> {
        let pb = NSPasteboard::generalPasteboard();
        Ok(pb.changeCount().to_string())
    }
}

#[allow(dead_code)]
fn _hash(item: &ClipboardItem) -> String {
    format!("{:x}", hash_item(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clipsync-pb-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_temp(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn desktop_paste_lists_every_absolute_path() {
        let root = scratch_dir("files-home");
        let dir = root.join("incoming");
        std::fs::create_dir_all(&dir).unwrap();
        let first = write_temp(&dir, "사진.txt", "one".as_bytes());
        let second = write_temp(&dir, "notes.txt", "two".as_bytes());
        let pb = NSPasteboard::pasteboardWithUniqueName();
        crate::stage::with_stage_root(&root, || {
            publish_files(&pb, &[first.clone(), second.clone()], None).unwrap();
            assert!(
                first.exists() && second.exists(),
                "files stay where they are until paste"
            );

            let expected = vec![
                first.canonicalize().unwrap(),
                second.canonicalize().unwrap(),
            ];
            let listed: Vec<_> = file_paths(&pb)
                .unwrap()
                .into_iter()
                .map(|path| path.canonicalize().expect("filename path opens"))
                .collect();
            assert_eq!(listed, expected);

            let item = read_pasteboard(&pb).unwrap();
            match item {
                Some(ClipboardItem::Files { files }) => {
                    assert_eq!(files.len(), 2);
                    assert!(files
                        .iter()
                        .any(|f| f.bytes == b"one" && f.name.ends_with(".txt")));
                    assert!(files
                        .iter()
                        .any(|f| f.name == "notes.txt" && f.bytes == b"two"));
                }
                other => panic!("expected files, got {other:?}"),
            }

            // A Finder preview bitmap must not hide a real file copy.
            let png = crate::item::encode_png_rgba(1, 1, &[1, 2, 3, 255]).unwrap();
            let png_ty = unsafe { NSPasteboardTypePNG };
            let _ = unsafe { pb.addTypes_owner(&NSArray::from_slice(&[png_ty]), None) };
            assert!(pb.setData_forType(Some(&NSData::with_bytes(&png)), png_ty));
            match read_pasteboard(&pb).unwrap() {
                Some(ClipboardItem::Files { files }) => assert_eq!(files.len(), 2),
                other => panic!("file copy became {other:?}"),
            }
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn desktop_image_paste_is_a_file_and_reads_back_as_the_same_image() {
        let root = scratch_dir("image-home");
        let png = crate::item::encode_png_rgba(1, 1, &[9, 8, 7, 255]).unwrap();
        let pb = NSPasteboard::pasteboardWithUniqueName();
        crate::stage::with_stage_root(&root, || {
            write_image(&pb, ImageMime::Png, &png).unwrap();
            let names = paths_from_filenames(&pb);
            assert_eq!(names.len(), 1, "Finder Desktop paste needs a filename");
            assert_eq!(
                names[0].file_name().and_then(|s| s.to_str()),
                Some("clipboard.png")
            );
            assert_eq!(std::fs::read(&names[0]).unwrap(), png);
            let stage = root.join("data").join("pasteboard").canonicalize().unwrap();
            assert!(names[0].canonicalize().unwrap().starts_with(&stage));

            let png_ty = unsafe { NSPasteboardTypePNG };
            let stored = pb.dataForType(png_ty).expect("png bytes for other apps");
            assert_eq!(ImageMime::detect(&stored.to_vec()), Some(ImageMime::Png));

            match read_pasteboard(&pb).unwrap() {
                Some(ClipboardItem::Image { mime, bytes, .. }) => {
                    assert_eq!(mime, ImageMime::Png);
                    assert_eq!(bytes, png);
                }
                other => panic!("sidecar was not read as the original image: {other:?}"),
            }
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn desktop_jpeg_paste_keeps_original_bytes() {
        let root = scratch_dir("jpeg-home");
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert_eq!(ImageMime::detect(&jpeg), Some(ImageMime::Jpeg));
        let pb = NSPasteboard::pasteboardWithUniqueName();
        crate::stage::with_stage_root(&root, || {
            write_image(&pb, ImageMime::Jpeg, &jpeg).unwrap();
            let names = paths_from_filenames(&pb);
            assert_eq!(names[0].extension().and_then(|s| s.to_str()), Some("jpg"));
            assert_eq!(std::fs::read(&names[0]).unwrap(), jpeg);
            let png_ty = unsafe { NSPasteboardTypePNG };
            let stored = pb.dataForType(png_ty).expect("transcoded png");
            assert_eq!(ImageMime::detect(&stored.to_vec()), Some(ImageMime::Png));
            assert_ne!(stored.to_vec(), jpeg);
            match read_pasteboard(&pb).unwrap() {
                Some(ClipboardItem::Image { mime, bytes, .. }) => {
                    assert_eq!(mime, ImageMime::Jpeg);
                    assert_eq!(bytes, jpeg);
                }
                other => panic!("expected original jpeg, got {other:?}"),
            }
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}
