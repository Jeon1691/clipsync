// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Golden vectors for the pairing expiry the CLI prints and persists.
//!
//! `room create` shows the relay's `expires_at` three ways: the `--json` event
//! (RFC 3339 with `Z`), the human `expires:` line, and the background pairing
//! file (RFC 3339 with `+00:00`, which `room status` reads back verbatim). A
//! scripted relay returns fixed instants so the exact strings are pinned.

use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use axum::extract::ws::WebSocketUpgrade;
use axum::routing::{get, post};
use axum::{Json, Router};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;

/// Bound on each wait for CLI output; process start-up dominates.
const OUTPUT_DEADLINE: Duration = Duration::from_secs(30);

/// (relay `expires_at`, `--json` value, human line value, pairing file value).
const VECTORS: &[(&str, &str, &str, &str)] = &[
    (
        "2099-01-02T03:04:05Z",
        "2099-01-02T03:04:05Z",
        "2099-01-02 03:04:05 UTC",
        "2099-01-02T03:04:05+00:00",
    ),
    (
        "2099-01-02T03:04:05.123456789Z",
        "2099-01-02T03:04:05.123456789Z",
        "2099-01-02 03:04:05.123456789 UTC",
        "2099-01-02T03:04:05.123456789+00:00",
    ),
    (
        "2099-01-02T03:04:05.120Z",
        "2099-01-02T03:04:05.120Z",
        "2099-01-02 03:04:05.120 UTC",
        "2099-01-02T03:04:05.120+00:00",
    ),
    (
        "2099-01-02T12:04:05.1234+09:00",
        "2099-01-02T03:04:05.123400Z",
        "2099-01-02 03:04:05.123400 UTC",
        "2099-01-02T03:04:05.123400+00:00",
    ),
    (
        "2098-12-31T23:59:60.5Z",
        "2098-12-31T23:59:60.500Z",
        "2098-12-31 23:59:60.500 UTC",
        "2098-12-31T23:59:60.500+00:00",
    ),
    (
        "+10000-01-01T00:00:00Z",
        "+10000-01-01T00:00:00Z",
        "+10000-01-01 00:00:00 UTC",
        "+10000-01-01T00:00:00+00:00",
    ),
];

fn seed_identity_file(home: &Path) {
    let paths = clipsync_storage::AppPaths::from_root(home.to_path_buf());
    paths.ensure().unwrap();
    let identity = clipsync_crypto::DeviceIdentity::generate();
    std::fs::write(
        paths.identity_file(),
        serde_json::to_vec(&identity.to_stored()).unwrap(),
    )
    .unwrap();
}

/// A relay that answers `create` with `expires_at` and then holds the pairing
/// WebSocket open without a joiner.
async fn scripted_relay(expires_at: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/v1/pairing/create",
            post(move || async move {
                Json(serde_json::json!({
                    "room_id": clipsync_protocol::RoomId::new(),
                    "pairing_code": "246810",
                    "pairing_token": "scripted-pairing-token",
                    "expires_at": expires_at,
                    "ws_path": "/v1/ws",
                }))
            }),
        )
        .route(
            "/v1/ws",
            get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(
                    |mut socket| async move { while let Some(Ok(_)) = socket.recv().await {} },
                )
            }),
        );
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (format!("http://{addr}"), task)
}

/// Run a foreground `room create`, return the line starting with `prefix`
/// and the pairing file's `expires_at`, then stop the process.
async fn create_until(home: &Path, relay: &str, json: bool, prefix: &str) -> (String, String) {
    let mut args = vec!["room", "create", "--foreground", "--no-auto-sync"];
    if json {
        args.insert(0, "--json");
    }
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_clipsync"))
        .args(&args)
        .env("RUST_LOG", "error")
        .env("CLIPSYNC_HOME", home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env("CLIPSYNC_RELAY_URL", relay)
        .env("CLIPSYNC_NO_DAEMON", "1")
        .env_remove("CLIPSYNC_LISTEN")
        .env_remove("CLIPSYNC_CA_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(OUTPUT_DEADLINE, async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if line.starts_with(prefix) {
                return line;
            }
        }
        panic!("room create exited before printing {prefix:?}");
    })
    .await
    .expect("room create output");
    // The pairing file is written before the code is printed.
    let store = clipsync_storage::LocalStore::open_from(clipsync_storage::AppPaths::from_root(
        home.to_path_buf(),
    ))
    .unwrap();
    let wait = store.load_pairing_wait().unwrap().expect("pairing file");
    child.kill().await.unwrap();
    (line, wait.expires_at)
}

#[tokio::test(flavor = "multi_thread")]
async fn room_create_prints_and_persists_pinned_expiry_strings() {
    for (relay_value, json_value, human_value, file_value) in VECTORS {
        let (relay, task) = scripted_relay(relay_value).await;

        let home = TempDir::new().unwrap();
        seed_identity_file(home.path());
        let (line, persisted) = create_until(home.path(), &relay, true, "{").await;
        let event: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(event["event"], "pairing_code", "{line}");
        assert_eq!(event["expires_at"], *json_value, "{relay_value} --json");
        assert_eq!(persisted, *file_value, "{relay_value} pairing file (json)");

        let home = TempDir::new().unwrap();
        seed_identity_file(home.path());
        let (line, persisted) = create_until(home.path(), &relay, false, "expires: ").await;
        assert_eq!(
            line,
            format!("expires: {human_value}"),
            "{relay_value} human"
        );
        assert_eq!(persisted, *file_value, "{relay_value} pairing file (human)");

        task.abort();
    }
}
