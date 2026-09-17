use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use async_trait::async_trait;

use crate::detect::{hash_item, tool_exists, which_bin, Capabilities};
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
        let path = file_uri_to_path(line)
            .ok_or_else(|| ClipboardError::Message(format!("invalid file uri: {line}")))?;
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

#[allow(dead_code)]
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

#[allow(dead_code)]
pub fn wayland_watch_available() -> bool {
    tool_exists("wl-paste")
}

#[allow(dead_code)]
pub fn command_timeout() -> Duration {
    Duration::from_secs(5)
}
