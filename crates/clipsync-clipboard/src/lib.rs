//! OS clipboard adapters with capability detection and fallbacks.

mod command;
mod detect;
mod item;
mod mock;
mod stage;
mod watch;

#[cfg(target_os = "macos")]
mod macos;

use async_trait::async_trait;

pub use detect::{collect_doctor_tools, detect_adapter, hash_item, AdapterInfo, Capabilities};
pub use item::{ClipboardItem, FileRef, ImageMime};
pub use mock::MockClipboard;
pub use watch::wait_for_change;

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("{0}")]
    Message(String),
    #[error("clipboard backend unavailable: {0}")]
    Unavailable(String),
    #[error("unsupported item type: {0}")]
    Unsupported(&'static str),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ClipboardError>;

#[async_trait]
pub trait ClipboardBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    async fn read(&self) -> Result<Option<ClipboardItem>>;
    async fn write(&self, item: &ClipboardItem) -> Result<()>;
    async fn signature(&self) -> Result<String>;
}

pub struct SystemClipboard {
    inner: Box<dyn ClipboardBackend>,
    info: AdapterInfo,
}

impl SystemClipboard {
    pub fn open() -> Result<Self> {
        let (inner, info) = detect_adapter()?;
        Ok(Self { inner, info })
    }

    pub fn mock(clip: MockClipboard) -> Self {
        Self {
            inner: Box::new(clip),
            info: AdapterInfo {
                name: "mock",
                source: "injected",
                capabilities: Capabilities::all(),
                fallback_reason: None,
            },
        }
    }

    pub fn from_backend(inner: Box<dyn ClipboardBackend>, info: AdapterInfo) -> Self {
        Self { inner, info }
    }

    pub fn info(&self) -> &AdapterInfo {
        &self.info
    }

    pub async fn read(&self) -> Result<Option<ClipboardItem>> {
        self.inner.read().await
    }

    pub async fn write(&self, item: &ClipboardItem) -> Result<()> {
        self.inner.write(item).await
    }

    pub async fn signature(&self) -> Result<String> {
        self.inner.signature().await
    }
}
