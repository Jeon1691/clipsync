// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::net::SocketAddr;
use std::time::Duration;

use clipsync_clipboard::{ClipboardItem, FileRef, ImageMime, MockClipboard, SystemClipboard};
use clipsync_core::{begin_create_room, join_room, App};
use clipsync_crypto::blake3_hex;
use clipsync_protocol::{FILE_MAX_COUNT, FRAME_HARD_CAP, IMAGE_MAX_BYTES, TEXT_MAX_BYTES};
use clipsync_relay::{router, RelayConfig};
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
    let wait_a = tokio::spawn(async move { pending.wait(&store_a, &id_a).await });
    wait_for_connection_count(relay, 1).await;
    let creator = async {
        wait_a
            .await
            .map_err(|_| "creator pairing task failed".to_string())?
            .map_err(|error| format!("creator pairing failed: {error}"))
    };
    let joiner = async {
        join_room(&app_b.store, &id_b, &cfg_b, &code)
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
                let body = axum::body::to_bytes(request.into_body(), FRAME_HARD_CAP)
                    .await
                    .unwrap();
                let response = client
                    .request(method, upstream)
                    .header("content-type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .unwrap();
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

#[tokio::test]
async fn wrong_pairing_code_rejected() {
    let (relay, _h) = start_relay().await;
    let dir = TempDir::new().unwrap();
    let app = app_in(&dir, &relay);
    let id = app.identity().unwrap();
    let cfg = app.store.load_config().unwrap();
    let err = join_room(&app.store, &id, &cfg, "000000")
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
