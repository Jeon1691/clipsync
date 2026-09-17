use crate::command::CommandClipboard;
use crate::{ClipboardBackend, ClipboardError, Result};

#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub text: bool,
    pub image: bool,
    pub files: bool,
    pub watch: bool,
}

impl Capabilities {
    pub fn all() -> Self {
        Self {
            text: true,
            image: true,
            files: true,
            watch: true,
        }
    }

    pub fn text_only() -> Self {
        Self {
            text: true,
            image: false,
            files: false,
            watch: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AdapterInfo {
    pub name: &'static str,
    pub source: &'static str,
    pub capabilities: Capabilities,
    pub fallback_reason: Option<String>,
}

pub fn detect_adapter() -> Result<(Box<dyn ClipboardBackend>, AdapterInfo)> {
    #[cfg(target_os = "macos")]
    {
        match crate::macos::MacClipboard::new() {
            Ok(native) => {
                let info = AdapterInfo {
                    name: "macos-nspasteboard",
                    source: "native",
                    capabilities: Capabilities::all(),
                    fallback_reason: None,
                };
                return Ok((Box::new(native), info));
            }
            Err(e) => {
                if let Some(pb) = CommandClipboard::macos_tools() {
                    let info = AdapterInfo {
                        name: "macos-pbpaste",
                        source: "command",
                        capabilities: pb.capabilities(),
                        fallback_reason: Some(e.to_string()),
                    };
                    return Ok((Box::new(pb), info));
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        match ArboardClipboard::new() {
            Ok(ar) => {
                let info = AdapterInfo {
                    name: "windows-clipboard",
                    source: "native",
                    capabilities: Capabilities {
                        text: true,
                        image: true,
                        files: false,
                        watch: true,
                    },
                    fallback_reason: None,
                };
                return Ok((Box::new(ar), info));
            }
            Err(e) => {
                return Err(ClipboardError::Unavailable(format!(
                    "windows clipboard: {e}"
                )));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Some(wl) = CommandClipboard::wayland_tools() {
                let info = AdapterInfo {
                    name: "wayland-wl-clipboard",
                    source: "command",
                    capabilities: wl.capabilities(),
                    fallback_reason: None,
                };
                return Ok((Box::new(wl), info));
            }
        }
        if std::env::var_os("DISPLAY").is_some() {
            if let Some(x11) = CommandClipboard::x11_tools() {
                let info = AdapterInfo {
                    name: "x11-xclip",
                    source: "command",
                    capabilities: x11.capabilities(),
                    fallback_reason: None,
                };
                return Ok((Box::new(x11), info));
            }
        }
        let info = AdapterInfo {
            name: "headless",
            source: "stdin-stdout",
            capabilities: Capabilities::text_only(),
            fallback_reason: Some("no graphical clipboard adapter".into()),
        };
        return Ok((Box::new(CommandClipboard::headless()), info));
    }

    if let Ok(ar) = ArboardClipboard::new() {
        let info = AdapterInfo {
            name: "arboard",
            source: "library",
            capabilities: Capabilities {
                text: true,
                image: true,
                files: false,
                watch: true,
            },
            fallback_reason: Some("generic fallback".into()),
        };
        return Ok((Box::new(ar), info));
    }

    Err(ClipboardError::Unavailable(
        "no clipboard adapter found".into(),
    ))
}

pub struct ArboardClipboard {
    // arboard Clipboard is not Sync on all platforms; wrap per-call.
}

impl ArboardClipboard {
    pub fn new() -> Result<Self> {
        let _ =
            arboard::Clipboard::new().map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
        Ok(Self {})
    }
}

use crate::item::{decode_image_rgba, encode_png_rgba, ClipboardItem, ImageMime};
use async_trait::async_trait;

#[async_trait]
impl ClipboardBackend for ArboardClipboard {
    fn name(&self) -> &'static str {
        "arboard"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            text: true,
            image: true,
            files: false,
            watch: true,
        }
    }

    async fn read(&self) -> Result<Option<ClipboardItem>> {
        tokio::task::spawn_blocking(|| {
            let mut c = arboard::Clipboard::new()
                .map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
            if let Ok(img) = c.get_image() {
                let png = encode_png_rgba(img.width as u32, img.height as u32, &img.bytes)?;
                return Ok(Some(ClipboardItem::Image {
                    mime: ImageMime::Png,
                    bytes: png,
                    width: Some(img.width as u32),
                    height: Some(img.height as u32),
                }));
            }
            match c.get_text() {
                Ok(text) if !text.is_empty() => Ok(Some(ClipboardItem::Text { text })),
                Ok(_) => Ok(None),
                Err(_) => Ok(None),
            }
        })
        .await
        .map_err(|e| ClipboardError::Message(e.to_string()))?
    }

    async fn write(&self, item: &ClipboardItem) -> Result<()> {
        let item = item.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = arboard::Clipboard::new()
                .map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
            match item {
                ClipboardItem::Text { text } => c
                    .set_text(text)
                    .map_err(|e| ClipboardError::Message(e.to_string()))?,
                ClipboardItem::Image { bytes, .. } => {
                    let (w, h, rgba) = decode_image_rgba(&bytes)?;
                    let img = arboard::ImageData {
                        width: w as usize,
                        height: h as usize,
                        bytes: rgba.into(),
                    };
                    c.set_image(img)
                        .map_err(|e| ClipboardError::Message(e.to_string()))?;
                }
                ClipboardItem::Files { .. } => {
                    return Err(ClipboardError::Unsupported("files"));
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| ClipboardError::Message(e.to_string()))?
    }

    async fn signature(&self) -> Result<String> {
        match self.read().await? {
            Some(item) => Ok(format!("{:x}", hash_item(&item))),
            None => Ok("empty".into()),
        }
    }
}

pub fn hash_item(item: &ClipboardItem) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match item {
        ClipboardItem::Text { text } => {
            1u8.hash(&mut h);
            text.hash(&mut h);
        }
        ClipboardItem::Image { bytes, .. } => {
            2u8.hash(&mut h);
            bytes.hash(&mut h);
        }
        ClipboardItem::Files { files } => {
            3u8.hash(&mut h);
            for f in files {
                f.name.hash(&mut h);
                f.bytes.hash(&mut h);
            }
        }
    }
    h.finish()
}

pub fn which_bin(name: &str) -> Option<std::path::PathBuf> {
    which::which(name).ok()
}

pub fn tool_exists(name: &str) -> bool {
    which_bin(name).is_some()
}

pub fn collect_doctor_tools() -> Vec<(String, bool)> {
    let names = [
        "pbcopy",
        "pbpaste",
        "osascript",
        "xclip",
        "xsel",
        "wl-copy",
        "wl-paste",
        "notify-send",
        "clip",
        "powershell",
    ];
    names
        .into_iter()
        .map(|n| (n.to_string(), tool_exists(n)))
        .collect()
}
