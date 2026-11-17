// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::net::SocketAddr;
use std::time::Duration;

use clipsync_clipboard::{ClipboardItem, FileRef, ImageMime, MockClipboard, SystemClipboard};
use clipsync_core::{begin_create_room, join_room, App, SasCheck, SasDecision};
use clipsync_crypto::blake3_hex;
use clipsync_protocol::{
    ClientRole, ControlMessage, CHUNK_SIZE, FILE_MAX_COUNT, FRAME_HARD_CAP, IMAGE_MAX_BYTES,
    PROTOCOL_VERSION, TEXT_MAX_BYTES,
};
use clipsync_relay::{router, MetricsToken, RelayConfig};

/// Scrape credential for this fixture's relay; readiness polls read `/metrics`.
const METRICS_TEST_TOKEN: &str = "e2e-metrics-scrape-token-0123456789";
use clipsync_storage::AppPaths;
use clipsync_transfer::safe_filename;
use tempfile::TempDir;
use tokio::net::TcpListener;

const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

// These fixtures use loopback HTTP, so they need neither proxies nor TLS roots.
fn loopback_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .tls_built_in_root_certs(false)
        .build()
        .expect("loopback HTTP test client")
}

async fn start_relay() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = RelayConfig {
        bind: addr,
        public_url: format!("http://{addr}"),
        token_secret: b"test-secret-test-secret-test-sec".to_vec(),
        webhook_url: None,
        webhook_secret: None,
        metrics_token: Some(MetricsToken::new(METRICS_TEST_TOKEN).unwrap()),
    };
    let app = router(cfg);
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let url = format!("http://{addr}");
    let client = loopback_http_client();
    for _ in 0..50 {
        if client.get(format!("{url}/healthz")).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (url, handle)
}

fn app_in(dir: &TempDir, relay: &str) -> App {
    let app = App::open_from(AppPaths::from_root(dir.path().to_path_buf())).unwrap();
    let identity = clipsync_crypto::DeviceIdentity::generate();
    // File-backed test identities avoid serialized OS authorization during a
    // parallel workspace run. Pairing still uses the real crypto and relay.
    std::fs::write(
        app.store.paths.identity_file(),
        serde_json::to_vec(&identity.to_stored()).unwrap(),
    )
    .unwrap();
    app.init(Some(relay.to_string())).unwrap();
    app
}

fn store_in(dir: &TempDir) -> clipsync_storage::LocalStore {
    App::open_from(AppPaths::from_root(dir.path().to_path_buf()))
        .unwrap()
        .store
}

type SasConfirm = Box<
    dyn FnOnce(SasCheck) -> std::pin::Pin<Box<dyn std::future::Future<Output = SasDecision> + Send>>
        + Send,
>;

/// Two confirmers that compare SAS values the way two users would: each
/// device shows its value to the other and confirms only on a match.
fn sas_comparison() -> (SasConfirm, SasConfirm) {
    fn side(
        mine: tokio::sync::oneshot::Sender<String>,
        theirs: tokio::sync::oneshot::Receiver<String>,
    ) -> SasConfirm {
        Box::new(move |check: SasCheck| {
            Box::pin(async move {
                let _ = mine.send(check.sas.clone());
                match theirs.await {
                    Ok(other) if check.matches(&other) => SasDecision::Confirmed,
                    _ => SasDecision::Rejected,
                }
            })
        })
    }
    let (creator_tx, creator_rx) = tokio::sync::oneshot::channel();
    let (joiner_tx, joiner_rx) = tokio::sync::oneshot::channel();
    (side(creator_tx, joiner_rx), side(joiner_tx, creator_rx))
}

fn stored_secrets(dir: &TempDir) -> Vec<String> {
    let store = store_in(dir);
    std::fs::read_dir(&store.paths.data_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".secret"))
        .collect()
}

// Two peers share this runtime; keep workers free for I/O during OS keychain calls.
async fn pair_homes(relay: &str) -> (TempDir, TempDir) {
    let a_dir = TempDir::new().unwrap();
    let b_dir = TempDir::new().unwrap();
    let app_a = app_in(&a_dir, relay);
    let app_b = app_in(&b_dir, relay);
    let id_a = app_a.identity().unwrap();
    let id_b = app_b.identity().unwrap();
    let cfg_a = app_a.store.load_config().unwrap();
    let cfg_b = app_b.store.load_config().unwrap();

    let pending = begin_create_room(&id_a, &cfg_a, 60).await.unwrap();
    let code = pending.offer.pairing_code.clone();
    assert_eq!(code.len(), 6);

    let store_a = store_in(&a_dir);
    let (creator_sas, joiner_sas) = sas_comparison();
    let wait_a = tokio::spawn(async move { pending.wait(&store_a, &id_a, creator_sas).await });
    wait_for_connection_count(relay, 1).await;
    let creator = async {
        wait_a
            .await
            .map_err(|_| "creator pairing task failed".to_string())?
            .map_err(|error| format!("creator pairing failed: {error}"))
    };
    let joiner = async {
        join_room(&app_b.store, &id_b, &cfg_b, &code, joiner_sas)
            .await
            .map(|_| ())
            .map_err(|error| format!("joiner pairing failed: {error}"))
    };
    tokio::try_join!(creator, joiner).expect("both peers complete pairing");
    assert!(store_in(&a_dir).current_room().is_ok());
    assert!(store_in(&b_dir).current_room().is_ok());
    (a_dir, b_dir)
}

// Observe the real peers' outbound pairing control frames without decrypting them.
async fn start_pairing_observer(
    relay: &str,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::routing::{any, get};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as UpstreamMessage;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (bind_tx, bind_rx) = tokio::sync::mpsc::unbounded_channel();
    let websocket_url = format!("{}/v1/ws", relay.replacen("http://", "ws://", 1));
    let http_url = relay.to_string();
    let http_client = loopback_http_client();
    let proxy = axum::Router::new()
        .route(
            "/v1/ws",
            get(move |upgrade: WebSocketUpgrade| {
                let upstream = websocket_url.clone();
                let binds = bind_tx.clone();
                async move {
                    upgrade.on_upgrade(move |client| async move {
                        let (upstream, _) =
                            tokio_tungstenite::connect_async(upstream).await.unwrap();
                        let (mut client_tx, mut client_rx) = client.split();
                        let (mut upstream_tx, mut upstream_rx) = upstream.split();
                        let outbound = async {
                            while let Some(Ok(message)) = client_rx.next().await {
                                match message {
                                    Message::Text(text) => {
                                        if let Ok(clipsync_protocol::ControlMessage::DeviceBind {
                                            nonce,
                                            ..
                                        }) = clipsync_protocol::ControlMessage::decode(&text)
                                        {
                                            binds.send(nonce).unwrap();
                                        }
                                        if upstream_tx
                                            .send(UpstreamMessage::Text(text.to_string().into()))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Message::Close(_) => break,
                                    _ => {}
                                }
                            }
                        };
                        let inbound = async {
                            while let Some(Ok(message)) = upstream_rx.next().await {
                                match message {
                                    UpstreamMessage::Text(text) => {
                                        if client_tx
                                            .send(Message::Text(text.to_string().into()))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    UpstreamMessage::Close(_) => break,
                                    _ => {}
                                }
                            }
                        };
                        tokio::select! {
                            _ = outbound => {}
                            _ = inbound => {}
                        }
                    })
                }
            }),
        )
        .fallback(any(move |request: axum::extract::Request| {
            let upstream = format!("{http_url}{}", request.uri());
            let client = http_client.clone();
            async move {
                let method = request.method().clone();
                // Forward the scrape credential so readiness polls reach `/metrics`.
                let authorization = request
                    .headers()
                    .get(axum::http::header::AUTHORIZATION)
                    .cloned();
                let body = axum::body::to_bytes(request.into_body(), FRAME_HARD_CAP)
                    .await
                    .unwrap();
                let mut upstream_request = client
                    .request(method, upstream)
                    .header("content-type", "application/json");
                if let Some(authorization) = authorization {
                    upstream_request =
                        upstream_request.header(axum::http::header::AUTHORIZATION, authorization);
                }
                let response = upstream_request.body(body).send().await.unwrap();
                (response.status(), response.bytes().await.unwrap())
            }
        }));
    tokio::spawn(async move { axum::serve(listener, proxy).await.unwrap() });
    (format!("http://{addr}"), bind_rx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_device_bind_nonces_are_unique() {
    let (relay, _relay_task) = start_relay().await;
    let (observed_relay, mut binds) = start_pairing_observer(&relay).await;
    // Allow for synchronous OS keychain setup and persistence in this full fixture.
    let _homes = tokio::time::timeout(Duration::from_secs(30), pair_homes(&observed_relay))
        .await
        .expect("pairing through the observer timed out");
    let first = binds.try_recv().expect("first peer sent DeviceBind");
    let second = binds.try_recv().expect("second peer sent DeviceBind");
    assert_ne!(first, second, "pairing peers reused a DeviceBind nonce");
}

#[tokio::test]
async fn healthz_ok() {
    let (relay, _h) = start_relay().await;
    let resp = loopback_http_client()
        .get(format!("{relay}/healthz"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_and_text_push_pull() {
    let (relay, _h) = start_relay().await;
    let (a_dir, b_dir) = pair_homes(&relay).await;

    clipsync_core::engine_oneshot_push(
        &store_in(&a_dir),
        ClipboardItem::Text {
            text: "hello from a".into(),
        },
    )
    .await
    .unwrap();

    let pulled = clipsync_core::engine_oneshot_pull(&store_in(&b_dir), false, true, None)
        .await
        .unwrap();
    assert_eq!(pulled.text.as_deref(), Some("hello from a"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_push_pull_writes_output() {
    let (relay, _h) = start_relay().await;
    let (a_dir, b_dir) = pair_homes(&relay).await;
    let out = b_dir.path().join("inbox");
    let store_b = store_in(&b_dir);
    let out_clone = out.clone();
    let pull = tokio::spawn(async move {
        clipsync_core::engine_oneshot_pull(&store_b, false, true, Some(out_clone)).await
    });
    tokio::time::sleep(Duration::from_millis(400)).await;

    clipsync_core::engine_oneshot_push(
        &store_in(&a_dir),
        ClipboardItem::Files {
            files: vec![FileRef {
                name: "report.txt".into(),
                bytes: b"file-payload-ok\n".to_vec(),
                mime: Some("text/plain".into()),
                staged_path: None,
            }],
        },
    )
    .await
    .unwrap();

    let pulled = pull.await.unwrap().expect("file pull");
    assert_eq!(pulled.kind, "files");
    let saved = std::fs::read_to_string(out.join("report.txt")).unwrap();
    assert_eq!(saved, "file-payload-ok\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn image_push_pull_writes_output() {
    let (relay, _h) = start_relay().await;
    let (a_dir, b_dir) = pair_homes(&relay).await;
    let out = b_dir.path().join("inbox");
    let store_b = store_in(&b_dir);
    let out_clone = out.clone();
    let pull = tokio::spawn(async move {
        clipsync_core::engine_oneshot_pull(&store_b, false, true, Some(out_clone)).await
    });
    tokio::time::sleep(Duration::from_millis(400)).await;

    clipsync_core::engine_oneshot_push(
        &store_in(&a_dir),
        ClipboardItem::Image {
            mime: ImageMime::Png,
            bytes: TINY_PNG.to_vec(),
            width: Some(1),
            height: Some(1),
        },
    )
    .await
    .unwrap();

    let pulled = pull.await.unwrap().expect("image pull");
    assert_eq!(pulled.kind, "image");
    let saved = std::fs::read(out.join("clipboard.png")).unwrap();
    assert_eq!(saved, TINY_PNG);
}

type RelaySocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// Bounds a single relay round trip on loopback; waits are condition-driven.
const SESSION_FRAME_DEADLINE: Duration = Duration::from_secs(10);
// Enough chunks that the push spans many relay frames, not one.
const MULTI_CHUNK_PUSH_CHUNKS: usize = 4;

/// Opens a room socket the way the sync daemon's session does (a device-token
/// member hello for the home's current room) and waits for `HelloOk`.
async fn daemon_session(dir: &TempDir) -> RelaySocket {
    use futures_util::SinkExt;
    let app = App::open_from(AppPaths::from_root(dir.path().to_path_buf())).unwrap();
    let room = app.store.current_room().unwrap();
    let identity = app.identity().unwrap();
    let relay = app.store.load_config().unwrap().relay_url;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("{}/v1/ws", relay.replacen("http", "ws", 1)))
            .await
            .unwrap();
    let hello = ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: identity.device_id.clone(),
        device_name: "daemon session".into(),
        os: "fixture".into(),
        role: ClientRole::Member,
        room_id: Some(room.room_id.clone()),
        token: room.device_token.clone(),
        pairing_session_id: None,
    };
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            hello.encode().unwrap().into(),
        ))
        .await
        .unwrap();
    session_frames_until(&mut socket, |m| matches!(m, ControlMessage::HelloOk { .. }))
        .await
        .expect("daemon session closed before HelloOk");
    socket
}

/// Control frames received before one matching `done`, or `None` if the relay
/// closed the socket first.
async fn session_frames_until(
    socket: &mut RelaySocket,
    done: impl Fn(&ControlMessage) -> bool,
) -> Option<Vec<ControlMessage>> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    tokio::time::timeout(SESSION_FRAME_DEADLINE, async {
        let mut before = Vec::new();
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let message = ControlMessage::decode(&text).unwrap();
                    if done(&message) {
                        return Some(before);
                    }
                    before.push(message);
                }
                Some(Ok(Message::Ping(bytes))) => {
                    let _ = socket.send(Message::Pong(bytes)).await;
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("daemon session frame deadline")
}

/// Requires the daemon session to still be open and returns what it received.
async fn daemon_still_open(socket: &mut RelaySocket, ts: i64) -> Vec<ControlMessage> {
    use futures_util::SinkExt;
    // A socket the relay already closed may refuse the write; the read below
    // then reports the close.
    let _ = socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            ControlMessage::Ping { ts }.encode().unwrap().into(),
        ))
        .await;
    session_frames_until(
        socket,
        |m| matches!(m, ControlMessage::Pong { ts: received } if *received == ts),
    )
    .await
    .expect("the relay closed the daemon session of a device running a CLI command")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oneshot_pull_works_while_the_device_daemon_session_reconnects() {
    let (relay, _h) = start_relay().await;
    let (a_dir, b_dir) = pair_homes(&relay).await;
    wait_for_connection_count(&relay, 0).await;
    let mut daemon = daemon_session(&b_dir).await;
    wait_for_connection_count(&relay, 1).await;

    // `clipsync pull --wait` falls back to its own socket for the same device
    // when the daemon has no item yet.
    let store_b = store_in(&b_dir);
    let pull = tokio::spawn(async move {
        clipsync_core::engine_oneshot_pull(&store_b, false, true, None).await
    });
    wait_for_connection_count(&relay, 2).await;
    daemon_still_open(&mut daemon, 1).await;

    // The daemon's session drops and reconnects while the pull is waiting.
    let mut reconnected = daemon_session(&b_dir).await;
    wait_for_connection_count(&relay, 3).await;
    daemon.close(None).await.unwrap();
    wait_for_connection_count(&relay, 2).await;

    clipsync_core::engine_oneshot_push(
        &store_in(&a_dir),
        ClipboardItem::Text {
            text: "pulled beside the daemon".into(),
        },
    )
    .await
    .unwrap();
    let pulled = pull.await.unwrap().expect("pull beside the daemon session");
    assert_eq!(pulled.text.as_deref(), Some("pulled beside the daemon"));
    let daemon_frames = daemon_still_open(&mut reconnected, 2).await;
    assert!(
        daemon_frames
            .iter()
            .any(|m| matches!(m, ControlMessage::TextEnvelope { .. })),
        "the daemon session must receive the text too: {daemon_frames:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_chunk_oneshot_push_works_while_the_device_daemon_session_reconnects() {
    let (relay, _h) = start_relay().await;
    let (a_dir, b_dir) = pair_homes(&relay).await;
    wait_for_connection_count(&relay, 0).await;
    let mut daemon = daemon_session(&a_dir).await;
    wait_for_connection_count(&relay, 1).await;

    let out = b_dir.path().join("inbox");
    let store_b = store_in(&b_dir);
    let out_owned = out.clone();
    let pull = tokio::spawn(async move {
        clipsync_core::engine_oneshot_pull(&store_b, false, true, Some(out_owned)).await
    });
    wait_for_connection_count(&relay, 2).await;

    // `clipsync push` sends files over its own socket even with a daemon up.
    let payload: Vec<u8> = (0..CHUNK_SIZE * MULTI_CHUNK_PUSH_CHUNKS + 1)
        .map(|i| (i % 251) as u8)
        .collect();
    let item = ClipboardItem::Files {
        files: vec![FileRef {
            name: "beside-daemon.bin".into(),
            bytes: payload.clone(),
            mime: Some("application/octet-stream".into()),
            staged_path: None,
        }],
    };
    let store_a = store_in(&a_dir);
    let push =
        tokio::spawn(async move { clipsync_core::engine_oneshot_push(&store_a, item).await });
    // The daemon reconnects while the push is in flight.
    let mut reconnected = daemon_session(&a_dir).await;
    push.await.unwrap().expect("push beside the daemon session");

    let pulled = pull.await.unwrap().expect("multi-chunk pull");
    assert_eq!(pulled.kind, "files");
    assert_eq!(
        std::fs::read(out.join("beside-daemon.bin")).unwrap(),
        payload
    );
    daemon_still_open(&mut daemon, 1).await;
    daemon_still_open(&mut reconnected, 2).await;
}

fn encode_image(format: image::ImageFormat) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(2, 2, image::Rgb([12, 80, 160]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), format)
        .unwrap();
    buf
}

fn tiny_webp() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBPVP8L");
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&[0x2F, 0, 0, 0]);
    bytes
}

/// Wait for this test's relay to observe connection setup or teardown.
async fn wait_for_connection_count(relay: &str, expected: usize) {
    let client = loopback_http_client();
    let target = format!("clipsync_ws_connections {expected}");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let metrics = client
                .get(format!("{relay}/metrics"))
                .bearer_auth(METRICS_TEST_TOKEN)
                .send()
                .await
                .expect("relay metrics request")
                .error_for_status()
                .expect("relay metrics status")
                .text()
                .await
                .expect("relay metrics body");
            if metrics.lines().any(|line| line == target.as_str()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("relay did not reach {expected} connections"));
}

async fn transfer_item(a: &TempDir, b: &TempDir, item: ClipboardItem, out: &std::path::Path) {
    let store_b = store_in(b);
    let relay = store_b.load_config().unwrap().relay_url;
    wait_for_connection_count(&relay, 0).await;
    let out_owned = out.to_path_buf();
    let pull = tokio::spawn(async move {
        clipsync_core::engine_oneshot_pull(&store_b, false, true, Some(out_owned)).await
    });
    wait_for_connection_count(&relay, 1).await;
    clipsync_core::engine_oneshot_push(&store_in(a), item)
        .await
        .expect("push");
    pull.await.unwrap().expect("pull");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn format_matrix_images_roundtrip() {
    let (relay, _h) = start_relay().await;
    let (a_dir, b_dir) = pair_homes(&relay).await;
    let cases = [
        (
            "png",
            ImageMime::Png,
            encode_image(image::ImageFormat::Png),
            "clipboard.png",
        ),
        (
            "jpeg",
            ImageMime::Jpeg,
            encode_image(image::ImageFormat::Jpeg),
            "clipboard.jpg",
        ),
        (
            "tiff-as-file-not-here",
            ImageMime::Webp,
            tiny_webp(),
            "clipboard.webp",
        ),
    ];
    for (label, mime, bytes, filename) in cases {
        assert_eq!(ImageMime::detect(&bytes), Some(mime), "{label}");
        let out = b_dir.path().join(format!("img-{label}"));
        transfer_item(
            &a_dir,
            &b_dir,
            ClipboardItem::Image {
                mime,
                bytes: bytes.clone(),
                width: Some(2),
                height: Some(2),
            },
            &out,
        )
        .await;
        let saved = std::fs::read(out.join(filename)).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(saved, bytes, "{label} bytes changed in transit");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn format_matrix_files_roundtrip() {
    let (relay, _h) = start_relay().await;
    let (a_dir, b_dir) = pair_homes(&relay).await;
    let samples: Vec<(&str, &str, Vec<u8>)> = vec![
        ("notes.txt", "text/plain", b"plain text\n".to_vec()),
        ("sheet.csv", "text/csv", b"a,b\n1,2\n".to_vec()),
        ("data.json", "application/json", br#"{"ok":true}"#.to_vec()),
        ("page.html", "text/html", b"<p>hi</p>".to_vec()),
        (
            "icon.svg",
            "image/svg+xml",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
        ),
        ("feed.xml", "application/xml", b"<root/>".to_vec()),
        ("readme.md", "text/markdown", b"# title\n".to_vec()),
        ("style.css", "text/css", b"body{}".to_vec()),
        ("app.js", "text/javascript", b"console.log(1)\n".to_vec()),
        ("main.rs", "text/x-rust", b"fn main() {}\n".to_vec()),
        ("Main.kt", "text/x-kotlin", b"fun main() {}\n".to_vec()),
        ("App.swift", "text/x-swift", b"print(1)\n".to_vec()),
        ("app.py", "text/x-python", b"print(1)\n".to_vec()),
        ("run.sh", "text/x-shellscript", b"#!/bin/sh\n".to_vec()),
        ("setup.ps1", "text/plain", b"Write-Host hi\n".to_vec()),
        (
            "doc.pdf",
            "application/pdf",
            b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec(),
        ),
        ("scan.ps", "application/postscript", b"%!PS\n".to_vec()),
        ("note.rtf", "application/rtf", br"{\rtf1 hi}".to_vec()),
        (
            "book.epub",
            "application/epub+zip",
            b"PK\x03\x04epub".to_vec(),
        ),
        (
            "sheet.xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            b"PK\x03\x04xlsx".to_vec(),
        ),
        (
            "deck.pptx",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            b"PK\x03\x04pptx".to_vec(),
        ),
        (
            "memo.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            b"PK\x03\x04docx".to_vec(),
        ),
        (
            "text.odt",
            "application/vnd.oasis.opendocument.text",
            b"PK\x03\x04odt".to_vec(),
        ),
        (
            "archive.zip",
            "application/zip",
            b"PK\x03\x04\x14\x00".to_vec(),
        ),
        (
            "archive.tar.gz",
            "application/gzip",
            b"\x1f\x8b\x08\x00payload".to_vec(),
        ),
        (
            "archive.7z",
            "application/x-7z-compressed",
            b"7z\xBC\xAF\x27\x1C".to_vec(),
        ),
        ("disk.iso", "application/x-iso9660-image", b"CD001".to_vec()),
        ("photo.gif", "image/gif", b"GIF89a\x01\x00\x01\x00".to_vec()),
        ("photo.bmp", "image/bmp", b"BM\x00\x00\x00\x00".to_vec()),
        ("photo.tif", "image/tiff", b"II*\x00".to_vec()),
        (
            "photo.heic",
            "image/heic",
            b"\x00\x00\x00\x18ftypheic".to_vec(),
        ),
        (
            "photo.avif",
            "image/avif",
            b"\x00\x00\x00\x18ftypavif".to_vec(),
        ),
        ("clip.ico", "image/x-icon", b"\x00\x00\x01\x00".to_vec()),
        ("clip.webp", "image/webp", tiny_webp()),
        ("song.mp3", "audio/mpeg", b"ID3\x04\x00\x00".to_vec()),
        (
            "song.wav",
            "audio/wav",
            b"RIFF\x00\x00\x00\x00WAVE".to_vec(),
        ),
        ("song.flac", "audio/flac", b"fLaC".to_vec()),
        ("song.ogg", "audio/ogg", b"OggS".to_vec()),
        (
            "clip.mp4",
            "video/mp4",
            b"\x00\x00\x00\x18ftypisom".to_vec(),
        ),
        ("clip.webm", "video/webm", b"\x1A\x45\xDF\xA3".to_vec()),
        (
            "clip.mkv",
            "video/x-matroska",
            b"\x1A\x45\xDF\xA3mkv".to_vec(),
        ),
        (
            "clip.avi",
            "video/x-msvideo",
            b"RIFF\x00\x00\x00\x00AVI ".to_vec(),
        ),
        (
            "mod.wasm",
            "application/wasm",
            b"\0asm\x01\x00\x00\x00".to_vec(),
        ),
        (
            "db.sqlite",
            "application/vnd.sqlite3",
            b"SQLite format 3\x00".to_vec(),
        ),
        ("font.ttf", "font/ttf", b"\x00\x01\x00\x00".to_vec()),
        ("font.woff2", "font/woff2", b"wOFF2".to_vec()),
        (
            "app.exe",
            "application/vnd.microsoft.portable-executable",
            b"MZ".to_vec(),
        ),
        ("app.elf", "application/x-elf", b"\x7fELF".to_vec()),
        (
            "cert.pem",
            "application/x-pem-file",
            b"-----BEGIN CERTIFICATE-----\n".to_vec(),
        ),
        ("keys.p12", "application/x-pkcs12", b"\x30\x80".to_vec()),
        (
            "model.npy",
            "application/octet-stream",
            b"\x93NUMPY\x01\x00".to_vec(),
        ),
        (
            "table.parquet",
            "application/vnd.apache.parquet",
            b"PAR1".to_vec(),
        ),
        ("rows.avro", "application/avro", b"Obj\x01".to_vec()),
        (
            "config.toml",
            "application/toml",
            b"name = \"clipsync\"\n".to_vec(),
        ),
        (
            "config.yaml",
            "application/yaml",
            b"name: clipsync\n".to_vec(),
        ),
        (
            "my report.pdf",
            "application/pdf",
            b"%PDF-1.7 spaced\n".to_vec(),
        ),
        (
            "사진.png",
            "image/png",
            encode_image(image::ImageFormat::Png),
        ),
        (
            "blob.bin",
            "application/octet-stream",
            vec![0xA5; 300 * 1024],
        ),
    ];

    let out = b_dir.path().join("files");
    for chunk in samples.chunks(FILE_MAX_COUNT) {
        let files = chunk
            .iter()
            .map(|(name, mime, bytes)| FileRef {
                name: (*name).to_string(),
                bytes: bytes.clone(),
                mime: Some((*mime).to_string()),
                staged_path: None,
            })
            .collect();
        transfer_item(&a_dir, &b_dir, ClipboardItem::Files { files }, &out).await;
    }
    for (name, _, bytes) in &samples {
        let saved = std::fs::read(out.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(&saved, bytes, "{name} bytes changed in transit");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_relay_pairing_shows_both_devices_the_same_sas_and_names() {
    let (relay, _h) = start_relay().await;
    let a_dir = TempDir::new().unwrap();
    let b_dir = TempDir::new().unwrap();
    let app_a = app_in(&a_dir, &relay);
    let app_b = app_in(&b_dir, &relay);
    let (id_a, id_b) = (app_a.identity().unwrap(), app_b.identity().unwrap());
    let cfg_a = app_a.store.load_config().unwrap();
    let cfg_b = app_b.store.load_config().unwrap();
    let pending = begin_create_room(&id_a, &cfg_a, 60).await.unwrap();
    let code = pending.offer.pairing_code.clone();
    let (seen_a_tx, seen_a) = tokio::sync::oneshot::channel();
    let (seen_b_tx, seen_b) = tokio::sync::oneshot::channel();
    let (creator_sas, joiner_sas) = sas_comparison();
    let store_a = store_in(&a_dir);
    let creator_id = id_a.clone();
    let creator = tokio::spawn(async move {
        pending
            .wait(&store_a, &creator_id, move |check: SasCheck| {
                let _ = seen_a_tx.send(check.clone());
                creator_sas(check)
            })
            .await
    });
    wait_for_connection_count(&relay, 1).await;
    tokio::time::timeout(
        Duration::from_secs(20),
        join_room(
            &app_b.store,
            &id_b,
            &cfg_b,
            &code,
            move |check: SasCheck| {
                let _ = seen_b_tx.send(check.clone());
                joiner_sas(check)
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    creator.await.unwrap().unwrap();
    let (seen_a, seen_b) = (seen_a.await.unwrap(), seen_b.await.unwrap());
    assert_eq!(seen_a.sas, seen_b.sas);
    // An updated relay forwards the commitment, so both devices ran the
    // commitment round and say so on the confirmation screen.
    assert_eq!(seen_a.protection, clipsync_core::SasProtection::Committed);
    assert_eq!(seen_b.protection, clipsync_core::SasProtection::Committed);
    assert_eq!(seen_a.peer_device_id, id_b.device_id);
    assert_eq!(seen_b.peer_device_id, id_a.device_id);
    assert!(!seen_a.peer_device_name.is_empty() && !seen_b.peer_device_name.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_sas_leaves_both_devices_without_room_keys_or_credentials() {
    let (relay, _h) = start_relay().await;
    let a_dir = TempDir::new().unwrap();
    let b_dir = TempDir::new().unwrap();
    let app_a = app_in(&a_dir, &relay);
    let app_b = app_in(&b_dir, &relay);
    let (id_a, id_b) = (app_a.identity().unwrap(), app_b.identity().unwrap());
    let cfg_a = app_a.store.load_config().unwrap();
    let cfg_b = app_b.store.load_config().unwrap();
    let pending = begin_create_room(&id_a, &cfg_a, 60).await.unwrap();
    let code = pending.offer.pairing_code.clone();
    let (release_tx, release) = tokio::sync::oneshot::channel::<()>();
    let store_a = store_in(&a_dir);
    let creator_id = id_a.clone();
    // The creator's user is slow and confirms only after the joiner refused.
    let creator = tokio::spawn(async move {
        pending
            .wait(&store_a, &creator_id, move |_| async move {
                let _ = release.await;
                SasDecision::Confirmed
            })
            .await
    });
    wait_for_connection_count(&relay, 1).await;
    let joined = tokio::time::timeout(
        Duration::from_secs(20),
        join_room(&app_b.store, &id_b, &cfg_b, &code, |_| async {
            SasDecision::Rejected
        }),
    )
    .await
    .unwrap();
    assert!(joined
        .unwrap_err()
        .to_string()
        .contains("verification code rejected"));
    // Fence on the relay dropping the aborted session before the creator binds.
    let client = loopback_http_client();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let body = client
                .post(format!("{relay}/v1/pairing/join"))
                .json(&clipsync_protocol::JoinPairingRequest {
                    pairing_code: code.clone(),
                    device_id: clipsync_protocol::DeviceId::new(),
                    device_name: "fence".into(),
                    os: "test".into(),
                    protocol_version: clipsync_protocol::PROTOCOL_VERSION,
                })
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            if body.contains("code_invalid") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("relay kept the rejected pairing session");
    // The joiner's abort notice usually ends the creator while its prompt is
    // still open; a late confirmation must then be refused by the relay.
    let _ = release_tx.send(());
    let created = tokio::time::timeout(Duration::from_secs(20), creator)
        .await
        .unwrap()
        .unwrap();
    let refused = created.expect_err("creator must not complete a rejected pairing");
    assert!(
        refused
            .to_string()
            .contains("the other device rejected the verification code"),
        "{refused}"
    );
    for dir in [&a_dir, &b_dir] {
        let store = store_in(dir);
        assert!(store.load_state().unwrap().rooms.is_empty());
        assert!(store.current_room().is_err());
        assert!(stored_secrets(dir).is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn joiner_rejection_after_creator_confirmed_ends_the_creator_promptly() {
    // Lets the creator's binding reach the relay before the joiner answers,
    // so the creator is already waiting for a commit that will never come.
    const CREATOR_BIND_GRACE: Duration = Duration::from_millis(300);
    // Far below the ~125 s pairing deadline a confirmed creator used to wait.
    const PROMPT_ABORT_BUDGET: Duration = Duration::from_secs(10);
    let (relay, _h) = start_relay().await;
    let a_dir = TempDir::new().unwrap();
    let b_dir = TempDir::new().unwrap();
    let app_a = app_in(&a_dir, &relay);
    let app_b = app_in(&b_dir, &relay);
    let (id_a, id_b) = (app_a.identity().unwrap(), app_b.identity().unwrap());
    let cfg_a = app_a.store.load_config().unwrap();
    let cfg_b = app_b.store.load_config().unwrap();
    let pending = begin_create_room(&id_a, &cfg_a, 60).await.unwrap();
    let code = pending.offer.pairing_code.clone();
    let (confirmed_tx, confirmed) = tokio::sync::oneshot::channel::<()>();
    let store_a = store_in(&a_dir);
    let creator_id = id_a.clone();
    let creator = tokio::spawn(async move {
        pending
            .wait(&store_a, &creator_id, move |_| async move {
                let _ = confirmed_tx.send(());
                SasDecision::Confirmed
            })
            .await
    });
    wait_for_connection_count(&relay, 1).await;
    let joined = join_room(&app_b.store, &id_b, &cfg_b, &code, move |_| async move {
        let _ = confirmed.await;
        tokio::time::sleep(CREATOR_BIND_GRACE).await;
        SasDecision::Rejected
    });
    let joined = tokio::time::timeout(PROMPT_ABORT_BUDGET, joined)
        .await
        .unwrap();
    assert!(joined
        .unwrap_err()
        .to_string()
        .contains("verification code rejected"));
    let created = tokio::time::timeout(PROMPT_ABORT_BUDGET, creator)
        .await
        .expect("a confirmed creator must learn of the rejection before its deadline")
        .unwrap();
    let refused = created.expect_err("creator must not complete a rejected pairing");
    assert!(
        refused
            .to_string()
            .contains("the other device rejected the verification code"),
        "{refused}"
    );
    for dir in [&a_dir, &b_dir] {
        let store = store_in(dir);
        assert!(store.load_state().unwrap().rooms.is_empty());
        assert!(stored_secrets(dir).is_empty());
    }
}

#[tokio::test]
async fn wrong_pairing_code_rejected() {
    // Six decimal digits, the relay's pairing-code format.
    const PAIRING_CODE_SPACE: u32 = 1_000_000;
    let (relay, _h) = start_relay().await;
    let dir = TempDir::new().unwrap();
    let app = app_in(&dir, &relay);
    let id = app.identity().unwrap();
    let cfg = app.store.load_config().unwrap();
    // No room exists on this relay, so every code is wrong.
    let unknown_code = format!("{:06}", rand::random_range(0..PAIRING_CODE_SPACE));
    let err = join_room(&app.store, &id, &cfg, &unknown_code, |_| async {
        SasDecision::Confirmed
    })
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("join failed") || msg.contains("code"),
        "unexpected error: {msg}"
    );
}

#[test]
fn limits_and_sanitize() {
    assert_eq!(TEXT_MAX_BYTES, 256 * 1024);
    assert_eq!(IMAGE_MAX_BYTES, 16 * 1024 * 1024);
    assert_eq!(FRAME_HARD_CAP, 384 * 1024);
    assert_eq!(FILE_MAX_COUNT, 16);
    assert!(safe_filename("../etc/passwd").is_err());
    assert_eq!(safe_filename("diagram.png").unwrap(), "diagram.png");
    let _ = blake3_hex(b"abc");
}

#[test]
fn mock_clipboard_roundtrip() {
    let mock = MockClipboard::new();
    mock.set(ClipboardItem::Text { text: "x".into() });
    let sys = SystemClipboard::mock(mock);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let item = sys.read().await.unwrap().unwrap();
        assert!(matches!(item, ClipboardItem::Text { .. }));
    });
}
