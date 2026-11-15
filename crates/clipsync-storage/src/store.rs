// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use chrono::{DateTime, Utc};
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
    pub created_at: DateTime<Utc>,
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
        let path = self.paths.config_file();
        if !path.exists() {
            return Ok(AppConfig::default());
        }
        let raw = std::fs::read_to_string(path)?;
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
        let path = self.paths.state_file();
        if !path.exists() {
            return Ok(StateFile::default());
        }
        let raw = std::fs::read_to_string(path)?;
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

    pub fn save_pairing_wait(&self, wait: &PairingWait) -> Result<()> {
        let path = self.paths.pairing_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(wait)?)?;
        set_private_file(&tmp)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn load_pairing_wait(&self) -> Result<Option<PairingWait>> {
        let path = self.paths.pairing_file();
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&raw)?))
    }

    pub fn clear_pairing_wait(&self) {
        let _ = std::fs::remove_file(self.paths.pairing_file());
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
            let _ = self.secrets.delete_named(&format!("room-{}", room.room_id));
            let _ = self
                .secrets
                .delete_named(&format!("epoch-{}", room.room_id));
        }
        Ok(removed)
    }

    // These credentials use canonical private files, without stale keychain fallback.
    fn load_credential(&self, name: &str) -> Result<String> {
        let value = std::fs::read_to_string(self.paths.data_dir.join(format!("{name}.secret")))?;
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
        self.secrets
            .save_named(&format!("room-{room_id}"), root_key_hex)?;
        self.secrets
            .save_named(&format!("epoch-{room_id}"), epoch_key_hex)?;
        Ok(())
    }

    pub fn load_epoch_key(&self, room_id: &RoomId) -> Result<[u8; 32]> {
        let hex_str = self.secrets.load_named(&format!("epoch-{room_id}"))?;
        decode_key(&hex_str)
    }

    pub fn load_room_root(&self, room_id: &RoomId) -> Result<[u8; 32]> {
        let hex_str = self.secrets.load_named(&format!("room-{room_id}"))?;
        decode_key(&hex_str)
    }

    pub fn save_epoch_key(&self, room_id: &RoomId, key: &[u8; 32]) -> Result<()> {
        self.secrets
            .save_named(&format!("epoch-{room_id}"), &hex::encode(key))
    }
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
