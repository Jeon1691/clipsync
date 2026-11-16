// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_protocol::SyncDirection;
use clipsync_storage::{AppConfig, AppPaths, LocalStore, StorageError};

#[test]
fn stored_toml_roundtrip_preserves_config_values_and_escapes() {
    let root = tempfile::tempdir().unwrap();
    let store = LocalStore::open_from(AppPaths::from_root(root.path().into())).unwrap();
    std::fs::write(
        store.paths.config_file(),
        r#"relay_url = "https://example.test/π?quote=\"quoted\"&path=C:\\clipsync\\inbox"
[sync]
enabled = false
direction = "receive_only"
auto_start = false
types = ["files", "image", "text"]
debounce_ms = 341
ttl_secs = 60
[notify]
enabled = false
preview = true
[webhook]
url = "https://hooks.example.test/π"
"#,
    )
    .unwrap();
    std::fs::write(
        store.paths.data_dir.join("webhook.secret"),
        b"synthetic-roundtrip-credential",
    )
    .unwrap();

    let assert_values = |config: &AppConfig| {
        assert_eq!(
            config.relay_url,
            "https://example.test/π?quote=\"quoted\"&path=C:\\clipsync\\inbox"
        );
        assert!(!config.sync.enabled);
        assert_eq!(config.sync.direction, SyncDirection::ReceiveOnly);
        assert!(!config.sync.auto_start);
        assert_eq!(config.sync.types, ["files", "image", "text"]);
        assert_eq!(config.sync.debounce_ms, 341);
        assert_eq!(config.sync.ttl_secs, 60);
        // Files written before the envelope v2 transition omit this key.
        assert!(!config.sync.legacy_envelopes);
        assert!(!config.notify.enabled);
        assert!(config.notify.preview);
        let webhook = config.webhook.as_ref().unwrap();
        assert_eq!(webhook.url, "https://hooks.example.test/π");
        assert_eq!(webhook.secret, "synthetic-roundtrip-credential");
    };
    let config = store.load_config().unwrap();
    assert_values(&config);
    store.save_config(&config).unwrap();
    assert_values(&store.load_config().unwrap());
}

#[test]
fn invalid_toml_preserves_config_and_does_not_migrate_credentials() {
    for raw in [
        "relay_url = 'https://example.test'\n[webhook]\nurl = 'https://hooks.example.test'\nsecret = 'synthetic-legacy-credential'\ninvalid = [\n",
        "relay_url = 'https://example.test'\nrelay_url = 'https://replacement.example.test'\n[webhook]\nurl = 'https://hooks.example.test'\nsecret = 'synthetic-legacy-credential'\n",
        "relay_url = 'https://example.test'\n[webhook]\nurl = 'https://hooks.example.test'\nsecret = 'synthetic-legacy-credential'\nurl = 'https://replacement.example.test'\n",
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = LocalStore::open_from(AppPaths::from_root(root.path().into())).unwrap();
        std::fs::write(store.paths.config_file(), raw).unwrap();

        assert!(matches!(store.load_config(), Err(StorageError::Toml(_))));
        assert_eq!(std::fs::read(store.paths.config_file()).unwrap(), raw.as_bytes());
        assert!(!store.paths.data_dir.join("webhook.secret").exists());
    }
}

#[test]
fn supported_configuration_keys_roundtrip_and_unknown_keys_do_not_mutate() {
    let mut config = AppConfig::default();
    for (key, value, expected) in [
        (
            "relay_url",
            "http://127.0.0.1:4567",
            "http://127.0.0.1:4567",
        ),
        ("sync.enabled", "off", "false"),
        ("sync.auto_start", "yes", "true"),
        ("notify.enabled", "1", "true"),
        ("notify.preview", "on", "true"),
        ("sync.ttl_secs", "60", "60"),
        ("sync.legacy_envelopes", "on", "true"),
        ("sync.legacy_envelopes", "no", "false"),
    ] {
        assert!(config.set(key, value));
        assert_eq!(config.get(key).as_deref(), Some(expected));
    }
    for (value, expected) in [
        ("send-only", SyncDirection::SendOnly),
        ("send_only", SyncDirection::SendOnly),
        ("receive-only", SyncDirection::ReceiveOnly),
        ("receive_only", SyncDirection::ReceiveOnly),
        ("both", SyncDirection::Both),
    ] {
        assert!(config.set("sync.direction", value));
        assert_eq!(config.sync.direction, expected);
        assert_eq!(
            config.get("sync.direction").unwrap(),
            format!("{expected:?}").to_lowercase()
        );
    }
    let previous = serde_json::to_value(&config).unwrap();
    assert!(!config.set("unknown.key", "value"));
    assert!(!config.set("sync.direction", "invalid"));
    assert!(!config.set("sync.ttl_secs", "-1"));
    assert_eq!(serde_json::to_value(&config).unwrap(), previous);
    assert!(config.get("unknown.key").is_none());
}

#[test]
fn configuration_clamps_ttl_and_never_serializes_a_legacy_webhook_secret() {
    let mut config: AppConfig = toml::from_str("relay_url='http://127.0.0.1'\n[webhook]\nurl='https://hooks.example.test'\nsecret='synthetic-legacy-value'").unwrap();
    assert!(config.set("sync.ttl_secs", &u64::MAX.to_string()));
    assert_eq!(config.sync.ttl_secs, clipsync_protocol::HARD_MAX_TTL_SECS);
    let json = serde_json::to_string(&config).unwrap();
    assert!(!json.contains("synthetic-legacy-value"));
    assert!(!json.contains("secret"));
    assert!(json.contains("hooks.example.test"));
}
