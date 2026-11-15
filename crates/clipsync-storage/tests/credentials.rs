// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use chrono::Utc;
use clipsync_protocol::{DeviceRole, RoomId};
use clipsync_storage::{AppConfig, AppPaths, LocalStore, RoomRecord, StateFile};

fn store(root: &tempfile::TempDir) -> LocalStore {
    LocalStore::open_from(AppPaths::from_root(root.path().to_path_buf())).unwrap()
}

fn token_path(store: &LocalStore, room: &RoomRecord) -> std::path::PathBuf {
    store.paths.data_dir.join(format!(
        "device-token-{}.secret",
        hex::encode(room.room_id.as_str())
    ))
}

fn room() -> RoomRecord {
    RoomRecord {
        room_id: RoomId::new(),
        epoch: 1,
        role: DeviceRole::Owner,
        created_at: Utc::now(),
        device_token: "synthetic-device-token".into(),
        peers: vec![],
    }
}

#[test]
fn device_tokens_are_separate_and_replaceable() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let mut room = room();
    store.upsert_room(room.clone()).unwrap();
    // Repeated saves exercise the existing-file flush path on Windows too.
    store.upsert_room(room.clone()).unwrap();
    let metadata = std::fs::read_to_string(store.paths.state_file()).unwrap();
    assert!(!metadata.contains(&room.device_token));
    assert!(!metadata.contains("device_token"));
    assert_eq!(
        store.current_room().unwrap().device_token,
        room.device_token
    );
    room.device_token = "replacement-device-token".into();
    store.upsert_room(room.clone()).unwrap();
    assert_eq!(
        store.current_room().unwrap().device_token,
        room.device_token
    );
    store.leave_current().unwrap();
    assert!(!token_path(&store, &room).exists());
}

#[test]
fn legacy_state_migrates_without_losing_room_or_token() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let room = room();
    let legacy = StateFile {
        current_room: Some(room.room_id.clone()),
        rooms: vec![room.clone()],
        ..Default::default()
    };
    // Build the old on-disk shape explicitly, even when new serialization omits secrets.
    let mut raw = serde_json::to_value(&legacy).unwrap();
    raw["rooms"][0]["device_token"] = room.device_token.clone().into();
    std::fs::write(store.paths.state_file(), raw.to_string()).unwrap();
    assert_eq!(
        store.current_room().unwrap().device_token,
        room.device_token
    );
    assert_eq!(store.load_state().unwrap().rooms.len(), 1);
    assert!(!std::fs::read_to_string(store.paths.state_file())
        .unwrap()
        .contains(&room.device_token));
    assert!(std::fs::read_to_string(token_path(&store, &room))
        .unwrap()
        .contains(&room.device_token));
}

#[test]
fn legacy_webhook_secret_migrates_and_is_removed_when_disabled() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let raw = "relay_url = 'https://example.com'\n[webhook]\nurl = 'https://example.com/events'\nsecret = 'synthetic-webhook-key'\n";
    std::fs::write(store.paths.config_file(), raw).unwrap();
    let mut cfg = store.load_config().unwrap();
    assert_eq!(
        cfg.webhook.as_ref().unwrap().secret,
        "synthetic-webhook-key"
    );
    assert!(!std::fs::read_to_string(store.paths.config_file())
        .unwrap()
        .contains("synthetic-webhook-key"));
    assert_eq!(
        store.load_config().unwrap().webhook.unwrap().secret,
        "synthetic-webhook-key"
    );
    cfg.webhook = None;
    store.save_config(&cfg).unwrap();
    assert!(!store.paths.data_dir.join("webhook.secret").exists());
}

#[test]
fn missing_credentials_fail_closed_and_leave_metadata_untouched() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let room = room();
    store.upsert_room(room.clone()).unwrap();
    let before = std::fs::read(store.paths.state_file()).unwrap();
    std::fs::remove_file(token_path(&store, &room)).unwrap();
    assert!(store.load_state().is_err());
    assert_eq!(std::fs::read(store.paths.state_file()).unwrap(), before);
}

#[test]
fn failed_migration_preserves_legacy_secret() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let raw = "relay_url = 'https://example.com'\n[webhook]\nurl = 'https://example.com/events'\nsecret = 'synthetic-webhook-key'\n";
    std::fs::write(store.paths.config_file(), raw).unwrap();
    std::fs::create_dir(store.paths.data_dir.join("webhook.secret")).unwrap();
    assert!(store.load_config().is_err());
    assert_eq!(
        std::fs::read_to_string(store.paths.config_file()).unwrap(),
        raw
    );
}

#[cfg(unix)]
#[test]
fn credentials_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let room = room();
    store.upsert_room(room.clone()).unwrap();
    assert_eq!(
        std::fs::metadata(token_path(&store, &room))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn default_config_does_not_need_credentials() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    store.save_config(&AppConfig::default()).unwrap();
    assert!(store.load_config().unwrap().webhook.is_none());
}

#[test]
fn truncated_credential_is_rejected_without_rewriting_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let room = room();
    store.upsert_room(room.clone()).unwrap();
    let before = std::fs::read(store.paths.state_file()).unwrap();
    std::fs::write(token_path(&store, &room), "").unwrap();
    assert!(store.load_state().is_err());
    assert_eq!(std::fs::read(store.paths.state_file()).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn partial_secret_write_does_not_destroy_the_previous_credential() {
    const CHILD: &str = "CLIPSYNC_TEST_PARTIAL_WRITE_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        let store = LocalStore::open_from(AppPaths::from_root(root.into())).unwrap();
        let previous = std::fs::read(store.paths.data_dir.join("partial.secret")).unwrap();
        assert!(store
            .secrets
            .save_named("partial", &"x".repeat(1024))
            .is_err());
        assert_eq!(
            std::fs::read(store.paths.data_dir.join("partial.secret")).unwrap(),
            previous
        );
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    std::fs::write(
        store.paths.data_dir.join("partial.secret"),
        "previous synthetic credential",
    )
    .unwrap();
    // The OS enforces a short write in a separate process. No fault-injection
    // hook, process-wide limit, or signal handler affects the normal test suite.
    let output = std::process::Command::new("python3")
        .arg("-c")
        .arg("import os,resource,signal,sys; signal.signal(signal.SIGXFSZ,signal.SIG_IGN); resource.setrlimit(resource.RLIMIT_FSIZE,(64,64)); os.execv(sys.argv[1],sys.argv[1:])")
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "partial_secret_write_does_not_destroy_the_previous_credential", "--nocapture"])
        .env(CHILD, root.path())
        // The intentional file-size limit would also truncate LLVM's exit-time
        // profile. Discard only this fault-injection child's profile; ordinary
        // storage tests still measure the real storage operations.
        .env("LLVM_PROFILE_FILE", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "partial-write worker failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(store.paths.data_dir.join("partial.secret")).unwrap(),
        "previous synthetic credential"
    );
}
