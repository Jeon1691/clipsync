#![cfg(target_os = "macos")]

use std::path::PathBuf;

use async_trait::async_trait;
use objc2::runtime::ProtocolObject;
use objc2::ClassType;
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
        let pb = NSPasteboard::generalPasteboard();
        if let Some(files) = read_file_urls(&pb)? {
            return Ok(Some(ClipboardItem::Files { files }));
        }
        let png_ty = unsafe { NSPasteboardTypePNG };
        if let Some(data) = pb.dataForType(&png_ty) {
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
        if let Some(data) = pb.dataForType(&tiff_ty) {
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
        let ty = unsafe { NSPasteboardTypeString };
        if let Some(s) = pb.stringForType(&ty) {
            let text = s.to_string();
            if !text.is_empty() {
                return Ok(Some(ClipboardItem::Text { text }));
            }
        }
        Ok(None)
    }

    fn write_blocking(&self, item: &ClipboardItem) -> Result<()> {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        match item {
            ClipboardItem::Text { text } => {
                let ty = unsafe { NSPasteboardTypeString };
                if !pb.setString_forType(&NSString::from_str(text), &ty) {
                    return Err(ClipboardError::Message("failed to set text".into()));
                }
            }
            ClipboardItem::Image { bytes, .. } => {
                let png_ty = unsafe { NSPasteboardTypePNG };
                let data = NSData::with_bytes(bytes);
                if !pb.setData_forType(Some(&data), &png_ty) {
                    return Err(ClipboardError::Message("failed to set PNG".into()));
                }
                if let Ok(tiff) = png_or_raw_to_tiff(bytes) {
                    let tiff_ty = unsafe { NSPasteboardTypeTIFF };
                    let tiff_data = NSData::with_bytes(&tiff);
                    let _ = pb.setData_forType(Some(&tiff_data), &tiff_ty);
                }
            }
            ClipboardItem::Files { files } => {
                let paths: Vec<PathBuf> =
                    files.iter().filter_map(|f| f.staged_path.clone()).collect();
                if paths.len() != files.len() {
                    return Err(ClipboardError::Message(
                        "file items need staged inbox paths to land on the pasteboard".into(),
                    ));
                }
                write_file_urls(&paths)?;
            }
        }
        Ok(())
    }
}

fn read_file_urls(pb: &NSPasteboard) -> Result<Option<Vec<FileRef>>> {
    let class_array = NSArray::from_slice(&[NSURL::class()]);
    let options = NSDictionary::from_slices(
        &[unsafe { NSPasteboardURLReadingFileURLsOnlyKey }],
        &[NSNumber::new_bool(true).as_ref()],
    );
    let objects = unsafe { pb.readObjectsForClasses_options(&class_array, Some(options.as_ref())) };
    let Some(array) = objects else {
        return Ok(None);
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
    load_file_refs(&paths)
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

pub fn write_file_urls(paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let urls: Vec<_> = paths
        .iter()
        .filter_map(|p| {
            let abs = p.canonicalize().ok()?;
            let s = abs.to_str()?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(s));
            Some(ProtocolObject::from_retained(url))
        })
        .collect();
    if urls.is_empty() {
        return Err(ClipboardError::Message("no file URLs to write".into()));
    }
    let pb = NSPasteboard::generalPasteboard();
    let objects = NSArray::from_retained_slice(&urls);
    if !pb.writeObjects(&objects) {
        return Err(ClipboardError::Message(
            "NSPasteboard writeObjects failed for files".into(),
        ));
    }
    Ok(())
}

fn png_or_raw_to_tiff(bytes: &[u8]) -> Result<Vec<u8>> {
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
