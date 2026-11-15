// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! OS clipboard adapters with capability detection and fallbacks.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod command;
mod detect;
mod item;
mod mock;
#[cfg(target_os = "macos")]
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod test_support;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod behavior_tests {
    use super::*;

    #[tokio::test]
    async fn injected_memory_clipboard_keeps_shared_state_and_change_signatures() {
        let memory = MockClipboard::new();
        assert!(memory.get().is_none());
        let clipboard = SystemClipboard::mock(memory.clone());
        assert_eq!(clipboard.info().name, "mock");
        assert_eq!(clipboard.info().source, "injected");
        let caps = memory.capabilities();
        assert!(caps.text && caps.image && caps.files && caps.watch);
        assert_eq!(memory.name(), "mock");
        assert!(clipboard.read().await.unwrap().is_none());
        let before = clipboard.signature().await.unwrap();
        memory.set(ClipboardItem::Text {
            text: "first event".into(),
        });
        crate::test_support::assert_text(clipboard.read().await.unwrap(), "first event");
        let first = clipboard.signature().await.unwrap();
        assert_ne!(first, before);
        let image = ClipboardItem::Image {
            mime: ImageMime::Png,
            bytes: vec![1, 2, 3],
            width: Some(1),
            height: Some(1),
        };
        clipboard.write(&image).await.unwrap();
        assert_eq!(memory.get(), Some(image));
        assert_ne!(clipboard.signature().await.unwrap(), first);
        let info = AdapterInfo {
            name: "caller-supplied",
            source: "fixture",
            capabilities: Capabilities::text_only(),
            fallback_reason: Some("headless".into()),
        };
        let delegated = SystemClipboard::from_backend(Box::new(memory.clone()), info);
        assert_eq!(
            delegated.info().fallback_reason.as_deref(),
            Some("headless")
        );
        assert!(!delegated.info().capabilities.image);
        let files = ClipboardItem::Files {
            files: vec![FileRef {
                name: "fixture.txt".into(),
                bytes: b"data".to_vec(),
                mime: None,
                staged_path: None,
            }],
        };
        delegated.write(&files).await.unwrap();
        assert_eq!(delegated.read().await.unwrap(), Some(files));
    }
}
