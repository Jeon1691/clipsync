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
