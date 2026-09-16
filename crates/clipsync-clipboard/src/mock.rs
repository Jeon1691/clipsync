use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;

use crate::item::ClipboardItem;
use crate::{Capabilities, ClipboardBackend, Result};

#[derive(Clone, Default)]
pub struct MockClipboard {
    inner: Arc<Mutex<Option<ClipboardItem>>>,
    sig: Arc<Mutex<u64>>,
}

impl MockClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, item: ClipboardItem) {
        *self.inner.lock() = Some(item);
        *self.sig.lock() += 1;
    }

    pub fn get(&self) -> Option<ClipboardItem> {
        self.inner.lock().clone()
    }
}

#[async_trait]
impl ClipboardBackend for MockClipboard {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }

    async fn read(&self) -> Result<Option<ClipboardItem>> {
        Ok(self.inner.lock().clone())
    }

    async fn write(&self, item: &ClipboardItem) -> Result<()> {
        *self.inner.lock() = Some(item.clone());
        *self.sig.lock() += 1;
        Ok(())
    }

    async fn signature(&self) -> Result<String> {
        Ok(self.sig.lock().to_string())
    }
}
