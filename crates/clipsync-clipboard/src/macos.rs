#![cfg(target_os = "macos")]
// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT

use std::path::PathBuf;

use async_trait::async_trait;
use objc2::runtime::ProtocolObject;
use objc2::ClassType;
#[expect(
    deprecated,
    reason = "Read the legacy filename pasteboard flavor alongside modern file URLs for compatibility."
)]
use objc2_app_kit::NSFilenamesPboardType;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    NSPasteboardURLReadingFileURLsOnlyKey,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber, NSString, NSURL};

use crate::detect::Capabilities;
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
        write_pasteboard(&NSPasteboard::generalPasteboard(), item)
    }
}

fn write_pasteboard(pb: &NSPasteboard, item: &ClipboardItem) -> Result<()> {
    match item {
        ClipboardItem::Text { text } => {
            pb.clearContents();
            let ty = unsafe { NSPasteboardTypeString };
            if !pb.setString_forType(&NSString::from_str(text), ty) {
                return Err(ClipboardError::Message("failed to set text".into()));
            }
        }
        ClipboardItem::Image { bytes, mime, .. } => write_image(pb, *mime, bytes)?,
        ClipboardItem::Files { files } => {
            let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.staged_path.clone()).collect();
            if paths.len() != files.len() {
                return Err(ClipboardError::Message(
                    "file items need staged inbox paths to land on the pasteboard".into(),
                ));
            }
            publish_files(pb, &paths, None)?;
        }
    }
    Ok(())
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

#[expect(
    deprecated,
    reason = "Read the legacy filename pasteboard flavor alongside modern file URLs for compatibility."
)]
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
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

    // Characterize actual TIFF interoperability on normal and fallback writes.
    // AppKit can synthesize TIFF from PNG, so this is not by itself evidence
    // that the historical explicit-TIFF-publication defect has a reproducer.
    #[test]
    fn png_clipboard_writes_publish_decodable_tiff_for_other_applications() {
        let root = scratch_dir("tiff-representation");
        let pixels = [12, 34, 56, 255, 78, 90, 123, 255];
        let png = crate::item::encode_png_rgba(2, 1, &pixels).unwrap();
        crate::stage::with_stage_root(&root, || {
            for fallback in [false, true] {
                let pb = NSPasteboard::pasteboardWithUniqueName();
                if fallback {
                    write_image_bytes_only(&pb, &png).unwrap();
                } else {
                    write_image(&pb, ImageMime::Png, &png).unwrap();
                }
                let tiff_type = unsafe { NSPasteboardTypeTIFF };
                assert!(
                    pb.types()
                        .unwrap()
                        .iter()
                        .any(|kind| kind.to_string() == tiff_type.to_string()),
                    "native applications must discover TIFF before choosing a representation"
                );
                let data = pb
                    .dataForType(tiff_type)
                    .expect("TIFF representation for native apps");
                let bytes = data.to_vec();
                assert_eq!(
                    image::guess_format(&bytes).unwrap(),
                    image::ImageFormat::Tiff
                );
                let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
                assert_eq!(decoded.dimensions(), (2, 1));
                assert_eq!(decoded.as_raw(), &pixels);
            }
        });
        let _ = std::fs::remove_dir_all(root);
    }

    // A real legacy filename property list must preserve all copied contents.
    // AppKit can synthesize NSURL objects from it, so this compatibility case
    // alone does not prove which native conversion/fallback path was used.
    #[test]
    #[expect(
        deprecated,
        reason = "Regression fixture intentionally publishes the legacy filename-only flavor."
    )]
    fn legacy_filename_clipboard_preserves_files_even_with_preview() {
        let root = scratch_dir("filename-only");
        let first = write_temp(&root, "legacy first.txt", b"legacy first");
        let second = write_temp(&root, "second.txt", b"legacy second");
        let names = NSArray::from_retained_slice(&[
            NSString::from_str(first.to_str().unwrap()),
            NSString::from_str(second.to_str().unwrap()),
        ]);
        let pb = NSPasteboard::pasteboardWithUniqueName();
        pb.clearContents();
        let filename_type = unsafe { NSFilenamesPboardType };
        // NSArray<NSString> is the documented filename pasteboard property list.
        assert!(unsafe { pb.setPropertyList_forType(&names, filename_type) });
        assert!(pb
            .types()
            .unwrap()
            .iter()
            .any(|kind| kind.to_string() == filename_type.to_string()));
        let png = crate::item::encode_png_rgba(1, 1, &[1, 2, 3, 255]).unwrap();
        let png_type = unsafe { NSPasteboardTypePNG };
        let _ = unsafe { pb.addTypes_owner(&NSArray::from_slice(&[png_type]), None) };
        assert!(pb.setData_forType(Some(&NSData::with_bytes(&png)), png_type));
        let Some(ClipboardItem::Files { files }) = read_pasteboard(&pb).unwrap() else {
            panic!("legacy file copy was lost or replaced by its preview bitmap");
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "legacy first.txt");
        assert_eq!(files[0].bytes, b"legacy first");
        assert_eq!(files[1].name, "second.txt");
        assert_eq!(files[1].bytes, b"legacy second");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod behavior_tests {
    use super::*;
    use crate::test_support::Scratch;

    fn image() -> Vec<u8> {
        crate::item::encode_png_rgba(2, 1, &[5, 6, 7, 255, 8, 9, 10, 255]).unwrap()
    }
    fn assert_pixels(pb: &NSPasteboard) {
        let data = pb.dataForType(unsafe { NSPasteboardTypePNG }).unwrap();
        assert_eq!(
            image::load_from_memory(&data.to_vec())
                .unwrap()
                .to_rgba8()
                .as_raw(),
            &[5, 6, 7, 255, 8, 9, 10, 255]
        );
    }
    #[test]
    fn private_pasteboard_child() {
        let Ok(case) = std::env::var("CLIPSYNC_PRIVATE_BOARD_CASE") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("CLIPSYNC_NATIVE_FIXTURE").unwrap());
        let pb = NSPasteboard::pasteboardWithUniqueName();
        match case.as_str() {
            "text" => {
                pb.clearContents();
                assert!(read_pasteboard(&pb).unwrap().is_none());
                write_pasteboard(
                    &pb,
                    &ClipboardItem::Text {
                        text: "native text payload".into(),
                    },
                )
                .unwrap();
                crate::test_support::assert_text(
                    read_pasteboard(&pb).unwrap(),
                    "native text payload",
                );
                write_pasteboard(
                    &pb,
                    &ClipboardItem::Text {
                        text: String::new(),
                    },
                )
                .unwrap();
                assert!(read_pasteboard(&pb).unwrap().is_none());
                pb.setData_forType(Some(&NSData::with_bytes(b"invalid PNG bytes")), unsafe {
                    NSPasteboardTypePNG
                });
                pb.setData_forType(Some(&NSData::with_bytes(b"invalid TIFF bytes")), unsafe {
                    NSPasteboardTypeTIFF
                });
                pb.setString_forType(
                    &NSString::from_str("text survives invalid image flavors"),
                    unsafe { NSPasteboardTypeString },
                );
                crate::test_support::assert_text(
                    read_pasteboard(&pb).unwrap(),
                    "text survives invalid image flavors",
                );
            }
            "files" => {
                let path = root.join("regular.txt");
                std::fs::write(&path, b"native file data").unwrap();
                let files = vec![FileRef {
                    name: "regular.txt".into(),
                    bytes: b"native file data".to_vec(),
                    mime: None,
                    staged_path: Some(path.clone()),
                }];
                write_pasteboard(
                    &pb,
                    &ClipboardItem::Files {
                        files: files.clone(),
                    },
                )
                .unwrap();
                let Some(ClipboardItem::Files { files: read }) = read_pasteboard(&pb).unwrap()
                else {
                    panic!("native file write was lost");
                };
                assert_eq!(read[0].bytes, b"native file data");
                assert_eq!(
                    read[0]
                        .staged_path
                        .as_ref()
                        .unwrap()
                        .canonicalize()
                        .unwrap(),
                    path.canonicalize().unwrap()
                );
                for bad in [
                    vec![],
                    vec![FileRef {
                        staged_path: None,
                        ..files[0].clone()
                    }],
                    vec![FileRef {
                        staged_path: Some(root.join("missing")),
                        ..files[0].clone()
                    }],
                ] {
                    assert!(write_pasteboard(&pb, &ClipboardItem::Files { files: bad }).is_err());
                    assert!(
                        matches!(
                            read_pasteboard(&pb).unwrap(),
                            Some(ClipboardItem::Files { .. })
                        ),
                        "rejected writes must preserve existing clipboard"
                    );
                }
                let link = root.join("symlink");
                std::os::unix::fs::symlink(&path, &link).unwrap();
                assert!(load_file_refs(&[root.join("missing"), root.clone(), link])
                    .unwrap()
                    .is_none());
                assert!(canonical_paths(&[]).is_err());
                assert!(canonical_paths(&[root.join("missing")]).is_err());
                use std::os::unix::ffi::OsStringExt;
                let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
                assert!(file_url_objects(&[invalid]).is_err());
            }
            "image" => {
                let bytes = image();
                write_pasteboard(
                    &pb,
                    &ClipboardItem::Image {
                        mime: ImageMime::Png,
                        bytes: bytes.clone(),
                        width: Some(2),
                        height: Some(1),
                    },
                )
                .unwrap();
                assert_pixels(&pb);
                let Some(ClipboardItem::Image { bytes: read, .. }) = read_pasteboard(&pb).unwrap()
                else {
                    panic!("image sidecar misclassified");
                };
                assert_eq!(read, bytes);
                assert!(image_from_sidecar(&[]).unwrap().is_none());
                assert!(image_from_sidecar(&[root.clone(), root.clone()])
                    .unwrap()
                    .is_none());
                assert!(image_from_sidecar(&[root.join("missing")])
                    .unwrap()
                    .is_none());
                let text = root.join("not-image");
                std::fs::write(&text, b"ordinary file").unwrap();
                assert!(image_from_sidecar(&[text]).unwrap().is_none());
            }
            "fallback" => {
                let dir = crate::stage::pasteboard_dir();
                std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
                std::fs::write(&dir, b"staging is deliberately unavailable").unwrap();
                write_pasteboard(
                    &pb,
                    &ClipboardItem::Image {
                        mime: ImageMime::Png,
                        bytes: image(),
                        width: None,
                        height: None,
                    },
                )
                .unwrap();
                assert_pixels(&pb);
                assert!(
                    file_paths(&pb).unwrap().is_empty(),
                    "fallback must not advertise nonexistent staged files"
                );
                let error = transcode_png(b"invalid image").unwrap_err();
                assert!(!error.to_string().is_empty());
                assert!(png_or_raw_to_tiff(b"invalid image").is_err());
            }
            _ => panic!("unknown private pasteboard case"),
        }
    }
    #[test]
    fn native_writes_and_failures_use_only_isolated_named_pasteboards() {
        for case in ["text", "files", "image", "fallback"] {
            let root = Scratch::new();
            let out = root
                .child("macos::behavior_tests::private_pasteboard_child")
                .env("CLIPSYNC_PRIVATE_BOARD_CASE", case)
                .env("CLIPSYNC_NATIVE_FIXTURE", &root.0)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "private pasteboard {case} failed: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
