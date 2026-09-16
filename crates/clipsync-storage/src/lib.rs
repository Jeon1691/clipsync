//! Local configuration, credentials, and secret storage.

mod config;
mod paths;
mod secrets;
mod store;

pub use config::{AppConfig, NotifyConfig, SyncConfig};
pub use paths::AppPaths;
pub use secrets::SecretStore;
pub use store::{LocalStore, PeerRecord, RoomRecord, StateFile};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml ser: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("secret store: {0}")]
    Secret(String),
    #[error("not initialized; run `clipsync init`")]
    NotInitialized,
    #[error("not paired with a room")]
    NotPaired,
}

pub type Result<T> = std::result::Result<T, StorageError>;
