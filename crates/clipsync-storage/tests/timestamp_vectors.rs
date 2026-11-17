// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Golden vectors for the timestamps in persisted state.
//!
//! `state.json` stores each room's `created_at` as RFC 3339, and existing
//! installs must keep loading it. The pairing file stores `expires_at` as an
//! opaque string. Both are pinned byte for byte across a load and save.

use clipsync_storage::{AppPaths, LocalStore, PairingWait};
use serde_json::{json, Value};

fn store(root: &tempfile::TempDir) -> LocalStore {
    LocalStore::open_from(AppPaths::from_root(root.path().to_path_buf())).unwrap()
}

/// (`created_at` as an older install wrote it, `created_at` after a re-save).
const CREATED_AT: &[(&str, &str)] = &[
    ("2026-11-15T00:00:00Z", "2026-11-15T00:00:00Z"),
    ("2026-11-15T01:02:03.123456Z", "2026-11-15T01:02:03.123456Z"),
    (
        "2026-11-15T01:02:03.123456789Z",
        "2026-11-15T01:02:03.123456789Z",
    ),
    ("2026-11-15T01:02:03.100Z", "2026-11-15T01:02:03.100Z"),
    ("2026-11-15T10:02:03+09:00", "2026-11-15T01:02:03Z"),
    ("2026-11-15T01:02:03.5+00:00", "2026-11-15T01:02:03.500Z"),
    ("2016-12-31T23:59:60Z", "2016-12-31T23:59:60Z"),
];

fn state_with(created_at: &str, legacy_token: bool) -> Value {
    let mut room = json!({
        "room_id": "room-vector",
        "epoch": 1,
        "role": "owner",
        "created_at": created_at,
        "peers": [],
    });
    if legacy_token {
        room["device_token"] = json!("synthetic-device-token");
    }
    json!({
        "current_room": "room-vector",
        "rooms": [room],
        "paused": false,
        "last_message_id": null,
    })
}

fn saved_created_at(store: &LocalStore) -> String {
    let raw = std::fs::read_to_string(store.paths.state_file()).unwrap();
    let saved: Value = serde_json::from_str(&raw).unwrap();
    saved["rooms"][0]["created_at"].as_str().unwrap().to_owned()
}

#[test]
fn room_created_at_survives_load_and_save_in_canonical_form() {
    for (written, canonical) in CREATED_AT {
        let root = tempfile::tempdir().unwrap();
        let store = store(&root);
        store.paths.ensure().unwrap();
        // A legacy inline token makes `load_state` migrate and re-save.
        std::fs::write(
            store.paths.state_file(),
            state_with(written, true).to_string(),
        )
        .unwrap();
        let mut state = store.load_state().unwrap();
        assert_eq!(saved_created_at(&store), *canonical, "migrated {written}");
        assert_eq!(
            serde_json::to_value(&state.rooms[0]).unwrap()["created_at"],
            *canonical
        );
        // Loading the canonical file and saving it again is a fixed point.
        state.paused = true;
        store.save_state(&state).unwrap();
        assert_eq!(saved_created_at(&store), *canonical, "re-saved {written}");
        let reloaded = store.load_state().unwrap();
        assert_eq!(
            serde_json::to_value(&reloaded.rooms[0]).unwrap()["created_at"],
            *canonical
        );
    }
}

#[test]
fn unreadable_created_at_fails_to_load() {
    for written in ["2026-11-15", "1763164800", "2026-11-15T00:00:00", ""] {
        let root = tempfile::tempdir().unwrap();
        let store = store(&root);
        store.paths.ensure().unwrap();
        std::fs::write(
            store.paths.state_file(),
            state_with(written, false).to_string(),
        )
        .unwrap();
        assert!(store.load_state().is_err(), "{written:?}");
    }
}

#[test]
fn pairing_file_expiry_is_kept_verbatim() {
    for expires_at in [
        "2099-01-02T03:04:05+00:00",
        "2099-01-02T03:04:05.123456789+00:00",
        "2099-01-02T03:04:05.120+00:00",
        "2098-12-31T23:59:60.500+00:00",
        "+10000-01-01T00:00:00+00:00",
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = store(&root);
        store.paths.ensure().unwrap();
        store
            .save_pairing_wait(&PairingWait {
                status: "waiting".into(),
                pairing_code: "246810".into(),
                room_id: "room-vector".into(),
                expires_at: expires_at.into(),
                pid: 1,
                error: None,
                sas: None,
                peer_device_name: None,
                sas_decision: None,
            })
            .unwrap();
        let loaded = store.load_pairing_wait().unwrap().unwrap();
        assert_eq!(loaded.expires_at, expires_at);
    }
}
