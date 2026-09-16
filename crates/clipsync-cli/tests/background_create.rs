use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use clipsync_relay::{router, RelayConfig};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_clipsync")
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
    (format!("http://{addr}"), handle)
}

fn clipsync(home: &Path, relay: &str, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .env("CLIPSYNC_HOME", home)
        .env("CLIPSYNC_RELAY_URL", relay)
        .env("CLIPSYNC_NO_DAEMON", "1")
        .output()
        .expect("run clipsync")
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

#[cfg(unix)]
fn libc_kill(pid: i32, sig: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, sig) }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_create_background_returns_then_pairs() {
    let (relay, _h) = start_relay().await;
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
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
        &["--json", "room", "create", "--background", "--no-auto-sync"],
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
    let offer = json_out(&create);
    let code = offer["pairing_code"].as_str().expect("pairing_code");
    assert_eq!(code.len(), 6);
    assert_eq!(offer["background"], true);
    let pid = offer["pid"].as_u64().expect("pid") as u32;

    let dup = clipsync(
        a.path(),
        &relay,
        &["room", "create", "--background", "--no-auto-sync"],
    );
    assert!(
        !dup.status.success(),
        "second background create should fail while first is waiting"
    );

    let join = clipsync(
        b.path(),
        &relay,
        &["--json", "room", "join", code, "--no-auto-sync"],
    );
    if !join.status.success() {
        kill_pid(pid);
        panic!(
            "join failed: stdout={} stderr={}",
            String::from_utf8_lossy(&join.stdout),
            String::from_utf8_lossy(&join.stderr)
        );
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut room = None;
    while Instant::now() < deadline {
        let st = clipsync(a.path(), &relay, &["--json", "status"]);
        if st.status.success() {
            let v = json_out(&st);
            if let Some(id) = v["room_id"].as_str() {
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
}
