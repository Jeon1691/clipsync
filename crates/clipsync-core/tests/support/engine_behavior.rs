// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Product behavior exercised through real loopback relay connections and isolated files.

#[path = "engine_coverage.rs"]
mod coverage_expansion;

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
/// Scrape credential for the fixture relay; readiness polls read `/metrics`.
const ENGINE_METRICS_TOKEN: &str = "engine-metrics-scrape-token-0123456789";
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

struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn product_logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOGS: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(move || CapturedLogs(writer.clone()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        logs
    })
}

impl Peer {
    fn new(url: &str, name: &str, secret: u8) -> Self {
        let _ = product_logs();
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
            metrics_token: Some(clipsync_relay::MetricsToken::new(ENGINE_METRICS_TOKEN).unwrap()),
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
        .bearer_auth(ENGINE_METRICS_TOKEN)
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
    reads: Arc<std::sync::atomic::AtomicUsize>,
    handle: SyncHandle,
    shutdown: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<(), CoreError>>>,
}

struct ObservedClipboard {
    clipboard: MockClipboard,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl ClipboardBackend for ObservedClipboard {
    fn name(&self) -> &'static str {
        "observed mock"
    }
    fn capabilities(&self) -> clipsync_clipboard::Capabilities {
        clipsync_clipboard::Capabilities::all()
    }
    async fn read(&self) -> clipsync_clipboard::Result<Option<ClipboardItem>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.clipboard.read().await
    }
    async fn write(&self, item: &ClipboardItem) -> clipsync_clipboard::Result<()> {
        self.clipboard.write(item).await
    }
    async fn signature(&self) -> clipsync_clipboard::Result<String> {
        self.clipboard.signature().await
    }
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
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = SystemClipboard::from_backend(
            Box::new(ObservedClipboard {
                clipboard: clipboard.clone(),
                reads: reads.clone(),
            }),
            clipsync_clipboard::AdapterInfo {
                name: "observed mock",
                source: "test fixture",
                capabilities: clipsync_clipboard::Capabilities::all(),
                fallback_reason: None,
            },
        );
        let handle = sync_handle();
        let task_handle = handle.clone_inner();
        let store = peer.app.store.clone();
        let (shutdown, mut receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            run_session(&store, &backend, &task_handle, &mut receiver).await
        });
        let session = Self {
            clipboard,
            reads,
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
async fn repeated_clipboard_announcements_and_invalid_local_content_do_not_echo_or_interrupt_sync()
{
    let pair = Pair::new().await;
    let a = Session::start(&pair.a).await;
    let b = Session::start(&pair.b).await;
    a.clipboard.set(ClipboardItem::Text {
        text: "remembered remote text".into(),
    });
    b.text("remembered remote text").await;
    let reads = b.reads.load(Ordering::Relaxed);
    b.clipboard.set(ClipboardItem::Text {
        text: "remembered remote text".into(),
    });
    wait_until(|| async { b.reads.load(Ordering::Relaxed) > reads }).await;
    assert_eq!(
        a.clipboard.signature().await.unwrap(),
        "1",
        "a repeated platform announcement echoed remote content"
    );
    let reads = b.reads.load(Ordering::Relaxed);
    b.clipboard.set(ClipboardItem::Text {
        text: "x".repeat(clipsync_protocol::TEXT_MAX_BYTES + 1),
    });
    wait_until(|| async { b.reads.load(Ordering::Relaxed) > reads }).await;
    assert_eq!(
        a.clipboard.signature().await.unwrap(),
        "1",
        "oversized text reached the peer"
    );
    b.clipboard.set(ClipboardItem::Text {
        text: "valid after local rejection".into(),
    });
    a.text("valid after local rejection").await;
    assert_eq!(
        a.clipboard.signature().await.unwrap(),
        "2",
        "rejected announcements reached the relay before the final valid item"
    );
    b.stop().await;
    a.stop().await;
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
    let logs = String::from_utf8(product_logs().lock().clone()).unwrap();
    assert!(
        logs.contains("text decrypt"),
        "decryption failure lost its diagnostic context"
    );
    assert!(
        !logs.contains("valid after rejection"),
        "clipboard plaintext entered product logs"
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
fn ipc_failure_empty_and_binary_responses_preserve_command_error_and_fallback_contracts() {
    const NAME: &str = "engine::behavior_tests::ipc_failure_empty_and_binary_responses_preserve_command_error_and_fallback_contracts";
    if !in_worker(NAME) {
        for case in [
            "push-error",
            "push-default",
            "push-invalid",
            "pull-invalid",
            "status-invalid",
            "pull-empty",
            "pull-image-copy",
            "status-empty",
            "binary-fallback",
            "oversized",
        ] {
            let input = if case == "binary-fallback" {
                PNG.to_vec()
            } else if case == "oversized" {
                vec![b'x'; clipsync_protocol::TEXT_MAX_BYTES + 1]
            } else {
                b"IPC payload".to_vec()
            };
            worker(NAME, &input, &[("CLIPSYNC_CORE_IPC_CASE", case)]);
        }
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use tokio::io::AsyncWriteExt;
        let case = std::env::var("CLIPSYNC_CORE_IPC_CASE").unwrap();
        let peer = Peer::new("http://127.0.0.1:1", "ipc-negative", 56);
        if case == "oversized" {
            assert!(matches!(
                peer.app.push(None, &[], false).await,
                Err(CoreError::Clipboard(_))
            ));
            return;
        }
        let server = IpcServer::bind(&peer.app.store.paths).await.unwrap();
        let mode = case.clone();
        let task = tokio::spawn(async move {
            let mut stream = server.accept().await.unwrap();
            let request = read_request(&mut stream).await.unwrap();
            if mode == "binary-fallback" {
                assert!(request.is_none());
                return;
            }
            assert!(request.is_some());
            if mode.ends_with("invalid") {
                stream.write_all(b"invalid JSON\n").await.unwrap();
                return;
            }
            write_response(
                &mut stream,
                &IpcResponse {
                    ok: !mode.starts_with("push"),
                    error: if mode == "push-error" {
                        Some("daemon rejected payload".into())
                    } else {
                        None
                    },
                    status: None,
                    pull: if mode == "pull-image-copy" {
                        Some(PullPayload {
                            kind: "image".into(),
                            text: None,
                            summary: "stored image".into(),
                        })
                    } else {
                        None
                    },
                },
            )
            .await
            .unwrap();
        });
        match case.as_str() {
            "push-error" => assert!(peer
                .app
                .push(None, &[], false)
                .await
                .unwrap_err()
                .to_string()
                .contains("daemon rejected payload")),
            "push-default" => assert!(peer
                .app
                .push(None, &[], false)
                .await
                .unwrap_err()
                .to_string()
                .contains("push failed")),
            "push-invalid" => assert!(matches!(
                peer.app.push(None, &[], false).await,
                Err(CoreError::Daemon(_))
            )),
            "pull-invalid" => assert!(matches!(
                peer.app.pull(false, false, None).await,
                Err(CoreError::Daemon(_))
            )),
            "status-invalid" => {
                assert!(matches!(peer.app.status().await, Err(CoreError::Daemon(_))))
            }
            "pull-empty" => assert!(matches!(
                peer.app.pull(false, false, None).await,
                Err(CoreError::Storage(
                    clipsync_storage::StorageError::NotPaired
                ))
            )),
            "binary-fallback" => assert!(matches!(
                peer.app.push(Some("image"), &[], false).await,
                Err(CoreError::Storage(
                    clipsync_storage::StorageError::NotPaired
                ))
            )),
            "pull-image-copy" => {
                let result = peer.app.pull(true, false, None).await.unwrap();
                assert_eq!(result.kind, "image");
                assert!(result.text.is_none());
            }
            "status-empty" => assert_eq!(peer.app.status().await.unwrap()["daemon"], false),
            _ => panic!("unknown IPC case"),
        }
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
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
        // Socket creation precedes listen on Unix. Readiness means a client can
        // connect, including when the rest of the workspace is testing in parallel.
        let mut connection = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(connection) = clipsync_daemon::connect_ipc(&paths).await {
                    break connection;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("daemon did not begin accepting IPC connections");
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

fn encrypted_text(routing: RoutingHeader, text: &str) -> ControlMessage {
    let key = derive_content_key(&EPOCH, routing.transfer_id.as_str()).unwrap();
    let nonce = derive_nonce(&key, 0).unwrap();
    let ciphertext = seal(
        &key,
        &nonce,
        format!("{}:text", routing.message_id).as_bytes(),
        text.as_bytes(),
    )
    .unwrap();
    ControlMessage::TextEnvelope {
        routing,
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
    }
}

fn encrypted_manifest(routing: RoutingHeader, manifest: &ManifestPlain) -> ControlMessage {
    let key = derive_content_key(&EPOCH, routing.transfer_id.as_str()).unwrap();
    let nonce = derive_nonce(&key, u32::MAX).unwrap();
    let ciphertext = seal(
        &key,
        &nonce,
        format!("{}:manifest", routing.transfer_id).as_bytes(),
        &serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
    ControlMessage::Manifest {
        routing,
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
        chunk_count: manifest.chunk_count,
        total_bytes: manifest.total_bytes,
    }
}

fn hello(room_id: &RoomId) -> ControlMessage {
    ControlMessage::HelloOk {
        room_id: room_id.clone(),
        peer_online: false,
        peers: Vec::new(),
        queued_text: None,
        device_token: None,
    }
}

fn scripted_peer(url: &str, room_id: RoomId) -> Peer {
    let local = Peer::new(url, "script-device-a", 51);
    let remote = Peer::new(url, "script-device-z", 52);
    local.save_room(room_id, "script-token".into(), &remote, DeviceRole::Owner);
    local
}

#[tokio::test]
async fn live_session_recovers_from_repeated_hello_duplicate_text_and_corrupt_transfers() {
    use crate::test_relay::{receive, send, Server};
    use axum::extract::ws::Message;
    let room_id = RoomId::new();
    let room = room_id.clone();
    let mut server = Server::websocket(move |mut socket| async move {
        assert!(matches!(
            receive(&mut socket).await,
            ControlMessage::Hello { .. }
        ));
        send(&mut socket, hello(&room)).await;
        let queued_routing = text_routing();
        let ControlMessage::TextEnvelope {
            routing,
            nonce,
            ciphertext,
        } = encrypted_text(queued_routing, "queued update")
        else {
            unreachable!()
        };
        send(
            &mut socket,
            ControlMessage::HelloOk {
                room_id: room.clone(),
                peer_online: true,
                peers: vec![clipsync_protocol::PeerInfo {
                    device_id: "script-device-z".into(),
                    device_name: "peer".into(),
                    os: "fixture".into(),
                    role: DeviceRole::Member,
                    online: true,
                }],
                queued_text: Some(clipsync_protocol::QueuedEnvelope {
                    routing,
                    nonce,
                    ciphertext,
                }),
                device_token: None,
            },
        )
        .await;
        assert!(matches!(
            receive(&mut socket).await,
            ControlMessage::NoiseHandshake { .. }
        ));
        send(
            &mut socket,
            ControlMessage::NoiseHandshake {
                message: "bad handshake".into(),
            },
        )
        .await;
        let text = encrypted_text(text_routing(), "deduplicated text");
        send(&mut socket, text.clone()).await;
        assert!(matches!(
            receive(&mut socket).await,
            ControlMessage::Ack {
                status: AckStatus::Delivered,
                ..
            }
        ));
        send(&mut socket, text).await;
        send(
            &mut socket,
            ControlMessage::Manifest {
                routing: text_routing(),
                nonce: "not hex".into(),
                ciphertext: "invalid".into(),
                chunk_count: 1,
                total_bytes: 1,
            },
        )
        .await;
        socket
            .send(Message::Binary(vec![0, 1, 2].into()))
            .await
            .unwrap();
        let orphan = split_and_encrypt(&EPOCH, &TransferId::new(), b"orphan").unwrap();
        socket
            .send(Message::Binary(orphan.frames[0].clone().into()))
            .await
            .unwrap();
        for failure in ["count", "hash", "mime"] {
            let routing = text_routing();
            let mut manifest = manifest(ItemKind::Image, b"invalid image");
            if failure == "count" {
                manifest.chunk_count = 2;
            }
            if failure == "hash" {
                manifest.content_hash = "wrong hash".into();
            }
            let chunks = split_and_encrypt(&EPOCH, &routing.transfer_id, b"invalid image").unwrap();
            send(&mut socket, encrypted_manifest(routing, &manifest)).await;
            socket
                .send(Message::Binary(chunks.frames[0].clone().into()))
                .await
                .unwrap();
        }
        send(
            &mut socket,
            ControlMessage::Error {
                code: "temporary".into(),
                message: "retry allowed".into(),
            },
        )
        .await;
        send(&mut socket, ControlMessage::Pong { ts: 7 }).await;
        socket.send(Message::Pong(vec![].into())).await.unwrap();
        send(
            &mut socket,
            encrypted_text(text_routing(), "recovered after invalid transfers"),
        )
        .await;
        assert!(matches!(
            receive(&mut socket).await,
            ControlMessage::Ack {
                status: AckStatus::Delivered,
                ..
            }
        ));
        socket.send(Message::Close(None)).await.unwrap();
    })
    .await;
    let peer = scripted_peer(&server.url, room_id);
    let clipboard = MockClipboard::new();
    let handle = sync_handle();
    let (_, mut shutdown) = watch::channel(false);
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        run_session(
            &peer.app.store,
            &SystemClipboard::mock(clipboard.clone()),
            &handle,
            &mut shutdown,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    server.finish().await;
    assert!(error.to_string().contains("relay disconnected"));
    assert!(
        matches!(clipboard.get(), Some(ClipboardItem::Text { text }) if text == "recovered after invalid transfers")
    );
    assert_eq!(
        clipboard.signature().await.unwrap(),
        "3",
        "duplicate or rejected input wrote the clipboard"
    );
    assert_eq!(
        std::fs::read_dir(&peer.app.store.paths.inbox_dir)
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn pull_reports_empty_closed_and_live_text_states_and_recovers_from_corrupt_binary() {
    use crate::test_relay::{receive, send, Server};
    use axum::extract::ws::Message;
    for case in ["empty", "closed", "text", "binary"] {
        let room_id = RoomId::new();
        let room = room_id.clone();
        let (consumed, release) = tokio::sync::oneshot::channel();
        let mut server = Server::websocket(move |mut socket| async move {
            assert!(matches!(
                receive(&mut socket).await,
                ControlMessage::Hello { .. }
            ));
            if case == "closed" {
                return;
            }
            send(&mut socket, hello(&room)).await;
            if case == "empty" {
                let _ = release.await;
                return;
            }
            if case == "text" {
                send(&mut socket, encrypted_text(text_routing(), "live pull")).await;
                let _ = release.await;
                return;
            }
            send(
                &mut socket,
                ControlMessage::Manifest {
                    routing: text_routing(),
                    nonce: "!".into(),
                    ciphertext: "!".into(),
                    chunk_count: 1,
                    total_bytes: 0,
                },
            )
            .await;
            socket.send(Message::Binary(vec![0].into())).await.unwrap();
            let orphan = split_and_encrypt(&EPOCH, &TransferId::new(), b"orphan").unwrap();
            socket
                .send(Message::Binary(orphan.frames[0].clone().into()))
                .await
                .unwrap();
            for failure in ["count", "hash", "valid"] {
                let routing = text_routing();
                let mut manifest = manifest(ItemKind::Text, b"binary pull");
                if failure == "count" {
                    manifest.chunk_count = 2;
                }
                if failure == "hash" {
                    manifest.content_hash = "bad hash".into();
                }
                let chunks =
                    split_and_encrypt(&EPOCH, &routing.transfer_id, b"binary pull").unwrap();
                send(&mut socket, encrypted_manifest(routing, &manifest)).await;
                socket
                    .send(Message::Binary(chunks.frames[0].clone().into()))
                    .await
                    .unwrap();
            }
            // Delivery is complete only when the application has consumed it.
            // Closing immediately can race the transport's heartbeat writer.
            let _ = release.await;
        })
        .await;
        let peer = scripted_peer(&server.url, room_id);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            oneshot_pull(&peer.app.store, false, case != "empty", None),
        )
        .await
        .unwrap();
        let _ = consumed.send(());
        server.finish().await;
        match case {
            "empty" => assert!(result.unwrap_err().to_string().contains("no queued item")),
            "closed" => assert!(matches!(result, Err(CoreError::Timeout))),
            "text" => assert_eq!(result.unwrap().text.as_deref(), Some("live pull")),
            "binary" => assert_eq!(result.unwrap().text.as_deref(), Some("binary pull")),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn hello_wait_rejects_relay_errors_and_disconnects_and_skips_unrelated_frames() {
    use crate::test_relay::{send, Server};
    for case in ["error", "closed", "success"] {
        let room = RoomId::new();
        let (consumed, release) = tokio::sync::oneshot::channel();
        let mut server = Server::websocket(move |mut socket| async move {
            send(&mut socket, ControlMessage::Pong { ts: 1 }).await;
            if case == "error" {
                send(
                    &mut socket,
                    ControlMessage::Error {
                        code: "revoked".into(),
                        message: "device removed".into(),
                    },
                )
                .await;
            } else if case == "success" {
                send(&mut socket, hello(&room)).await;
            }
            if case != "closed" {
                let _ = release.await;
            }
        })
        .await;
        let mut connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), wait_hello_ok(&mut connection))
            .await
            .unwrap();
        let _ = consumed.send(());
        server.finish().await;
        match case {
            "error" => assert!(result
                .unwrap_err()
                .to_string()
                .contains("relay revoked: device removed")),
            "closed" => assert!(result
                .unwrap_err()
                .to_string()
                .contains("relay closed before hello")),
            _ => result.unwrap(),
        }
    }
    assert_eq!(_unused_ws("https://relay.example"), "wss://relay.example");
}

#[tokio::test]
async fn silent_relay_connections_reach_hello_and_pull_deadlines_without_leaving_tasks_running() {
    use crate::test_relay::{receive, send, Server};
    for mode in ["hello", "pull"] {
        let room = RoomId::new();
        let room_id = room.clone();
        let (ready, connected) = tokio::sync::oneshot::channel();
        let (consumed, release) = tokio::sync::oneshot::channel();
        let mut server = Server::websocket_with_deadline(
            axum::Router::new(),
            Duration::from_secs(180),
            move |mut socket| async move {
                if mode == "pull" {
                    assert!(matches!(
                        receive(&mut socket).await,
                        ControlMessage::Hello { .. }
                    ));
                    send(&mut socket, hello(&room)).await;
                }
                let _ = ready.send(());
                let _ = release.await;
            },
        )
        .await;
        let peer = scripted_peer(&server.url, room_id);
        let task = if mode == "hello" {
            let mut connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            tokio::spawn(async move { wait_hello_ok(&mut connection).await })
        } else {
            let store = peer.app.store.clone();
            tokio::spawn(async move { oneshot_pull(&store, false, true, None).await.map(|_| ()) })
        };
        tokio::time::timeout(Duration::from_secs(5), connected)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        let result = tokio::time::timeout(Duration::from_secs(40), task)
            .await
            .unwrap()
            .unwrap();
        tokio::time::resume();
        let _ = consumed.send(());
        server.finish().await;
        assert!(
            matches!(result, Err(CoreError::Timeout)),
            "{mode}: {result:?}"
        );
    }
}

#[tokio::test]
async fn hello_presence_only_starts_noise_for_a_known_online_peer_with_a_valid_public_key() {
    use crate::test_relay::{receive, Server};
    let mut server = Server::websocket(|mut socket| async move {
        // The eligible case below sends exactly one handshake. Other presence
        // records must not try to authenticate an absent or unknown device.
        assert!(matches!(
            receive(&mut socket).await,
            ControlMessage::NoiseHandshake { .. }
        ));
    })
    .await;
    let room_id = RoomId::new();
    let peer = scripted_peer(&server.url, room_id.clone());
    let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
        .await
        .unwrap();
    let room = peer.app.store.current_room().unwrap();
    let clipboard = SystemClipboard::mock(MockClipboard::new());
    let mut guard = LoopGuard::default();
    let handle = sync_handle();
    let notifier = Notifier::new(clipsync_storage::NotifyConfig {
        enabled: false,
        preview: false,
    });
    let mut signature = "unchanged".into();
    for case in [
        "unrelated",
        "no-online",
        "unknown",
        "bad-key",
        "responder",
        "already-done",
        "eligible",
    ] {
        let mut room = room.clone();
        let mut info = clipsync_protocol::PeerInfo {
            device_id: "script-device-z".into(),
            device_name: "peer".into(),
            os: "fixture".into(),
            role: DeviceRole::Member,
            online: case != "no-online",
        };
        if case == "unknown" {
            info.device_id = "unrecognized".into();
        }
        if case == "bad-key" {
            room.peers[0].public_key_hex = "not a key".into();
        }
        if case == "responder" {
            room.peers[0].device_id = "000-first-device".into();
            info.device_id = room.peers[0].device_id.clone();
        }
        let message = if case == "unrelated" {
            ControlMessage::PeerLeft {
                device_id: info.device_id,
            }
        } else {
            ControlMessage::HelloOk {
                room_id: room_id.clone(),
                peer_online: true,
                peers: vec![info],
                queued_text: None,
                device_token: None,
            }
        };
        let mut noise = None;
        let mut done = case == "already-done";
        apply_hello_ok(
            &message,
            &peer.identity,
            &room,
            &EPOCH,
            &clipboard,
            &mut guard,
            &handle,
            &notifier,
            &mut signature,
            &mut noise,
            &mut done,
            &connection,
        )
        .await;
        assert_eq!(noise.is_some(), case == "eligible", "{case}");
        assert_eq!(done, case == "already-done");
    }
    server.finish().await;
    assert_eq!(signature, "unchanged");
}

#[tokio::test]
async fn ipc_keeps_in_memory_pause_and_last_binary_item_available_when_state_is_corrupt() {
    let peer = Peer::new("http://127.0.0.1:1", "ipc-recovery", 53);
    std::fs::write(peer.app.store.paths.state_file(), b"not JSON").unwrap();
    let handle = sync_handle();
    for (request, paused) in [(IpcRequest::Pause, true), (IpcRequest::Resume, false)] {
        assert!(handle_ipc(&peer.app.store, &handle, request).await.ok);
        assert_eq!(handle.paused.load(Ordering::Relaxed), paused);
        assert_eq!(
            std::fs::read(peer.app.store.paths.state_file()).unwrap(),
            b"not JSON"
        );
    }
    let empty = handle_ipc(&peer.app.store, &handle, IpcRequest::Pull).await;
    assert!(empty.ok && empty.pull.is_none());
    *handle.last_item.lock() = Some(ClipboardItem::Image {
        mime: ImageMime::Png,
        bytes: PNG.into(),
        width: None,
        height: None,
    });
    let image = handle_ipc(&peer.app.store, &handle, IpcRequest::Pull)
        .await
        .pull
        .unwrap();
    assert_eq!(image.kind, "image");
    assert!(image.text.is_none());
    assert!(
        handle_ipc(&peer.app.store, &handle, IpcRequest::Shutdown)
            .await
            .ok
    );
    let status = handle_ipc(&peer.app.store, &handle, IpcRequest::Status)
        .await
        .status
        .unwrap();
    assert!(status.room_id.is_none());
    assert_eq!(status.device_id, Some(peer.identity.device_id.clone()));
    assert_eq!(status.last_item_kind, Some(ItemKind::Image));
}

#[tokio::test]
async fn create_failure_retains_the_offer_and_error_for_background_status() {
    use crate::test_relay::{receive, send, Server};
    use axum::{routing::post, Json, Router};
    let room = RoomId::new();
    let id = room.clone();
    let (consumed, release) = tokio::sync::oneshot::channel();
    let mut server = Server::websocket_routes(
        Router::new().route(
            "/v1/pairing/create",
            post(move || {
                let room_id = id.clone();
                async move {
                    Json(CreatePairingResponse {
                        room_id,
                        pairing_code: "482913".into(),
                        pairing_token: "fixture".into(),
                        expires_at: Utc::now() + ChronoDuration::seconds(60),
                        ws_path: "/v1/ws".into(),
                    })
                }
            }),
        ),
        move |mut socket| async move {
            assert!(matches!(
                receive(&mut socket).await,
                ControlMessage::Hello {
                    role: ClientRole::PairingA,
                    ..
                }
            ));
            send(
                &mut socket,
                ControlMessage::Error {
                    code: "expired".into(),
                    message: "create deadline passed".into(),
                },
            )
            .await;
            let _ = release.await;
        },
    )
    .await;
    let peer = Peer::new(&server.url, "create-failure", 54);
    let printed = Arc::new(Mutex::new(None));
    let offer = printed.clone();
    let error = peer
        .app
        .room_create(Some(Duration::from_secs(999)), true, false, move |value| {
            *offer.lock() = Some(value.pairing_code.clone());
        })
        .await
        .err()
        .unwrap();
    let _ = consumed.send(());
    server.finish().await;
    assert!(error
        .to_string()
        .contains("expired: create deadline passed"));
    assert_eq!(printed.lock().as_deref(), Some("482913"));
    let saved = peer.app.store.load_pairing_wait().unwrap().unwrap();
    assert_eq!(saved.status, "failed");
    assert_eq!(saved.room_id, room.to_string());
    assert_eq!(saved.error.as_deref(), Some(error.to_string().as_str()));
    assert_eq!(
        peer.app.status().await.unwrap()["pairing"]["status"],
        "failed"
    );
    assert!(peer.app.store.load_state().unwrap().rooms.is_empty());
}

#[test]
fn app_pairing_enables_sync_and_supports_one_shot_pull_without_installing_a_service() {
    const NAME: &str = "engine::behavior_tests::app_pairing_enables_sync_and_supports_one_shot_pull_without_installing_a_service";
    if !in_worker(NAME) {
        worker(NAME, b"", &[]);
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let pair = Pair::new().await;
        let (sent, received) = tokio::sync::oneshot::channel();
        let offer_channel = Arc::new(Mutex::new(Some(sent)));
        let app = pair.a.app.clone();
        let creator = tokio::spawn(async move {
            app.room_create(Some(Duration::from_secs(60)), false, true, move |offer| {
                let _ = offer_channel
                    .lock()
                    .take()
                    .unwrap()
                    .send(offer.pairing_code.clone());
            })
            .await
        });
        let code = tokio::time::timeout(Duration::from_secs(5), received)
            .await
            .unwrap()
            .unwrap();
        let joined =
            tokio::time::timeout(Duration::from_secs(10), pair.b.app.room_join(&code, false))
                .await
                .unwrap()
                .unwrap();
        let created = tokio::time::timeout(Duration::from_secs(10), creator)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(created.room_id, joined.room_id);
        let state = pair.a.app.store.load_pairing_wait().unwrap().unwrap();
        assert_eq!(state.status, "paired");
        for peer in [&pair.a, &pair.b] {
            let cfg = peer.app.store.load_config().unwrap();
            assert!(cfg.sync.enabled && cfg.sync.auto_start);
            assert_eq!(cfg.sync.direction, SyncDirection::Both);
        }
        wait_until(|| client_connection_count(&pair.url, 0)).await;
        oneshot_push(
            &pair.a.app.store,
            ClipboardItem::Text {
                text: "facade fallback pull".into(),
            },
        )
        .await
        .unwrap();
        let pulled = pair.b.app.pull(false, false, None).await.unwrap();
        assert_eq!(pulled.text.as_deref(), Some("facade fallback pull"));
    });
}

#[tokio::test]
async fn every_engine_entrypoint_preserves_invalid_local_state_and_reports_preflight_failures() {
    for case in ["config", "identity", "room", "epoch", "connection"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = listener.local_addr().unwrap();
        drop(listener);
        let peer = scripted_peer(&format!("http://{closed}"), RoomId::new());
        let room = peer.app.store.current_room().unwrap();
        let path = match case {
            "config" => peer.app.store.paths.config_file(),
            "identity" => peer.app.store.paths.identity_file(),
            "room" => peer.app.store.paths.state_file(),
            _ => peer
                .app
                .store
                .paths
                .data_dir
                .join(format!("epoch-{}.secret", room.room_id)),
        };
        if case != "connection" {
            std::fs::write(&path, b"truncated local data").unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let clipboard = SystemClipboard::mock(MockClipboard::new());
        let handle = sync_handle();
        let (_, mut shutdown) = watch::channel(false);
        let session = run_session(&peer.app.store, &clipboard, &handle, &mut shutdown).await;
        let push = oneshot_push(
            &peer.app.store,
            ClipboardItem::Text {
                text: "must not send".into(),
            },
        )
        .await;
        let pull = oneshot_pull(&peer.app.store, false, false, None).await;
        assert!(session.is_err() && push.is_err() && pull.is_err(), "{case}");
        assert!(!handle.connected.load(Ordering::Relaxed));
        assert!(handle.last_item.lock().is_none());
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "{case} failure rewrote user data"
        );
    }
}

#[tokio::test]
async fn file_push_detects_image_mime_and_rejects_non_utf8_names_before_network_access() {
    let directory = TempDir::new().unwrap();
    let image = directory.path().join("photo.png");
    std::fs::write(&image, PNG).unwrap();
    let item = crate::load_push_item(None, std::slice::from_ref(&image), false)
        .await
        .unwrap();
    let ClipboardItem::Files { files } = item else {
        panic!("file push changed item kind")
    };
    assert_eq!(files[0].mime.as_deref(), Some("image/png"));
    assert_eq!(files[0].staged_path.as_ref(), Some(&image));
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::ffi::OsStringExt;
        let path = directory
            .path()
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        std::fs::write(&path, b"preserve file").unwrap();
        assert!(
            crate::load_push_item(None, std::slice::from_ref(&path), false)
                .await
                .unwrap_err()
                .to_string()
                .contains("invalid file name")
        );
        assert_eq!(std::fs::read(path).unwrap(), b"preserve file");
    }
}

#[test]
fn command_facade_reports_corrupt_config_and_state_without_replacing_them() {
    const NAME: &str = "engine::behavior_tests::command_facade_reports_corrupt_config_and_state_without_replacing_them";
    if !in_worker(NAME) {
        worker(NAME, b"", &[]);
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let peer = Peer::new("http://127.0.0.1:1", "facade-preflight", 55);
        let config = peer.app.store.paths.config_file();
        std::fs::write(&config, b"invalid = [").unwrap();
        assert!(peer.app.config_get(None).is_err());
        assert!(peer.app.config_set("sync.enabled", "true").is_err());
        assert!(peer.app.status().await.is_err());
        assert!(peer
            .app
            .room_create(None, true, false, |_| {})
            .await
            .is_err());
        assert!(peer.app.room_join("123456", true).await.is_err());
        assert_eq!(std::fs::read(&config).unwrap(), b"invalid = [");
        peer.app.store.save_config(&AppConfig::default()).unwrap();
        let state = peer.app.store.paths.state_file();
        std::fs::write(&state, b"invalid state").unwrap();
        assert!(peer.app.room_list().is_err());
        assert!(peer.app.devices().is_err());
        assert!(peer.app.status().await.is_err());
        assert!(peer.app.pause().await.is_err());
        assert!(peer.app.resume().await.is_err());
        assert_eq!(std::fs::read(state).unwrap(), b"invalid state");
    });
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_local_metadata_reports_failed_mutations_without_losing_saved_values() {
    use std::os::unix::fs::PermissionsExt;
    let peer = Peer::new("http://127.0.0.1:1", "read-only-metadata", 57);
    peer.app.store.save_state(&Default::default()).unwrap();
    let before_state = std::fs::read(peer.app.store.paths.state_file()).unwrap();
    std::fs::set_permissions(
        &peer.app.store.paths.data_dir,
        std::fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let pause = peer.app.pause().await;
    let resume = peer.app.resume().await;
    std::fs::set_permissions(
        &peer.app.store.paths.data_dir,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(pause.is_err() && resume.is_err());
    assert_eq!(
        std::fs::read(peer.app.store.paths.state_file()).unwrap(),
        before_state
    );
    let before_config = std::fs::read(peer.app.store.paths.config_file()).unwrap();
    std::fs::set_permissions(
        &peer.app.store.paths.config_dir,
        std::fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let setting = peer.app.config_set("sync.enabled", "false");
    let init = peer.app.init(Some("http://127.0.0.1:2".into()));
    std::fs::set_permissions(
        &peer.app.store.paths.config_dir,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(setting.is_err() && init.is_err());
    assert_eq!(
        std::fs::read(peer.app.store.paths.config_file()).unwrap(),
        before_config
    );
}

#[tokio::test]
async fn terminal_push_uses_explicit_clipboard_input_and_reports_empty_unavailable_or_missing_input(
) {
    let mock = MockClipboard::new();
    mock.set(ClipboardItem::Text {
        text: "private mock clipboard".into(),
    });
    let item = crate::load_push_input(
        None,
        true,
        &mut std::io::Cursor::new(b"not stdin"),
        true,
        || Ok(SystemClipboard::mock(mock.clone())),
    )
    .await
    .unwrap();
    assert!(matches!(item, ClipboardItem::Text { text } if text == "private mock clipboard"));
    let empty = MockClipboard::new();
    assert!(
        crate::load_push_input(Some("text"), false, &mut std::io::empty(), true, || Ok(
            SystemClipboard::mock(empty)
        ))
        .await
        .unwrap_err()
        .to_string()
        .contains("clipboard is empty")
    );
    assert!(
        crate::load_push_input(None, true, &mut std::io::empty(), true, || Err(
            clipsync_clipboard::ClipboardError::Unavailable("fixture adapter".into())
        ))
        .await
        .unwrap_err()
        .to_string()
        .contains("fixture adapter")
    );
    let missing = crate::load_push_input(None, false, &mut std::io::empty(), true, || {
        panic!("implicit terminal input must not open the clipboard")
    })
    .await
    .unwrap_err();
    assert!(missing.to_string().contains("nothing to push"));
    struct BrokenInput;
    impl std::io::Read for BrokenInput {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("stdin read failed"))
        }
    }
    assert!(
        crate::load_push_input(None, false, &mut BrokenInput, false, || panic!(
            "piped input must not open the clipboard"
        ))
        .await
        .unwrap_err()
        .to_string()
        .contains("stdin read failed")
    );
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

#[tokio::test]
async fn pull_reports_output_file_failures_for_each_item_kind_without_overwriting_directories() {
    let root = TempDir::new().unwrap();
    for (name, item) in [
        (
            "clipboard.txt",
            ClipboardItem::Text {
                text: "private text".into(),
            },
        ),
        (
            "clipboard.png",
            ClipboardItem::Image {
                mime: ImageMime::Png,
                bytes: PNG.to_vec(),
                width: None,
                height: None,
            },
        ),
        (
            "report.txt",
            ClipboardItem::Files {
                files: vec![FileRef {
                    name: "report.txt".into(),
                    bytes: b"private file".to_vec(),
                    mime: None,
                    staged_path: None,
                }],
            },
        ),
    ] {
        let output = root.path().join(name);
        std::fs::create_dir(&output).unwrap();
        let blocked = output.join(name);
        std::fs::create_dir(&blocked).unwrap();
        assert!(matches!(
            finish_pull(item, false, Some(output)).await,
            Err(CoreError::Io(_))
        ));
        assert!(blocked.is_dir());
        assert_eq!(std::fs::read_dir(blocked).unwrap().count(), 0);
    }
}

#[test]
fn manifest_rejection_distinguishes_encoding_authentication_and_authenticated_json_errors() {
    let routing = text_routing();
    let nonce = [6; 12];
    assert!(matches!(
        open_manifest(&EPOCH, &routing, &hex::encode(nonce), "!"),
        Err(CoreError::Message(_))
    ));
    assert!(matches!(
        open_manifest(&EPOCH, &routing, &hex::encode(nonce), "00"),
        Err(CoreError::Crypto(_))
    ));
    let key = derive_content_key(&EPOCH, routing.transfer_id.as_str()).unwrap();
    let ciphertext = seal(
        &key,
        &nonce,
        format!("{}:manifest", routing.transfer_id).as_bytes(),
        b"not JSON",
    )
    .unwrap();
    assert!(matches!(
        open_manifest(
            &EPOCH,
            &routing,
            &hex::encode(nonce),
            &hex::encode(ciphertext)
        ),
        Err(CoreError::Json(_))
    ));
}

#[tokio::test]
async fn closed_relay_senders_report_failure_for_text_images_and_file_transfers() {
    use crate::test_relay::Server;
    let mut server = Server::websocket(|mut socket| async move {
        socket
            .send(axum::extract::ws::Message::Close(None))
            .await
            .unwrap();
    })
    .await;
    let peer = scripted_peer(&server.url, RoomId::new());
    let mut connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), connection.incoming.recv())
            .await
            .unwrap()
            .is_none()
    );
    let room = peer.app.store.current_room().unwrap();
    for item in [
        ClipboardItem::Text {
            text: "must not silently succeed".into(),
        },
        ClipboardItem::Image {
            mime: ImageMime::Png,
            bytes: PNG.to_vec(),
            width: None,
            height: None,
        },
        ClipboardItem::Files {
            files: vec![FileRef {
                name: "report.txt".into(),
                bytes: b"private file".to_vec(),
                mime: None,
                staged_path: None,
            }],
        },
    ] {
        assert!(matches!(
            send_item(&connection, &peer.identity, &room, &EPOCH, 1, &item).await,
            Err(CoreError::Transport(
                clipsync_transport::TransportError::Closed
            ))
        ));
    }
    server.finish().await;
}

#[tokio::test]
async fn session_setup_propagates_rejected_hello_and_inbox_failures_without_clipboard_mutation() {
    use crate::test_relay::{receive, send, Server};
    for mode in [
        "push-rejected",
        "session-rejected",
        "session-inbox",
        "pull-inbox",
    ] {
        let room = RoomId::new();
        let room_id = room.clone();
        let (consumed, release) = tokio::sync::oneshot::channel();
        let mut server = Server::websocket(move |mut socket| async move {
            assert!(matches!(
                receive(&mut socket).await,
                ControlMessage::Hello {
                    role: ClientRole::Member,
                    ..
                }
            ));
            send(
                &mut socket,
                if mode.ends_with("rejected") {
                    ControlMessage::Error {
                        code: "revoked".into(),
                        message: "device revoked".into(),
                    }
                } else {
                    hello(&room)
                },
            )
            .await;
            let _ = release.await;
        })
        .await;
        let peer = scripted_peer(&server.url, room_id);
        if mode.ends_with("inbox") {
            std::fs::remove_dir(&peer.app.store.paths.inbox_dir).unwrap();
            std::fs::write(&peer.app.store.paths.inbox_dir, b"preserve inbox marker").unwrap();
        }
        let clipboard = MockClipboard::new();
        let handle = sync_handle();
        let (_, mut shutdown) = watch::channel(false);
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            match mode {
                "push-rejected" => {
                    oneshot_push(
                        &peer.app.store,
                        ClipboardItem::Text {
                            text: "must not send".into(),
                        },
                    )
                    .await
                }
                "pull-inbox" => oneshot_pull(&peer.app.store, false, true, None)
                    .await
                    .map(|_| ()),
                _ => {
                    run_session(
                        &peer.app.store,
                        &SystemClipboard::mock(clipboard.clone()),
                        &handle,
                        &mut shutdown,
                    )
                    .await
                }
            }
        })
        .await
        .unwrap();
        let _ = consumed.send(());
        server.finish().await;
        let error = result.unwrap_err();
        if mode.ends_with("rejected") {
            assert!(error.to_string().contains("relay revoked: device revoked"));
        } else {
            assert!(matches!(error, CoreError::Io(_)));
            assert_eq!(
                std::fs::read(&peer.app.store.paths.inbox_dir).unwrap(),
                b"preserve inbox marker"
            );
        }
        assert!(clipboard.get().is_none());
        assert!(!handle.connected.load(Ordering::Relaxed));
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn daemon_start_facade_uses_isolated_service_managers_and_propagates_installer_errors() {
    const NAME: &str = "engine::behavior_tests::daemon_start_facade_uses_isolated_service_managers_and_propagates_installer_errors";
    if !in_worker(NAME) {
        use std::os::unix::fs::PermissionsExt;
        for blocked in [false, true] {
            let root = TempDir::new().unwrap();
            let home = root.path().join("home");
            if blocked {
                std::fs::write(&home, b"preserve home marker").unwrap();
            } else {
                std::fs::create_dir(&home).unwrap();
            }
            let tools = root.path().join("tools");
            std::fs::create_dir(&tools).unwrap();
            for name in ["launchctl", "systemctl"] {
                let path = tools.join(name);
                std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            worker(
                NAME,
                b"",
                &[
                    ("HOME", home.to_str().unwrap()),
                    ("USERPROFILE", home.to_str().unwrap()),
                    ("PATH", tools.to_str().unwrap()),
                    ("CLIPSYNC_NO_DAEMON", "0"),
                    (
                        "CLIPSYNC_CORE_SERVICE_BLOCKED",
                        if blocked { "1" } else { "0" },
                    ),
                ],
            );
            if blocked {
                assert_eq!(std::fs::read(home).unwrap(), b"preserve home marker");
            }
        }
        return;
    }
    let peer = Peer::new("http://127.0.0.1:1", "daemon-facade", 58);
    let result = crate::maybe_start_daemon(&peer.app.store);
    if std::env::var("CLIPSYNC_CORE_SERVICE_BLOCKED").as_deref() == Ok("1") {
        assert!(matches!(result, Err(CoreError::Daemon(_))));
    } else {
        result.unwrap();
        assert!(clipsync_daemon::is_service_installed());
        assert!(String::from_utf8(product_logs().lock().clone())
            .unwrap()
            .contains("daemon start requested"));
    }
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
