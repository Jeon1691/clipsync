// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_protocol::SyncDirection;
use clipsync_storage::AppConfig;

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
