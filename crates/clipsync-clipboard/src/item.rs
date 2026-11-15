// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
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

#[cfg(not(target_os = "linux"))]
pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| ClipboardError::Message("invalid rgba buffer".into()))?;
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| ClipboardError::Message(e.to_string()))?;
    Ok(buf)
}

pub fn decode_image_limited(bytes: &[u8]) -> Result<image::DynamicImage> {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(32 * 1024 * 1024);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ClipboardError::Message(e.to_string()))?;
    reader.limits(limits);
    reader
        .decode()
        .map_err(|e| ClipboardError::Message(e.to_string()))
}

#[cfg(not(target_os = "linux"))]
pub fn decode_image_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = decode_image_limited(bytes)?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok((w, h, img.into_raw()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn tiff_to_png(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = decode_image_limited(bytes)?;
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| ClipboardError::Message(e.to_string()))?;
    Ok(buf)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod allocation_regression_tests {
    use super::*;

    // F34: removing reader.limits restores the old unbounded decode and makes
    // this valid, small-on-the-wire PNG succeed instead of respecting the budget.
    #[test]
    fn compressed_image_cannot_allocate_beyond_the_decode_budget() {
        let image = image::RgbaImage::from_pixel(3072, 3072, image::Rgba([7, 8, 9, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(
            png.len() < 1024 * 1024,
            "fixture must be compressed on the wire"
        );
        // A normal decoder proves this is valid image data, not a corrupt-input
        // rejection. The decoded pixel buffer is 36 MiB and is dropped before
        // exercising the bounded production path.
        let decoded = image::load_from_memory(&png).unwrap();
        assert_eq!(decoded.width(), 3072);
        assert_eq!(decoded.height(), 3072);
        drop(decoded);
        assert!(
            decode_image_limited(&png).is_err(),
            "a valid compressed image must not bypass the allocation limit"
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod behavior_tests {
    use super::*;

    fn file(name: &str, size: usize) -> FileRef {
        FileRef {
            name: name.into(),
            bytes: vec![0; size],
            mime: None,
            staged_path: None,
        }
    }

    #[test]
    fn clipboard_size_limits_accept_boundaries_and_reject_the_next_byte_or_file() {
        let mut text = ClipboardItem::Text {
            text: "x".repeat(TEXT_MAX_BYTES),
        };
        text.validate().unwrap();
        if let ClipboardItem::Text { text } = &mut text {
            text.push('x');
        }
        assert!(text
            .validate()
            .unwrap_err()
            .to_string()
            .contains("text exceeds"));
        drop(text);
        let mut image = ClipboardItem::Image {
            mime: ImageMime::Png,
            bytes: vec![0; IMAGE_MAX_BYTES],
            width: None,
            height: None,
        };
        image.validate().unwrap();
        if let ClipboardItem::Image { bytes, .. } = &mut image {
            bytes.push(0);
        }
        assert!(image
            .validate()
            .unwrap_err()
            .to_string()
            .contains("image exceeds"));
        drop(image);
        let mut files = (0..FILE_MAX_COUNT)
            .map(|i| file(&format!("{i}.txt"), 0))
            .collect::<Vec<_>>();
        ClipboardItem::Files {
            files: files.clone(),
        }
        .validate()
        .unwrap();
        files.push(file("extra.txt", 0));
        assert!(ClipboardItem::Files { files }
            .validate()
            .unwrap_err()
            .to_string()
            .contains("too many files"));
        let oversized = ClipboardItem::Files {
            files: vec![file("too-large", FILE_MAX_BYTES as usize + 1)],
        };
        assert!(oversized
            .validate()
            .unwrap_err()
            .to_string()
            .contains("file too-large exceeds"));
        drop(oversized);
        let mut files = vec![
            file("first", FILE_MAX_BYTES as usize),
            file("second", FILE_MAX_BYTES as usize),
        ];
        let exact = ClipboardItem::Files { files };
        exact.validate().unwrap();
        files = match exact {
            ClipboardItem::Files { files } => files,
            _ => unreachable!(),
        };
        files.push(file("one-more-byte", 1));
        assert!(ClipboardItem::Files { files }
            .validate()
            .unwrap_err()
            .to_string()
            .contains("transfer exceeds"));
    }

    #[test]
    fn item_metadata_distinguishes_types_and_reports_payload_sizes() {
        let text = ClipboardItem::Text {
            text: "안녕".into(),
        };
        assert_eq!(text.kind(), ItemKind::Text);
        assert_eq!(text.content_bytes(), "안녕".as_bytes());
        assert!(text.summary().contains("6 bytes"));
        for (mime, name) in [
            (ImageMime::Png, "image/png"),
            (ImageMime::Jpeg, "image/jpeg"),
            (ImageMime::Webp, "image/webp"),
        ] {
            assert_eq!(mime.as_str(), name);
            assert_eq!(ImageMime::from_mime(name), Some(mime));
            let image = ClipboardItem::Image {
                mime,
                bytes: vec![1, 2, 3],
                width: Some(1),
                height: Some(1),
            };
            assert_eq!(image.kind(), ItemKind::Image);
            assert_eq!(image.content_bytes(), [1, 2, 3]);
            assert!(image.summary().contains(name));
            assert!(image.summary().contains("3 bytes"));
        }
        assert_eq!(ImageMime::from_mime("image/jpg"), Some(ImageMime::Jpeg));
        assert_eq!(ImageMime::from_mime("text/plain"), None);
        let files = ClipboardItem::Files {
            files: vec![
                FileRef {
                    name: "a".into(),
                    bytes: b"12".to_vec(),
                    mime: None,
                    staged_path: None,
                },
                FileRef {
                    name: "b".into(),
                    bytes: b"345".to_vec(),
                    mime: None,
                    staged_path: None,
                },
            ],
        };
        assert_eq!(files.kind(), ItemKind::Files);
        assert_eq!(files.content_bytes(), b"a12b345");
        assert_eq!(files.summary(), "2 files 5 bytes");
    }

    #[test]
    fn image_codec_preserves_rgba_pixels_and_reports_invalid_inputs() {
        let pixels = [4, 5, 6, 255, 7, 8, 9, 255];
        #[cfg(not(target_os = "linux"))]
        {
            let png = encode_png_rgba(2, 1, &pixels).unwrap();
            let (width, height, decoded) = decode_image_rgba(&png).unwrap();
            assert_eq!((width, height), (2, 1));
            assert_eq!(decoded, pixels);
            assert!(encode_png_rgba(2, 1, &[1, 2, 3]).is_err());
            assert!(decode_image_rgba(b"not an image").is_err());
        }
        let mut tiff = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(2, 1, pixels.to_vec()).unwrap())
            .write_to(
                &mut std::io::Cursor::new(&mut tiff),
                image::ImageFormat::Tiff,
            )
            .unwrap();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let png = tiff_to_png(&tiff).unwrap();
            assert_eq!(ImageMime::detect(&png), Some(ImageMime::Png));
            assert_eq!(
                decode_image_limited(&png).unwrap().to_rgba8().as_raw(),
                &pixels
            );
            assert!(tiff_to_png(b"corrupt tiff").is_err());
        }
        assert!(decode_image_limited(b"unknown format").is_err());
    }
}
