// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Product behavior exercised through real loopback relay connections and isolated files.

use super::*;
use crate::{App, AppPaths};
use clipsync_clipboard::{ClipboardBackend, MockClipboard};
use clipsync_protocol::{
    CreatePairingRequest, CreatePairingResponse, DeviceRole, JoinPairingRequest,
    JoinPairingResponse, PairingSessionId, RoomId, SyncDirection,
};
use clipsync_storage::{AppConfig, PeerRecord};
use std::net::SocketAddr;
use tempfile::TempDir;

const EPOCH: [u8; 32] = [7; 32];
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0a, 0x49, 0x44, 0x41, 0x54,
    0x78, 0x9c, 0x63, 0, 1, 0, 0, 5, 0, 1, 0x0d, 0x0a, 0x2d, 0xb4, 0, 0, 0, 0, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

struct Peer {
    _directory: TempDir,
    app: App,
    identity: DeviceIdentity,
}

impl Peer {
    fn new(url: &str, name: &str, secret: u8) -> Self {
        let directory = TempDir::new().unwrap();
        let app = App::open_from(AppPaths::from_root(directory.path().to_path_buf())).unwrap();
        let identity = DeviceIdentity::from_bytes(name.into(), [secret; 32]);
        // Write the isolated file directly: these fixtures never alter an OS keychain.
        std::fs::write(
            app.store.paths.identity_file(),
            serde_json::to_vec(&identity.to_stored()).unwrap(),
        )
        .unwrap();
        let config = AppConfig {
            relay_url: url.into(),
            notify: clipsync_storage::NotifyConfig {
                enabled: false,
                preview: false,
            },
            sync: clipsync_storage::SyncConfig {
                auto_start: false,
                ..Default::default()
            },
            ..Default::default()
        };
        app.store.save_config(&config).unwrap();
        Self {
            _directory: directory,
            app,
            identity,
        }
    }

    fn configure(&self, change: impl FnOnce(&mut AppConfig)) {
        let mut config = self.app.store.load_config().unwrap();
        change(&mut config);
        self.app.store.save_config(&config).unwrap();
    }

    fn save_room(&self, room_id: RoomId, token: String, other: &Peer, role: DeviceRole) {
        self.app
            .store
            .upsert_room(RoomRecord {
                room_id: room_id.clone(),
                epoch: 1,
                role,
                created_at: Utc::now(),
                device_token: token,
                peers: vec![PeerRecord {
                    device_id: other.identity.device_id.clone(),
                    device_name: "fixture peer".into(),
                    os: "test".into(),
                    role: if role == DeviceRole::Owner {
                        DeviceRole::Member
                    } else {
                        DeviceRole::Owner
                    },
                    public_key_hex: hex::encode(other.identity.public_bytes()),
                    fingerprint: other.identity.fingerprint(),
                }],
            })
            .unwrap();
        std::fs::write(
            self.app
                .store
                .paths
                .data_dir
                .join(format!("epoch-{room_id}.secret")),
            hex::encode(EPOCH),
        )
        .unwrap();
    }
}

struct Pair {
    url: String,
    server: tokio::task::JoinHandle<()>,
    a: Peer,
    b: Peer,
}

impl Drop for Pair {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .tls_built_in_root_certs(false)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

async fn next_control(connection: &mut RelayConnection) -> ControlMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match connection.incoming.recv().await {
                Some(Incoming::Control(message)) => return message,
                Some(_) => {}
                None => panic!("relay connection closed"),
            }
        }
    })
    .await
    .expect("relay response timed out")
}

async fn await_hello(connection: &mut RelayConnection) {
    loop {
        match next_control(connection).await {
            ControlMessage::HelloOk { .. } => return,
            ControlMessage::Error { code, message } => panic!("{code}: {message}"),
            _ => {}
        }
    }
}

impl Pair {
    async fn new() -> Self {
        tokio::time::timeout(Duration::from_secs(15), Self::bootstrap())
            .await
            .expect("local relay fixture bootstrap timed out")
    }

    async fn bootstrap() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");
        let router = clipsync_relay::router(clipsync_relay::RelayConfig {
            bind: address,
            public_url: url.clone(),
            token_secret: vec![41; 32],
            webhook_url: None,
            webhook_secret: None,
        });
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let a = Peer::new(&url, "01ARZ3NDEKTSV4RRFFQ69G5FAZ", 1);
        let b = Peer::new(&url, "01ARZ3NDEKTSV4RRFFQ69G5FAV", 2);
        let sid = PairingSessionId::new();
        let created: CreatePairingResponse = client()
            .post(format!("{url}/v1/pairing/create"))
            .json(&CreatePairingRequest {
                pairing_session_id: sid.clone(),
                device_id: a.identity.device_id.clone(),
                device_name: "first".into(),
                os: "test".into(),
                protocol_version: PROTOCOL_VERSION,
                ttl_secs: Some(120),
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let joined: JoinPairingResponse = client()
            .post(format!("{url}/v1/pairing/join"))
            .json(&JoinPairingRequest {
                pairing_code: created.pairing_code,
                device_id: b.identity.device_id.clone(),
                device_name: "second".into(),
                os: "test".into(),
                protocol_version: PROTOCOL_VERSION,
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut ca = RelayConnection::connect(&join_ws(&url, "/v1/ws"))
            .await
            .unwrap();
        let mut cb = RelayConnection::connect(&join_ws(&url, "/v1/ws"))
            .await
            .unwrap();
        for (connection, peer, role, token) in [
            (&mut ca, &a, ClientRole::PairingA, created.pairing_token),
            (&mut cb, &b, ClientRole::PairingB, joined.pairing_token),
        ] {
            connection
                .send_control(ControlMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    device_id: peer.identity.device_id.clone(),
                    device_name: "fixture".into(),
                    os: "test".into(),
                    role,
                    room_id: Some(created.room_id.clone()),
                    token,
                    pairing_session_id: Some(sid.clone()),
                })
                .await
                .unwrap();
            await_hello(connection).await;
        }
        // The zero-knowledge relay treats binds as opaque. These pre-paired client
        // fixtures use a shared epoch key, while real relay-issued credentials are used.
        for connection in [&ca, &cb] {
            connection
                .send_control(ControlMessage::DeviceBind {
                    pairing_session_id: sid.clone(),
                    nonce: "00".into(),
                    ciphertext: "00".into(),
                })
                .await
                .unwrap();
        }
        let mut tokens = Vec::new();
        for connection in [&mut ca, &mut cb] {
            loop {
                if let ControlMessage::PairingComplete { device_token, .. } =
                    next_control(connection).await
                {
                    tokens.push(device_token);
                    break;
                }
            }
        }
        a.save_room(
            created.room_id.clone(),
            tokens.remove(0),
            &b,
            DeviceRole::Owner,
        );
        b.save_room(created.room_id, tokens.remove(0), &a, DeviceRole::Member);
        ca.close().await;
        cb.close().await;
        wait_until(|| client_connection_count(&url, 0)).await;
        Self { url, server, a, b }
    }

    async fn member(&self, peer: &Peer) -> RelayConnection {
        let room = peer.app.store.current_room().unwrap();
        let mut connection = RelayConnection::connect(&join_ws(&self.url, "/v1/ws"))
            .await
            .unwrap();
        connection
            .send_control(ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: peer.identity.device_id.clone(),
                device_name: "fixture".into(),
                os: "test".into(),
                role: ClientRole::Member,
                room_id: Some(room.room_id),
                token: room.device_token,
                pairing_session_id: None,
            })
            .await
            .unwrap();
        await_hello(&mut connection).await;
        connection
    }
}

async fn client_connection_count(url: &str, expected: usize) -> bool {
    client()
        .get(format!("{url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .lines()
        .any(|line| line == format!("clipsync_ws_connections {expected}"))
}

async fn wait_until<F, Fut>(mut ready: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("product state did not reach the expected value");
}

fn sync_handle() -> SyncHandle {
    SyncHandle {
        paused: Arc::new(AtomicBool::new(false)),
        connected: Arc::new(AtomicBool::new(false)),
        peer_online: Arc::new(AtomicBool::new(false)),
        last_item: Arc::new(Mutex::new(None)),
    }
}

struct Session {
    clipboard: MockClipboard,
    handle: SyncHandle,
    shutdown: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<(), CoreError>>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Session {
    async fn start(peer: &Peer) -> Self {
        let clipboard = MockClipboard::new();
        let backend = SystemClipboard::mock(clipboard.clone());
        let handle = sync_handle();
        let task_handle = handle.clone_inner();
        let store = peer.app.store.clone();
        let (shutdown, mut receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            run_session(&store, &backend, &task_handle, &mut receiver).await
        });
        let session = Self {
            clipboard,
            handle,
            shutdown,
            task: Some(task),
        };
        wait_until(|| async { session.handle.connected.load(Ordering::Relaxed) }).await;
        session
    }

    async fn stop(mut self) {
        self.shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task.take().unwrap())
            .await
            .expect("session failed to stop")
            .unwrap()
            .unwrap();
    }

    async fn text(&self, expected: &str) {
        wait_until(|| async {
            matches!(self.clipboard.get(), Some(ClipboardItem::Text { text }) if text == expected)
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_sessions_sync_text_images_and_files_without_echoing_remote_content() {
    let pair = Pair::new().await;
    let a = Session::start(&pair.a).await;
    let b = Session::start(&pair.b).await;
    wait_until(|| async {
        a.handle.peer_online.load(Ordering::Relaxed) && b.handle.peer_online.load(Ordering::Relaxed)
    })
    .await;
    a.clipboard.set(ClipboardItem::Text {
        text: "automatic first transfer".into(),
    });
    b.text("automatic first transfer").await;
    let signature = a.clipboard.signature().await.unwrap();
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert_eq!(
        a.clipboard.signature().await.unwrap(),
        signature,
        "remote receive echoed into the source"
    );

    b.clipboard.set(ClipboardItem::Image {
        mime: ImageMime::Png,
        bytes: PNG.to_vec(),
        width: Some(1),
        height: Some(1),
    });
    wait_until(|| async {
        matches!(a.clipboard.get(), Some(ClipboardItem::Image { bytes, .. }) if bytes == PNG)
    })
    .await;
    a.stop().await;
    wait_until(|| async { !b.handle.peer_online.load(Ordering::Relaxed) }).await;
    let input = pair.a.app.store.paths.data_dir.join("invoice.txt");
    std::fs::write(&input, b"invoice payload").unwrap();
    assert_eq!(
        pair.a.app.push(None, &[input], false).await.unwrap(),
        "sent"
    );
    wait_until(|| async { matches!(b.clipboard.get(), Some(ClipboardItem::Files { files }) if files.len() == 1 && files[0].bytes == b"invoice payload") }).await;
    let Some(ClipboardItem::Files { files }) = b.clipboard.get() else {
        panic!("received files")
    };
    assert_eq!(files[0].name, "invoice.txt");
    assert_eq!(
        std::fs::read(files[0].staged_path.as_ref().unwrap()).unwrap(),
        b"invoice payload"
    );
    wait_until(|| async { !b.handle.peer_online.load(Ordering::Relaxed) }).await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_disabled_and_receive_only_sessions_do_not_send_local_changes() {
    for mode in ["paused", "disabled", "receive-only"] {
        let pair = Pair::new().await;
        let a = Session::start(&pair.a).await;
        pair.b.configure(|config| {
            config.sync.enabled = mode != "disabled";
            config.sync.direction = if mode == "receive-only" {
                SyncDirection::ReceiveOnly
            } else {
                SyncDirection::Both
            };
        });
        let b = Session::start(&pair.b).await;
        wait_until(|| async {
            a.handle.peer_online.load(Ordering::Relaxed)
                && b.handle.peer_online.load(Ordering::Relaxed)
        })
        .await;
        b.handle.paused.store(mode == "paused", Ordering::Relaxed);
        b.clipboard.set(ClipboardItem::Text {
            text: format!("blocked by {mode}"),
        });
        tokio::time::sleep(Duration::from_millis(650)).await;
        assert!(a.clipboard.get().is_none(), "{mode} transmitted local data");
        if mode == "receive-only" {
            a.clipboard.set(ClipboardItem::Text {
                text: "receive-only still receives".into(),
            });
            b.text("receive-only still receives").await;
        }
        b.stop().await;
        a.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initial_hello_delivers_offline_text_before_live_clipboard_changes() {
    let pair = Pair::new().await;
    oneshot_push(
        &pair.a.app.store,
        ClipboardItem::Text {
            text: "queued while offline".into(),
        },
    )
    .await
    .unwrap();
    let b = Session::start(&pair.b).await;
    b.text("queued while offline").await;
    assert!(
        matches!(&*b.handle.last_item.lock(), Some(ClipboardItem::Text { text }) if text == "queued while offline")
    );
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authenticated_but_invalid_text_is_ignored_and_the_next_message_still_arrives() {
    let pair = Pair::new().await;
    let b = Session::start(&pair.b).await;
    let mut sender = pair.member(&pair.a).await;
    let room = pair.a.app.store.current_room().unwrap();
    let now = Utc::now();
    let routing = RoutingHeader {
        version: PROTOCOL_VERSION,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: room.room_id.clone(),
        sender_device_id: pair.a.identity.device_id.clone(),
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + ChronoDuration::seconds(60),
        item_kind: ItemKind::Text,
        seq: 1,
    };
    let key = derive_content_key(&EPOCH, routing.transfer_id.as_str()).unwrap();
    let nonce = derive_nonce(&key, 0).unwrap();
    let encrypted = seal(
        &key,
        &nonce,
        format!("{}:text", routing.message_id).as_bytes(),
        &[0xff],
    )
    .unwrap();
    sender
        .send_control(ControlMessage::TextEnvelope {
            routing,
            nonce: hex::encode(nonce),
            ciphertext: hex::encode(encrypted),
        })
        .await
        .unwrap();
    loop {
        if matches!(next_control(&mut sender).await, ControlMessage::Ack { .. }) {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        b.clipboard.get().is_none(),
        "invalid UTF-8 reached the clipboard"
    );
    send_item(
        &sender,
        &pair.a.identity,
        &room,
        &EPOCH,
        2,
        &ClipboardItem::Text {
            text: "valid after rejection".into(),
        },
    )
    .await
    .unwrap();
    b.text("valid after rejection").await;
    assert_eq!(
        b.clipboard.signature().await.unwrap(),
        "1",
        "rejected bytes caused an extra clipboard write"
    );
    sender.close().await;
    b.stop().await;
}

fn in_worker(name: &str) -> bool {
    std::env::var("CLIPSYNC_CORE_BEHAVIOR_WORKER").as_deref() == Ok(name)
}

fn worker(name: &str, input: &[u8], extra: &[(&str, &str)]) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let home = TempDir::new().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env("CLIPSYNC_CORE_BEHAVIOR_WORKER", name)
        .env("CLIPSYNC_HOME", home.path())
        .env("CLIPSYNC_NO_DAEMON", "1")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("CLIPSYNC_CA_FILE")
        .env_remove("CLIPSYNC_LISTEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for proxy in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(proxy);
    }
    for (key, value) in extra {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let finished = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert!(
        finished && output.status.success(),
        "isolated case failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn app_commands_use_real_ipc_and_persist_pause_config_and_status() {
    const NAME: &str =
        "engine::behavior_tests::app_commands_use_real_ipc_and_persist_pause_config_and_status";
    if !in_worker(NAME) {
        worker(NAME, b"message from stdin", &[]);
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let peer = Peer::new("http://127.0.0.1:1", "command-device", 5);
        let app = peer.app;
        assert_eq!(
            crate::device_id_or_new(&app.store).unwrap(),
            peer.identity.device_id
        );
        assert_eq!(
            app.identity().unwrap().fingerprint(),
            peer.identity.fingerprint()
        );
        assert!(app.room_list().unwrap().is_empty());
        assert!(app.devices().is_err());
        app.config_set("sync.ttl_secs", "999").unwrap();
        assert_eq!(
            app.config_get(Some("sync.ttl_secs")).unwrap()["sync.ttl_secs"],
            "300"
        );
        app.config_set("sync.direction", "receive-only").unwrap();
        assert_eq!(
            app.config_get(None).unwrap()["sync"]["direction"],
            "receive_only"
        );
        let unchanged = std::fs::read(app.store.paths.config_file()).unwrap();
        assert!(app.config_set("sync.direction", "invalid").is_err());
        assert!(app.config_set("unknown", "value").is_err());
        assert_eq!(
            std::fs::read(app.store.paths.config_file()).unwrap(),
            unchanged
        );
        let status = app.status().await.unwrap();
        assert_eq!(status["daemon"], false);
        assert_eq!(status["device_id"], "command-device");

        let handle = sync_handle();
        handle.connected.store(true, Ordering::Relaxed);
        handle.peer_online.store(true, Ordering::Relaxed);
        let server = IpcServer::bind(&app.store.paths).await.unwrap();
        let store = app.store.clone();
        let state = handle.clone_inner();
        let serving = tokio::spawn(async move {
            for _ in 0..6 {
                let mut stream = server.accept().await.unwrap();
                let request = read_request(&mut stream).await.unwrap().unwrap();
                let response = handle_ipc(&store, &state, request).await;
                write_response(&mut stream, &response).await.unwrap();
            }
        });
        assert_eq!(app.push(None, &[], false).await.unwrap(), "text 18 bytes");
        let pulled = app.pull(false, false, None).await.unwrap();
        assert_eq!(pulled.kind, "text");
        assert_eq!(pulled.text.as_deref(), Some("message from stdin"));
        assert_eq!(app.status().await.unwrap()["connected"], true);
        app.pause().await.unwrap();
        assert!(app.store.load_state().unwrap().paused);
        assert!(handle.paused.load(Ordering::Relaxed));
        app.resume().await.unwrap();
        assert!(!app.store.load_state().unwrap().paused);
        assert!(!handle.paused.load(Ordering::Relaxed));
        assert_eq!(app.status().await.unwrap()["last_item_kind"], "text");
        serving.await.unwrap();
        assert!(!app.store.paths.socket_file().exists());
    });
}

#[test]
fn stdin_validation_rejects_bad_utf8_and_images_and_accepts_real_image_input() {
    const NAME: &str = "engine::behavior_tests::stdin_validation_rejects_bad_utf8_and_images_and_accepts_real_image_input";
    if !in_worker(NAME) {
        for (case, bytes) in [
            ("utf8", &[0xff][..]),
            ("image", b"not an image".as_slice()),
            ("png", PNG),
        ] {
            worker(NAME, bytes, &[("CLIPSYNC_CORE_INPUT_CASE", case)]);
        }
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        match std::env::var("CLIPSYNC_CORE_INPUT_CASE").unwrap().as_str() {
            "utf8" => assert!(crate::load_push_item(None, &[], false).await.unwrap_err().to_string().contains("UTF-8")),
            "image" => assert!(crate::load_push_item(Some("image"), &[], false).await.unwrap_err().to_string().contains("supported image")),
            "png" => assert!(matches!(crate::load_push_item(Some("image"), &[], false).await.unwrap(), ClipboardItem::Image { mime: ImageMime::Png, bytes, .. } if bytes == PNG)),
            _ => panic!("unknown input fixture"),
        }
    });
}

#[tokio::test]
async fn file_input_rejects_directories_missing_files_and_symbolic_links() {
    let directory = TempDir::new().unwrap();
    assert!(
        crate::load_push_item(None, &[directory.path().to_path_buf()], false)
            .await
            .unwrap_err()
            .to_string()
            .contains("directory and symlink")
    );
    assert!(
        crate::load_push_item(None, &[directory.path().join("missing")], false)
            .await
            .is_err()
    );
    #[cfg(unix)]
    {
        let file = directory.path().join("actual.txt");
        let link = directory.path().join("link.txt");
        std::fs::write(&file, b"private").unwrap();
        std::os::unix::fs::symlink(file, &link).unwrap();
        assert!(crate::load_push_item(None, &[link], false)
            .await
            .unwrap_err()
            .to_string()
            .contains("directory and symlink"));
    }
}

#[test]
fn doctor_reports_local_relay_health_and_closed_port_failure_without_system_changes() {
    const NAME: &str = "engine::behavior_tests::doctor_reports_local_relay_health_and_closed_port_failure_without_system_changes";
    if !in_worker(NAME) {
        worker(NAME, b"", &[]);
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let pair = Pair::new().await;
        let report = pair.a.app.doctor().await.unwrap();
        assert_eq!(report["identity"], true);
        assert_eq!(
            report["room"],
            pair.a.app.store.current_room().unwrap().room_id.to_string()
        );
        assert_eq!(report["relay"]["ok"], true);
        assert_eq!(report["relay"]["status"], 200);
        assert_eq!(report["tls"]["verifier"], "platform");
        assert!(report["paths"]["data_dir"]
            .as_str()
            .unwrap()
            .starts_with(pair.a._directory.path().to_str().unwrap()));
        pair.a
            .configure(|config| config.relay_url = "http://127.0.0.1:1".into());
        let failed = pair.a.app.doctor().await.unwrap();
        assert_eq!(failed["relay"]["ok"], false);
        assert!(failed["relay"]["error"]
            .as_str()
            .is_some_and(|message| !message.is_empty()));
    });
}

#[cfg(unix)]
#[test]
fn daemon_answers_commands_and_removes_pid_and_socket_after_interrupt() {
    const NAME: &str = "engine::behavior_tests::daemon_answers_commands_and_removes_pid_and_socket_after_interrupt";
    if !in_worker(NAME) {
        worker(NAME, b"", &[]);
        return;
    }
    let paths = AppPaths::from_env();
    paths.ensure().unwrap();
    let identity = DeviceIdentity::from_bytes("isolated-daemon".into(), [9; 32]);
    std::fs::write(
        paths.identity_file(),
        serde_json::to_vec(&identity.to_stored()).unwrap(),
    )
    .unwrap();
    let store = LocalStore::open_from(paths.clone()).unwrap();
    let config = AppConfig {
        relay_url: "http://127.0.0.1:1".into(),
        notify: clipsync_storage::NotifyConfig {
            enabled: false,
            preview: false,
        },
        sync: clipsync_storage::SyncConfig {
            auto_start: false,
            ..Default::default()
        },
        ..Default::default()
    };
    store.save_config(&config).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let task = tokio::spawn(run_daemon(Some(
            SystemClipboard::mock(MockClipboard::new()),
        )));
        wait_until(|| async { paths.socket_file().exists() }).await;
        let mut connection = clipsync_daemon::connect_ipc(&paths).await.unwrap();
        let status = connection
            .request(&IpcRequest::Status)
            .await
            .unwrap()
            .status
            .unwrap();
        assert!(status.daemon);
        assert!(!status.connected);
        assert_eq!(status.device_id, Some(identity.device_id.clone()));
        assert!(paths.pid_file().exists());
        drop(connection);
        assert!(std::process::Command::new("kill")
            .args(["-INT", &std::process::id().to_string()])
            .status()
            .unwrap()
            .success());
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("daemon did not stop")
            .unwrap()
            .unwrap();
        assert!(!paths.pid_file().exists());
        assert!(!paths.socket_file().exists());
    });
}

fn text_routing() -> RoutingHeader {
    let now = Utc::now();
    RoutingHeader {
        version: PROTOCOL_VERSION,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: RoomId::new(),
        sender_device_id: clipsync_protocol::DeviceId::new(),
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + ChronoDuration::seconds(60),
        item_kind: ItemKind::Text,
        seq: 1,
    }
}

#[test]
fn text_envelopes_reject_malformed_nonce_ciphertext_and_changed_message_context() {
    let routing = text_routing();
    let key = derive_content_key(&EPOCH, routing.transfer_id.as_str()).unwrap();
    let nonce = [4; 12];
    let ciphertext = seal(
        &key,
        &nonce,
        format!("{}:text", routing.message_id).as_bytes(),
        "안녕 📝".as_bytes(),
    )
    .unwrap();
    assert!(
        matches!(open_text(&EPOCH, &routing, &hex::encode(nonce), &hex::encode(&ciphertext)).unwrap(), ClipboardItem::Text { text } if text == "안녕 📝")
    );
    for bad_nonce in ["not hex", "00"] {
        assert!(open_text(&EPOCH, &routing, bad_nonce, &hex::encode(&ciphertext)).is_err());
    }
    assert!(open_text(&EPOCH, &routing, &hex::encode(nonce), "not hex").is_err());
    let mut other_message = routing;
    other_message.message_id = MessageId::new();
    assert!(
        open_text(
            &EPOCH,
            &other_message,
            &hex::encode(nonce),
            &hex::encode(ciphertext)
        )
        .is_err(),
        "ciphertext accepted under another message's authenticated context"
    );
}

fn manifest(kind: ItemKind, payload: &[u8]) -> ManifestPlain {
    ManifestPlain {
        item_kind: kind,
        total_bytes: payload.len() as u64,
        chunk_count: 1,
        content_hash: clipsync_crypto::blake3_hex(payload),
        files: Vec::new(),
        image: None,
        text_chars: None,
    }
}

#[tokio::test]
async fn received_payloads_validate_image_mime_utf8_and_file_integrity_before_exposure() {
    let directory = TempDir::new().unwrap();
    let staging = StagingArea::new(directory.path().join("inbox")).unwrap();
    let id = TransferId::new();
    let unknown = manifest(ItemKind::Image, b"not an image");
    assert!(
        item_from_payload(&unknown, b"not an image".to_vec(), &staging, &id)
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown image mime")
    );
    let inferred = manifest(ItemKind::Image, PNG);
    assert!(
        matches!(item_from_payload(&inferred, PNG.to_vec(), &staging, &id).await.unwrap(), ClipboardItem::Image { mime: ImageMime::Png, bytes, width: None, height: None } if bytes == PNG)
    );
    let text = manifest(ItemKind::Text, "received text".as_bytes());
    assert!(
        matches!(item_from_payload(&text, b"received text".to_vec(), &staging, &id).await.unwrap(), ClipboardItem::Text { text } if text == "received text")
    );
    let bad_utf8 = manifest(ItemKind::Text, &[0xff]);
    assert!(item_from_payload(&bad_utf8, vec![0xff], &staging, &id)
        .await
        .unwrap_err()
        .to_string()
        .contains("text payload utf-8"));
    let mut files = manifest(ItemKind::Files, b"tampered");
    files.files.push(FileManifest {
        name: "report.txt".into(),
        size: 8,
        mime: None,
        hash: clipsync_crypto::blake3_hex(b"expected"),
    });
    assert!(
        item_from_payload(&files, b"tampered".to_vec(), &staging, &id)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(staging.inbox()).unwrap().count(),
        0,
        "rejected file data was exposed"
    );
}

#[tokio::test]
async fn text_pull_writes_exact_utf8_output_and_propagates_output_path_failures() {
    let directory = TempDir::new().unwrap();
    let output = directory.path().join("output");
    let result = finish_pull(
        ClipboardItem::Text {
            text: "saved 안녕\n".into(),
        },
        false,
        Some(output.clone()),
    )
    .await
    .unwrap();
    assert_eq!(result.kind, "text");
    assert_eq!(result.text.as_deref(), Some("saved 안녕\n"));
    assert_eq!(
        std::fs::read(output.join("clipboard.txt")).unwrap(),
        "saved 안녕\n".as_bytes()
    );
    let blocked = directory.path().join("not-a-directory");
    std::fs::write(&blocked, b"preserve me").unwrap();
    assert!(finish_pull(
        ClipboardItem::Text {
            text: "new data".into()
        },
        false,
        Some(blocked.clone())
    )
    .await
    .is_err());
    assert_eq!(std::fs::read(blocked).unwrap(), b"preserve me");
}

struct ControlledClipboard {
    value: Arc<Mutex<Option<ClipboardItem>>>,
    write_fails: bool,
    signature_fails: bool,
}

#[async_trait::async_trait]
impl ClipboardBackend for ControlledClipboard {
    fn name(&self) -> &'static str {
        "controlled clipboard"
    }
    fn capabilities(&self) -> clipsync_clipboard::Capabilities {
        clipsync_clipboard::Capabilities::all()
    }
    async fn read(&self) -> clipsync_clipboard::Result<Option<ClipboardItem>> {
        Ok(self.value.lock().clone())
    }
    async fn write(&self, item: &ClipboardItem) -> clipsync_clipboard::Result<()> {
        if self.write_fails {
            return Err(clipsync_clipboard::ClipboardError::Message(
                "clipboard is locked".into(),
            ));
        }
        *self.value.lock() = Some(item.clone());
        Ok(())
    }
    async fn signature(&self) -> clipsync_clipboard::Result<String> {
        if self.signature_fails {
            return Err(clipsync_clipboard::ClipboardError::Message(
                "signature unavailable".into(),
            ));
        }
        Ok("new signature".into())
    }
}

fn controlled_clipboard(
    write_fails: bool,
    signature_fails: bool,
) -> (SystemClipboard, Arc<Mutex<Option<ClipboardItem>>>) {
    let value = Arc::new(Mutex::new(None));
    let backend = ControlledClipboard {
        value: value.clone(),
        write_fails,
        signature_fails,
    };
    let clipboard = SystemClipboard::from_backend(
        Box::new(backend),
        clipsync_clipboard::AdapterInfo {
            name: "controlled",
            source: "test fixture",
            capabilities: clipsync_clipboard::Capabilities::all(),
            fallback_reason: None,
        },
    );
    (clipboard, value)
}

#[tokio::test]
async fn received_data_survives_a_signature_failure_after_a_successful_clipboard_write() {
    let (clipboard, written) = controlled_clipboard(false, true);
    let handle = sync_handle();
    let mut guard = LoopGuard::default();
    let notifier = Notifier::new(clipsync_storage::NotifyConfig {
        enabled: false,
        preview: false,
    });
    let mut signature = "previous signature".into();
    apply_remote(
        &clipboard,
        &mut guard,
        &handle,
        &notifier,
        "signature-failure",
        ClipboardItem::Text {
            text: "kept despite signature failure".into(),
        },
        &mut signature,
    )
    .await;
    assert!(
        matches!(&*written.lock(), Some(ClipboardItem::Text { text }) if text == "kept despite signature failure")
    );
    assert_eq!(signature, "previous signature");
    assert!(
        matches!(&*handle.last_item.lock(), Some(ClipboardItem::Text { text }) if text == "kept despite signature failure")
    );
}

#[cfg(unix)]
#[test]
fn failed_clipboard_writes_do_not_emit_success_notifications_and_remain_available_to_pull() {
    use std::os::unix::fs::PermissionsExt;
    const NAME: &str = "engine::behavior_tests::failed_clipboard_writes_do_not_emit_success_notifications_and_remain_available_to_pull";
    if !in_worker(NAME) {
        let directory = TempDir::new().unwrap();
        let log = directory.path().join("notifications");
        for program in ["osascript", "notify-send"] {
            let file = directory.path().join(program);
            std::fs::write(
                &file,
                "#!/bin/sh\nprintf 'notification' >> \"$CLIPSYNC_CORE_NOTIFICATION_LOG\"\n",
            )
            .unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        worker(
            NAME,
            b"",
            &[
                ("PATH", directory.path().to_str().unwrap()),
                ("CLIPSYNC_CORE_NOTIFICATION_LOG", log.to_str().unwrap()),
            ],
        );
        assert_eq!(
            std::fs::read(log).unwrap(),
            b"notification",
            "the notification capture positive control did not run"
        );
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let peer = Peer::new("http://127.0.0.1:1", "failed-paste", 8);
        let (clipboard, written) = controlled_clipboard(true, false);
        let handle = sync_handle();
        let mut guard = LoopGuard::default();
        let notifier = Notifier::new(clipsync_storage::NotifyConfig {
            enabled: true,
            preview: true,
        });
        let mut signature = "unchanged".into();
        apply_remote(
            &clipboard,
            &mut guard,
            &handle,
            &notifier,
            "paste-failure",
            ClipboardItem::Text {
                text: "received but not pasted".into(),
            },
            &mut signature,
        )
        .await;
        let log = std::env::var_os("CLIPSYNC_CORE_NOTIFICATION_LOG").unwrap();
        assert!(
            !std::path::Path::new(&log).exists(),
            "paste failure was reported as a successful receive"
        );
        assert!(written.lock().is_none());
        assert_eq!(signature, "unchanged");
        let response = handle_ipc(&peer.app.store, &handle, IpcRequest::Pull).await;
        assert_eq!(
            response.pull.unwrap().text.as_deref(),
            Some("received but not pasted")
        );
        notifier.received("text", "fixture", "capture positive control");
    });
}
