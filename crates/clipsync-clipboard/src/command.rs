// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::path::PathBuf;
use std::process::{Command, Stdio};

use async_trait::async_trait;

use crate::detect::{hash_item, which_bin, Capabilities};
use crate::item::{tiff_to_png, ClipboardItem, FileRef, ImageMime};
use crate::{ClipboardBackend, ClipboardError, Result};

pub struct CommandClipboard {
    name: &'static str,
    read_text: Option<Tool>,
    write_text: Option<Tool>,
    read_image: Option<Tool>,
    write_image: Option<Tool>,
    read_files: Option<Tool>,
    write_files: Option<Tool>,
}

#[derive(Clone)]
pub struct Tool {
    pub bin: PathBuf,
    pub args: Vec<String>,
}

impl Tool {
    fn new(bin: PathBuf, args: &[&str]) -> Self {
        Self {
            bin,
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl CommandClipboard {
    #[cfg(target_os = "macos")]
    pub fn macos_tools() -> Option<Self> {
        let pbcopy = which_bin("pbcopy")?;
        let pbpaste = which_bin("pbpaste")?;
        Some(Self {
            name: "macos-pbpaste",
            read_text: Some(Tool::new(pbpaste, &[])),
            write_text: Some(Tool::new(pbcopy, &[])),
            read_image: None,
            write_image: None,
            read_files: None,
            write_files: None,
        })
    }

    #[cfg(target_os = "linux")]
    pub fn x11_tools() -> Option<Self> {
        if let Some(xclip) = which_bin("xclip") {
            return Some(Self {
                name: "x11-xclip",
                read_text: Some(Tool::new(xclip.clone(), &["-selection", "clipboard", "-o"])),
                write_text: Some(Tool::new(xclip.clone(), &["-selection", "clipboard", "-i"])),
                read_image: Some(Tool::new(
                    xclip.clone(),
                    &["-selection", "clipboard", "-t", "image/png", "-o"],
                )),
                write_image: Some(Tool::new(
                    xclip.clone(),
                    &["-selection", "clipboard", "-t", "image/png", "-i"],
                )),
                read_files: Some(Tool::new(
                    xclip.clone(),
                    &["-selection", "clipboard", "-t", "text/uri-list", "-o"],
                )),
                write_files: Some(Tool::new(
                    xclip,
                    &["-selection", "clipboard", "-t", "text/uri-list", "-i"],
                )),
            });
        }
        let xsel = which_bin("xsel")?;
        Some(Self {
            name: "x11-xsel",
            read_text: Some(Tool::new(xsel.clone(), &["--clipboard", "--output"])),
            write_text: Some(Tool::new(xsel, &["--clipboard", "--input"])),
            read_image: None,
            write_image: None,
            read_files: None,
            write_files: None,
        })
    }

    #[cfg(target_os = "linux")]
    pub fn wayland_tools() -> Option<Self> {
        let copy = which_bin("wl-copy")?;
        let paste = which_bin("wl-paste")?;
        Some(Self {
            name: "wayland-wl-clipboard",
            read_text: Some(Tool::new(paste.clone(), &["--no-newline"])),
            write_text: Some(Tool::new(copy.clone(), &["--type", "text/plain"])),
            read_image: Some(Tool::new(paste.clone(), &["--type", "image/png"])),
            write_image: Some(Tool::new(copy.clone(), &["--type", "image/png"])),
            read_files: Some(Tool::new(paste, &["--type", "text/uri-list"])),
            write_files: Some(Tool::new(copy, &["--type", "text/uri-list"])),
        })
    }

    #[cfg(target_os = "linux")]
    pub fn headless() -> Self {
        Self {
            name: "headless",
            read_text: None,
            write_text: None,
            read_image: None,
            write_image: None,
            read_files: None,
            write_files: None,
        }
    }

    fn run_out(tool: &Tool) -> Result<Vec<u8>> {
        let mut cmd = Command::new(&tool.bin);
        cmd.args(&tool.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let output = cmd
            .output()
            .map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
        if !output.status.success() {
            return Err(ClipboardError::Message(format!(
                "{} exited {}",
                tool.bin.display(),
                output.status
            )));
        }
        Ok(output.stdout)
    }

    fn run_in(tool: &Tool, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        let mut child = Command::new(&tool.bin)
            .args(&tool.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(bytes)?;
        }
        let status = child
            .wait()
            .map_err(|e| ClipboardError::Message(e.to_string()))?;
        if !status.success() {
            return Err(ClipboardError::Message(format!(
                "{} exited {status}",
                tool.bin.display()
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl ClipboardBackend for CommandClipboard {
    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            text: self.read_text.is_some() && self.write_text.is_some(),
            image: self.read_image.is_some() && self.write_image.is_some(),
            files: self.read_files.is_some() && self.write_files.is_some(),
            watch: self.read_text.is_some(),
        }
    }

    async fn read(&self) -> Result<Option<ClipboardItem>> {
        let this = self.clone_tools();
        tokio::task::spawn_blocking(move || this.read_blocking())
            .await
            .map_err(|e| ClipboardError::Message(e.to_string()))?
    }

    async fn write(&self, item: &ClipboardItem) -> Result<()> {
        let this = self.clone_tools();
        let item = item.clone();
        tokio::task::spawn_blocking(move || this.write_blocking(&item))
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

impl CommandClipboard {
    fn clone_tools(&self) -> Self {
        Self {
            name: self.name,
            read_text: self.read_text.clone(),
            write_text: self.write_text.clone(),
            read_image: self.read_image.clone(),
            write_image: self.write_image.clone(),
            read_files: self.read_files.clone(),
            write_files: self.write_files.clone(),
        }
    }

    fn read_blocking(&self) -> Result<Option<ClipboardItem>> {
        if let Some(tool) = &self.read_files {
            if let Ok(bytes) = Self::run_out(tool) {
                if let Some(item) = parse_uri_list(&bytes)? {
                    return Ok(Some(item));
                }
            }
        }
        if let Some(tool) = &self.read_image {
            if let Ok(bytes) = Self::run_out(tool) {
                if bytes.len() > 16 {
                    let mime = ImageMime::detect(&bytes);
                    let bytes = if mime.is_none() && bytes.len() > 4 {
                        tiff_to_png(&bytes).unwrap_or(bytes)
                    } else {
                        bytes
                    };
                    if let Some(mime) = ImageMime::detect(&bytes) {
                        return Ok(Some(ClipboardItem::Image {
                            mime,
                            bytes,
                            width: None,
                            height: None,
                        }));
                    }
                }
            }
        }
        if let Some(tool) = &self.read_text {
            if let Ok(bytes) = Self::run_out(tool) {
                let text = String::from_utf8_lossy(&bytes).to_string();
                if !text.is_empty() {
                    return Ok(Some(ClipboardItem::Text { text }));
                }
            }
        }
        Ok(None)
    }

    fn write_blocking(&self, item: &ClipboardItem) -> Result<()> {
        match item {
            ClipboardItem::Text { text } => {
                let tool = self
                    .write_text
                    .as_ref()
                    .ok_or(ClipboardError::Unsupported("text"))?;
                Self::run_in(tool, text.as_bytes())
            }
            ClipboardItem::Image { bytes, .. } => {
                let tool = self
                    .write_image
                    .as_ref()
                    .ok_or(ClipboardError::Unsupported("image"))?;
                Self::run_in(tool, bytes)
            }
            ClipboardItem::Files { files } => {
                let tool = self
                    .write_files
                    .as_ref()
                    .ok_or(ClipboardError::Unsupported("files"))?;
                let paths: Vec<std::path::PathBuf> =
                    files.iter().filter_map(|f| f.staged_path.clone()).collect();
                if paths.len() != files.len() {
                    return files_to_uri_list(files)
                        .and_then(|list| Self::run_in(tool, list.as_bytes()));
                }
                let list = files_to_uri_list_from_paths(&paths);
                Self::run_in(tool, list.as_bytes())
            }
        }
    }
}

fn parse_uri_list(bytes: &[u8]) -> Result<Option<ClipboardItem>> {
    let text = String::from_utf8_lossy(bytes);
    let mut files = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(path) = file_uri_to_path(line) else {
            return Ok(None);
        };
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
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
        files.push(FileRef {
            mime: infer::get(&bytes).map(|k| k.mime_type().to_string()),
            name,
            bytes,
            staged_path: Some(path),
        });
    }
    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ClipboardItem::Files { files }))
    }
}

pub fn files_to_uri_list_from_paths(paths: &[std::path::PathBuf]) -> String {
    paths
        .iter()
        .map(|p| path_to_file_uri(p))
        .collect::<Vec<_>>()
        .join("\n")
}

fn path_to_file_uri(path: &std::path::Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy();
    let mut out = String::from("file://");
    if cfg!(windows) {
        out.push('/');
    }
    for b in s.replace('\\', "/").bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn files_to_uri_list(files: &[FileRef]) -> Result<String> {
    let _ = files;
    Err(ClipboardError::Message(
        "command adapter cannot write in-memory files; stage to inbox first".into(),
    ))
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    Some(PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod regression_tests {
    use super::*;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            loop {
                let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "clipsync-command-regression-{}-{unique}",
                    std::process::id()
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("create isolated fixture: {e}"),
                }
            }
        }

        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn concurrent_command_fixtures_never_share_files() {
        let start = std::sync::Arc::new(std::sync::Barrier::new(16));
        let workers: Vec<_> = (0..16)
            .map(|id| {
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let root = Scratch::new();
                    root.file("same-name", id.to_string().as_bytes());
                    (root, id)
                })
            })
            .collect();
        let fixtures: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let unique: std::collections::HashSet<_> = fixtures.iter().map(|(s, _)| &s.0).collect();
        assert_eq!(unique.len(), fixtures.len());
        for (root, id) in &fixtures {
            assert_eq!(
                std::fs::read_to_string(root.0.join("same-name")).unwrap(),
                id.to_string()
            );
        }
    }

    fn cat(path: &std::path::Path) -> Tool {
        Tool {
            bin: PathBuf::from("/bin/cat"),
            args: vec![path.display().to_string()],
        }
    }

    fn adapter(
        read_files: Tool,
        read_text: Option<Tool>,
        write_files: Option<Tool>,
    ) -> CommandClipboard {
        CommandClipboard {
            name: "fixture-command-clipboard",
            read_text,
            write_text: None,
            read_image: None,
            write_image: None,
            read_files: Some(read_files),
            write_files,
        }
    }

    // F20: restoring the pre-f1a1acd raw file:// paths corrupts percent escapes
    // and splits newline-containing filenames into multiple clipboard entries.
    #[test]
    fn command_file_clipboard_roundtrip_preserves_uri_special_characters() {
        let scratch = Scratch::new();
        let first_name = "report #50%25? 사진\nsecond line.txt";
        let first = scratch.file(first_name, b"first file payload");
        let second = scratch.file("plain.txt", b"second file payload");
        let uri_file = scratch.file("clipboard-uri-list", b"");
        let writer = Tool {
            bin: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                "cat > \"$1\"".into(),
                "clipsync-clipboard-fixture".into(),
                uri_file.display().to_string(),
            ],
        };
        let clipboard = adapter(cat(&uri_file), None, Some(writer));
        let files = vec![
            FileRef {
                name: first_name.into(),
                bytes: b"first file payload".to_vec(),
                mime: None,
                staged_path: Some(first.clone()),
            },
            FileRef {
                name: "plain.txt".into(),
                bytes: b"second file payload".to_vec(),
                mime: None,
                staged_path: Some(second),
            },
        ];
        clipboard
            .write_blocking(&ClipboardItem::Files { files })
            .unwrap();
        let exported = std::fs::read_to_string(&uri_file).unwrap();
        assert_eq!(exported.lines().count(), 2, "one URI per copied file");
        assert!(exported.contains("report%20%2350%2525%3F%20"));
        assert!(exported.contains("%0Asecond%20line.txt"));
        let Some(ClipboardItem::Files { files }) = clipboard.read_blocking().unwrap() else {
            panic!("URI clipboard did not decode to files");
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, first_name);
        assert_eq!(files[0].bytes, b"first file payload");
        assert_eq!(files[0].staged_path, Some(first.canonicalize().unwrap()));
        assert_eq!(files[1].name, "plain.txt");
        assert_eq!(files[1].bytes, b"second file payload");
    }

    // F29: restoring the pre-9c837f1 invalid-URI error prevents read_blocking
    // from reaching the text adapter when a clipboard advertises misleading MIME.
    #[test]
    fn misleading_file_mime_falls_back_to_actual_text() {
        let scratch = Scratch::new();
        let files = scratch.file("file-mime", b"ordinary clipboard text, not a file URI");
        let text = scratch.file("text-mime", b"actual text with a newline\nand more text");
        let clipboard = adapter(cat(&files), Some(cat(&text)), None);
        assert_eq!(
            clipboard.read_blocking().unwrap(),
            Some(ClipboardItem::Text {
                text: "actual text with a newline\nand more text".into(),
            })
        );
    }

    #[test]
    fn vanished_file_uri_falls_back_to_actual_text() {
        let scratch = Scratch::new();
        let missing = scratch.0.join("does-not-exist.txt");
        let files = scratch.file(
            "file-mime",
            format!("file://{}", missing.display()).as_bytes(),
        );
        let text = scratch.file("text-mime", b"text remains available");
        let clipboard = adapter(cat(&files), Some(cat(&text)), None);
        assert_eq!(
            clipboard.read_blocking().unwrap(),
            Some(ClipboardItem::Text {
                text: "text remains available".into(),
            })
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod coverage_tests {
    use super::*;
    use crate::test_support::Scratch;

    fn cat(path: &std::path::Path) -> Tool {
        Tool {
            bin: "/bin/cat".into(),
            args: vec![path.display().to_string()],
        }
    }
    fn sink(path: &std::path::Path) -> Tool {
        Tool {
            bin: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "cat > \"$1\"".into(),
                "clipboard-test".into(),
                path.display().to_string(),
            ],
        }
    }
    fn backend() -> CommandClipboard {
        CommandClipboard {
            name: "isolated-command",
            read_text: None,
            write_text: None,
            read_image: None,
            write_image: None,
            read_files: None,
            write_files: None,
        }
    }
    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([4, 5, 6, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        bytes
    }

    #[tokio::test]
    async fn asynchronous_text_backend_roundtrip_updates_the_change_signature() {
        let root = Scratch::new();
        let buffer = root.file("buffer", b"");
        let mut clipboard = backend();
        clipboard.read_text = Some(cat(&buffer));
        clipboard.write_text = Some(sink(&buffer));
        assert_eq!(clipboard.name(), "isolated-command");
        let caps = clipboard.capabilities();
        assert!(caps.text && caps.watch && !caps.image && !caps.files);
        assert_eq!(clipboard.signature().await.unwrap(), "empty");
        let item = ClipboardItem::Text {
            text: "text with \0 and unicode 사진".into(),
        };
        clipboard.write(&item).await.unwrap();
        assert_eq!(clipboard.read().await.unwrap(), Some(item.clone()));
        let before = clipboard.signature().await.unwrap();
        assert_eq!(before, clipboard.signature().await.unwrap());
        clipboard
            .write(&ClipboardItem::Text {
                text: "new clipboard".into(),
            })
            .await
            .unwrap();
        assert_ne!(before, clipboard.signature().await.unwrap());
        assert_eq!(std::fs::read(&buffer).unwrap(), b"new clipboard");
    }

    #[test]
    fn command_reads_prefer_files_then_images_then_text() {
        let root = Scratch::new();
        let file = root.file("regular.txt", b"file contents");
        let list = root.file(
            "uri-list",
            files_to_uri_list_from_paths(std::slice::from_ref(&file)).as_bytes(),
        );
        let picture = png();
        let image = root.file("image", &picture);
        let text = root.file("text", b"text fallback");
        let mut clipboard = backend();
        clipboard.read_files = Some(cat(&list));
        clipboard.read_image = Some(cat(&image));
        clipboard.read_text = Some(cat(&text));
        let Some(ClipboardItem::Files { files }) = clipboard.read_blocking().unwrap() else {
            panic!("file flavor lost priority");
        };
        assert_eq!(files[0].bytes, b"file contents");
        std::fs::write(&list, b"not a file URI").unwrap();
        let Some(ClipboardItem::Image {
            mime,
            bytes,
            width,
            height,
        }) = clipboard.read_blocking().unwrap()
        else {
            panic!("image flavor lost priority");
        };
        assert_eq!(mime, ImageMime::Png);
        assert_eq!(bytes, picture);
        assert!(width.is_none() && height.is_none());
        std::fs::write(&image, b"unrecognized picture bytes long enough to decode").unwrap();
        crate::test_support::assert_text(clipboard.read_blocking().unwrap(), "text fallback");
        std::fs::write(&text, b"").unwrap();
        assert!(clipboard.read_blocking().unwrap().is_none());
    }

    #[test]
    fn command_image_reader_converts_tiff_and_ignores_tiny_or_failed_flavors() {
        let root = Scratch::new();
        let mut tiff = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([7, 8, 9, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut tiff),
            image::ImageFormat::Tiff,
        )
        .unwrap();
        let image = root.file("image", &tiff);
        let text = root.file("text", b"available text");
        let mut clipboard = backend();
        clipboard.read_image = Some(cat(&image));
        clipboard.read_text = Some(cat(&text));
        let Some(ClipboardItem::Image { mime, bytes, .. }) = clipboard.read_blocking().unwrap()
        else {
            panic!("TIFF not converted");
        };
        assert_eq!(mime, ImageMime::Png);
        assert_eq!(
            image::load_from_memory(&bytes)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [7, 8, 9, 255]
        );
        std::fs::write(&image, b"tiny").unwrap();
        crate::test_support::assert_text(clipboard.read_blocking().unwrap(), "available text");
        clipboard.read_files = Some(Tool {
            bin: root.0.join("missing-reader"),
            args: vec![],
        });
        clipboard.read_image = Some(Tool {
            bin: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 9".into()],
        });
        crate::test_support::assert_text(clipboard.read_blocking().unwrap(), "available text");
        clipboard.read_text = Some(Tool {
            bin: root.0.join("also-missing"),
            args: vec![],
        });
        assert!(clipboard.read_blocking().unwrap().is_none());
    }

    #[test]
    fn command_writes_preserve_image_bytes_and_require_staged_files() {
        let root = Scratch::new();
        let output = root.file("output", b"unchanged");
        let mut clipboard = backend();
        clipboard.write_image = Some(sink(&output));
        clipboard.write_files = Some(sink(&output));
        let picture = png();
        clipboard
            .write_blocking(&ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: picture.clone(),
                width: None,
                height: None,
            })
            .unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), picture);
        let unstaged = ClipboardItem::Files {
            files: vec![FileRef {
                name: "unmaterialized.txt".into(),
                bytes: b"secret payload".to_vec(),
                mime: None,
                staged_path: None,
            }],
        };
        assert!(clipboard
            .write_blocking(&unstaged)
            .unwrap_err()
            .to_string()
            .contains("stage to inbox"));
        assert_eq!(
            std::fs::read(&output).unwrap(),
            picture,
            "rejected file must not reach the native tool"
        );
        let unavailable = backend();
        for item in [
            ClipboardItem::Text {
                text: "text".into(),
            },
            ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: picture,
                width: None,
                height: None,
            },
            unstaged,
        ] {
            assert!(matches!(
                unavailable.write_blocking(&item),
                Err(ClipboardError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn command_failures_report_spawn_and_exit_errors_without_succeeding() {
        let root = Scratch::new();
        let missing = Tool {
            bin: root.0.join("does-not-exist"),
            args: vec![],
        };
        assert!(matches!(
            CommandClipboard::run_out(&missing),
            Err(ClipboardError::Unavailable(_))
        ));
        assert!(matches!(
            CommandClipboard::run_in(&missing, b"data"),
            Err(ClipboardError::Unavailable(_))
        ));
        let failure = Tool {
            bin: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 23".into()],
        };
        assert!(CommandClipboard::run_out(&failure)
            .unwrap_err()
            .to_string()
            .contains("23"));
        assert!(CommandClipboard::run_in(&failure, b"small").is_err());
        assert!(CommandClipboard::run_in(&failure, &vec![0; 2 * 1024 * 1024]).is_err());
    }

    #[test]
    fn uri_file_reader_skips_comments_and_missing_files_but_rejects_unsafe_entries() {
        let root = Scratch::new();
        assert!(parse_uri_list(b"# clipboard comment\n\n")
            .unwrap()
            .is_none());
        assert!(parse_uri_list(b"file:///not/present/clipsync-fixture")
            .unwrap()
            .is_none());
        let directory = root.dir("directory");
        assert!(parse_uri_list(files_to_uri_list_from_paths(&[directory]).as_bytes()).is_err());
        let literal_percent = root.file("bad%ZZ%name.txt", b"literal percent bytes");
        let encoded = format!("# ignored\nfile://{}\n", literal_percent.display());
        let Some(ClipboardItem::Files { files }) = parse_uri_list(encoded.as_bytes()).unwrap()
        else {
            panic!("literal malformed escapes were discarded");
        };
        assert_eq!(files[0].bytes, b"literal percent bytes");
        let link = root.0.join("link.txt");
        std::os::unix::fs::symlink(&literal_percent, &link).unwrap();
        assert!(parse_uri_list(format!("file://{}", link.display()).as_bytes()).is_err());
    }

    #[test]
    fn discovered_tools_child() {
        let Ok(case) = std::env::var("CLIPSYNC_CLIPBOARD_TOOL_CASE") else {
            return;
        };
        let mut clipboard = match case.as_str() {
            #[cfg(target_os = "macos")]
            "macos" => CommandClipboard::macos_tools().unwrap(),
            #[cfg(target_os = "macos")]
            "missing" => {
                assert!(CommandClipboard::macos_tools().is_none());
                return;
            }
            #[cfg(target_os = "linux")]
            "xclip" | "xsel" => CommandClipboard::x11_tools().unwrap(),
            #[cfg(target_os = "linux")]
            "wayland" => CommandClipboard::wayland_tools().unwrap(),
            _ => panic!("unknown tool case"),
        };
        let caps = clipboard.capabilities();
        assert!(caps.text && caps.watch);
        if case == "xsel" || case == "macos" {
            assert!(!caps.image && !caps.files);
        }
        clipboard
            .write_blocking(&ClipboardItem::Text {
                text: "tool roundtrip".into(),
            })
            .unwrap();
        // Clear richer readers here so this test specifically observes the
        // discovered text commands; priority/conversion is tested separately.
        clipboard.read_files = None;
        clipboard.read_image = None;
        crate::test_support::assert_text(clipboard.read_blocking().unwrap(), "tool roundtrip");
    }

    #[test]
    fn available_native_tools_are_discovered_without_using_the_users_path() {
        #[cfg(target_os = "macos")]
        let cases = ["macos", "missing"];
        #[cfg(target_os = "linux")]
        let cases = ["xclip", "xsel", "wayland"];
        for case in cases {
            let root = Scratch::new();
            let buffer = root.file("buffer", b"");
            let reader = "#!/bin/sh\nexec /bin/cat \"$CLIPSYNC_TOOL_BUFFER\"\n";
            let writer = "#!/bin/sh\nexec /bin/cat > \"$CLIPSYNC_TOOL_BUFFER\"\n";
            match case {
                "macos" => {
                    root.executable("pbcopy", writer);
                    root.executable("pbpaste", reader);
                }
                "missing" => {
                    root.executable("pbcopy", writer);
                }
                "xclip" | "xsel" => {
                    root.executable(case, "#!/bin/sh\ncase \"$*\" in *-o*|*--output*) exec /bin/cat \"$CLIPSYNC_TOOL_BUFFER\" ;; *) exec /bin/cat > \"$CLIPSYNC_TOOL_BUFFER\" ;; esac\n");
                }
                "wayland" => {
                    root.executable("wl-copy", writer);
                    root.executable("wl-paste", reader);
                }
                _ => unreachable!(),
            }
            let out = root
                .child("command::coverage_tests::discovered_tools_child")
                .env("CLIPSYNC_CLIPBOARD_TOOL_CASE", case)
                .env("CLIPSYNC_TOOL_BUFFER", buffer)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "tool discovery failed: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
