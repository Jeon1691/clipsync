use std::net::SocketAddr;
use std::time::Duration;

use clipsync_clipboard::{ClipboardItem, MockClipboard, SystemClipboard};
use clipsync_core::{begin_create_room, join_room, App};
use clipsync_crypto::blake3_hex;
use clipsync_protocol::{FRAME_HARD_CAP, IMAGE_MAX_BYTES, TEXT_MAX_BYTES};
use clipsync_relay::{router, RelayConfig};
use clipsync_storage::AppPaths;
use clipsync_transfer::safe_filename;
use tempfile::TempDir;
use tokio::net::TcpListener;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_and_text_push_pull() {
    let (relay, _h) = start_relay().await;
    let a_dir = TempDir::new().unwrap();
    let b_dir = TempDir::new().unwrap();
    std::env::set_var("CLIPSYNC_NO_DAEMON", "1");

    let app_a = app_in(&a_dir, &relay);
    let app_b = app_in(&b_dir, &relay);
    let id_a = app_a.identity().unwrap();
    let id_b = app_b.identity().unwrap();
    let cfg_a = app_a.store.load_config().unwrap();
    let cfg_b = app_b.store.load_config().unwrap();

    let pending = begin_create_room(&id_a, &cfg_a, 60).await.unwrap();
    let code = pending.offer.pairing_code.clone();
    assert_eq!(code.len(), 6);

    let store_a = App::open_from(AppPaths::from_root(a_dir.path().to_path_buf()))
        .unwrap()
        .store;
    let wait_a = tokio::spawn(async move { pending.wait(&store_a, &id_a).await });

    tokio::time::sleep(Duration::from_millis(150)).await;
    join_room(&app_b.store, &id_b, &cfg_b, &code).await.unwrap();
    wait_a.await.unwrap().unwrap();

    assert!(
        app_a.store.current_room().is_ok() || {
            App::open_from(AppPaths::from_root(a_dir.path().to_path_buf()))
                .unwrap()
                .store
                .current_room()
                .is_ok()
        }
    );
    assert!(app_b.store.current_room().is_ok());

    let store_a = App::open_from(AppPaths::from_root(a_dir.path().to_path_buf()))
        .unwrap()
        .store;
    clipsync_core::engine_oneshot_push(
        &store_a,
        ClipboardItem::Text {
            text: "hello from a".into(),
        },
    )
    .await
    .unwrap();

    let store_b = App::open_from(AppPaths::from_root(b_dir.path().to_path_buf()))
        .unwrap()
        .store;
    let pulled = clipsync_core::engine_oneshot_pull(&store_b, false, true, None)
        .await
        .unwrap();
    assert_eq!(pulled.text.as_deref(), Some("hello from a"));
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
