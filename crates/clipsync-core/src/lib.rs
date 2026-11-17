// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! ClipSync client: pairing, sync engine, loop prevention, and command facade.

mod engine;
mod error;
mod loop_guard;
mod notify;
mod pairing;
mod untrusted_text;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/support/relay_script.rs"]
mod test_relay;

/// Number of digits in a verification code (SAS).
pub use clipsync_crypto::SAS_DIGITS;
pub use engine::{
    oneshot_pull as engine_oneshot_pull, oneshot_push as engine_oneshot_push, run_daemon,
    SyncHandle,
};
pub use error::{CoreError, ExitCode};
pub use pairing::{
    begin_create_room, create_room, display_sas, join_room, leave_room, normalize_sas, parse_sas,
    PendingCreate, RoomOffer, SasCheck, SasDecision,
};
pub use untrusted_text::{sanitize_untrusted, MAX_PEER_NAME_CHARS, MAX_RELAY_MESSAGE_CHARS};

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use clipsync_clipboard::{ClipboardItem, FileRef, ImageMime, SystemClipboard};
use clipsync_crypto::DeviceIdentity;
use clipsync_daemon::{
    connect_ipc, install_and_start, is_service_installed, stop_and_uninstall, IpcRequest,
};
use clipsync_protocol::{clamp_ttl, DeviceId, DEFAULT_TTL_SECS};
use clipsync_storage::{LocalStore, PairingWait};
use tracing::info;

pub use clipsync_storage::AppPaths;

#[derive(Clone)]
pub struct App {
    pub store: LocalStore,
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
impl App {
    pub fn open() -> Result<Self, CoreError> {
        Ok(Self {
            store: LocalStore::open()?,
        })
    }

    pub fn open_from(paths: AppPaths) -> Result<Self, CoreError> {
        Ok(Self {
            store: LocalStore::open_from(paths)?,
        })
    }

    pub fn init(&self, relay_url: Option<String>) -> Result<DeviceIdentity, CoreError> {
        // Absence is handled by load_config; errors must not reset user data.
        let mut cfg = self.store.load_config()?;
        let identity = match self.store.secrets.load_identity() {
            Ok(id) => id,
            Err(_) => {
                let identity = DeviceIdentity::generate();
                self.store.secrets.save_identity(&identity)?;
                identity
            }
        };
        if let Some(url) = relay_url {
            cfg.relay_url = url;
        }
        self.store.save_config(&cfg)?;
        Ok(identity)
    }

    pub fn identity(&self) -> Result<DeviceIdentity, CoreError> {
        self.init(None)
    }

    pub async fn room_create<C, F>(
        &self,
        ttl: Option<std::time::Duration>,
        no_auto_sync: bool,
        json: bool,
        print_code: impl Fn(&RoomOffer),
        confirm: C,
    ) -> Result<RoomOffer, CoreError>
    where
        C: FnOnce(SasCheck) -> F + Send,
        F: std::future::Future<Output = SasDecision> + Send,
    {
        let identity = self.identity()?;
        let cfg = self.store.load_config()?;
        let ttl_secs = ttl
            .map(|d| clamp_ttl(d.as_secs()))
            .unwrap_or(DEFAULT_TTL_SECS);
        let pending = begin_create_room(&identity, &cfg, ttl_secs).await?;
        let offer = RoomOffer {
            pairing_code: pending.offer.pairing_code.clone(),
            room_id: pending.offer.room_id.clone(),
            expires_at: pending.offer.expires_at,
        };
        let wait_file = PairingWait {
            status: "waiting".into(),
            pairing_code: offer.pairing_code.clone(),
            room_id: offer.room_id.to_string(),
            expires_at: offer.expires_at.to_rfc3339(),
            pid: std::process::id(),
            error: None,
            sas: None,
            peer_device_name: None,
            sas_decision: None,
        };
        let _ = self.store.save_pairing_wait(&wait_file);
        print_code(&pending.offer);
        // A rejected background pairing may be replaced by a newer `room
        // create` while this one winds down; never overwrite that file.
        let finish = |result: PairingWait| {
            let _ = self.store.update_pairing_wait(|current| {
                let replaced = matches!(
                    current,
                    Some(current)
                        if current.pid != result.pid || current.room_id != result.room_id
                );
                ((!replaced).then_some(result), ())
            });
        };
        match pending.wait(&self.store, &identity, confirm).await {
            Ok(()) => {
                finish(PairingWait {
                    status: "paired".into(),
                    ..wait_file
                });
                if !no_auto_sync {
                    maybe_start_daemon(&self.store)?;
                }
                let _ = json;
                Ok(offer)
            }
            Err(e) => {
                finish(PairingWait {
                    status: "failed".into(),
                    error: Some(e.to_string()),
                    ..wait_file
                });
                Err(e)
            }
        }
    }

    pub async fn room_join<C, F>(
        &self,
        code: &str,
        no_auto_sync: bool,
        confirm: C,
    ) -> Result<clipsync_storage::RoomRecord, CoreError>
    where
        C: FnOnce(SasCheck) -> F + Send,
        F: std::future::Future<Output = SasDecision> + Send,
    {
        let identity = self.identity()?;
        let cfg = self.store.load_config()?;
        let room = join_room(&self.store, &identity, &cfg, code, confirm).await?;
        if !no_auto_sync {
            maybe_start_daemon(&self.store)?;
        }
        Ok(room)
    }

    pub fn room_leave(&self) -> Result<(), CoreError> {
        let _ = stop_and_uninstall();
        leave_room(&self.store)?;
        self.store.clear_pairing_wait();
        Ok(())
    }

    pub fn room_list(&self) -> Result<Vec<clipsync_storage::RoomRecord>, CoreError> {
        Ok(self.store.load_state()?.rooms)
    }

    pub async fn push(
        &self,
        r#type: Option<&str>,
        files: &[PathBuf],
        copy_from_clipboard: bool,
    ) -> Result<String, CoreError> {
        let item = load_push_item(r#type, files, copy_from_clipboard).await?;
        item.validate()
            .map_err(|e| CoreError::Clipboard(e.to_string()))?;
        if let Ok(mut ipc) = connect_ipc(&self.store.paths).await {
            if let ClipboardItem::Text { text } = &item {
                let resp = ipc
                    .request(&IpcRequest::PushText { text: text.clone() })
                    .await?;
                if !resp.ok {
                    return Err(CoreError::Message(
                        resp.error.unwrap_or_else(|| "push failed".into()),
                    ));
                }
                return Ok(item.summary());
            }
        }
        engine::oneshot_push(&self.store, item).await?;
        Ok("sent".into())
    }

    pub async fn pull(
        &self,
        copy: bool,
        wait: bool,
        output: Option<PathBuf>,
    ) -> Result<PullResult, CoreError> {
        if let Ok(mut ipc) = connect_ipc(&self.store.paths).await {
            let resp = ipc.request(&IpcRequest::Pull).await?;
            if let Some(p) = resp.pull {
                if copy {
                    if let Some(text) = &p.text {
                        if let Ok(c) = SystemClipboard::open() {
                            let _ = c.write(&ClipboardItem::Text { text: text.clone() }).await;
                        }
                    }
                }
                return Ok(PullResult {
                    kind: p.kind,
                    text: p.text,
                    summary: p.summary,
                    files: Vec::new(),
                });
            }
        }
        engine::oneshot_pull(&self.store, copy, wait, output).await
    }

    pub async fn status(&self) -> Result<serde_json::Value, CoreError> {
        if let Ok(mut ipc) = connect_ipc(&self.store.paths).await {
            let resp = ipc.request(&IpcRequest::Status).await?;
            if let Some(s) = resp.status {
                return Ok(serde_json::to_value(s)?);
            }
        }
        let cfg = self.store.load_config()?;
        let state = self.store.load_state()?;
        let identity = self.store.secrets.load_identity().ok();
        let mut v = serde_json::json!({
            "daemon": false,
            "paused": state.paused,
            "connected": false,
            "peer_online": false,
            "room_id": state.current_room,
            "device_id": identity.as_ref().map(|i| i.device_id.clone()),
            "relay_url": cfg.relay_url,
        });
        if let Ok(Some(pairing)) = self.store.load_pairing_wait() {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("pairing".into(), serde_json::to_value(pairing)?);
            }
        }
        Ok(v)
    }

    pub async fn pause(&self) -> Result<(), CoreError> {
        let mut state = self.store.load_state()?;
        state.paused = true;
        self.store.save_state(&state)?;
        if let Ok(mut ipc) = connect_ipc(&self.store.paths).await {
            let _ = ipc.request(&IpcRequest::Pause).await;
        }
        Ok(())
    }

    pub async fn resume(&self) -> Result<(), CoreError> {
        let mut state = self.store.load_state()?;
        state.paused = false;
        self.store.save_state(&state)?;
        if let Ok(mut ipc) = connect_ipc(&self.store.paths).await {
            let _ = ipc.request(&IpcRequest::Resume).await;
        } else {
            maybe_start_daemon(&self.store)?;
        }
        Ok(())
    }

    pub fn devices(&self) -> Result<Vec<clipsync_storage::PeerRecord>, CoreError> {
        Ok(self.store.current_room()?.peers)
    }

    pub fn config_get(&self, key: Option<&str>) -> Result<serde_json::Value, CoreError> {
        let cfg = self.store.load_config()?;
        if let Some(k) = key {
            return Ok(serde_json::json!({ k: cfg.get(k) }));
        }
        Ok(serde_json::to_value(cfg)?)
    }

    pub fn config_set(&self, key: &str, value: &str) -> Result<(), CoreError> {
        let mut cfg = self.store.load_config()?;
        if !cfg.set(key, value) {
            return Err(CoreError::Message(format!("unknown config key {key}")));
        }
        self.store.save_config(&cfg)?;
        Ok(())
    }

    pub async fn doctor(&self) -> Result<serde_json::Value, CoreError> {
        let clip = SystemClipboard::open().ok();
        let info = clip.as_ref().map(|c| c.info().clone());
        let identity = self.store.secrets.load_identity().ok();
        let room = self.store.current_room().ok();
        let cfg = self.store.load_config().unwrap_or_default();
        let relay = check_relay(&cfg.relay_url).await;
        Ok(serde_json::json!({
            "identity": identity.is_some(),
            "device_id": identity.as_ref().map(|i| i.device_id.clone()),
            "fingerprint": identity.as_ref().map(|i| i.fingerprint()),
            "room": room.as_ref().map(|r| r.room_id.clone()),
            "clipboard": info.as_ref().map(|i| serde_json::json!({
                "name": i.name,
                "source": i.source,
                "text": i.capabilities.text,
                "image": i.capabilities.image,
                "files": i.capabilities.files,
                "fallback": i.fallback_reason,
            })),
            "tools": clipsync_clipboard::collect_doctor_tools(),
            "relay_url": cfg.relay_url,
            "relay": relay,
            "daemon_socket": self.store.paths.socket_file().exists(),
            "login_service": is_service_installed(),
            "tls": {
                "verifier": "platform",
                "ca_file": clipsync_transport::ca_file_path(),
            },
            "paths": {
                "config_dir": self.store.paths.config_dir.display().to_string(),
                "data_dir": self.store.paths.data_dir.display().to_string(),
                "log_file": self.store.paths.log_file().display().to_string(),
            },
        }))
    }
}

#[derive(Debug)]
pub struct PullResult {
    pub kind: String,
    pub text: Option<String>,
    pub summary: String,
    pub files: Vec<PathBuf>,
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub fn maybe_start_daemon(store: &LocalStore) -> Result<(), CoreError> {
    if std::env::var("CLIPSYNC_NO_DAEMON").ok().as_deref() == Some("1") {
        return Ok(());
    }
    install_and_start(&store.paths)?;
    info!("daemon start requested");
    Ok(())
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn load_push_item(
    r#type: Option<&str>,
    files: &[PathBuf],
    from_clipboard: bool,
) -> Result<ClipboardItem, CoreError> {
    if !files.is_empty() {
        let mut out = Vec::new();
        for p in files {
            let meta = std::fs::symlink_metadata(p)?;
            if meta.file_type().is_symlink() || meta.is_dir() {
                return Err(CoreError::Message(
                    "directory and symlink paths are rejected".into(),
                ));
            }
            let bytes = std::fs::read(p)?;
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| CoreError::Message("invalid file name".into()))?
                .to_string();
            out.push(FileRef {
                mime: infer::get(&bytes).map(|k| k.mime_type().to_string()),
                name,
                bytes,
                staged_path: Some(p.clone()),
            });
        }
        return Ok(ClipboardItem::Files { files: out });
    }
    let mut stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    load_push_input(
        r#type,
        from_clipboard,
        &mut stdin,
        terminal,
        SystemClipboard::open,
    )
    .await
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn load_push_input(
    r#type: Option<&str>,
    from_clipboard: bool,
    stdin: &mut impl Read,
    terminal: bool,
    open_clipboard: impl FnOnce() -> clipsync_clipboard::Result<SystemClipboard>,
) -> Result<ClipboardItem, CoreError> {
    if !terminal {
        let mut buf = Vec::new();
        stdin.read_to_end(&mut buf)?;
        match r#type {
            Some("image") => {
                let mime = ImageMime::detect(&buf)
                    .ok_or_else(|| CoreError::Message("stdin is not a supported image".into()))?;
                return Ok(ClipboardItem::Image {
                    mime,
                    bytes: buf,
                    width: None,
                    height: None,
                });
            }
            _ => {
                let text = String::from_utf8(buf)
                    .map_err(|_| CoreError::Message("stdin is not valid UTF-8".into()))?;
                return Ok(ClipboardItem::Text { text });
            }
        }
    }
    if from_clipboard || r#type.is_some() {
        let clip = open_clipboard()?;
        let item = clip
            .read()
            .await?
            .ok_or_else(|| CoreError::Message("clipboard is empty".into()))?;
        return Ok(item);
    }
    Err(CoreError::Message(
        "nothing to push: pipe stdin, pass --file, or copy to the clipboard".into(),
    ))
}

pub(crate) fn http_client() -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .user_agent(concat!("clipsync-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20));
    let builder = clipsync_transport::configure_reqwest(builder).expect("tls config");
    builder.build().expect("http client")
}

async fn check_relay(url: &str) -> serde_json::Value {
    let health = format!("{}/healthz", url.trim_end_matches('/'));
    match http_client().get(&health).send().await {
        Ok(r) => serde_json::json!({"ok": r.status().is_success(), "status": r.status().as_u16()}),
        Err(e) => serde_json::json!({"ok": false, "error": crate::error::format_error(&e)}),
    }
}

pub fn current_os() -> &'static str {
    static OS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OS.get_or_init(|| {
        if let Ok(os) = std::env::var("CLIPSYNC_OS") {
            let os = os.to_ascii_lowercase();
            if matches!(
                os.as_str(),
                "ios" | "android" | "macos" | "linux" | "windows"
            ) {
                return os;
            }
        }
        match std::env::consts::OS {
            os @ ("macos" | "linux" | "windows" | "ios" | "android") => os.into(),
            _ => "unknown".into(),
        }
    })
    .as_str()
}

pub fn device_name() -> String {
    if let Ok(name) = std::env::var("CLIPSYNC_DEVICE_NAME") {
        if !name.trim().is_empty() {
            return name;
        }
    }
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "clipsync-device".into())
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub fn device_id_or_new(store: &LocalStore) -> Result<DeviceId, CoreError> {
    Ok(store.secrets.load_identity()?.device_id.clone())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod regression_tests {
    #[test]
    fn http_requests_identify_the_current_package_version() {
        const CHILD: &str = "CLIPSYNC_TEST_USER_AGENT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "regression_tests::http_requests_identify_the_current_package_version",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("NO_PROXY", "127.0.0.1")
                .env("no_proxy", "127.0.0.1");
            for name in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                child.env_remove(name);
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "HTTP fixture failed: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let mut buffer = [0; 1024];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0);
                    request.extend_from_slice(&buffer[..count]);
                    assert!(request.len() < 32768);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(
                    request.contains(&format!(
                        "\r\nuser-agent: clipsync-cli/{}\r\n",
                        env!("CARGO_PKG_VERSION")
                    )),
                    "wrong public client version: {request}"
                );
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            });
            assert!(super::http_client()
                .get(format!("http://{address}/healthz"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success());
            tokio::time::timeout(std::time::Duration::from_secs(30), server)
                .await
                .unwrap()
                .unwrap();
        });
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/support/facade_behavior.rs"]
mod facade_behavior;
