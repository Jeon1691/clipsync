// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::process::{Command, Output};

fn cli(root: &std::path::Path, json: bool, args: &[&str]) -> Output {
    command(root, json, args).output().unwrap()
}

fn command(root: &std::path::Path, json: bool, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_clipsync"));
    command
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root.join("appdata"))
        .env("LOCALAPPDATA", root.join("local-appdata"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tools = root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        for name in ["systemctl", "launchctl"] {
            let path = tools.join(name);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut paths = vec![tools];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
    command
        .env("RUST_LOG", "error")
        .env("CLIPSYNC_HOME", root)
        .env("CLIPSYNC_NO_DAEMON", "1")
        .env("CLIPSYNC_RELAY_URL", "http://127.0.0.1:1")
        .env_remove("CLIPSYNC_LISTEN");
    if json {
        command.arg("--json");
    }
    command.args(args);
    command
}

#[test]
fn local_cli_configuration_and_confirmation_contracts() {
    let root = tempfile::tempdir().unwrap();
    for json in [false, true] {
        for args in [
            vec!["config", "get"],
            vec!["config", "get", "sync.enabled"],
            vec!["config", "set", "notify.preview", "true"],
            vec!["status"],
            vec!["room", "list"],
            vec!["sync", "pause"],
            vec!["sync", "resume"],
            vec!["doctor"],
        ] {
            let output = cli(root.path(), json, &args);
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if json {
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
            }
            if args == ["config", "set", "notify.preview", "true"] {
                let readback = cli(root.path(), true, &["config", "get", "notify.preview"]);
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&readback.stdout).unwrap()
                        ["notify.preview"],
                    "true"
                );
            }
            if args == ["sync", "pause"] || args == ["sync", "resume"] {
                let status = cli(root.path(), true, &["status"]);
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&status.stdout).unwrap()["paused"],
                    args[1] == "pause"
                );
            }
        }
    }
    let rejected = cli(root.path(), true, &["room", "leave"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("confirmation required"));
    for args in [
        vec!["room", "leave", "--yes"],
        vec!["--non-interactive", "room", "leave"],
        vec!["--yes", "room", "leave"],
    ] {
        assert!(cli(root.path(), true, &args).status.success());
    }
    assert!(!cli(root.path(), true, &["devices"]).status.success());
    assert!(
        !cli(root.path(), true, &["config", "set", "invalid.key", "x"])
            .status
            .success()
    );
    let unknown = cli(root.path(), true, &["config", "get", "invalid.key"]);
    assert!(unknown.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&unknown.stdout).unwrap(),
        serde_json::json!({"invalid.key": null})
    );
}

#[test]
fn logs_report_absence_then_preserve_saved_text() {
    let root = tempfile::tempdir().unwrap();
    let output = cli(root.path(), true, &["logs"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("no logs yet")
            || String::from_utf8_lossy(&output.stderr).contains("no logs yet")
    );
    let paths = clipsync_storage::AppPaths::from_root(root.path().to_path_buf());
    std::fs::write(paths.log_file(), "saved log\nsecond line\n").unwrap();
    let output = cli(root.path(), false, &["logs"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"saved log\nsecond line\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_push_and_pull_preserve_daemon_payloads_in_json_and_human_output() {
    const CHILD: &str = "CLIPSYNC_TEST_CLI_IPC";
    if std::env::var_os(CHILD).is_none() {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "cli_push_and_pull_preserve_daemon_payloads_in_json_and_human_output",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove(clipsync_daemon::LISTEN_ENV)
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(20), command.output())
            .await
            .expect("isolated IPC exchange exceeded its deadline")
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use clipsync_daemon::{
        read_request, write_response, IpcRequest, IpcResponse, IpcServer, PullPayload,
    };
    let root = tempfile::tempdir().unwrap();
    let paths = clipsync_storage::AppPaths::from_root(root.path().to_path_buf());
    paths.ensure().unwrap();
    let server = IpcServer::bind(&paths).await.unwrap();
    let responder = tokio::spawn(async move {
        for _ in 0..2 {
            for payload in [
                None,
                Some("received text"),
                Some("received text\n"),
                Some(""),
            ] {
                let mut stream = server.accept().await.unwrap();
                let request = read_request(&mut stream).await.unwrap().unwrap();
                let pull = if let Some(text) = payload {
                    assert!(matches!(request, IpcRequest::Pull));
                    Some(PullPayload {
                        kind: if text.is_empty() { "image" } else { "text" }.into(),
                        text: if text.is_empty() {
                            None
                        } else {
                            Some(text.into())
                        },
                        summary: "received item".into(),
                    })
                } else {
                    assert!(
                        matches!(request, IpcRequest::PushText { text } if text == "stdin payload")
                    );
                    None
                };
                write_response(
                    &mut stream,
                    &IpcResponse {
                        ok: true,
                        error: None,
                        status: None,
                        pull,
                    },
                )
                .await
                .unwrap();
            }
        }
    });
    for json in [false, true] {
        use std::io::Write;
        use std::process::Stdio;
        let mut push = Command::new(env!("CARGO_BIN_EXE_clipsync"));
        if json {
            push.arg("--json");
        }
        let mut push = push
            .args(["push", "--type", "text"])
            .env("RUST_LOG", "error")
            .env("CLIPSYNC_HOME", root.path())
            .env_remove(clipsync_daemon::LISTEN_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        push.stdin
            .take()
            .unwrap()
            .write_all(b"stdin payload")
            .unwrap();
        let output = push.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if json {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["ok"],
                true
            );
        }
        for expected in ["received text\n", "received text\n", "received item\n"] {
            let output = cli(root.path(), json, &["pull"]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            if json {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["summary"],
                    "received item"
                );
            } else {
                assert_eq!(output.stdout, expected.as_bytes());
            }
        }
    }
    responder.await.unwrap();
}

#[test]
fn initialized_identity_and_paired_devices_are_visible_in_both_output_modes() {
    use clipsync_protocol::{DeviceId, DeviceRole, RoomId};
    use clipsync_storage::{AppPaths, LocalStore, PeerRecord, RoomRecord};
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_root(root.path().to_path_buf());
    let store = LocalStore::open_from(paths.clone()).unwrap();
    // A valid file-backed identity avoids creating native Keychain entries.
    let identity = clipsync_crypto::DeviceIdentity::generate();
    std::fs::write(
        paths.identity_file(),
        serde_json::to_vec(&identity.to_stored()).unwrap(),
    )
    .unwrap();
    let peer = PeerRecord {
        device_id: DeviceId::new(),
        device_name: "fixture peer".into(),
        os: "fixture-os".into(),
        role: DeviceRole::Member,
        public_key_hex: "public fixture".into(),
        fingerprint: "fixture-fingerprint".into(),
    };
    let room = RoomRecord {
        room_id: RoomId::new(),
        epoch: 3,
        role: DeviceRole::Owner,
        created_at: chrono::Utc::now(),
        device_token: "synthetic fixture token".into(),
        peers: vec![peer.clone()],
    };
    store.upsert_room(room.clone()).unwrap();
    for json in [false, true] {
        let initialized = cli(
            root.path(),
            json,
            &["init", "--relay-url", "http://127.0.0.1:4567"],
        );
        assert!(
            initialized.status.success(),
            "{}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let output = String::from_utf8(initialized.stdout).unwrap();
        assert!(output.contains(identity.device_id.as_str()));
        assert!(output.contains(&identity.fingerprint()));
        assert_eq!(
            store.load_config().unwrap().relay_url,
            "http://127.0.0.1:4567"
        );
        let devices = cli(root.path(), json, &["devices"]);
        assert!(devices.status.success());
        let output = String::from_utf8(devices.stdout).unwrap();
        assert!(output.contains(peer.device_id.as_str()));
        assert!(output.contains("fixture peer"));
        assert!(output.contains("fixture-fingerprint"));
        let rooms = cli(root.path(), json, &["room", "list"]);
        assert!(rooms.status.success());
        assert!(String::from_utf8_lossy(&rooms.stdout).contains(room.room_id.as_str()));
    }
}

#[cfg(unix)]
#[test]
fn daemon_commands_write_only_isolated_service_files_and_preserve_json_contracts() {
    let root = tempfile::tempdir().unwrap();
    let paths = clipsync_storage::AppPaths::from_root(root.path().to_path_buf());
    paths.ensure().unwrap();
    let identity = clipsync_crypto::DeviceIdentity::generate();
    std::fs::write(
        paths.identity_file(),
        serde_json::to_vec(&identity.to_stored()).unwrap(),
    )
    .unwrap();
    for action in ["start", "restart", "stop"] {
        let output = cli(root.path(), true, &["daemon", action]);
        assert!(
            output.status.success(),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = match action {
            "start" => "started",
            "restart" => "restarted",
            _ => "stopped",
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["daemon"],
            expected
        );
        #[cfg(target_os = "macos")]
        let service = root
            .path()
            .join("Library/LaunchAgents/dev.clipsync.daemon.plist");
        #[cfg(target_os = "linux")]
        let service = root.path().join(".config/systemd/user/clipsync.service");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert_eq!(service.exists(), action != "stop");
    }
    let invalid_ttl = cli(
        root.path(),
        false,
        &["room", "create", "--foreground", "--ttl", "not-a-duration"],
    );
    assert_eq!(invalid_ttl.status.code(), Some(1));
    // The in-house `--ttl` parser keeps humantime's rejection text.
    assert!(String::from_utf8_lossy(&invalid_ttl.stderr).contains("error: expected number at 0"));
    // One second of nanoseconds carried into u64::MAX seconds used to panic.
    let overflowing_ttl = cli(
        root.path(),
        false,
        &[
            "room",
            "create",
            "--foreground",
            "--ttl",
            "18446744073709551615s 1000000000ns",
        ],
    );
    assert_eq!(overflowing_ttl.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&overflowing_ttl.stderr).contains("error: number is too large"));
}

#[test]
fn every_local_command_reports_unavailable_storage_without_resetting_it() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("not-a-directory");
    std::fs::write(&blocked, b"preserve selected home").unwrap();
    for args in [
        vec!["init"],
        vec!["room", "create", "--foreground"],
        vec!["room", "join", "482913"],
        vec!["room", "leave", "--yes"],
        vec!["room", "list"],
        vec!["push", "--type", "text"],
        vec!["pull"],
        vec!["watch"],
        vec!["sync", "pause"],
        vec!["sync", "resume"],
        vec!["daemon", "start"],
        vec!["daemon", "restart"],
        vec!["daemon", "run"],
        vec!["status"],
        vec!["devices"],
        vec!["config", "get"],
        vec!["config", "set", "sync.enabled", "false"],
        vec!["doctor"],
        vec!["logs"],
    ] {
        let output = command(root.path(), true, &args)
            .env("CLIPSYNC_HOME", &blocked)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
        assert_eq!(std::fs::read(&blocked).unwrap(), b"preserve selected home");
    }
}

#[test]
fn commands_preserve_corrupt_config_and_state_and_report_the_responsible_failure() {
    let root = tempfile::tempdir().unwrap();
    let paths = clipsync_storage::AppPaths::from_root(root.path().into());
    paths.ensure().unwrap();
    let identity = clipsync_crypto::DeviceIdentity::generate();
    std::fs::write(
        paths.identity_file(),
        serde_json::to_vec(&identity.to_stored()).unwrap(),
    )
    .unwrap();
    std::fs::write(paths.config_file(), b"invalid = [").unwrap();
    for args in [
        vec!["init"],
        vec!["room", "create", "--foreground"],
        vec!["room", "join", "482913"],
        vec!["push", "--type", "text"],
        vec!["pull"],
        vec!["status"],
        vec!["config", "get"],
        vec!["config", "set", "sync.enabled", "true"],
        vec!["daemon", "start"],
        vec!["daemon", "restart"],
    ] {
        let output = cli(root.path(), true, &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("toml"));
        assert_eq!(std::fs::read(paths.config_file()).unwrap(), b"invalid = [");
    }
    std::fs::remove_file(paths.config_file()).unwrap();
    std::fs::write(paths.state_file(), b"invalid state").unwrap();
    for args in [
        vec!["room", "list"],
        vec!["room", "leave", "--yes"],
        vec!["devices"],
        vec!["status"],
        vec!["sync", "pause"],
        vec!["sync", "resume"],
    ] {
        let output = cli(root.path(), true, &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("serialize"));
        assert_eq!(std::fs::read(paths.state_file()).unwrap(), b"invalid state");
    }
}

fn saved_relay_url(root: &std::path::Path) -> serde_json::Value {
    let readback = cli(root, true, &["config", "get", "relay_url"]);
    assert!(
        readback.status.success(),
        "{}",
        String::from_utf8_lossy(&readback.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&readback.stdout).unwrap()["relay_url"].clone()
}

#[test]
fn init_and_config_set_refuse_plaintext_relays_unless_loopback_or_opted_in() {
    let root = tempfile::tempdir().unwrap();
    // `config set` avoids creating an identity (and an OS keychain entry).
    let saved = cli(
        root.path(),
        false,
        &["config", "set", "relay_url", "https://relay.example.test"],
    );
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    assert_eq!(saved_relay_url(root.path()), "https://relay.example.test");

    for args in [
        vec!["init", "--relay-url", "http://relay.example.test"],
        vec!["config", "set", "relay_url", "http://relay.example.test"],
        vec!["config", "set", "relay_url", "ws://192.168.1.20:7600"],
    ] {
        let rejected = cli(root.path(), false, &args);
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert_eq!(rejected.status.code(), Some(2), "{args:?}: {stderr}");
        for hint in ["plaintext", "https://", "CLIPSYNC_ALLOW_INSECURE_RELAY"] {
            assert!(stderr.contains(hint), "{args:?}: {stderr} lacks {hint}");
        }
        assert_eq!(saved_relay_url(root.path()), "https://relay.example.test");
    }
    for args in [
        vec!["init", "--relay-url", "relay.example.test"],
        vec!["config", "set", "relay_url", "localhost:7600"],
    ] {
        let rejected = cli(root.path(), false, &args);
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert_eq!(rejected.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("no scheme"), "{args:?}: {stderr}");
        assert_eq!(saved_relay_url(root.path()), "https://relay.example.test");
    }

    let loopback = cli(
        root.path(),
        false,
        &["config", "set", "relay_url", "http://localhost:7600"],
    );
    assert!(
        loopback.status.success(),
        "{}",
        String::from_utf8_lossy(&loopback.stderr)
    );
    assert_eq!(saved_relay_url(root.path()), "http://localhost:7600");

    let opted_in = command(
        root.path(),
        false,
        &["config", "set", "relay_url", "http://relay.example.test"],
    )
    .env("CLIPSYNC_ALLOW_INSECURE_RELAY", "1")
    .env("RUST_LOG", "warn")
    .output()
    .unwrap();
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&opted_in.stdout),
        String::from_utf8_lossy(&opted_in.stderr)
    );
    assert!(opted_in.status.success(), "{logs}");
    assert!(logs.contains("CLIPSYNC_ALLOW_INSECURE_RELAY"), "{logs}");
    assert!(logs.contains("plaintext"), "{logs}");
    assert_eq!(saved_relay_url(root.path()), "http://relay.example.test");
}
