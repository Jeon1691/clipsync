#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::{Command, Stdio};

use async_trait::async_trait;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSString};

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
        if let Some(files) = read_files_osascript()? {
            return Ok(Some(ClipboardItem::Files { files }));
        }
        let pb = NSPasteboard::generalPasteboard();
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
                let ty = unsafe { NSPasteboardTypePNG };
                let data = NSData::with_bytes(bytes);
                if !pb.setData_forType(Some(&data), &ty) {
                    return Err(ClipboardError::Message("failed to set image".into()));
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

fn read_files_osascript() -> Result<Option<Vec<FileRef>>> {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"try
set theFiles to {}
set pb to (the clipboard as «class furl»)
POSIX path of pb
end try"#,
        ])
        .stdin(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let path = PathBuf::from(text.trim());
    if path.as_os_str().is_empty() || !path.exists() {
        return Ok(None);
    }
    let meta = std::fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() || meta.is_dir() {
        return Err(ClipboardError::Message(
            "directory and symlink clipboard items are rejected".into(),
        ));
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| ClipboardError::Message("invalid file name".into()))?
        .to_string();
    let bytes = std::fs::read(&path)?;
    Ok(Some(vec![FileRef {
        mime: infer::get(&bytes).map(|k| k.mime_type().to_string()),
        name,
        bytes,
        staged_path: Some(path),
    }]))
}

pub fn write_file_urls(paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let posix = paths
        .iter()
        .map(|p| format!("POSIX file \"{}\"", p.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!("set the clipboard to {{{posix}}}");
    let status = Command::new("osascript")
        .args(["-e", &script])
        .status()
        .map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
    if !status.success() {
        return Err(ClipboardError::Message(
            "osascript file url write failed".into(),
        ));
    }
    Ok(())
}

#[allow(dead_code)]
fn _hash(item: &ClipboardItem) -> String {
    format!("{:x}", hash_item(item))
}
