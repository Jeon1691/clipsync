// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use clipsync_relay::{router, RelayConfig};
use tempfile::TempDir;
use tokio::net::TcpListener;

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

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_clipsync")
}

async fn start_relay() -> (String, tokio::task::JoinHandle<()>) {
    let (url, task, _) = start_relay_observed().await;
    (url, task)
}

async fn start_relay_observed() -> (
    String,
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = RelayConfig {
        bind: addr,
        public_url: format!("http://{addr}"),
        token_secret: b"test-secret-test-secret-test-sec".to_vec(),
        webhook_url: None,
        webhook_secret: None,
        metrics_token: None,
    };
    let ttl = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = ttl.clone();
    let app = router(cfg).layer(axum::middleware::from_fn(
        move |mut request: axum::extract::Request, next: axum::middleware::Next| {
            let observed = observed.clone();
            async move {
                if request.uri().path() == "/v1/pairing/create" {
                    let body = std::mem::replace(request.body_mut(), axum::body::Body::empty());
                    let bytes = axum::body::to_bytes(body, 16 * 1024).await.unwrap();
                    let create: clipsync_protocol::CreatePairingRequest =
                        serde_json::from_slice(&bytes).unwrap();
                    observed.store(
                        create.ttl_secs.unwrap_or_default(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    *request.body_mut() = axum::body::Body::from(bytes);
                }
                next.run(request).await
            }
        },
    ));
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (format!("http://{addr}"), handle, ttl)
}

fn clipsync(home: &Path, relay: &str, args: &[&str]) -> std::process::Output {
    use std::process::Stdio;
    let mut child = Command::new(bin())
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("RUST_LOG", "error")
        .env("CLIPSYNC_HOME", home)
        .env("CLIPSYNC_RELAY_URL", relay)
        .env("CLIPSYNC_NO_DAEMON", "1")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("CLIPSYNC_LISTEN")
        .env_remove("CLIPSYNC_CA_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run clipsync");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "CLI fixture exceeded its deadline: {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

fn json_out(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim().starts_with('{'))
        .unwrap_or(stdout.trim());
    serde_json::from_str(line).unwrap_or_else(|_| {
        panic!(
            "expected json, status={} stdout={} stderr={}",
            out.status,
            stdout,
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        libc_kill(pid as i32, 15);
    }
    let _ = pid;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_offer_is_flushed_before_a_peer_joins() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    let (relay, relay_task) = start_relay().await;
    let home = TempDir::new().unwrap();
    seed_identity_file(home.path());
    let mut child = tokio::process::Command::new(bin())
        .args(["--json", "room", "create", "--foreground", "--no-auto-sync"])
        .env("RUST_LOG", "error")
        .env("CLIPSYNC_HOME", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("CLIPSYNC_RELAY_URL", &relay)
        .env("CLIPSYNC_NO_DAEMON", "1")
        .env_remove("CLIPSYNC_LISTEN")
        .env_remove("CLIPSYNC_CA_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(30), output.read_line(&mut line))
        .await
        .expect("foreground code must be visible without completing pairing")
        .unwrap();
    let offer: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(offer["event"], "pairing_code");
    assert_eq!(offer["pairing_code"].as_str().unwrap().len(), 6);
    assert!(
        child.try_wait().unwrap().is_none(),
        "pairing still waits for its peer"
    );
    // A separate CLI process cannot inherit a test keyring builder. Complete
    // this extra pairing only on Linux or an explicitly disposable CI host.
    if !cfg!(target_os = "linux")
        && std::env::var("CLIPSYNC_TEST_DISPOSABLE_HOST").as_deref() != Ok("1")
    {
        tokio::time::timeout(Duration::from_secs(5), child.kill())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        relay_task.abort();
        return;
    }
    let peer = TempDir::new().unwrap();
    seed_identity_file(peer.path());
    let joined = clipsync(
        peer.path(),
        &relay,
        &[
            "--json",
            "room",
            "join",
            offer["pairing_code"].as_str().unwrap(),
            "--no-auto-sync",
        ],
    );
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    let joined = json_out(&joined);
    let mut completed = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        output.read_to_string(&mut completed),
    )
    .await
    .unwrap()
    .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    let completed: serde_json::Value = serde_json::from_str(&completed).unwrap();
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["room_id"], joined["room_id"]);
    assert_eq!(completed["room_id"], offer["room_id"]);
    assert_eq!(completed["pairing_code"], offer["pairing_code"]);
    relay_task.abort();
}

#[test]
fn background_failure_preserves_the_child_relay_error() {
    // Refuse connections without depending on an external host or its DNS.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let relay = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let home = TempDir::new().unwrap();
    seed_identity_file(home.path());
    let output = clipsync(home.path(), &relay, &["room", "create", "--no-auto-sync"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&relay),
        "child failure lost the affected relay: {stderr}"
    );
    assert!(!stderr.trim().ends_with("background pairing failed"));
}

#[cfg(unix)]
fn libc_kill(pid: i32, sig: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, sig) }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_create_background_returns_then_pairs() {
    let (relay, relay_task, requested_ttl) = start_relay_observed().await;
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    seed_identity_file(a.path());
    seed_identity_file(b.path());
    assert!(clipsync(a.path(), &relay, &["init", "--relay-url", &relay])
        .status
        .success());
    assert!(clipsync(b.path(), &relay, &["init", "--relay-url", &relay])
        .status
        .success());

    let started = Instant::now();
    let create = clipsync(
        a.path(),
        &relay,
        &["room", "create", "--ttl", "45s", "--no-auto-sync"],
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "background create blocked for {elapsed:?}"
    );
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let offer = String::from_utf8(create.stdout).unwrap();
    let code = offer
        .lines()
        .find_map(|line| line.strip_prefix("pairing code: "))
        .expect("human pairing code");
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
    let store = clipsync_storage::LocalStore::open_from(clipsync_storage::AppPaths::from_root(
        a.path().into(),
    ))
    .unwrap();
    let waiting = store.load_pairing_wait().unwrap().unwrap();
    let pid = waiting.pid;
    assert_eq!(waiting.pairing_code, code);
    assert!(offer.contains(&format!("room: {}", waiting.room_id)));
    assert!(offer.contains(&format!("expires: {}", waiting.expires_at)));
    assert!(offer.contains(&format!("waiting in background (pid {pid})")));
    assert!(offer.contains(&format!(
        "join from another device: clipsync room join {code}"
    )));
    assert_eq!(requested_ttl.load(std::sync::atomic::Ordering::Relaxed), 45);

    let dup = clipsync(a.path(), &relay, &["room", "create", "--no-auto-sync"]);
    assert!(
        !dup.status.success(),
        "second background create should fail while first is waiting"
    );

    let join = clipsync(b.path(), &relay, &["room", "join", code, "--no-auto-sync"]);
    if !join.status.success() {
        kill_pid(pid);
        panic!(
            "join failed: stdout={} stderr={}",
            String::from_utf8_lossy(&join.stdout),
            String::from_utf8_lossy(&join.stderr)
        );
    }
    assert!(String::from_utf8_lossy(&join.stdout).contains("joined room"));

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut room = None;
    while Instant::now() < deadline {
        let st = clipsync(a.path(), &relay, &["--json", "status"]);
        if st.status.success() {
            let v = json_out(&st);
            if let Some(id) = v["room_id"]
                .as_str()
                .filter(|_| v["pairing"]["status"] == "paired")
            {
                room = Some(id.to_string());
                break;
            }
            if v["pairing"]["status"] == "paired" {
                if let Some(id) = v["pairing"]["room_id"].as_str() {
                    room = Some(id.to_string());
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if room.is_none() {
        kill_pid(pid);
        panic!("creator never recorded the room after background pairing");
    }
    assert_eq!(room.as_deref(), Some(waiting.room_id.as_str()));
    assert!(String::from_utf8_lossy(&join.stdout).contains(&waiting.room_id));
    relay_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_create_auto_inits_without_explicit_init() {
    let (relay, _h) = start_relay().await;
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let started = Instant::now();
    let create = clipsync(
        a.path(),
        &relay,
        &["--json", "room", "create", "--no-auto-sync"],
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "auto-init create blocked"
    );
    assert!(
        create.status.success(),
        "create without init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let offer = json_out(&create);
    let code = offer["pairing_code"].as_str().expect("pairing_code");
    let pid = offer["pid"].as_u64().expect("pid") as u32;
    let join = clipsync(
        b.path(),
        &relay,
        &["--json", "room", "join", code, "--no-auto-sync"],
    );
    if !join.status.success() {
        kill_pid(pid);
        panic!(
            "join without init failed: {}",
            String::from_utf8_lossy(&join.stderr)
        );
    }
}
