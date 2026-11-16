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

/// Start a CLI process whose stdin and stdout the test drives, so it can
/// compare verification codes between two devices the way a user would.
fn spawn_cli(home: &Path, relay: &str, args: &[&str]) -> tokio::process::Child {
    use std::process::Stdio;
    tokio::process::Command::new(bin())
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("run clipsync")
}

/// Read stdout lines until one starts with `prefix`; return the remainder.
async fn line_with_prefix(
    reader: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
    prefix: &str,
) -> String {
    use tokio::io::AsyncBufReadExt;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut line = String::new();
            assert_ne!(
                reader.read_line(&mut line).await.unwrap(),
                0,
                "stdout closed before {prefix:?}"
            );
            if let Some(rest) = line.trim_end().strip_prefix(prefix) {
                return rest.to_string();
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {prefix:?} line"))
}

fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

fn waiting_store(home: &Path) -> clipsync_storage::LocalStore {
    clipsync_storage::LocalStore::open_from(clipsync_storage::AppPaths::from_root(home.into()))
        .unwrap()
}

/// Poll the background worker's pairing file until it asks for the SAS.
async fn background_sas(home: &Path) -> clipsync_storage::PairingWait {
    let store = waiting_store(home);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(Some(wait)) = store.load_pairing_wait() {
                if wait.status == "confirm_sas" && wait.sas.is_some() {
                    return wait;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("background worker never asked to confirm the verification code")
}

fn secrets_in(home: &Path) -> Vec<String> {
    let store = waiting_store(home);
    std::fs::read_dir(&store.paths.data_dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".secret"))
                .collect()
        })
        .unwrap_or_default()
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut creator_input = child.stdin.take().unwrap();
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
    let mut joiner = spawn_cli(
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
    let mut joiner_input = joiner.stdin.take().unwrap();
    let mut joiner_output = BufReader::new(joiner.stdout.take().unwrap());
    let joiner_sas: serde_json::Value =
        serde_json::from_str(&line_with_prefix(&mut joiner_output, "").await).unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(20), output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let creator_sas: serde_json::Value = serde_json::from_str(&line).unwrap();
    for event in [&creator_sas, &joiner_sas] {
        assert_eq!(event["event"], "sas_confirmation");
        assert_eq!(event["sas"].as_str().unwrap().len(), 6);
        assert!(!event["peer_device_name"].as_str().unwrap().is_empty());
    }
    assert_eq!(creator_sas["sas"], joiner_sas["sas"]);
    // Each user types the code the other device shows.
    use tokio::io::AsyncWriteExt;
    for (input, other) in [
        (&mut creator_input, &joiner_sas),
        (&mut joiner_input, &creator_sas),
    ] {
        input
            .write_all(format!("{}\n", other["sas"].as_str().unwrap()).as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
    }
    let mut joined = String::new();
    tokio::time::timeout(
        Duration::from_secs(20),
        joiner_output.read_to_string(&mut joined),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(10), joiner.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    let joined: serde_json::Value = serde_json::from_str(joined.trim()).unwrap();
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

    let mut joiner = spawn_cli(b.path(), &relay, &["room", "join", code, "--no-auto-sync"]);
    let mut joiner_input = joiner.stdin.take().unwrap();
    let mut joiner_output = tokio::io::BufReader::new(joiner.stdout.take().unwrap());
    let joiner_sas = digits(&line_with_prefix(&mut joiner_output, "verification code: ").await);
    let creator_wait = background_sas(a.path()).await;
    assert_eq!(creator_wait.sas.as_deref(), Some(joiner_sas.as_str()));
    assert!(!creator_wait
        .peer_device_name
        .as_deref()
        .unwrap_or_default()
        .is_empty());
    let status = json_out(&clipsync(a.path(), &relay, &["--json", "status"]));
    assert_eq!(status["pairing"]["status"], "confirm_sas");
    assert_eq!(status["pairing"]["sas"], joiner_sas.as_str());
    // The joiner's user types the creator's code; the creator's user passes
    // the joiner's code to `room confirm`.
    use tokio::io::AsyncWriteExt;
    joiner_input
        .write_all(format!("{}\n", creator_wait.sas.as_deref().unwrap()).as_bytes())
        .await
        .unwrap();
    joiner_input.flush().await.unwrap();
    let confirm = clipsync(a.path(), &relay, &["room", "confirm", &joiner_sas]);
    if !confirm.status.success() {
        kill_pid(pid);
        panic!(
            "confirm failed: stdout={} stderr={}",
            String::from_utf8_lossy(&confirm.stdout),
            String::from_utf8_lossy(&confirm.stderr)
        );
    }
    assert!(String::from_utf8_lossy(&confirm.stdout).contains("paired"));
    let join = tokio::time::timeout(Duration::from_secs(20), joiner.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let mut join_stdout = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut joiner_output, &mut join_stdout)
        .await
        .unwrap();
    if !join.status.success() {
        kill_pid(pid);
        panic!(
            "join failed: stdout={join_stdout} stderr={}",
            String::from_utf8_lossy(&join.stderr)
        );
    }
    assert!(join_stdout.contains("joined room"));

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
    assert!(join_stdout.contains(&waiting.room_id));
    relay_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_or_rejected_verification_codes_store_nothing_on_either_device() {
    let (relay, relay_task) = start_relay().await;
    for reject_command in [false, true] {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        seed_identity_file(a.path());
        seed_identity_file(b.path());
        let create = clipsync(
            a.path(),
            &relay,
            &["--json", "room", "create", "--no-auto-sync"],
        );
        assert!(
            create.status.success(),
            "{}",
            String::from_utf8_lossy(&create.stderr)
        );
        let offer = json_out(&create);
        let code = offer["pairing_code"].as_str().unwrap();
        let pid = offer["pid"].as_u64().unwrap() as u32;
        let mut joiner = spawn_cli(b.path(), &relay, &["room", "join", code, "--no-auto-sync"]);
        let joiner_input = joiner.stdin.take().unwrap();
        let mut joiner_output = tokio::io::BufReader::new(joiner.stdout.take().unwrap());
        let joiner_sas = digits(&line_with_prefix(&mut joiner_output, "verification code: ").await);
        let creator_wait = background_sas(a.path()).await;
        assert_eq!(creator_wait.sas.as_deref(), Some(joiner_sas.as_str()));
        // `room confirm` shows no code before reading its argument, so "yes"
        // compares nothing: it is refused and leaves the request unanswered.
        for not_a_code in ["yes", "y"] {
            let refused = clipsync(a.path(), &relay, &["room", "confirm", not_a_code]);
            assert!(!refused.status.success(), "{not_a_code:?} confirmed");
            let stderr = String::from_utf8_lossy(&refused.stderr);
            assert!(stderr.contains("digits"), "{stderr}");
            let unanswered = waiting_store(a.path())
                .load_pairing_wait()
                .unwrap()
                .unwrap();
            assert_eq!(
                (unanswered.status.as_str(), unanswered.sas_decision),
                ("confirm_sas", None)
            );
        }
        let answer = if reject_command {
            clipsync(a.path(), &relay, &["room", "reject"])
        } else {
            // A code that differs from the creator's in every digit.
            let wrong: String = joiner_sas
                .chars()
                .map(|digit| if digit == '0' { '1' } else { '0' })
                .collect();
            clipsync(a.path(), &relay, &["room", "confirm", &wrong])
        };
        let answer_stderr = String::from_utf8_lossy(&answer.stderr).into_owned();
        if reject_command {
            assert!(answer.status.success(), "{answer_stderr}");
            assert!(String::from_utf8_lossy(&answer.stdout).contains("nothing was stored"));
        } else {
            assert!(!answer.status.success());
            assert!(answer_stderr.contains("does not match"), "{answer_stderr}");
        }
        // The joiner's user also sees no matching code and gives up.
        drop(joiner_input);
        let join = tokio::time::timeout(Duration::from_secs(20), joiner.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(!join.status.success());
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let wait = waiting_store(a.path())
                .load_pairing_wait()
                .unwrap()
                .unwrap();
            if wait.status == "failed" {
                assert!(
                    wait.error
                        .as_deref()
                        .unwrap_or_default()
                        .contains("rejected"),
                    "{:?}",
                    wait.error
                );
                break;
            }
            if Instant::now() > deadline {
                kill_pid(pid);
                panic!("background worker did not abort: {}", wait.status);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        for home in [a.path(), b.path()] {
            let rooms = json_out(&clipsync(home, &relay, &["--json", "room", "list"]));
            assert_eq!(rooms, serde_json::json!([]));
            assert!(secrets_in(home).is_empty());
        }
    }
    relay_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn piped_yes_does_not_confirm_a_join() {
    // A script that pipes "yes" answers before anyone compared the codes.
    // Without a terminal only the other device's digits confirm.
    let (relay, relay_task) = start_relay().await;
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    seed_identity_file(a.path());
    seed_identity_file(b.path());
    let create = clipsync(
        a.path(),
        &relay,
        &["--json", "room", "create", "--no-auto-sync"],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let offer = json_out(&create);
    let code = offer["pairing_code"].as_str().unwrap();
    let pid = offer["pid"].as_u64().unwrap() as u32;
    let mut joiner = spawn_cli(b.path(), &relay, &["room", "join", code, "--no-auto-sync"]);
    let mut joiner_input = joiner.stdin.take().unwrap();
    let mut joiner_output = tokio::io::BufReader::new(joiner.stdout.take().unwrap());
    line_with_prefix(&mut joiner_output, "verification code: ").await;
    use tokio::io::AsyncWriteExt;
    joiner_input.write_all(b"yes\n").await.unwrap();
    joiner_input.flush().await.unwrap();
    let join = tokio::time::timeout(Duration::from_secs(20), joiner.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    if join.status.success() {
        kill_pid(pid);
        panic!("a piped \"yes\" confirmed the join");
    }
    let stderr = String::from_utf8_lossy(&join.stderr);
    assert!(stderr.contains("interactive"), "{stderr}");
    // The creator's worker stops on the joiner's abort notice.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let wait = waiting_store(a.path())
            .load_pairing_wait()
            .unwrap()
            .unwrap();
        if wait.status == "failed" {
            break;
        }
        if Instant::now() > deadline {
            kill_pid(pid);
            panic!("background worker did not abort: {}", wait.status);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for home in [a.path(), b.path()] {
        let rooms = json_out(&clipsync(home, &relay, &["--json", "room", "list"]));
        assert_eq!(rooms, serde_json::json!([]));
        assert!(secrets_in(home).is_empty());
    }
    relay_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_create_auto_inits_without_explicit_init() {
    let (relay, _h) = start_relay().await;
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let started = Instant::now();
    // Scripted setups may opt out of the comparison explicitly on both sides.
    let create = clipsync(
        a.path(),
        &relay,
        &[
            "--json",
            "room",
            "create",
            "--no-auto-sync",
            "--insecure-skip-sas",
        ],
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
        &[
            "--json",
            "room",
            "join",
            code,
            "--no-auto-sync",
            "--insecure-skip-sas",
        ],
    );
    if !join.status.success() {
        kill_pid(pid);
        panic!(
            "join without init failed: {}",
            String::from_utf8_lossy(&join.stderr)
        );
    }
    assert!(String::from_utf8_lossy(&join.stderr).contains("--insecure-skip-sas"));
    assert_eq!(json_out(&join)["role"], "member");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejecting_before_a_join_lets_a_new_room_create_replace_the_pairing() {
    let (relay, relay_task) = start_relay().await;
    let a = TempDir::new().unwrap();
    seed_identity_file(a.path());
    let first = clipsync(
        a.path(),
        &relay,
        &["--json", "room", "create", "--no-auto-sync"],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first = json_out(&first);
    let first_pid = first["pid"].as_u64().unwrap() as u32;
    let blocked = clipsync(a.path(), &relay, &["room", "create", "--no-auto-sync"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("room join"));
    // "yes" before anyone joined would approve a code nobody has seen yet.
    let premature = clipsync(a.path(), &relay, &["room", "confirm", "yes"]);
    assert!(!premature.status.success());
    assert!(String::from_utf8_lossy(&premature.stderr).contains("digits"));
    let unanswered = waiting_store(a.path())
        .load_pairing_wait()
        .unwrap()
        .unwrap();
    assert_eq!(
        (unanswered.status.as_str(), unanswered.sas_decision),
        ("waiting", None)
    );

    let rejected = clipsync(a.path(), &relay, &["--json", "room", "reject"]);
    assert!(
        rejected.status.success(),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(json_out(&rejected)["joined"], false);

    let second = clipsync(
        a.path(),
        &relay,
        &["--json", "room", "create", "--no-auto-sync"],
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = json_out(&second);
    let second_pid = second["pid"].as_u64().unwrap() as u32;
    assert_ne!(second_pid, first_pid);
    assert_ne!(second["pairing_code"], first["pairing_code"]);
    let wait = waiting_store(a.path())
        .load_pairing_wait()
        .unwrap()
        .unwrap();
    assert_eq!((wait.status.as_str(), wait.pid), ("waiting", second_pid));
    assert!(
        wait.sas_decision.is_none(),
        "the new pairing starts unanswered"
    );
    kill_pid(first_pid);
    kill_pid(second_pid);
    relay_task.abort();
}
