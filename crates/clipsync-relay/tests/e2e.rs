use std::net::SocketAddr;
use std::time::Duration;

use clipsync_clipboard::{ClipboardItem, FileRef, ImageMime, MockClipboard, SystemClipboard};
use clipsync_core::{begin_create_room, join_room, App};
use clipsync_crypto::blake3_hex;
use clipsync_protocol::{FRAME_HARD_CAP, IMAGE_MAX_BYTES, TEXT_MAX_BYTES};
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
    for _ in 0..50 {
        if reqwest::get(format!("{url}/healthz")).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (url, handle)
}

fn app_in(dir: &TempDir, relay: &str) -> App {
    let app = App::open_from(AppPaths::from_root(dir.path().to_path_buf())).unwrap();
    app.init(Some(relay.to_string())).unwrap();
    app
}

fn store_in(dir: &TempDir) -> clipsync_storage::LocalStore {
    App::open_from(AppPaths::from_root(dir.path().to_path_buf()))
        .unwrap()
        .store
}

async fn pair_homes(relay: &str) -> (TempDir, TempDir) {
    std::env::set_var("CLIPSYNC_NO_DAEMON", "1");
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
    tokio::time::sleep(Duration::from_millis(150)).await;
    join_room(&app_b.store, &id_b, &cfg_b, &code).await.unwrap();
    wait_a.await.unwrap().unwrap();
    assert!(store_in(&a_dir).current_room().is_ok());
    assert!(store_in(&b_dir).current_room().is_ok());
    (a_dir, b_dir)
}

#[tokio::test]
async fn healthz_ok() {
    let (relay, _h) = start_relay().await;
    let resp = reqwest::get(format!("{relay}/healthz")).await.unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

async fn transfer_item(a: &TempDir, b: &TempDir, item: ClipboardItem, out: &std::path::Path) {
    let store_b = store_in(b);
    let out_owned = out.to_path_buf();
    let pull = tokio::spawn(async move {
        clipsync_core::engine_oneshot_pull(&store_b, false, true, Some(out_owned)).await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    clipsync_core::engine_oneshot_push(&store_in(a), item)
        .await
        .expect("push");
    pull.await.unwrap().expect("pull");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    let files = samples
        .iter()
        .map(|(name, mime, bytes)| FileRef {
            name: (*name).to_string(),
            bytes: bytes.clone(),
            mime: Some((*mime).to_string()),
            staged_path: None,
        })
        .collect();
    let out = b_dir.path().join("files");
    transfer_item(&a_dir, &b_dir, ClipboardItem::Files { files }, &out).await;
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
