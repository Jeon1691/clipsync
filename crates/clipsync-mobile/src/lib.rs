// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
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
        app.init(Some(relay_url))?;
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use clipsync_clipboard::FileRef;
    use clipsync_crypto::DeviceIdentity;
    use clipsync_storage::AppConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[derive(Default)]
    struct HostState {
        text: Mutex<Option<String>>,
        image: Mutex<Option<Vec<u8>>>,
        texts: Mutex<Vec<String>>,
        images: Mutex<Vec<Vec<u8>>>,
        notifications: Mutex<Vec<(String, String)>>,
        drops: AtomicUsize,
    }
    struct Bridge(Arc<HostState>);
    impl Drop for Bridge {
        fn drop(&mut self) {
            self.0.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl ClipboardBridge for Bridge {
        fn read_text(&self) -> Option<String> {
            self.0.text.lock().unwrap().clone()
        }
        fn write_text(&self, text: String) {
            self.0.texts.lock().unwrap().push(text);
        }
        fn read_image_png(&self) -> Option<Vec<u8>> {
            self.0.image.lock().unwrap().clone()
        }
        fn write_image_png(&self, png: Vec<u8>) {
            self.0.images.lock().unwrap().push(png);
        }
        fn notify(&self, title: String, body: String) {
            self.0.notifications.lock().unwrap().push((title, body));
        }
    }
    fn host(state: &Arc<HostState>) -> HostClipboard {
        HostClipboard {
            inner: Arc::new(Bridge(state.clone())),
        }
    }

    #[tokio::test]
    async fn host_reads_prefer_images_and_ignore_empty_or_incomplete_content() {
        let state = Arc::new(HostState::default());
        let clipboard = host(&state);
        assert_eq!(clipboard.name(), "host");
        let caps = clipboard.capabilities();
        assert!(caps.text && caps.image && caps.watch && !caps.files);
        assert_eq!(clipboard.read().await.unwrap(), None);
        assert_eq!(clipboard.signature().await.unwrap(), "empty");
        *state.text.lock().unwrap() = Some(String::new());
        assert_eq!(clipboard.read().await.unwrap(), None);
        *state.text.lock().unwrap() = Some("native text\n한글".into());
        assert_eq!(
            clipboard.read().await.unwrap(),
            Some(ClipboardItem::Text {
                text: "native text\n한글".into()
            })
        );
        let text_signature = clipboard.signature().await.unwrap();
        assert_ne!(text_signature, "empty");
        assert_eq!(clipboard.signature().await.unwrap(), text_signature);
        *state.image.lock().unwrap() = Some(vec![1; 16]);
        assert!(matches!(
            clipboard.read().await.unwrap(),
            Some(ClipboardItem::Text { .. })
        ));
        let png = b"\x89PNG\r\n\x1a\nsynthetic native image".to_vec();
        *state.image.lock().unwrap() = Some(png.clone());
        assert_eq!(
            clipboard.read().await.unwrap(),
            Some(ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: png,
                width: None,
                height: None
            })
        );
        assert_ne!(clipboard.signature().await.unwrap(), text_signature);
    }

    #[tokio::test]
    async fn host_writes_forward_text_images_and_file_notifications_to_native_callbacks() {
        let state = Arc::new(HostState::default());
        let clipboard = host(&state);
        clipboard
            .write(&ClipboardItem::Text {
                text: "unaltered\n한글".into(),
            })
            .await
            .unwrap();
        clipboard
            .write(&ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: vec![1, 2, 3],
                width: Some(1),
                height: Some(1),
            })
            .await
            .unwrap();
        clipboard
            .write(&ClipboardItem::Files {
                files: vec![FileRef {
                    name: "photo.png".into(),
                    bytes: vec![4, 5, 6],
                    mime: Some("image/png".into()),
                    staged_path: None,
                }],
            })
            .await
            .unwrap();
        clipboard
            .write(&ClipboardItem::Files {
                files: vec![FileRef {
                    name: "report.txt".into(),
                    bytes: b"private body must not enter notification".to_vec(),
                    mime: None,
                    staged_path: None,
                }],
            })
            .await
            .unwrap();
        clipboard
            .write(&ClipboardItem::Files { files: vec![] })
            .await
            .unwrap();
        assert_eq!(*state.texts.lock().unwrap(), ["unaltered\n한글"]);
        assert_eq!(
            *state.images.lock().unwrap(),
            [vec![1, 2, 3], vec![4, 5, 6]]
        );
        assert_eq!(
            *state.notifications.lock().unwrap(),
            [("ClipSync".into(), "received file report.txt".into())]
        );
    }

    #[test]
    fn ffi_records_keep_utf8_optional_fields_and_the_declared_wire_order() {
        let offer = PairingOffer {
            pairing_code: "482913".into(),
            room_id: "방".into(),
            expires_at: "2026-11-15T00:00:00Z".into(),
        };
        let bytes = <PairingOffer as uniffi::Lower<UniFfiTag>>::lower(offer).destroy_into_vec();
        assert_eq!(
            bytes,
            b"\0\0\0\x06482913\0\0\0\x03\xeb\xb0\xa9\0\0\0\x142026-11-15T00:00:00Z"
        );
        let lifted = <PairingOffer as uniffi::Lift<UniFfiTag>>::try_lift(
            uniffi::RustBuffer::from_vec(bytes),
        )
        .unwrap();
        assert_eq!(
            (
                lifted.pairing_code.as_str(),
                lifted.room_id.as_str(),
                lifted.expires_at.as_str()
            ),
            ("482913", "방", "2026-11-15T00:00:00Z")
        );
        for room_id in [None, Some("mobile-room".to_string())] {
            let status = MobileStatus {
                device_id: "device".into(),
                fingerprint: "fingerprint".into(),
                room_id: room_id.clone(),
                relay_url: "https://relay.example/".into(),
                os: "ios".into(),
            };
            let buffer = <MobileStatus as uniffi::Lower<UniFfiTag>>::lower(status);
            let lifted = <MobileStatus as uniffi::Lift<UniFfiTag>>::try_lift(buffer).unwrap();
            assert_eq!(lifted.device_id, "device");
            assert_eq!(lifted.fingerprint, "fingerprint");
            assert_eq!(lifted.room_id, room_id);
            assert_eq!(lifted.relay_url, "https://relay.example/");
            assert_eq!(lifted.os, "ios");
        }
        assert!(
            <MobileStatus as uniffi::Lift<UniFfiTag>>::try_lift(uniffi::RustBuffer::from_vec(
                vec![0, 0, 0, 5, b'x']
            ))
            .is_err()
        );
    }

    #[test]
    fn ffi_errors_preserve_core_and_storage_diagnostics() {
        let errors = [
            MobileError::from(clipsync_core::CoreError::Message(
                "relay unavailable".into(),
            )),
            MobileError::from(clipsync_storage::StorageError::NotPaired),
        ];
        for (error, expected) in errors
            .into_iter()
            .zip(["relay unavailable", "not paired with a room"])
        {
            assert_eq!(error.to_string(), expected);
            let buffer = <MobileError as uniffi::Lower<UniFfiTag>>::lower(error);
            let lifted = <MobileError as uniffi::Lift<UniFfiTag>>::try_lift(buffer).unwrap();
            assert_eq!(lifted.to_string(), expected);
        }
    }

    fn seed_home(root: &std::path::Path) {
        let app = App::open_from(AppPaths::from_root(root.to_path_buf())).unwrap();
        let identity = DeviceIdentity::from_bytes("mobile-test-device".into(), [11; 32]);
        std::fs::write(
            app.store.paths.identity_file(),
            serde_json::to_vec(&identity.to_stored()).unwrap(),
        )
        .unwrap();
        app.store
            .save_config(&AppConfig {
                relay_url: "http://127.0.0.1:0".into(),
                ..Default::default()
            })
            .unwrap();
    }

    #[test]
    fn mobile_constructor_and_commands_are_exercised_in_an_isolated_host() {
        for case in ["valid", "bad-root", "bad-config"] {
            let root = tempfile::tempdir().unwrap();
            seed_home(root.path());
            if case == "bad-config" {
                std::fs::write(root.path().join("config/config.toml"), "broken = [").unwrap();
            }
            if case == "bad-root" {
                std::fs::write(root.path().join("not-a-directory"), "file").unwrap();
            }
            let data = if case == "bad-root" {
                root.path().join("not-a-directory")
            } else {
                root.path().to_path_buf()
            };
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::mobile_client_child", "--nocapture"])
                .env("CLIPSYNC_TEST_MOBILE_CASE", case)
                .env("CLIPSYNC_TEST_MOBILE_HOME", data)
                .env_remove("CLIPSYNC_LISTEN")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "case={case}\nstdout={}\nstderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn mobile_client_child() {
        let Ok(case) = std::env::var("CLIPSYNC_TEST_MOBILE_CASE") else {
            return;
        };
        let home = std::env::var("CLIPSYNC_TEST_MOBILE_HOME").unwrap();
        let created = ClipSyncClient::new(
            home,
            "http://127.0.0.1:0".into(),
            "ios".into(),
            "Native test host".into(),
        );
        if case != "valid" {
            assert!(
                created.is_err(),
                "constructor must report initialization/configuration failure"
            );
            return;
        }
        let client = created.unwrap();
        let status = client.status().unwrap();
        assert_eq!(status.device_id, "mobile-test-device");
        assert_eq!(status.relay_url, "http://127.0.0.1:0");
        assert_eq!(status.os, "ios");
        assert!(status.room_id.is_none());
        assert_eq!(status.fingerprint.len(), 64);
        assert!(status.fingerprint.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(client
            .create_room()
            .err()
            .unwrap()
            .to_string()
            .contains("relay create"));
        assert!(client
            .join_room("482913".into())
            .unwrap_err()
            .to_string()
            .contains("relay join"));
        let room = serde_json::from_value(serde_json::json!({
            "room_id":"mobile-room", "epoch":1, "role":"owner",
            "created_at":"2026-11-15T00:00:00Z", "device_token":"synthetic", "peers":[]
        }))
        .unwrap();
        client.app.store.upsert_room(room).unwrap();
        assert_eq!(
            client.status().unwrap().room_id.as_deref(),
            Some("mobile-room")
        );
        let listener = client
            .rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let address = listener.local_addr().unwrap();
        let config = clipsync_relay::RelayConfig {
            bind: address,
            public_url: format!("http://{address}"),
            token_secret: vec![3; 32],
            webhook_url: None,
            webhook_secret: None,
        };
        let server = client.rt.spawn(async move {
            axum::serve(
                listener,
                clipsync_relay::router(config)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let mut cfg = client.app.store.load_config().unwrap();
        cfg.relay_url = format!("http://{address}");
        client.app.store.save_config(&cfg).unwrap();
        let offer = client.create_room().unwrap();
        assert_eq!(offer.pairing_code.len(), 6);
        assert!(offer.pairing_code.bytes().all(|b| b.is_ascii_digit()));
        assert!(!offer.room_id.is_empty());
        let expiry = chrono::DateTime::parse_from_rfc3339(&offer.expires_at)
            .unwrap()
            .timestamp();
        assert!((60..=130).contains(&(expiry - chrono::Utc::now().timestamp())));
        assert!(client
            .join_room("not-a-valid-code".into())
            .unwrap_err()
            .to_string()
            .contains("unknown or expired pairing code"));
        let second_home = tempfile::tempdir().unwrap();
        seed_home(second_home.path());
        let second_identity = DeviceIdentity::from_bytes("mobile-joining-device".into(), [12; 32]);
        let second_paths = AppPaths::from_root(second_home.path().into());
        std::fs::write(
            second_paths.identity_file(),
            serde_json::to_vec(&second_identity.to_stored()).unwrap(),
        )
        .unwrap();
        let joiner = ClipSyncClient::new(
            second_home.path().display().to_string(),
            format!("http://{address}"),
            String::new(),
            String::new(),
        )
        .unwrap();
        let joined = joiner.join_room(offer.pairing_code).unwrap();
        assert_eq!(joined, offer.room_id);
        assert_eq!(
            joiner.status().unwrap().room_id.as_deref(),
            Some(joined.as_str())
        );
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while client.app.store.current_room().unwrap().room_id.as_str() != joined {
            assert!(
                std::time::Instant::now() < until,
                "creator did not commit completed pairing"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            client
                .app
                .store
                .load_epoch_key(&joined.clone().into())
                .unwrap(),
            joiner.app.store.load_epoch_key(&joined.into()).unwrap()
        );
        std::env::set_var(
            "CLIPSYNC_HOME",
            client.app.store.paths.data_dir.parent().unwrap(),
        );
        server.abort();

        // A malformed configuration makes daemon initialization fail before
        // clipboard access or IPC binding. Observe adapter lifetime rather than
        // starting a persistent sync service in the test process.
        std::fs::write(client.app.store.paths.config_file(), "broken = [").unwrap();
        assert!(client.status().is_err());
        let state = Arc::new(HostState::default());
        client.start_sync(Box::new(Bridge(state.clone()))).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while state.drops.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let duplicate = Arc::new(HostState::default());
        client
            .start_sync(Box::new(Bridge(duplicate.clone())))
            .unwrap();
        assert_eq!(duplicate.drops.load(Ordering::SeqCst), 1);
    }
}
