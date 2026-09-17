//! Native mobile bindings. The OS clipboard stays in Swift/Kotlin.

uniffi::setup_scaffolding!();

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use clipsync_clipboard::{
    hash_item, AdapterInfo, Capabilities, ClipboardBackend, ClipboardItem, ImageMime,
    SystemClipboard,
};
use clipsync_core::{begin_create_room, join_room, run_daemon, App};
use clipsync_storage::AppPaths;
use tokio::runtime::Runtime;
use tracing::warn;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    #[error("{0}")]
    Message(String),
}

impl From<clipsync_core::CoreError> for MobileError {
    fn from(value: clipsync_core::CoreError) -> Self {
        Self::Message(value.to_string())
    }
}

impl From<clipsync_storage::StorageError> for MobileError {
    fn from(value: clipsync_storage::StorageError) -> Self {
        Self::Message(value.to_string())
    }
}

#[uniffi::export(callback_interface)]
pub trait ClipboardBridge: Send + Sync {
    fn read_text(&self) -> Option<String>;
    fn write_text(&self, text: String);
    fn read_image_png(&self) -> Option<Vec<u8>>;
    fn write_image_png(&self, png: Vec<u8>);
    fn notify(&self, title: String, body: String);
}

#[derive(uniffi::Record)]
pub struct PairingOffer {
    pub pairing_code: String,
    pub room_id: String,
    pub expires_at: String,
}

#[derive(uniffi::Record)]
pub struct MobileStatus {
    pub device_id: String,
    pub fingerprint: String,
    pub room_id: Option<String>,
    pub relay_url: String,
    pub os: String,
}

struct HostClipboard {
    inner: Arc<dyn ClipboardBridge>,
}

#[async_trait]
impl ClipboardBackend for HostClipboard {
    fn name(&self) -> &'static str {
        "host"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            text: true,
            image: true,
            files: false,
            watch: true,
        }
    }

    async fn read(&self) -> clipsync_clipboard::Result<Option<ClipboardItem>> {
        if let Some(png) = self.inner.read_image_png() {
            if png.len() > 16 {
                return Ok(Some(ClipboardItem::Image {
                    mime: ImageMime::Png,
                    bytes: png,
                    width: None,
                    height: None,
                }));
            }
        }
        if let Some(text) = self.inner.read_text() {
            if !text.is_empty() {
                return Ok(Some(ClipboardItem::Text { text }));
            }
        }
        Ok(None)
    }

    async fn write(&self, item: &ClipboardItem) -> clipsync_clipboard::Result<()> {
        match item {
            ClipboardItem::Text { text } => self.inner.write_text(text.clone()),
            ClipboardItem::Image { bytes, .. } => self.inner.write_image_png(bytes.clone()),
            ClipboardItem::Files { files } => {
                if let Some(first) = files.first() {
                    if first
                        .mime
                        .as_deref()
                        .is_some_and(|m| m.starts_with("image/"))
                    {
                        self.inner.write_image_png(first.bytes.clone());
                    } else {
                        self.inner
                            .notify("ClipSync".into(), format!("received file {}", first.name));
                    }
                }
            }
        }
        Ok(())
    }

    async fn signature(&self) -> clipsync_clipboard::Result<String> {
        match self.read().await? {
            Some(item) => Ok(format!("{:x}", hash_item(&item))),
            None => Ok("empty".into()),
        }
    }
}

#[derive(uniffi::Object)]
pub struct ClipSyncClient {
    app: App,
    rt: Runtime,
    syncing: Mutex<bool>,
}

#[uniffi::export]
impl ClipSyncClient {
    #[uniffi::constructor]
    pub fn new(
        data_dir: String,
        relay_url: String,
        os: String,
        device_name: String,
    ) -> Result<Arc<Self>, MobileError> {
        std::env::set_var("CLIPSYNC_HOME", &data_dir);
        if !os.is_empty() {
            std::env::set_var("CLIPSYNC_OS", &os);
        }
        if !device_name.is_empty() {
            std::env::set_var("CLIPSYNC_DEVICE_NAME", &device_name);
        }
        let paths = AppPaths::from_root(data_dir.into());
        paths
            .ensure()
            .map_err(|e| MobileError::Message(e.to_string()))?;
        let app = App::open_from(paths).map_err(MobileError::from)?;
        let _ = app.init(Some(relay_url));
        let rt = Runtime::new().map_err(|e| MobileError::Message(e.to_string()))?;
        Ok(Arc::new(Self {
            app,
            rt,
            syncing: Mutex::new(false),
        }))
    }

    pub fn status(&self) -> Result<MobileStatus, MobileError> {
        let id = self.app.identity()?;
        let cfg = self.app.store.load_config().map_err(MobileError::from)?;
        let room = self.app.store.current_room().ok();
        Ok(MobileStatus {
            device_id: id.device_id.to_string(),
            fingerprint: id.fingerprint(),
            room_id: room.map(|r| r.room_id.to_string()),
            relay_url: cfg.relay_url,
            os: clipsync_core::current_os().to_string(),
        })
    }

    pub fn create_room(&self) -> Result<PairingOffer, MobileError> {
        let app = self.app.clone();
        self.rt.block_on(async move {
            let identity = app.identity()?;
            let cfg = app.store.load_config()?;
            let pending = begin_create_room(&identity, &cfg, 120).await?;
            let offer = PairingOffer {
                pairing_code: pending.offer.pairing_code.clone(),
                room_id: pending.offer.room_id.to_string(),
                expires_at: pending.offer.expires_at.to_rfc3339(),
            };
            let store = app.store.clone();
            tokio::spawn(async move {
                if let Err(e) = pending.wait(&store, &identity).await {
                    warn!(error = %e, "mobile pairing wait failed");
                }
            });
            Ok(offer)
        })
    }

    pub fn join_room(&self, code: String) -> Result<String, MobileError> {
        let app = self.app.clone();
        self.rt.block_on(async move {
            let identity = app.identity()?;
            let cfg = app.store.load_config()?;
            join_room(&app.store, &identity, &cfg, &code).await?;
            let room = app.store.current_room()?;
            Ok(room.room_id.to_string())
        })
    }

    pub fn start_sync(&self, clipboard: Box<dyn ClipboardBridge>) -> Result<(), MobileError> {
        {
            let mut g = self.syncing.lock().unwrap();
            if *g {
                return Ok(());
            }
            *g = true;
        }
        let clip = SystemClipboard::from_backend(
            Box::new(HostClipboard {
                inner: Arc::from(clipboard),
            }),
            AdapterInfo {
                name: "host",
                source: "native",
                capabilities: Capabilities {
                    text: true,
                    image: true,
                    files: false,
                    watch: true,
                },
                fallback_reason: None,
            },
        );
        let handle = self.rt.handle().clone();
        std::thread::Builder::new()
            .name("clipsync-sync".into())
            .spawn(move || {
                if let Err(e) = handle.block_on(run_daemon(Some(clip))) {
                    warn!(error = %e, "mobile sync loop ended");
                }
            })
            .map_err(|e| MobileError::Message(e.to_string()))?;
        Ok(())
    }
}
