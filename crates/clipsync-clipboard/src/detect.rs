// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::command::CommandClipboard;
#[cfg(not(target_os = "linux"))]
use crate::ClipboardError;
use crate::{ClipboardBackend, Result};

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
        detect_macos_adapter(
            || crate::macos::MacClipboard::new().map(|c| Box::new(c) as Box<dyn ClipboardBackend>),
            || CommandClipboard::macos_tools().map(|c| Box::new(c) as Box<dyn ClipboardBackend>),
            || ArboardClipboard::new().map(|c| Box::new(c) as Box<dyn ClipboardBackend>),
        )
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
                        files: true,
                        watch: true,
                    },
                    fallback_reason: None,
                };
                Ok((Box::new(ar), info))
            }
            Err(e) => Err(ClipboardError::Unavailable(format!(
                "windows clipboard: {e}"
            ))),
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
        Ok((Box::new(CommandClipboard::headless()), info))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        if let Ok(ar) = ArboardClipboard::new() {
            let info = AdapterInfo {
                name: "arboard",
                source: "library",
                capabilities: Capabilities {
                    text: true,
                    image: true,
                    files: true,
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
}

#[cfg(target_os = "macos")]
fn detect_macos_adapter(
    native: impl FnOnce() -> Result<Box<dyn ClipboardBackend>>,
    command: impl FnOnce() -> Option<Box<dyn ClipboardBackend>>,
    generic: impl FnOnce() -> Result<Box<dyn ClipboardBackend>>,
) -> Result<(Box<dyn ClipboardBackend>, AdapterInfo)> {
    match native() {
        Ok(backend) => Ok((
            backend,
            AdapterInfo {
                name: "macos-nspasteboard",
                source: "native",
                capabilities: Capabilities::all(),
                fallback_reason: None,
            },
        )),
        Err(error) => {
            if let Some(backend) = command() {
                let capabilities = backend.capabilities();
                return Ok((
                    backend,
                    AdapterInfo {
                        name: "macos-pbpaste",
                        source: "command",
                        capabilities,
                        fallback_reason: Some(error.to_string()),
                    },
                ));
            }
            if let Ok(backend) = generic() {
                return Ok((
                    backend,
                    AdapterInfo {
                        name: "arboard",
                        source: "library",
                        capabilities: Capabilities::all(),
                        fallback_reason: Some("generic fallback".into()),
                    },
                ));
            }
            Err(ClipboardError::Unavailable(
                "no clipboard adapter found".into(),
            ))
        }
    }
}

#[cfg(not(target_os = "linux"))]
type NativeFactory = std::sync::Arc<dyn Fn() -> Result<Box<dyn NativeClipboard>> + Send + Sync>;

#[cfg(not(target_os = "linux"))]
trait NativeClipboard {
    fn file_list(&mut self) -> Result<Vec<std::path::PathBuf>>;
    fn image(&mut self) -> Result<arboard::ImageData<'_>>;
    fn text(&mut self) -> Result<String>;
    fn put_text(&mut self, text: String) -> Result<()>;
    fn put_files(&mut self, paths: &[std::path::PathBuf]) -> Result<()>;
    fn put_image(&mut self, image: arboard::ImageData<'_>) -> Result<()>;
}

#[cfg(not(target_os = "linux"))]
impl NativeClipboard for arboard::Clipboard {
    fn file_list(&mut self) -> Result<Vec<std::path::PathBuf>> {
        self.get()
            .file_list()
            .map_err(|e| ClipboardError::Message(e.to_string()))
    }
    fn image(&mut self) -> Result<arboard::ImageData<'_>> {
        self.get_image()
            .map_err(|e| ClipboardError::Message(e.to_string()))
    }
    fn text(&mut self) -> Result<String> {
        self.get_text()
            .map_err(|e| ClipboardError::Message(e.to_string()))
    }
    fn put_text(&mut self, text: String) -> Result<()> {
        self.set_text(text)
            .map_err(|e| ClipboardError::Message(e.to_string()))
    }
    fn put_files(&mut self, paths: &[std::path::PathBuf]) -> Result<()> {
        self.set()
            .file_list(paths)
            .map_err(|e| ClipboardError::Message(e.to_string()))
    }
    fn put_image(&mut self, image: arboard::ImageData<'_>) -> Result<()> {
        self.set_image(image)
            .map_err(|e| ClipboardError::Message(e.to_string()))
    }
}

#[cfg(not(target_os = "linux"))]
pub struct ArboardClipboard {
    // Native handles are still opened and dropped on each worker thread, as
    // arboard Clipboard is not Sync on every supported platform.
    open: NativeFactory,
}

#[cfg(not(target_os = "linux"))]
impl ArboardClipboard {
    pub fn new() -> Result<Self> {
        Self::with_factory(std::sync::Arc::new(|| {
            arboard::Clipboard::new()
                .map(|clipboard| Box::new(clipboard) as Box<dyn NativeClipboard>)
                .map_err(|e| ClipboardError::Unavailable(e.to_string()))
        }))
    }

    fn with_factory(open: NativeFactory) -> Result<Self> {
        let _ = open()?;
        Ok(Self { open })
    }
}

use crate::item::ClipboardItem;
#[cfg(not(target_os = "linux"))]
use crate::item::{decode_image_rgba, encode_png_rgba, FileRef, ImageMime};
#[cfg(not(target_os = "linux"))]
use async_trait::async_trait;

#[cfg(not(target_os = "linux"))]
#[async_trait]
impl ClipboardBackend for ArboardClipboard {
    fn name(&self) -> &'static str {
        "arboard"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            text: true,
            image: true,
            files: true,
            watch: true,
        }
    }

    async fn read(&self) -> Result<Option<ClipboardItem>> {
        let open = self.open.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = open()?;
            if let Ok(paths) = c.file_list() {
                if let Some(files) = load_file_refs(&paths)? {
                    return Ok(Some(ClipboardItem::Files { files }));
                }
            }
            if let Ok(img) = c.image() {
                let png = encode_png_rgba(img.width as u32, img.height as u32, &img.bytes)?;
                return Ok(Some(ClipboardItem::Image {
                    mime: ImageMime::Png,
                    bytes: png,
                    width: Some(img.width as u32),
                    height: Some(img.height as u32),
                }));
            }
            match c.text() {
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
        let open = self.open.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = open()?;
            match item {
                ClipboardItem::Text { text } => c.put_text(text)?,
                ClipboardItem::Files { files } => {
                    let paths: Vec<std::path::PathBuf> =
                        files.iter().filter_map(|f| f.staged_path.clone()).collect();
                    if paths.len() != files.len() || paths.is_empty() {
                        return Err(ClipboardError::Unsupported("files"));
                    }
                    c.put_files(&paths)?;
                }
                ClipboardItem::Image { bytes, .. } => {
                    let (w, h, rgba) = decode_image_rgba(&bytes)?;
                    let img = arboard::ImageData {
                        width: w as usize,
                        height: h as usize,
                        bytes: rgba.into(),
                    };
                    c.put_image(img)?;
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

#[cfg(not(target_os = "linux"))]
fn load_file_refs(paths: &[std::path::PathBuf]) -> Result<Option<Vec<FileRef>>> {
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

#[cfg(all(test, not(target_os = "linux")))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod native_tests {
    use super::*;
    use crate::test_support::Scratch;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        files: Option<Vec<std::path::PathBuf>>,
        image: Option<(usize, usize, Vec<u8>)>,
        text: Option<String>,
        write_error: bool,
    }
    struct Provider(Arc<Mutex<State>>);
    fn absent() -> ClipboardError {
        ClipboardError::Unavailable("format unavailable".into())
    }
    impl NativeClipboard for Provider {
        fn file_list(&mut self) -> Result<Vec<std::path::PathBuf>> {
            self.0.lock().unwrap().files.clone().ok_or_else(absent)
        }
        fn image(&mut self) -> Result<arboard::ImageData<'_>> {
            let (width, height, bytes) = self.0.lock().unwrap().image.clone().ok_or_else(absent)?;
            Ok(arboard::ImageData {
                width,
                height,
                bytes: bytes.into(),
            })
        }
        fn text(&mut self) -> Result<String> {
            self.0.lock().unwrap().text.clone().ok_or_else(absent)
        }
        fn put_text(&mut self, text: String) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            if state.write_error {
                return Err(absent());
            }
            state.files = None;
            state.image = None;
            state.text = Some(text);
            Ok(())
        }
        fn put_files(&mut self, paths: &[std::path::PathBuf]) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            if state.write_error {
                return Err(absent());
            }
            state.text = None;
            state.image = None;
            state.files = Some(paths.to_vec());
            Ok(())
        }
        fn put_image(&mut self, image: arboard::ImageData<'_>) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            if state.write_error {
                return Err(absent());
            }
            state.files = None;
            state.text = None;
            state.image = Some((image.width, image.height, image.bytes.into_owned()));
            Ok(())
        }
    }
    fn backend(state: Arc<Mutex<State>>) -> ArboardClipboard {
        ArboardClipboard::with_factory(Arc::new(move || Ok(Box::new(Provider(state.clone())))))
            .unwrap()
    }
    fn png() -> Vec<u8> {
        encode_png_rgba(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]).unwrap()
    }

    #[tokio::test]
    async fn native_reader_selects_files_before_pixels_and_pixels_before_text() {
        let root = Scratch::new();
        let path = root.file("file.txt", b"file payload");
        let directory = root.dir("directory");
        let state = Arc::new(Mutex::new(State {
            files: Some(vec![root.0.join("missing"), directory, path]),
            image: Some((2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255])),
            text: Some("text fallback".into()),
            ..Default::default()
        }));
        let clipboard = backend(state.clone());
        assert_eq!(clipboard.name(), "arboard");
        let caps = clipboard.capabilities();
        assert!(caps.text && caps.image && caps.files && caps.watch);
        let Some(ClipboardItem::Files { files }) = clipboard.read().await.unwrap() else {
            panic!("files lost priority");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "file.txt");
        assert_eq!(files[0].bytes, b"file payload");
        state.lock().unwrap().files = Some(vec![]);
        let Some(ClipboardItem::Image {
            mime,
            bytes,
            width,
            height,
        }) = clipboard.read().await.unwrap()
        else {
            panic!("pixels lost priority");
        };
        assert_eq!(mime, ImageMime::Png);
        assert_eq!((width, height), (Some(2), Some(1)));
        assert_eq!(
            decode_image_rgba(&bytes).unwrap().2,
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
        state.lock().unwrap().image = None;
        crate::test_support::assert_text(clipboard.read().await.unwrap(), "text fallback");
        state.lock().unwrap().text = Some(String::new());
        assert!(clipboard.read().await.unwrap().is_none());
        state.lock().unwrap().text = None;
        assert_eq!(clipboard.signature().await.unwrap(), "empty");
    }

    #[tokio::test]
    async fn native_writer_materializes_text_file_references_and_rgba_pixels() {
        let root = Scratch::new();
        let path = root.file("staged.txt", b"file bytes");
        let state = Arc::new(Mutex::new(State::default()));
        let clipboard = backend(state.clone());
        clipboard
            .write(&ClipboardItem::Text {
                text: "text payload".into(),
            })
            .await
            .unwrap();
        crate::test_support::assert_text(clipboard.read().await.unwrap(), "text payload");
        let before = clipboard.signature().await.unwrap();
        clipboard
            .write(&ClipboardItem::Text {
                text: "changed payload".into(),
            })
            .await
            .unwrap();
        assert_ne!(clipboard.signature().await.unwrap(), before);
        clipboard
            .write(&ClipboardItem::Files {
                files: vec![FileRef {
                    name: "staged.txt".into(),
                    bytes: b"file bytes".to_vec(),
                    mime: None,
                    staged_path: Some(path.clone()),
                }],
            })
            .await
            .unwrap();
        let Some(ClipboardItem::Files { files }) = clipboard.read().await.unwrap() else {
            panic!("native file reference not published");
        };
        assert_eq!(files[0].staged_path.as_ref(), Some(&path));
        assert_eq!(files[0].bytes, b"file bytes");
        clipboard
            .write(&ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: png(),
                width: None,
                height: None,
            })
            .await
            .unwrap();
        assert_eq!(
            state.lock().unwrap().image,
            Some((2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]))
        );
        assert_eq!(std::fs::read(path).unwrap(), b"file bytes");
    }

    #[tokio::test]
    async fn native_writer_rejects_unstaged_empty_and_corrupt_items() {
        let state = Arc::new(Mutex::new(State::default()));
        let clipboard = backend(state.clone());
        for files in [
            vec![],
            vec![FileRef {
                name: "missing".into(),
                bytes: vec![1],
                mime: None,
                staged_path: None,
            }],
        ] {
            assert!(matches!(
                clipboard.write(&ClipboardItem::Files { files }).await,
                Err(ClipboardError::Unsupported("files"))
            ));
        }
        assert!(clipboard
            .write(&ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: b"not encoded image data".to_vec(),
                width: None,
                height: None
            })
            .await
            .is_err());
        assert!(state.lock().unwrap().files.is_none());
        assert!(state.lock().unwrap().image.is_none());
        state.lock().unwrap().image = Some((2, 2, vec![1]));
        assert!(
            clipboard.read().await.is_err(),
            "invalid native RGBA must not become a valid image"
        );
    }

    #[tokio::test]
    async fn native_write_errors_and_factory_failures_reach_the_caller() {
        let root = Scratch::new();
        let path = root.file("staged.txt", b"data");
        let state = Arc::new(Mutex::new(State {
            write_error: true,
            ..Default::default()
        }));
        let clipboard = backend(state);
        for item in [
            ClipboardItem::Text {
                text: "text".into(),
            },
            ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: png(),
                width: None,
                height: None,
            },
            ClipboardItem::Files {
                files: vec![FileRef {
                    name: "staged.txt".into(),
                    bytes: b"data".to_vec(),
                    mime: None,
                    staged_path: Some(path),
                }],
            },
        ] {
            assert!(clipboard.write(&item).await.is_err());
        }
        assert!(ArboardClipboard::with_factory(Arc::new(|| Err(absent()))).is_err());
        let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = fail.clone();
        let clipboard = ArboardClipboard::with_factory(Arc::new(move || {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                Err(absent())
            } else {
                Ok(Box::new(Provider(Arc::new(Mutex::new(State::default())))))
            }
        }))
        .unwrap();
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(clipboard.read().await.is_err());
        assert!(clipboard
            .write(&ClipboardItem::Text {
                text: "data".into()
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn worker_panics_are_reported_instead_of_escaping_the_backend() {
        let crash = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = crash.clone();
        let clipboard = ArboardClipboard::with_factory(Arc::new(move || {
            assert!(
                !flag.load(std::sync::atomic::Ordering::SeqCst),
                "injected native worker failure"
            );
            Ok(Box::new(Provider(Arc::new(Mutex::new(State::default())))))
        }))
        .unwrap();
        crash.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(
            clipboard.read().await,
            Err(ClipboardError::Message(_))
        ));
        assert!(matches!(
            clipboard
                .write(&ClipboardItem::Text {
                    text: "text".into()
                })
                .await,
            Err(ClipboardError::Message(_))
        ));
    }

    #[test]
    fn native_file_loader_skips_symlinks_directories_and_non_utf8_names() {
        let root = Scratch::new();
        assert!(
            load_file_refs(&[root.0.join("missing"), root.dir("directory")])
                .unwrap()
                .is_none()
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let file = root.file("regular", b"regular");
            let link = root.0.join("symlink");
            std::os::unix::fs::symlink(file, &link).unwrap();
            let invalid = root.0.join(std::ffi::OsString::from_vec(vec![b'n', 0xff]));
            // Some filesystems reject non-UTF8 bytes before an entry can exist.
            let _ = std::fs::write(&invalid, b"invalid filename");
            assert!(load_file_refs(&[link, invalid]).unwrap().is_none());
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod detector_tests {
    use super::*;
    fn memory(text: &str) -> Box<dyn ClipboardBackend> {
        let clip = crate::MockClipboard::new();
        clip.set(ClipboardItem::Text { text: text.into() });
        Box::new(clip)
    }
    #[tokio::test]
    async fn detector_prefers_native_and_reports_real_fallback_capabilities() {
        let (backend, info) = detect_macos_adapter(
            || Ok(memory("native")),
            || panic!("command fallback should stay idle"),
            || panic!("generic fallback should stay idle"),
        )
        .unwrap();
        assert_eq!(info.source, "native");
        assert!(info.fallback_reason.is_none());
        crate::test_support::assert_text(backend.read().await.unwrap(), "native");
        let (backend, info) = detect_macos_adapter(
            || Err(ClipboardError::Unavailable("native denied".into())),
            || Some(memory("command")),
            || panic!("generic fallback should stay idle"),
        )
        .unwrap();
        assert_eq!(info.name, "macos-pbpaste");
        assert_eq!(info.source, "command");
        assert!(info.fallback_reason.unwrap().contains("native denied"));
        crate::test_support::assert_text(backend.read().await.unwrap(), "command");
        let (backend, info) = detect_macos_adapter(
            || Err(ClipboardError::Unavailable("native denied".into())),
            || None,
            || Ok(memory("generic")),
        )
        .unwrap();
        assert_eq!(info.name, "arboard");
        assert_eq!(info.source, "library");
        crate::test_support::assert_text(backend.read().await.unwrap(), "generic");
        assert!(detect_macos_adapter(
            || Err(ClipboardError::Unavailable("native denied".into())),
            || None,
            || Err(ClipboardError::Unavailable("generic denied".into()))
        )
        .is_err());
    }
    #[test]
    fn doctor_probe_child() {
        let Ok(root) = std::env::var("CLIPSYNC_DOCTOR_FIXTURE") else {
            return;
        };
        assert_eq!(
            which_bin("pbcopy"),
            Some(std::path::PathBuf::from(root).join("pbcopy"))
        );
        assert!(tool_exists("pbcopy"));
        assert!(!tool_exists("pbpaste"));
        let tools = collect_doctor_tools();
        assert_eq!(tools.len(), 10);
        assert_eq!(
            tools
                .iter()
                .filter(|(_, present)| *present)
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["pbcopy", "xclip"]
        );
    }
    #[test]
    fn doctor_reports_available_and_missing_tools_from_the_supplied_environment() {
        let root = crate::test_support::Scratch::new();
        root.executable("pbcopy", "#!/bin/sh\nexit 0\n");
        root.executable("xclip", "#!/bin/sh\nexit 0\n");
        let out = root
            .child("detect::detector_tests::doctor_probe_child")
            .env("CLIPSYNC_DOCTOR_FIXTURE", &root.0)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "doctor fixture failed: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod hash_tests {
    use super::*;
    #[test]
    fn clipboard_fingerprints_track_contents_and_file_names_without_local_paths() {
        let text = ClipboardItem::Text {
            text: "same".into(),
        };
        let image = ClipboardItem::Image {
            mime: crate::ImageMime::Png,
            bytes: b"same".to_vec(),
            width: None,
            height: None,
        };
        assert_ne!(hash_item(&text), hash_item(&image));
        let first = crate::FileRef {
            name: "a".into(),
            bytes: b"payload".to_vec(),
            mime: None,
            staged_path: Some("/first".into()),
        };
        let mut moved = first.clone();
        moved.staged_path = Some("/other-machine".into());
        assert_eq!(
            hash_item(&ClipboardItem::Files {
                files: vec![first.clone()]
            }),
            hash_item(&ClipboardItem::Files { files: vec![moved] })
        );
        let mut renamed = first.clone();
        renamed.name = "b".into();
        assert_ne!(
            hash_item(&ClipboardItem::Files { files: vec![first] }),
            hash_item(&ClipboardItem::Files {
                files: vec![renamed]
            })
        );
    }
}
