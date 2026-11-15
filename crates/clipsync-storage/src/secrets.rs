// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_crypto::{DeviceIdentity, StoredIdentity};
use std::io::Write;
use std::path::Path;
use tracing::warn;

use crate::paths::{set_private_file, AppPaths};
use crate::{Result, StorageError};

#[cfg(any(target_os = "macos", target_os = "windows"))]
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
        write_private(&path, payload.as_bytes())?;
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
        write_private(&path, value.as_bytes())?;
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

/// Create with private permissions before writing, then atomically replace.
/// A failed write leaves the previous credential available for retry/recovery.
pub(crate) fn write_private(path: &Path, value: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    set_private_file(tmp.path())?;
    tmp.write_all(value)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|error| error.error)?;
    // Make credential renames durable before removing a legacy metadata copy.
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn set_keyring(user: &str, value: &str) -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, user).map_err(|e| e.to_string())?;
    entry.set_password(value).map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn set_keyring(_user: &str, _value: &str) -> std::result::Result<(), String> {
    Err("os keychain not enabled on this platform".into())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn get_keyring(user: &str) -> std::result::Result<String, String> {
    let entry = keyring::Entry::new(SERVICE, user).map_err(|e| e.to_string())?;
    entry.get_password().map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn get_keyring(_user: &str) -> std::result::Result<String, String> {
    Err("os keychain not enabled on this platform".into())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn delete_keyring(user: &str) -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, user).map_err(|e| e.to_string())?;
    entry.delete_credential().map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn delete_keyring(_user: &str) -> std::result::Result<(), String> {
    Ok(())
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod regression_tests {
    use super::*;

    #[test]
    fn keyring_only_identity_is_cached_for_file_based_background_clients() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().to_path_buf());
        paths.ensure().unwrap();
        let store = SecretStore::new(paths.clone());
        let identity = DeviceIdentity::generate();
        store.save_identity(&identity).unwrap();
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = delete_keyring(&self.0);
            }
        }
        let _cleanup = Cleanup(store.account("identity"));
        std::fs::remove_file(paths.identity_file()).unwrap();
        let loaded = store
            .load_identity()
            .expect("recover the fixture's identity from the native keyring");
        assert_eq!(loaded.device_id, identity.device_id);
        assert!(
            paths.identity_file().is_file(),
            "background clients need the recovered file cache"
        );
        // A subsequent read must work even after the native entry is removed.
        delete_keyring(&store.account("identity")).unwrap();
        assert_eq!(store.load_identity().unwrap().device_id, identity.device_id);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(paths.identity_file())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
