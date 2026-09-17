use clipsync_crypto::{DeviceIdentity, StoredIdentity};
use tracing::warn;

use crate::paths::{set_private_file, AppPaths};
use crate::{Result, StorageError};

#[cfg(target_os = "macos")]
const SERVICE: &str = "dev.clipsync.cli";

#[derive(Clone)]
pub struct SecretStore {
    paths: AppPaths,
}

impl SecretStore {
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    fn account(&self, name: &str) -> String {
        format!("{}:{name}", self.paths.data_dir.display())
    }

    pub fn save_identity(&self, identity: &DeviceIdentity) -> Result<()> {
        let stored = identity.to_stored();
        let payload = serde_json::to_string(&stored)?;
        let path = self.paths.identity_file();
        std::fs::write(&path, &payload)?;
        set_private_file(&path)?;
        if let Err(e) = set_keyring(&self.account("identity"), &payload) {
            warn!(error = %e, "keychain unavailable; identity kept in 0600 file");
        }
        Ok(())
    }

    pub fn load_identity(&self) -> Result<DeviceIdentity> {
        let path = self.paths.identity_file();
        if path.exists() {
            let payload = std::fs::read_to_string(path)?;
            let stored: StoredIdentity = serde_json::from_str(&payload)?;
            return DeviceIdentity::from_stored(&stored).ok_or(StorageError::NotInitialized);
        }
        if let Ok(payload) = get_keyring(&self.account("identity")) {
            let stored: StoredIdentity = serde_json::from_str(&payload)?;
            let identity =
                DeviceIdentity::from_stored(&stored).ok_or(StorageError::NotInitialized)?;
            if let Err(e) = std::fs::write(&path, &payload) {
                warn!(error = %e, "could not cache identity file from keychain");
            } else {
                let _ = set_private_file(&path);
            }
            return Ok(identity);
        }
        Err(StorageError::NotInitialized)
    }

    pub fn save_named(&self, name: &str, value: &str) -> Result<()> {
        let path = self.paths.data_dir.join(format!("{name}.secret"));
        std::fs::write(&path, value)?;
        set_private_file(&path)?;
        if let Err(e) = set_keyring(&self.account(name), value) {
            warn!(error = %e, key = name, "keychain unavailable");
        }
        Ok(())
    }

    pub fn load_named(&self, name: &str) -> Result<String> {
        let path = self.paths.data_dir.join(format!("{name}.secret"));
        if path.exists() {
            return Ok(std::fs::read_to_string(path)?);
        }
        get_keyring(&self.account(name)).map_err(|_| StorageError::NotPaired)
    }

    pub fn delete_named(&self, name: &str) -> Result<()> {
        let _ = delete_keyring(&self.account(name));
        let path = self.paths.data_dir.join(format!("{name}.secret"));
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn set_keyring(user: &str, value: &str) -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, user).map_err(|e| e.to_string())?;
    entry.set_password(value).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn set_keyring(_user: &str, _value: &str) -> std::result::Result<(), String> {
    Err("os keychain not enabled on this platform".into())
}

#[cfg(target_os = "macos")]
fn get_keyring(user: &str) -> std::result::Result<String, String> {
    let entry = keyring::Entry::new(SERVICE, user).map_err(|e| e.to_string())?;
    entry.get_password().map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn get_keyring(_user: &str) -> std::result::Result<String, String> {
    Err("os keychain not enabled on this platform".into())
}

#[cfg(target_os = "macos")]
fn delete_keyring(user: &str) -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, user).map_err(|e| e.to_string())?;
    entry.delete_credential().map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn delete_keyring(_user: &str) -> std::result::Result<(), String> {
    Ok(())
}
