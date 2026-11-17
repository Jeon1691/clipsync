// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Public facade contracts with private files, IPC, relay and clipboard commands.

use crate::{App, AppPaths, CoreError};
use clipsync_crypto::DeviceIdentity;
#[cfg(unix)]
use clipsync_protocol::SyncDirection;
use clipsync_protocol::{DeviceRole, RoomId};
use clipsync_storage::{AppConfig, RoomRecord};
use std::path::Path;
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Scrape credential for the facade relay; the readiness fence reads `/metrics`.
#[cfg(unix)]
const FACADE_METRICS_TOKEN: &str = "facade-metrics-scrape-token-0123456789";

const WORKER: &str = "CLIPSYNC_FACADE_WORKER";

fn child_case(name: &str) -> bool {
    std::env::var(WORKER).as_deref() == Ok(name)
}

fn worker(name: &str, relay: Option<&str>, mode: &str) {
    let directory = tempfile::Builder::new()
        .prefix("cs-facade-")
        .tempdir()
        .unwrap();
    let home = directory.path();
    let tools = home.join("tools");
    std::fs::create_dir(&tools).unwrap();
    #[cfg(unix)]
    {
        executable(
            &tools.join("systemctl"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CLIPSYNC_FACADE_SERVICE_CALLS\"\nexit 17\n",
        );
        executable(
            &tools.join("launchctl"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CLIPSYNC_FACADE_SERVICE_CALLS\"\nexit 17\n",
        );
    }
    #[cfg(target_os = "linux")]
    executable(&tools.join("xsel"), "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CLIPSYNC_FACADE_CLIP_CALLS\"\ncase \"$*\" in\n  *--input*) /bin/cat > \"$CLIPSYNC_FACADE_COPIED\"; test \"$CLIPSYNC_FACADE_COPY_MODE\" != fail;;\n  *--output*) printf 'private clipboard seed';;\n  *) exit 23;;\nesac\n");
    let mut command = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CLIPSYNC_") {
            command.env_remove(key);
        }
    }
    command
        .args(["--exact", name, "--nocapture"])
        .env(WORKER, name)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("XDG_RUNTIME_DIR", home.join("xdg-run"))
        .env("CLIPSYNC_HOME", home.join("app"))
        .env("CLIPSYNC_DEVICE_NAME", "private facade fixture")
        .env("CLIPSYNC_RELAY_URL", relay.unwrap_or("http://127.0.0.1:0"))
        .env("CLIPSYNC_FACADE_SERVICE_CALLS", home.join("service-calls"))
        .env("CLIPSYNC_FACADE_CLIP_CALLS", home.join("clip-calls"))
        .env("CLIPSYNC_FACADE_COPIED", home.join("copied"))
        .env("CLIPSYNC_FACADE_COPY_MODE", mode)
        .env("PATH", tools)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.env("CLIPSYNC_LISTEN", home.join("ipc.sock"));
    #[cfg(windows)]
    command.env(
        "CLIPSYNC_LISTEN",
        format!(r"\\.\pipe\facade-{}", clipsync_protocol::DeviceId::new()),
    );
    #[cfg(target_os = "linux")]
    if mode != "headless" {
        command.env("DISPLAY", "private-command-fixture");
    }
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(key);
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let completed = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert!(
        completed && output.status.success(),
        "private facade child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn app_at(root: &Path, relay: &str, name: &str, secret: u8) -> (App, DeviceIdentity) {
    let app = App::open_from(AppPaths::from_root(root.into())).unwrap();
    let identity = DeviceIdentity::from_bytes(name.into(), [secret; 32]);
    std::fs::write(
        app.store.paths.identity_file(),
        serde_json::to_vec(&identity.to_stored()).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            app.store.paths.identity_file(),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let mut config = AppConfig {
        relay_url: relay.into(),
        ..Default::default()
    };
    config.sync.enabled = false;
    config.sync.auto_start = false;
    config.notify.enabled = false;
    app.store.save_config(&config).unwrap();
    (app, identity)
}

fn private_backend() {
    // This changes only this filtered child process. No OS credential entry is
    // constructed before selecting the library's memory-only test backend.
    keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
}

#[test]
fn readonly_status_reports_corrupt_identity_and_missing_room_credentials_without_repair() {
    const NAME: &str = "facade_behavior::readonly_status_reports_corrupt_identity_and_missing_room_credentials_without_repair";
    if !child_case(NAME) {
        worker(NAME, None, "success");
        return;
    }
    private_backend();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = std::env::var_os("CLIPSYNC_HOME").unwrap();
        let (app, _) = app_at(Path::new(&root), "http://127.0.0.1:0", "status-fixture", 64);
        let room = RoomRecord {
            room_id: RoomId::new(),
            epoch: 1,
            role: DeviceRole::Owner,
            created_at: chrono::Utc::now(),
            device_token: "synthetic-device-token".into(),
            peers: Vec::new(),
        };
        app.store.upsert_room(room.clone()).unwrap();
        std::fs::write(app.store.paths.identity_file(), b"invalid identity JSON").unwrap();
        let status = app.status().await.unwrap();
        assert_eq!(status["daemon"], false);
        assert_eq!(status["room_id"], room.room_id.to_string());
        assert!(status["device_id"].is_null());
        assert!(matches!(
            crate::device_id_or_new(&app.store),
            Err(CoreError::Storage(_))
        ));
        assert_eq!(
            std::fs::read(app.store.paths.identity_file()).unwrap(),
            b"invalid identity JSON"
        );
        let original_state = std::fs::read(app.store.paths.state_file()).unwrap();
        let token = app.store.paths.data_dir.join(format!(
            "device-token-{}.secret",
            hex::encode(room.room_id.as_str())
        ));
        std::fs::write(&token, b"").unwrap();
        assert!(matches!(app.room_list(), Err(CoreError::Storage(_))));
        assert!(matches!(app.devices(), Err(CoreError::Storage(_))));
        assert!(matches!(app.status().await, Err(CoreError::Storage(_))));
        assert_eq!(
            std::fs::read(app.store.paths.state_file()).unwrap(),
            original_state
        );
        assert_eq!(std::fs::read(token).unwrap(), b"");
    });
}

#[cfg(unix)]
#[test]
fn explicit_pairing_without_auto_sync_returns_matching_rooms_and_installs_no_service() {
    const NAME: &str = "facade_behavior::explicit_pairing_without_auto_sync_returns_matching_rooms_and_installs_no_service";
    if !child_case(NAME) {
        worker(NAME, None, "success");
        return;
    }
    private_backend();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(20), async {
            let config = clipsync_relay::RelayConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                public_url: "http://127.0.0.1".into(),
                token_secret: vec![65; 32],
                webhook_url: None,
                webhook_secret: None,
                metrics_token: Some(
                    clipsync_relay::MetricsToken::new(FACADE_METRICS_TOKEN).unwrap(),
                ),
            };
            let server = crate::test_relay::Server::http(clipsync_relay::router(config)).await;
            let home = std::path::PathBuf::from(std::env::var_os("CLIPSYNC_HOME").unwrap());
            let (creator, a) = app_at(&home.join("creator"), &server.url, "facade-creator", 66);
            let (joiner, b) = app_at(&home.join("joiner"), &server.url, "facade-joiner", 67);
            let (sent, received) = tokio::sync::oneshot::channel();
            let offer_channel = Arc::new(Mutex::new(Some(sent)));
            let store = creator.store.clone();
            let create_app = creator.clone();
            let create = tokio::spawn(async move {
                create_app
                    .room_create(None, true, false, move |offer| {
                        let waiting = store.load_pairing_wait().unwrap().unwrap();
                        assert_eq!(waiting.status, "waiting");
                        assert_eq!(waiting.pairing_code, offer.pairing_code);
                        offer_channel
                            .lock()
                            .unwrap()
                            .take()
                            .unwrap()
                            .send(offer.pairing_code.clone())
                            .unwrap();
                    })
                    .await
            });
            let code = received.await.unwrap();
            // Fence creator registration so this success case exercises the
            // no-auto-sync facade contract independently of early-join ordering.
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let metrics = client
                        .get(format!("{}/metrics", server.url))
                        .bearer_auth(FACADE_METRICS_TOKEN)
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap();
                    if metrics
                        .lines()
                        .any(|line| line == "clipsync_ws_connections 1")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let joined = joiner.room_join(&code, true).await.unwrap();
            let created = create.await.unwrap().unwrap();
            assert_eq!(created.room_id, joined.room_id);
            assert_eq!(
                creator.store.current_room().unwrap().room_id,
                joined.room_id
            );
            assert_eq!(
                creator.store.load_pairing_wait().unwrap().unwrap().status,
                "paired"
            );
            assert_eq!(creator.devices().unwrap()[0].device_id, b.device_id);
            assert_eq!(joiner.devices().unwrap()[0].device_id, a.device_id);
            assert_eq!(
                creator.store.load_epoch_key(&joined.room_id).unwrap(),
                joiner.store.load_epoch_key(&joined.room_id).unwrap()
            );
            for app in [&creator, &joiner] {
                let config = app.store.load_config().unwrap();
                assert!(config.sync.enabled && config.sync.auto_start);
                assert_eq!(config.sync.direction, SyncDirection::Both);
                assert!(!app.store.paths.pid_file().exists());
            }
            assert!(
                !Path::new(&std::env::var_os("CLIPSYNC_FACADE_SERVICE_CALLS").unwrap()).exists(),
                "no-auto-sync must not invoke a service manager"
            );
        })
        .await
        .expect("private facade pairing exceeded its deadline");
    });
}

#[cfg(target_os = "linux")]
#[test]
fn ipc_copy_uses_only_the_selected_private_command_and_keeps_the_payload_on_write_error() {
    const NAME: &str = "facade_behavior::ipc_copy_uses_only_the_selected_private_command_and_keeps_the_payload_on_write_error";
    if !child_case(NAME) {
        worker(NAME, None, "success");
        worker(NAME, None, "fail");
        return;
    }
    private_backend();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = std::env::var_os("CLIPSYNC_HOME").unwrap();
        let (app, _) = app_at(Path::new(&root), "http://127.0.0.1:0", "copy-fixture", 68);
        let server = clipsync_daemon::IpcServer::bind(&app.store.paths)
            .await
            .unwrap();
        let serving = tokio::spawn(async move {
            let mut stream = server.accept().await.unwrap();
            assert!(matches!(
                clipsync_daemon::read_request(&mut stream).await.unwrap(),
                Some(clipsync_daemon::IpcRequest::Pull)
            ));
            clipsync_daemon::write_response(
                &mut stream,
                &clipsync_daemon::IpcResponse {
                    ok: true,
                    error: None,
                    status: None,
                    pull: Some(clipsync_daemon::PullPayload {
                        kind: "text".into(),
                        text: Some("private copied UTF-8: 한글".into()),
                        summary: "from private IPC".into(),
                    }),
                },
            )
            .await
            .unwrap();
        });
        let copied = app.pull(true, false, None).await.unwrap();
        assert_eq!(copied.kind, "text");
        assert_eq!(copied.text.as_deref(), Some("private copied UTF-8: 한글"));
        assert_eq!(copied.summary, "from private IPC");
        assert!(copied.files.is_empty());
        serving.await.unwrap();
        assert_eq!(
            std::fs::read(std::env::var_os("CLIPSYNC_FACADE_COPIED").unwrap()).unwrap(),
            "private copied UTF-8: 한글".as_bytes()
        );
        assert!(
            std::fs::read_to_string(std::env::var_os("CLIPSYNC_FACADE_CLIP_CALLS").unwrap())
                .unwrap()
                .contains("--clipboard --input")
        );
    });
}

#[cfg(target_os = "linux")]
struct HealthFixture {
    url: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
    count: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl HealthFixture {
    fn start() -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let thread_stop = stop.clone();
        let thread_count = count.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("private health fixture: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut buffer = [0; 1024];
                    let read = socket.read(&mut buffer).unwrap();
                    assert_ne!(read, 0);
                    request.extend_from_slice(&buffer[..read]);
                    assert!(request.len() < 16_384);
                }
                assert!(request.starts_with(b"GET /healthz HTTP/1.1\r\n"));
                match thread_count.fetch_add(1, Ordering::Relaxed) {
                    0 => socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{not-json").unwrap(),
                    1 => socket.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{not-json").unwrap(),
                    2 => socket.write_all(b"invalid HTTP response\r\n").unwrap(),
                    count => panic!("unexpected health request {count}"),
                }
            }
        });
        Self {
            url,
            stop,
            count,
            thread: Some(thread),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for HealthFixture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn doctor_preserves_bad_metadata_and_reports_private_clipboard_caps_and_health_failures() {
    const NAME: &str = "facade_behavior::doctor_preserves_bad_metadata_and_reports_private_clipboard_caps_and_health_failures";
    if !child_case(NAME) {
        for mode in ["command", "headless"] {
            let server = HealthFixture::start();
            worker(NAME, Some(&server.url), mode);
            assert_eq!(server.count.load(std::sync::atomic::Ordering::Relaxed), 3);
        }
        return;
    }
    private_backend();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = std::env::var_os("CLIPSYNC_HOME").unwrap();
        let url = std::env::var("CLIPSYNC_RELAY_URL").unwrap();
        let (app, identity) = app_at(Path::new(&root), &url, "doctor-fixture", 69);
        let room = RoomRecord {
            room_id: RoomId::new(),
            epoch: 1,
            role: DeviceRole::Owner,
            created_at: chrono::Utc::now(),
            device_token: "doctor-private-token".into(),
            peers: Vec::new(),
        };
        app.store.upsert_room(room.clone()).unwrap();
        let report = app.doctor().await.unwrap();
        assert_eq!(report["identity"], true);
        assert_eq!(report["device_id"], identity.device_id.to_string());
        assert_eq!(report["fingerprint"], identity.fingerprint());
        assert_eq!(report["room"], room.room_id.to_string());
        assert_eq!(report["relay"]["status"], 200);
        assert_eq!(
            report["relay"]["ok"], true,
            "the doctor checks HTTP reachability without parsing the health body"
        );
        assert_eq!(report["daemon_socket"], false);
        assert_eq!(report["login_service"], false);
        let command = std::env::var("CLIPSYNC_FACADE_COPY_MODE").unwrap() == "command";
        assert_eq!(
            report["clipboard"]["name"],
            if command { "x11-xsel" } else { "headless" }
        );
        assert_eq!(
            report["clipboard"]["source"],
            if command { "command" } else { "stdin-stdout" }
        );
        assert_eq!(report["clipboard"]["text"], command);
        assert_eq!(report["clipboard"]["image"], false);
        assert_eq!(report["clipboard"]["files"], false);
        assert_eq!(report["clipboard"]["fallback"].is_null(), command);
        assert!(
            !Path::new(&std::env::var_os("CLIPSYNC_FACADE_CLIP_CALLS").unwrap()).exists(),
            "doctor must not read or write clipboard contents"
        );
        let invalid_config = b"invalid = [";
        std::fs::write(app.store.paths.config_file(), invalid_config).unwrap();
        std::fs::write(app.store.paths.state_file(), b"invalid room state").unwrap();
        std::fs::write(app.store.paths.identity_file(), b"invalid identity state").unwrap();
        let rejected = app.doctor().await.unwrap();
        assert_eq!(
            rejected["relay_url"], url,
            "config fallback must stay on the explicitly private default relay"
        );
        assert_eq!(rejected["relay"]["status"], 503);
        assert_eq!(rejected["relay"]["ok"], false);
        assert_eq!(rejected["identity"], false);
        assert!(
            rejected["device_id"].is_null()
                && rejected["fingerprint"].is_null()
                && rejected["room"].is_null()
        );
        assert_eq!(
            std::fs::read(app.store.paths.config_file()).unwrap(),
            invalid_config
        );
        assert_eq!(
            std::fs::read(app.store.paths.state_file()).unwrap(),
            b"invalid room state"
        );
        assert_eq!(
            std::fs::read(app.store.paths.identity_file()).unwrap(),
            b"invalid identity state"
        );
        let failed = app.doctor().await.unwrap();
        assert_eq!(failed["relay"]["ok"], false);
        let error = failed["relay"]["error"].as_str().unwrap();
        assert!(error.contains("/healthz") && error.contains("127.0.0.1"));
        assert!(!error.contains("doctor-private-token"));
    });
}
