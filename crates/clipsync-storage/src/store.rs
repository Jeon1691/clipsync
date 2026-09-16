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
        Ok(toml::from_str(&raw)?)
    }

    pub fn save_config(&self, cfg: &AppConfig) -> Result<()> {
        std::fs::create_dir_all(&self.paths.config_dir)?;
        let raw = toml::to_string_pretty(cfg)?;
        std::fs::write(self.paths.config_file(), raw)?;
        Ok(())
    }

    pub fn load_state(&self) -> Result<StateFile> {
        let path = self.paths.state_file();
        if !path.exists() {
            return Ok(StateFile::default());
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save_state(&self, state: &StateFile) -> Result<()> {
        let path = self.paths.state_file();
        std::fs::write(&path, serde_json::to_string_pretty(state)?)?;
        set_private_file(&path)?;
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
            let _ = self.secrets.delete_named(&format!("room-{}", room.room_id));
            let _ = self
                .secrets
                .delete_named(&format!("epoch-{}", room.room_id));
        }
        Ok(removed)
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

fn decode_key(hex_str: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(hex_str.trim()).map_err(|e| StorageError::Secret(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(StorageError::Secret("invalid key length".into()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}
