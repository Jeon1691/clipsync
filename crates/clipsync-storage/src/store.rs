// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_protocol::Timestamp;
use serde::{Deserialize, Serialize};

use clipsync_protocol::{DeviceId, DeviceRole, RoomId};

use crate::config::AppConfig;
use crate::paths::{set_private_file, AppPaths};
use crate::secrets::SecretStore;
use crate::{Result, StorageError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomRecord {
    pub room_id: RoomId,
    pub epoch: u64,
    pub role: DeviceRole,
    pub created_at: Timestamp,
    // Kept in memory for callers; legacy inline credentials are migrated on load.
    #[serde(default, skip_serializing)]
    pub device_token: String,
    pub peers: Vec<PeerRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerRecord {
    pub device_id: DeviceId,
    pub device_name: String,
    pub os: String,
    pub role: DeviceRole,
    pub public_key_hex: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct StateFile {
    pub current_room: Option<RoomId>,
    pub rooms: Vec<RoomRecord>,
    pub paused: bool,
    pub last_message_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairingWait {
    pub status: String,
    pub pairing_code: String,
    pub room_id: String,
    pub expires_at: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// SAS the background worker asks the user to compare (status `confirm_sas`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sas: Option<String>,
    /// Relay-reported name of the joining device, shown next to the SAS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_device_name: Option<String>,
    /// The user's answer, written by `clipsync room confirm` or `room reject`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sas_decision: Option<String>,
}

/// Exclusive lock on the pairing lock file, held until dropped.
struct PairingFileLock(std::fs::File);

impl Drop for PairingFileLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock too, but Windows may do that
        // lazily; unlock explicitly so the next writer is not delayed.
        let _ = crate::replay::unlock(&self.0);
    }
}

#[derive(Clone)]
pub struct LocalStore {
    pub paths: AppPaths,
    pub secrets: SecretStore,
}

impl LocalStore {
    pub fn open() -> Result<Self> {
        let paths = AppPaths::from_env();
        paths.ensure()?;
        Ok(Self {
            secrets: SecretStore::new(paths.clone()),
            paths,
        })
    }

    pub fn open_from(paths: AppPaths) -> Result<Self> {
        paths.ensure()?;
        Ok(Self {
            secrets: SecretStore::new(paths.clone()),
            paths,
        })
    }

    pub fn load_config(&self) -> Result<AppConfig> {
        let Some(raw) = crate::sharing::read_if_present(&self.paths.config_file())? else {
            return Ok(AppConfig::default());
        };
        let mut cfg: AppConfig = toml::from_str(&raw)?;
        if let Some(webhook) = cfg.webhook.as_mut() {
            if webhook.secret.is_empty() {
                webhook.secret = self.load_credential("webhook")?;
            } else {
                // Persist the secret successfully before removing the legacy copy.
                self.save_config(&cfg)?;
            }
        }
        Ok(cfg)
    }

    pub fn save_config(&self, cfg: &AppConfig) -> Result<()> {
        std::fs::create_dir_all(&self.paths.config_dir)?;
        if let Some(webhook) = &cfg.webhook {
            if webhook.secret.is_empty() {
                return Err(StorageError::Secret("missing webhook credential".into()));
            }
            self.save_credential("webhook", &webhook.secret)?;
        }
        let raw = toml::to_string_pretty(cfg)?;
        crate::secrets::write_private(&self.paths.config_file(), raw.as_bytes())?;
        if cfg.webhook.is_none() {
            self.delete_credential("webhook")?;
        }
        Ok(())
    }

    pub fn load_state(&self) -> Result<StateFile> {
        let Some(raw) = crate::sharing::read_if_present(&self.paths.state_file())? else {
            return Ok(StateFile::default());
        };
        let mut state: StateFile = serde_json::from_str(&raw)?;
        let legacy = state.rooms.iter().any(|room| !room.device_token.is_empty());
        for room in &mut state.rooms {
            if room.device_token.is_empty() {
                room.device_token = self.load_credential(&device_token_name(&room.room_id))?;
            }
        }
        if legacy {
            self.save_state(&state)?;
        }
        Ok(state)
    }

    pub fn save_state(&self, state: &StateFile) -> Result<()> {
        let path = self.paths.state_file();
        for room in &state.rooms {
            if room.device_token.is_empty() {
                return Err(StorageError::Secret("missing device credential".into()));
            }
            self.save_credential(&device_token_name(&room.room_id), &room.device_token)?;
        }
        crate::secrets::write_private(&path, &serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    /// Replace the pairing file. Writes hold the pairing lock, so they never
    /// interleave with [`Self::update_pairing_wait`].
    pub fn save_pairing_wait(&self, wait: &PairingWait) -> Result<()> {
        let _lock = self.lock_pairing_file()?;
        self.write_pairing_wait(wait)
    }

    pub fn load_pairing_wait(&self) -> Result<Option<PairingWait>> {
        self.load_pairing_wait_pausing(crate::sharing::pause_before_retry)
    }

    /// [`Self::load_pairing_wait`] with the pause between retries supplied,
    /// so tests can change the file at exactly that point.
    fn load_pairing_wait_pausing(&self, pause: impl FnMut()) -> Result<Option<PairingWait>> {
        let path = self.paths.pairing_file();
        let raw = crate::sharing::read_unless_removed(
            || std::fs::read_to_string(&path),
            crate::sharing::ACCESS_DENIED_RETRIES,
            pause,
        )?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    /// Read, change and write the pairing file as one step.
    ///
    /// The background worker and `room confirm`/`room reject` run in separate
    /// processes and both change this file. `update` sees the current
    /// contents and returns what to write (`None` writes nothing) plus a
    /// value for the caller. An exclusive lock is held throughout, so no
    /// other writer's change can land between the read and the write and be
    /// lost.
    pub fn update_pairing_wait<T>(
        &self,
        update: impl FnOnce(Option<PairingWait>) -> (Option<PairingWait>, T),
    ) -> Result<T> {
        let _lock = self.lock_pairing_file()?;
        let (next, out) = update(self.load_pairing_wait()?);
        if let Some(next) = next {
            self.write_pairing_wait(&next)?;
        }
        Ok(out)
    }

    pub fn clear_pairing_wait(&self) {
        // Without the lock the file may still be removed; a writer that is
        // mid-update would then just recreate it, as before this lock existed.
        let _lock = self.lock_pairing_file();
        let _ = std::fs::remove_file(self.paths.pairing_file());
    }

    /// Hold an exclusive OS lock on a sidecar file until the guard drops.
    /// The lock file is never removed: deleting it would let two processes
    /// lock different files under the same name.
    fn lock_pairing_file(&self) -> Result<PairingFileLock> {
        let path = self.paths.pairing_lock_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        set_private_file(&path)?;
        // `flock` on Unix (Android included), `LockFileEx` on Windows.
        crate::replay::lock_exclusive(&file)?;
        Ok(PairingFileLock(file))
    }

    fn write_pairing_wait(&self, wait: &PairingWait) -> Result<()> {
        let path = self.paths.pairing_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // The pairing code is a live one-time password: write it like other secrets.
        crate::secrets::write_private(&path, &serde_json::to_vec_pretty(wait)?)?;
        Ok(())
    }

    pub fn current_room(&self) -> Result<RoomRecord> {
        let state = self.load_state()?;
        let id = state.current_room.ok_or(StorageError::NotPaired)?;
        state
            .rooms
            .into_iter()
            .find(|r| r.room_id == id)
            .ok_or(StorageError::NotPaired)
    }

    pub fn upsert_room(&self, room: RoomRecord) -> Result<()> {
        let mut state = self.load_state()?;
        state.rooms.retain(|r| r.room_id != room.room_id);
        state.current_room = Some(room.room_id.clone());
        state.rooms.push(room);
        self.save_state(&state)
    }

    pub fn leave_current(&self) -> Result<Option<RoomRecord>> {
        let mut state = self.load_state()?;
        let current = state.current_room.take();
        let removed = current.and_then(|id| {
            let pos = state.rooms.iter().position(|r| r.room_id == id)?;
            Some(state.rooms.remove(pos))
        });
        self.save_state(&state)?;
        if let Some(room) = &removed {
            self.delete_credential(&device_token_name(&room.room_id))?;
            for prefix in [ROOM_ROOT_PREFIX, ROOM_EPOCH_PREFIX] {
                // Never delete through an ID that cannot name a file: the
                // name could point outside the data directory, and keys are
                // never saved under one.
                if let Ok(name) = room_secret_name(prefix, &room.room_id) {
                    let _ = self.secrets.delete_named(&name);
                }
            }
        }
        Ok(removed)
    }

    // These credentials use canonical private files, without stale keychain fallback.
    fn load_credential(&self, name: &str) -> Result<String> {
        let value =
            crate::sharing::read_existing(&self.paths.data_dir.join(format!("{name}.secret")))?;
        if value.is_empty() {
            return Err(StorageError::Secret("empty credential file".into()));
        }
        Ok(value)
    }

    fn save_credential(&self, name: &str, value: &str) -> Result<()> {
        let path = self.paths.data_dir.join(format!("{name}.secret"));
        if std::fs::read_to_string(&path).ok().as_deref() == Some(value) {
            set_private_file(&path)?;
            // Windows FlushFileBuffers requires a writable handle.
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)?
                .sync_all()?;
            #[cfg(unix)]
            std::fs::File::open(&self.paths.data_dir)?.sync_all()?;
            return Ok(());
        }
        crate::secrets::write_private(&path, value.as_bytes())?;
        Ok(())
    }

    fn delete_credential(&self, name: &str) -> Result<()> {
        match std::fs::remove_file(self.paths.data_dir.join(format!("{name}.secret"))) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save_room_keys(
        &self,
        room_id: &RoomId,
        root_key_hex: &str,
        epoch_key_hex: &str,
    ) -> Result<()> {
        let root_name = room_secret_name(ROOM_ROOT_PREFIX, room_id)?;
        let epoch_name = room_secret_name(ROOM_EPOCH_PREFIX, room_id)?;
        self.secrets.save_named(&root_name, root_key_hex)?;
        self.secrets.save_named(&epoch_name, epoch_key_hex)?;
        Ok(())
    }

    pub fn load_epoch_key(&self, room_id: &RoomId) -> Result<[u8; 32]> {
        let hex_str = self
            .secrets
            .load_named(&room_secret_name(ROOM_EPOCH_PREFIX, room_id)?)?;
        decode_key(&hex_str)
    }

    pub fn load_room_root(&self, room_id: &RoomId) -> Result<[u8; 32]> {
        let hex_str = self
            .secrets
            .load_named(&room_secret_name(ROOM_ROOT_PREFIX, room_id)?)?;
        decode_key(&hex_str)
    }

    pub fn save_epoch_key(&self, room_id: &RoomId, key: &[u8; 32]) -> Result<()> {
        self.secrets.save_named(
            &room_secret_name(ROOM_EPOCH_PREFIX, room_id)?,
            &hex::encode(key),
        )
    }
}

/// Secret-name prefix of a room's root key (`room-{room_id}.secret`).
const ROOM_ROOT_PREFIX: &str = "room-";
/// Secret-name prefix of a room's current epoch key (`epoch-{room_id}.secret`).
const ROOM_EPOCH_PREFIX: &str = "epoch-";

/// Name of a per-room secret. The room ID becomes part of a file name and a
/// keychain account, so it is checked again here even though decoding
/// already checked it: a `RoomId` built with `From` is not checked.
fn room_secret_name(prefix: &str, room_id: &RoomId) -> Result<String> {
    room_id.validate()?;
    Ok(format!("{prefix}{room_id}"))
}

fn device_token_name(room_id: &RoomId) -> String {
    // Encoding prevents a deserialized ID from introducing path separators.
    format!("device-token-{}", hex::encode(room_id.as_str()))
}

fn decode_key(hex_str: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(hex_str.trim()).map_err(|e| StorageError::Secret(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(StorageError::Secret("invalid key length".into()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod failure_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, LocalStore) {
        let root = tempfile::tempdir().unwrap();
        let store = LocalStore::open_from(AppPaths::from_root(root.path().into())).unwrap();
        (root, store)
    }

    fn waiting() -> PairingWait {
        PairingWait {
            status: "waiting".into(),
            pairing_code: "482913".into(),
            room_id: "fixture-room".into(),
            expires_at: "2026-11-15T00:00:00Z".into(),
            pid: 1,
            error: None,
            sas: None,
            peer_device_name: None,
            sas_decision: None,
        }
    }

    #[test]
    fn metadata_read_failures_do_not_silently_initialize_or_replace_directories() {
        let (_root, store) = fixture();
        for path in [
            store.paths.config_file(),
            store.paths.state_file(),
            store.paths.pairing_file(),
        ] {
            std::fs::create_dir(path).unwrap();
        }
        assert!(matches!(store.load_config(), Err(StorageError::Io(_))));
        assert!(matches!(store.load_state(), Err(StorageError::Io(_))));
        assert!(matches!(
            store.load_pairing_wait(),
            Err(StorageError::Io(_))
        ));
        assert!(store.current_room().is_err());
        assert!(store
            .upsert_room(RoomRecord {
                room_id: RoomId::new(),
                epoch: 1,
                role: DeviceRole::Owner,
                created_at: Timestamp::now(),
                device_token: "fixture".into(),
                peers: vec![]
            })
            .is_err());
        assert!(store.leave_current().is_err());
        for path in [
            store.paths.config_file(),
            store.paths.state_file(),
            store.paths.pairing_file(),
        ] {
            assert!(path.is_dir());
        }
    }

    /// Remove `path` while `_reader` keeps it open, as `room leave` does
    /// while another process is reading the file. On Windows the file stays
    /// "delete pending" until that handle closes: closing a handle opened
    /// with `FILE_FLAG_DELETE_ON_CLOSE` marks it for deletion, and every
    /// open in the meantime is refused with "access denied".
    #[cfg(windows)]
    fn remove_while_open(path: &std::path::Path, _reader: &std::fs::File) {
        use std::os::windows::fs::OpenOptionsExt;
        // `DELETE` access right and `FILE_FLAG_DELETE_ON_CLOSE`, from the
        // Win32 `CreateFileW` documentation.
        const DELETE: u32 = 0x0001_0000;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        drop(
            std::fs::OpenOptions::new()
                .access_mode(DELETE)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
                .open(path)
                .unwrap(),
        );
        assert_eq!(
            std::fs::read(path).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "the file should be delete pending"
        );
    }

    #[cfg(not(windows))]
    fn remove_while_open(path: &std::path::Path, _reader: &std::fs::File) {
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_pairing_file_removed_while_it_is_read_reads_as_no_pairing() {
        let (_root, store) = fixture();
        store.save_pairing_wait(&waiting()).unwrap();
        let path = store.paths.pairing_file();
        let mut reader = Some(std::fs::File::open(&path).unwrap());
        remove_while_open(&path, reader.as_ref().unwrap());
        // The other reader finishes during the first pause, which completes
        // the delete.
        let loaded = store.load_pairing_wait_pausing(|| drop(reader.take()));
        assert!(loaded.unwrap().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_pairing_file_that_stays_delete_pending_is_an_error_after_bounded_retries() {
        let (_root, store) = fixture();
        store.save_pairing_wait(&waiting()).unwrap();
        let path = store.paths.pairing_file();
        let reader = std::fs::File::open(&path).unwrap();
        remove_while_open(&path, &reader);
        let mut pauses = 0;
        let error = store.load_pairing_wait_pausing(|| pauses += 1).unwrap_err();
        assert!(matches!(
            error,
            StorageError::Io(ref io) if io.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert_eq!(pauses, crate::sharing::ACCESS_DENIED_RETRIES);
        drop(reader);
    }

    #[test]
    fn failed_metadata_replacements_leave_existing_files_or_directories_available() {
        let (_root, store) = fixture();
        std::fs::create_dir(store.paths.config_file()).unwrap();
        assert!(store.save_config(&AppConfig::default()).is_err());
        assert!(store.paths.config_file().is_dir());
        std::fs::create_dir(store.paths.state_file()).unwrap();
        assert!(store.save_state(&StateFile::default()).is_err());
        assert!(store.paths.state_file().is_dir());
        std::fs::create_dir(store.paths.pairing_file()).unwrap();
        assert!(store.save_pairing_wait(&waiting()).is_err());
        assert!(store.paths.pairing_file().is_dir());
        // The failed replacement must not leave a temporary copy of the code
        // behind: only the pairing file and its lock file remain.
        let left: Vec<_> = std::fs::read_dir(&store.paths.runtime_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                *path != store.paths.pairing_file() && *path != store.paths.pairing_lock_file()
            })
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[cfg(unix)]
    #[test]
    fn pairing_wait_is_written_privately_without_following_a_planted_temp_link() {
        use crate::paths::{PERMISSION_BITS, PRIVATE_FILE_MODE};
        use std::os::unix::fs::PermissionsExt;

        let (root, store) = fixture();
        // The previous writer used this fixed sibling name and followed links there.
        let legacy_temp = store.paths.pairing_file().with_extension("json.tmp");
        let outside = root.path().join("outside-pairing.json");
        std::os::unix::fs::symlink(&outside, &legacy_temp).unwrap();

        store.save_pairing_wait(&waiting()).unwrap();

        assert!(
            !outside.exists(),
            "the pairing code must not be written through a link"
        );
        let metadata = std::fs::symlink_metadata(store.paths.pairing_file()).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(
            metadata.permissions().mode() & PERMISSION_BITS,
            PRIVATE_FILE_MODE
        );
        assert_eq!(
            store.load_pairing_wait().unwrap().unwrap().pairing_code,
            waiting().pairing_code
        );
    }

    #[test]
    fn pairing_file_updates_are_serialized_so_an_answer_is_never_lost() {
        use std::sync::mpsc;
        // Long enough for the second update to reach its read if nothing
        // serialized it; a correct lock passes no matter how long this is.
        const SECOND_UPDATE_HEAD_START: std::time::Duration = std::time::Duration::from_millis(200);
        let (_root, store) = fixture();
        store.save_pairing_wait(&waiting()).unwrap();
        let (inside_tx, inside_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        // The background worker publishes the verification code...
        let publish = std::thread::spawn({
            let store = store.clone();
            move || {
                store.update_pairing_wait(|current| {
                    inside_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                    let mut wait = current.unwrap();
                    wait.status = "confirm_sas".into();
                    wait.sas_decision = None;
                    (Some(wait), ())
                })
            }
        });
        inside_rx.recv().unwrap();
        // ...while `room reject` records the user's answer.
        let reject = std::thread::spawn({
            let store = store.clone();
            move || {
                store.update_pairing_wait(|current| {
                    let mut wait = current.unwrap();
                    wait.sas_decision = Some("rejected".into());
                    (Some(wait), ())
                })
            }
        });
        std::thread::sleep(SECOND_UPDATE_HEAD_START);
        go_tx.send(()).unwrap();
        publish.join().unwrap().unwrap();
        reject.join().unwrap().unwrap();
        let settled = store.load_pairing_wait().unwrap().unwrap();
        assert_eq!(settled.status, "confirm_sas");
        assert_eq!(settled.sas_decision.as_deref(), Some("rejected"));
        // Declining to write leaves the file as it was.
        let seen = store
            .update_pairing_wait(|current| (None, current.map(|wait| wait.status)))
            .unwrap();
        assert_eq!(seen.as_deref(), Some("confirm_sas"));
        store.clear_pairing_wait();
        assert!(store
            .update_pairing_wait(|current| (None, current.is_none()))
            .unwrap());
    }

    #[test]
    fn unavailable_parent_directories_and_credential_cleanup_errors_are_reported() {
        let (root, store) = fixture();
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, b"existing data").unwrap();
        assert!(LocalStore::open_from(AppPaths::from_root(blocked.clone())).is_err());
        let mut paths = store.paths.clone();
        paths.config_dir = blocked.clone();
        let unavailable = LocalStore {
            paths,
            secrets: store.secrets.clone(),
        };
        assert!(unavailable.save_config(&AppConfig::default()).is_err());
        let mut paths = store.paths.clone();
        paths.runtime_dir = blocked.clone();
        let unavailable = LocalStore {
            paths,
            secrets: store.secrets.clone(),
        };
        assert!(unavailable.save_pairing_wait(&waiting()).is_err());
        std::fs::create_dir(store.paths.data_dir.join("webhook.secret")).unwrap();
        assert!(store.delete_credential("webhook").is_err());
        assert!(store.save_config(&AppConfig::default()).is_err());
        assert_eq!(std::fs::read(blocked).unwrap(), b"existing data");
    }

    #[test]
    fn stale_current_room_references_fail_closed_and_leave_can_recover_them() {
        let (_root, store) = fixture();
        store
            .save_state(&StateFile {
                current_room: Some(RoomId::new()),
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(store.current_room(), Err(StorageError::NotPaired)));
        assert!(store.leave_current().unwrap().is_none());
        assert!(store.load_state().unwrap().current_room.is_none());
    }

    #[test]
    fn room_key_paths_refuse_identifiers_that_are_not_file_name_components() {
        let (root, store) = fixture();
        // A room ID such as `x/../../outside` would resolve to a file next to
        // the data directory once the `room-x` directory exists.
        std::fs::create_dir(store.paths.data_dir.join("room-x")).unwrap();
        std::fs::create_dir(store.paths.data_dir.join("epoch-x")).unwrap();
        let outside = root.path().join("outside.secret");
        std::fs::write(&outside, hex::encode([9; 32])).unwrap();
        let before = std::fs::read(&outside).unwrap();
        let hostile = RoomId::from("x/../../outside");
        assert!(matches!(
            store.load_room_root(&hostile),
            Err(StorageError::InvalidId(_))
        ));
        assert!(matches!(
            store.load_epoch_key(&hostile),
            Err(StorageError::InvalidId(_))
        ));
        assert!(matches!(
            store.save_room_keys(&hostile, &hex::encode([1; 32]), &hex::encode([2; 32])),
            Err(StorageError::InvalidId(_))
        ));
        assert!(matches!(
            store.save_epoch_key(&hostile, &[3; 32]),
            Err(StorageError::InvalidId(_))
        ));
        for unsafe_id in ["..", "a\\b", "nul\0", ""] {
            assert!(store
                .load_room_root(&RoomId::from(unsafe_id))
                .is_err_and(|error| matches!(error, StorageError::InvalidId(_))));
        }
        assert_eq!(std::fs::read(&outside).unwrap(), before);
        assert!(std::fs::read_dir(store.paths.data_dir.join("room-x"))
            .unwrap()
            .next()
            .is_none());

        // A state file naming such a room fails to load, so leaving it cannot
        // delete anything through the ID either.
        let state = serde_json::json!({
            "current_room": hostile,
            "rooms": [{
                "room_id": hostile,
                "epoch": 1,
                "role": "owner",
                "created_at": "2026-11-15T00:00:00Z",
                "peers": [],
            }],
        });
        std::fs::write(store.paths.state_file(), state.to_string()).unwrap();
        assert!(matches!(store.leave_current(), Err(StorageError::Serde(_))));
        assert_eq!(std::fs::read(&outside).unwrap(), before);
    }

    #[test]
    fn room_key_failures_stop_before_any_native_secret_store_write() {
        let (_root, store) = fixture();
        let id = RoomId::new();
        let root_path = store.paths.data_dir.join(format!("room-{id}.secret"));
        std::fs::create_dir(&root_path).unwrap();
        assert!(store
            .save_room_keys(&id, &hex::encode([1; 32]), &hex::encode([2; 32]))
            .is_err());
        assert!(store.load_room_root(&id).is_err());
        let epoch_path = store.paths.data_dir.join(format!("epoch-{id}.secret"));
        std::fs::create_dir(&epoch_path).unwrap();
        assert!(store.save_epoch_key(&id, &[3; 32]).is_err());
        assert!(store.load_epoch_key(&id).is_err());
        assert!(root_path.is_dir() && epoch_path.is_dir());
    }
}
