use clipsync_protocol::{
    ItemKind, FILE_MAX_BYTES, FILE_MAX_COUNT, IMAGE_MAX_BYTES, TEXT_MAX_BYTES, TRANSFER_MAX_BYTES,
};

use crate::{ClipboardError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardItem {
    Text {
        text: String,
    },
    Image {
        mime: ImageMime,
        bytes: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
    },
    Files {
        files: Vec<FileRef>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRef {
    pub name: String,
    pub bytes: Vec<u8>,
    pub mime: Option<String>,
    pub staged_path: Option<std::path::PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageMime {
    Png,
    Jpeg,
    Webp,
}

impl ImageMime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    pub fn from_mime(s: &str) -> Option<Self> {
        match s {
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }

    pub fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
        {
            return Some(Self::Png);
        }
        if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
            return Some(Self::Jpeg);
        }
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        None
    }
}

impl ClipboardItem {
    pub fn kind(&self) -> ItemKind {
        match self {
            Self::Text { .. } => ItemKind::Text,
            Self::Image { .. } => ItemKind::Image,
            Self::Files { .. } => ItemKind::Files,
        }
    }

    pub fn content_bytes(&self) -> Vec<u8> {
        match self {
            Self::Text { text } => text.as_bytes().to_vec(),
            Self::Image { bytes, .. } => bytes.clone(),
            Self::Files { files } => {
                let mut out = Vec::new();
                for f in files {
                    out.extend_from_slice(f.name.as_bytes());
                    out.extend_from_slice(&f.bytes);
                }
                out
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Text { text } => {
                if text.len() > TEXT_MAX_BYTES {
                    return Err(ClipboardError::Message(format!(
                        "text exceeds {TEXT_MAX_BYTES} bytes"
                    )));
                }
            }
            Self::Image { bytes, .. } => {
                if bytes.len() > IMAGE_MAX_BYTES {
                    return Err(ClipboardError::Message(format!(
                        "image exceeds {IMAGE_MAX_BYTES} bytes"
                    )));
                }
            }
            Self::Files { files } => {
                if files.len() > FILE_MAX_COUNT {
                    return Err(ClipboardError::Message(format!(
                        "too many files (max {FILE_MAX_COUNT})"
                    )));
                }
                let mut total = 0u64;
                for f in files {
                    if f.bytes.len() as u64 > FILE_MAX_BYTES {
                        return Err(ClipboardError::Message(format!(
                            "file {} exceeds {FILE_MAX_BYTES} bytes",
                            f.name
                        )));
                    }
                    total += f.bytes.len() as u64;
                }
                if total > TRANSFER_MAX_BYTES {
                    return Err(ClipboardError::Message(format!(
                        "transfer exceeds {TRANSFER_MAX_BYTES} bytes"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Text { text } => format!("text {} bytes", text.len()),
            Self::Image { mime, bytes, .. } => {
                format!("image {} {} bytes", mime.as_str(), bytes.len())
            }
            Self::Files { files } => {
                let n = files.len();
                let bytes: usize = files.iter().map(|f| f.bytes.len()).sum();
                format!("{n} files {bytes} bytes")
            }
        }
    }
}

pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| ClipboardError::Message("invalid rgba buffer".into()))?;
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| ClipboardError::Message(e.to_string()))?;
    Ok(buf)
}

pub fn decode_image_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| ClipboardError::Message(e.to_string()))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok((w, h, img.into_raw()))
}

pub fn tiff_to_png(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).map_err(|e| ClipboardError::Message(e.to_string()))?;
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| ClipboardError::Message(e.to_string()))?;
    Ok(buf)
}

#[cfg(test)]
mod format_tests {
    use super::ImageMime;

    #[test]
    fn detects_supported_image_formats() {
        assert_eq!(
            ImageMime::detect(b"\x89PNG\r\n\x1a\nrest"),
            Some(ImageMime::Png)
        );
        assert_eq!(
            ImageMime::detect(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageMime::Jpeg)
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&12u32.to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        assert_eq!(ImageMime::detect(&webp), Some(ImageMime::Webp));
        assert_eq!(ImageMime::from_mime("image/jpg"), Some(ImageMime::Jpeg));
    }

    #[test]
    fn rejects_non_image_payloads() {
        for bytes in [
            b"GIF89a".as_slice(),
            b"BM",
            b"II*\x00",
            b"%PDF",
            b"PK\x03\x04",
            b"<svg ",
        ] {
            assert_eq!(ImageMime::detect(bytes), None, "{bytes:?}");
        }
    }
}
