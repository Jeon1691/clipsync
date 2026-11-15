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
                    name: wl.name(),
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
                    name: x11.name(),
                    source: "command",
                    capabilities: x11.capabilities(),
                    fallback_reason: None,
                };
                return Ok((Box::new(x11), info));
            }
        }
        let headless = CommandClipboard::headless();
        let info = AdapterInfo {
            name: headless.name(),
            source: "stdin-stdout",
            capabilities: headless.capabilities(),
            fallback_reason: Some("no graphical clipboard adapter".into()),
        };
        Ok((Box::new(headless), info))
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
        #[cfg(unix)]
        let bytes = read_regular_file(path)?;
        #[cfg(not(unix))]
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

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn read_regular_file(path: &std::path::Path) -> Result<Vec<u8>> {
    use clipsync_protocol::FILE_MAX_BYTES;
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    // A file URL may refer to a FIFO, or change after the path metadata check.
    // Open without waiting for a writer, reject symlink substitutions, and
    // validate the opened descriptor before attempting to read any contents.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "clipboard file is not a regular file",
        )
        .into());
    }
    if metadata.len() > FILE_MAX_BYTES {
        return Err(ClipboardError::Message(format!(
            "clipboard file exceeds {FILE_MAX_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::new();
    // The descriptor can grow after metadata inspection. Read at most one
    // byte beyond the existing protocol limit so oversized input is rejected.
    file.take(FILE_MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > FILE_MAX_BYTES {
        return Err(ClipboardError::Message(format!(
            "clipboard file exceeds {FILE_MAX_BYTES} bytes"
        )));
    }
    Ok(bytes)
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
    async fn platform_host_arboard_roundtrip_preserves_text_pixels_and_private_files() {
        // The general clipboard belongs to the user on development machines.
        // Only disposable hosted desktop sessions explicitly opt into this test.
        if std::env::var("CLIPSYNC_TEST_HOST_CLIPBOARD").as_deref() != Ok("1") {
            return;
        }
        let root = Scratch::new();
        let clipboard = ArboardClipboard::new().expect("host Arboard adapter");
        assert_eq!(clipboard.name(), "arboard");
        let caps = clipboard.capabilities();
        assert!(caps.text && caps.image && caps.files && caps.watch);

        let text = format!("clipsync-arboard-{}: π🙂\nsecond line", std::process::id());
        clipboard
            .write(&ClipboardItem::Text { text: text.clone() })
            .await
            .unwrap();
        crate::test_support::assert_text(clipboard.read().await.unwrap(), &text);
        let text_signature = clipboard.signature().await.unwrap();
        assert_eq!(
            text_signature,
            format!("{:x}", hash_item(&ClipboardItem::Text { text }))
        );
        clipboard
            .write(&ClipboardItem::Text {
                text: "changed host fixture".into(),
            })
            .await
            .unwrap();
        assert_ne!(clipboard.signature().await.unwrap(), text_signature);

        clipboard
            .write(&ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: png(),
                width: Some(2),
                height: Some(1),
            })
            .await
            .unwrap();
        let image = clipboard.read().await.unwrap().expect("host image");
        let ClipboardItem::Image {
            mime,
            bytes,
            width,
            height,
        } = &image
        else {
            panic!("host image became {image:?}");
        };
        assert_eq!(*mime, ImageMime::Png);
        assert_eq!((*width, *height), (Some(2), Some(1)));
        assert_eq!(
            decode_image_rgba(bytes).unwrap(),
            (2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255])
        );
        let image_signature = clipboard.signature().await.unwrap();
        assert_eq!(image_signature, format!("{:x}", hash_item(&image)));
        assert_ne!(image_signature, text_signature);

        assert!(clipboard
            .write(&ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: b"corrupt host image fixture".to_vec(),
                width: None,
                height: None,
            })
            .await
            .is_err());
        for files in [
            vec![],
            vec![FileRef {
                name: "unstaged.txt".into(),
                bytes: b"unstaged".to_vec(),
                mime: None,
                staged_path: None,
            }],
        ] {
            assert!(matches!(
                clipboard.write(&ClipboardItem::Files { files }).await,
                Err(ClipboardError::Unsupported("files"))
            ));
        }
        assert_eq!(
            clipboard.signature().await.unwrap(),
            image_signature,
            "rejected input must not replace the host image"
        );

        let image_bytes = png();
        let text_path = root.file("first payload π.txt", b"private file payload");
        let image_path = root.file("second image.png", &image_bytes);
        let files = vec![
            FileRef {
                name: "first payload π.txt".into(),
                bytes: b"private file payload".to_vec(),
                mime: None,
                staged_path: Some(text_path.clone()),
            },
            FileRef {
                name: "second image.png".into(),
                bytes: image_bytes.clone(),
                mime: Some("image/png".into()),
                staged_path: Some(image_path.clone()),
            },
        ];
        clipboard
            .write(&ClipboardItem::Files { files })
            .await
            .unwrap();
        let item = clipboard.read().await.unwrap().expect("host file list");
        item.validate().unwrap();
        let ClipboardItem::Files { files } = &item else {
            panic!("host file list became {item:?}");
        };
        assert_eq!(files.len(), 2);
        for (name, expected_path, expected_bytes, expected_mime) in [
            (
                "first payload π.txt",
                text_path,
                b"private file payload".to_vec(),
                None,
            ),
            (
                "second image.png",
                image_path,
                image_bytes,
                Some("image/png"),
            ),
        ] {
            let file = files
                .iter()
                .find(|file| file.name == name)
                .expect("each staged filename must survive");
            assert_eq!(file.bytes, expected_bytes);
            assert_eq!(file.mime.as_deref(), expected_mime);
            assert_eq!(
                file.staged_path.as_ref().unwrap().canonicalize().unwrap(),
                expected_path.canonicalize().unwrap()
            );
        }
        assert_eq!(
            clipboard.signature().await.unwrap(),
            format!("{:x}", hash_item(&item))
        );

        #[cfg(target_os = "macos")]
        {
            // Arboard rejects an all-missing URL list on macOS. This exercises
            // the real native error mapping without depending on OS failures.
            assert!(matches!(
                clipboard
                    .write(&ClipboardItem::Files {
                        files: vec![FileRef {
                            name: "missing.txt".into(),
                            bytes: vec![],
                            mime: None,
                            staged_path: Some(root.0.join("missing.txt")),
                        }]
                    })
                    .await,
                Err(ClipboardError::Message(_))
            ));
        }
        // Leave no references to the fixture files when their directory drops.
        arboard::Clipboard::new().unwrap().clear().unwrap();
        assert!(clipboard.read().await.unwrap().is_none());
        assert_eq!(clipboard.signature().await.unwrap(), "empty");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn native_file_read_failures_do_not_silently_fall_back_to_text() {
        let root = Scratch::new();
        let path = root.0.join("not-a-readable-file");
        let _socket = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let clipboard = backend(Arc::new(Mutex::new(State {
            files: Some(vec![path.clone()]),
            text: Some("must not replace an unreadable file".into()),
            ..Default::default()
        })));
        assert!(matches!(
            load_file_refs(&[path]),
            Err(ClipboardError::Io(_))
        ));
        assert!(matches!(clipboard.read().await, Err(ClipboardError::Io(_))));
        assert!(matches!(
            clipboard.signature().await,
            Err(ClipboardError::Io(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_file_loader_rejects_private_fifo_without_blocking() {
        const CHILD: &str = "CLIPSYNC_DETECT_FIFO_CHILD";
        let Ok(path) = std::env::var(CHILD) else {
            let root = Scratch::new();
            let path = root.0.join("clipboard-fifo");
            assert!(std::process::Command::new("/usr/bin/mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success());
            let output = super::environment_tests::run_child_with_timeout(
                root.child("detect::native_tests::native_file_loader_rejects_private_fifo_without_blocking")
                    .env(CHILD, &path),
                std::time::Duration::from_secs(5),
            );
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let clipboard = backend(Arc::new(Mutex::new(State {
            files: Some(vec![path.into()]),
            text: Some("must not hide a rejected special file".into()),
            ..Default::default()
        })));
        assert!(matches!(clipboard.read().await, Err(ClipboardError::Io(_))));
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_loader_reads_regular_files_and_rejects_symlink_substitution() {
        let root = Scratch::new();
        let file = root.file("regular", b"unchanged regular-file bytes");
        assert_eq!(
            read_regular_file(&file).unwrap(),
            b"unchanged regular-file bytes"
        );
        let link = root.0.join("substituted-link");
        std::os::unix::fs::symlink(file, &link).unwrap();
        assert!(matches!(
            read_regular_file(&link),
            Err(ClipboardError::Io(_))
        ));
        // Path-level policy still skips links instead of following their target.
        assert!(load_file_refs(&[link]).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_loader_rejects_files_over_the_existing_protocol_limit() {
        let root = Scratch::new();
        let file = root.0.join("oversized-sparse-file");
        std::fs::File::create(&file)
            .unwrap()
            .set_len(clipsync_protocol::FILE_MAX_BYTES + 1)
            .unwrap();
        assert!(
            matches!(read_regular_file(&file), Err(ClipboardError::Message(message)) if message.contains("exceeds"))
        );
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

    #[test]
    fn native_file_loader_preserves_bytes_names_and_detected_mime() {
        let root = Scratch::new();
        let bytes = png();
        let path = root.file("clipboard image.png", &bytes);
        let files = load_file_refs(std::slice::from_ref(&path))
            .unwrap()
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "clipboard image.png");
        assert_eq!(files[0].mime.as_deref(), Some("image/png"));
        assert_eq!(files[0].bytes, bytes);
        assert_eq!(files[0].staged_path.as_ref(), Some(&path));
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
        assert_eq!(info.name, "macos-nspasteboard");
        assert_eq!(info.source, "native");
        let caps = info.capabilities;
        assert!(caps.text && caps.image && caps.files && caps.watch);
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
        assert_eq!(info.fallback_reason.as_deref(), Some("generic fallback"));
        let caps = info.capabilities;
        assert!(caps.text && caps.image && caps.files && caps.watch);
        crate::test_support::assert_text(backend.read().await.unwrap(), "generic");
        assert!(detect_macos_adapter(
            || Err(ClipboardError::Unavailable("native denied".into())),
            || None,
            || Err(ClipboardError::Unavailable("generic denied".into()))
        )
        .is_err());
    }

    #[tokio::test]
    async fn command_fallback_reports_text_only_and_uses_only_fixture_tools() {
        const CHILD: &str = "CLIPSYNC_DETECT_MAC_COMMAND_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let root = crate::test_support::Scratch::new();
            root.executable("pbpaste", "#!/bin/sh\nprintf 'fixture clipboard text'\n");
            root.executable(
                "pbcopy",
                "#!/bin/sh\n/bin/cat > \"$CLIPSYNC_DETECT_WRITTEN\"\n",
            );
            let written = root.0.join("written");
            let output = super::environment_tests::run_child(
                root.child("detect::detector_tests::command_fallback_reports_text_only_and_uses_only_fixture_tools")
                    .env(CHILD, "1")
                    .env("CLIPSYNC_DETECT_WRITTEN", &written),
            );
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(std::fs::read_to_string(written).unwrap(), "fixture write");
            return;
        }
        let (backend, info) = detect_macos_adapter(
            || Err(ClipboardError::Unavailable("native fixture denial".into())),
            || {
                CommandClipboard::macos_tools()
                    .map(|backend| Box::new(backend) as Box<dyn ClipboardBackend>)
            },
            || panic!("the complete command pair must win over generic fallback"),
        )
        .unwrap();
        assert_eq!(info.name, backend.name());
        assert_eq!(info.source, "command");
        assert!(info
            .fallback_reason
            .unwrap()
            .contains("native fixture denial"));
        let caps = info.capabilities;
        assert!(caps.text && !caps.image && !caps.files && caps.watch);
        crate::test_support::assert_text(backend.read().await.unwrap(), "fixture clipboard text");
        backend
            .write(&ClipboardItem::Text {
                text: "fixture write".into(),
            })
            .await
            .unwrap();
    }
}

#[cfg(all(test, unix))]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod environment_tests {
    use super::*;

    pub(super) fn run_child(command: &mut std::process::Command) -> std::process::Output {
        run_child_with_timeout(command, std::time::Duration::from_secs(10))
    }

    pub(crate) fn run_child_with_timeout(
        command: &mut std::process::Command,
        timeout: std::time::Duration,
    ) -> std::process::Output {
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "detector fixture exceeded {} seconds: {} {}",
                    timeout.as_secs(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
        assert!(!tool_exists("powershell"));
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
        root.file("pbpaste", b"not executable");
        root.dir("powershell");
        let out = run_child(
            root.child("detect::environment_tests::doctor_probe_child")
                .env("CLIPSYNC_DOCTOR_FIXTURE", &root.0),
        );
        assert!(
            out.status.success(),
            "doctor fixture failed: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_detection_prioritizes_sessions_tools_and_real_capabilities() {
        struct Case {
            wayland: bool,
            x11: bool,
            tools: &'static [&'static str],
            name: &'static str,
            rich_formats: bool,
        }
        let cases = [
            Case {
                wayland: true,
                x11: true,
                tools: &["wl-copy", "wl-paste", "xclip", "xsel"],
                name: "wayland-wl-clipboard",
                rich_formats: true,
            },
            Case {
                wayland: false,
                x11: true,
                tools: &["xclip", "xsel"],
                name: "x11-xclip",
                rich_formats: true,
            },
            Case {
                wayland: false,
                x11: true,
                tools: &["xsel"],
                name: "x11-xsel",
                rich_formats: false,
            },
            Case {
                wayland: true,
                x11: true,
                tools: &["wl-copy", "xclip"],
                name: "x11-xclip",
                rich_formats: true,
            },
            Case {
                wayland: true,
                x11: false,
                tools: &["wl-copy"],
                name: "headless",
                rich_formats: false,
            },
            Case {
                wayland: true,
                x11: false,
                tools: &["wl-paste"],
                name: "headless",
                rich_formats: false,
            },
            Case {
                wayland: true,
                x11: true,
                tools: &[],
                name: "headless",
                rich_formats: false,
            },
            Case {
                wayland: false,
                x11: false,
                tools: &["wl-copy", "wl-paste", "xclip", "xsel"],
                name: "headless",
                rich_formats: false,
            },
        ];
        const CHILD: &str = "CLIPSYNC_DETECT_LINUX_CASE";
        let Ok(index) = std::env::var(CHILD) else {
            for (index, case) in cases.iter().enumerate() {
                let root = crate::test_support::Scratch::new();
                for name in case.tools {
                    root.executable(name, "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> \"$CLIPSYNC_DETECT_LOG\"\ncase \"$*\" in *image/png*|*text/uri-list*) exit 1;; esac\nprintf 'selected fixture text'\n");
                }
                let log = root.0.join("tool-calls");
                let mut command = root.child("detect::environment_tests::linux_detection_prioritizes_sessions_tools_and_real_capabilities");
                command
                    .env(CHILD, index.to_string())
                    .env("CLIPSYNC_DETECT_LOG", &log);
                if case.wayland {
                    command.env("WAYLAND_DISPLAY", "clipsync-fixture-wayland");
                }
                if case.x11 {
                    command.env("DISPLAY", "clipsync-fixture:999");
                }
                let output = run_child(&mut command);
                assert!(
                    output.status.success(),
                    "case {index}: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                if case.name == "headless" {
                    assert!(
                        !log.exists(),
                        "headless detection must not invoke clipboard tools"
                    );
                } else {
                    let calls = std::fs::read_to_string(log).unwrap();
                    let executable = match case.name {
                        "wayland-wl-clipboard" => "wl-paste",
                        "x11-xsel" => "xsel",
                        _ => "xclip",
                    };
                    assert!(calls
                        .lines()
                        .all(|line| line.starts_with(root.0.join(executable).to_str().unwrap())));
                }
            }
            return;
        };
        let case = &cases[index.parse::<usize>().unwrap()];
        let (backend, info) = detect_adapter().unwrap();
        assert_eq!(backend.name(), case.name);
        assert_eq!(
            info.name, case.name,
            "adapter metadata must identify the selected executable"
        );
        assert_eq!(
            info.source,
            if case.name == "headless" {
                "stdin-stdout"
            } else {
                "command"
            }
        );
        let caps = info.capabilities;
        assert_eq!(caps.text, case.name != "headless");
        assert_eq!(
            (caps.image, caps.files, caps.watch),
            (
                case.rich_formats,
                case.rich_formats,
                case.name != "headless"
            )
        );
        let backend_caps = backend.capabilities();
        assert_eq!(
            (caps.text, caps.image, caps.files, caps.watch),
            (
                backend_caps.text,
                backend_caps.image,
                backend_caps.files,
                backend_caps.watch
            )
        );
        if case.name == "headless" {
            assert_eq!(
                info.fallback_reason.as_deref(),
                Some("no graphical clipboard adapter")
            );
            assert!(backend.read().await.unwrap().is_none());
            assert!(matches!(
                backend
                    .write(&ClipboardItem::Text {
                        text: "no graphical destination".into()
                    })
                    .await,
                Err(crate::ClipboardError::Unsupported("text"))
            ));
        } else {
            assert!(info.fallback_reason.is_none());
            crate::test_support::assert_text(
                backend.read().await.unwrap(),
                "selected fixture text",
            );
        }
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
