// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_core::{App, AppPaths};

#[test]
fn init_preserves_legacy_config_when_secret_migration_fails() {
    let root = tempfile::tempdir().unwrap();
    let app = App::open_from(AppPaths::from_root(root.path().to_path_buf())).unwrap();
    let raw = "relay_url = 'https://example.com'\n[webhook]\nurl = 'https://example.com/events'\nsecret = 'synthetic-webhook-key'\n";
    std::fs::write(app.store.paths.config_file(), raw).unwrap();
    std::fs::create_dir(app.store.paths.data_dir.join("webhook.secret")).unwrap();
    assert!(app.init(None).is_err());
    assert_eq!(
        std::fs::read_to_string(app.store.paths.config_file()).unwrap(),
        raw
    );
}

#[test]
fn init_preserves_invalid_configuration_instead_of_resetting_it() {
    let root = tempfile::tempdir().unwrap();
    let app = App::open_from(AppPaths::from_root(root.path().to_path_buf())).unwrap();
    let raw = "relay_url = [invalid toml";
    std::fs::write(app.store.paths.config_file(), raw).unwrap();
    assert!(app.init(None).is_err());
    assert_eq!(
        std::fs::read_to_string(app.store.paths.config_file()).unwrap(),
        raw
    );
}

#[test]
fn opening_an_unavailable_home_preserves_the_existing_path() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("selected-home");
    std::fs::write(&blocked, b"preserve existing file").unwrap();
    assert!(App::open_from(AppPaths::from_root(blocked.clone())).is_err());
    assert_eq!(std::fs::read(blocked).unwrap(), b"preserve existing file");
}

#[test]
fn failed_identity_persistence_preserves_the_target_and_configuration() {
    let root = tempfile::tempdir().unwrap();
    let app = App::open_from(AppPaths::from_root(root.path().into())).unwrap();
    // save_identity persists its private file before touching a native keyring.
    std::fs::create_dir(app.store.paths.identity_file()).unwrap();
    assert!(app.init(Some("http://127.0.0.1:2".into())).is_err());
    assert!(app.store.paths.identity_file().is_dir());
    assert!(!app.store.paths.config_file().exists());
}
