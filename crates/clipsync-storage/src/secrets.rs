// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_crypto::{DeviceIdentity, StoredIdentity};
use std::io::Write;
use std::path::Path;
use tracing::warn;

use crate::paths::{set_private_file, AppPaths};
use crate::{Result, StorageError};

/// Keychain service the product mirrors secrets under.
const SERVICE: &str = "dev.clipsync.cli";

/// Keychain service for test runs that deliberately reach the native keychain,
/// kept apart from [`SERVICE`] so that their entries can be found and removed.
const TEST_KEYCHAIN_SERVICE: &str = "dev.clipsync.test";

/// Whether this platform has an OS keychain to mirror secrets into.
const PLATFORM_HAS_KEYCHAIN: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// Whether this is a test build: this crate's own unit tests, or a build with
/// the `test-isolated-keychain` feature, which only `[dev-dependencies]` enable.
const TEST_BUILD: bool = cfg!(any(test, feature = "test-isolated-keychain"));

/// Set to [`TEST_NATIVE_KEYCHAIN_OPT_IN`] in a test build to mirror under
/// [`TEST_KEYCHAIN_SERVICE`], for the harness that measures native keychain
/// latency. Product builds do not contain this switch.
#[cfg(any(test, feature = "test-isolated-keychain"))]
const TEST_NATIVE_KEYCHAIN_ENV: &str = "CLIPSYNC_TEST_NATIVE_KEYCHAIN";

/// The only [`TEST_NATIVE_KEYCHAIN_ENV`] value that opts in.
#[cfg(any(test, feature = "test-isolated-keychain"))]
const TEST_NATIVE_KEYCHAIN_OPT_IN: &str = "1";

// Release artifacts are optimized builds; refuse one that could skip the
// keychain on a platform that has one.
#[cfg(all(
    feature = "test-isolated-keychain",
    not(debug_assertions),
    any(target_os = "macos", target_os = "windows")
))]
compile_error!("test-isolated-keychain is for test builds; enable it only from [dev-dependencies]");

#[derive(Clone)]
pub struct SecretStore {
    paths: AppPaths,
    /// Service secrets are mirrored under in the OS keychain, or `None` to
    /// keep them in their private files only.
    keychain: Option<&'static str>,
}

impl SecretStore {
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            keychain: keychain_service(),
        }
    }

    fn set_keyring(&self, account: &str, value: &str) -> std::result::Result<(), String> {
        let service = self.keychain.ok_or(NO_KEYCHAIN)?;
        native_set(service, account, value)
    }

    fn get_keyring(&self, account: &str) -> std::result::Result<String, String> {
        let service = self.keychain.ok_or(NO_KEYCHAIN)?;
        native_get(service, account)
    }

    fn delete_keyring(&self, account: &str) -> std::result::Result<(), String> {
        self.keychain
            .map_or(Ok(()), |service| native_delete(service, account))
    }

    fn account(&self, name: &str) -> String {
        format!("{}:{name}", self.paths.data_dir.display())
    }

    pub fn save_identity(&self, identity: &DeviceIdentity) -> Result<()> {
        let stored = identity.to_stored();
        let payload = serde_json::to_string(&stored)?;
        let path = self.paths.identity_file();
        write_private(&path, payload.as_bytes())?;
        if let Err(e) = self.set_keyring(&self.account("identity"), &payload) {
            warn!(error = %e, "keychain unavailable; identity kept in 0600 file");
        }
        Ok(())
    }

    pub fn load_identity(&self) -> Result<DeviceIdentity> {
        self.load_identity_with(|account| self.get_keyring(account))
    }

    /// `keychain` is the native-keychain reader; tests inject a stub so the
    /// keychain-to-file restore path is exercised on every platform.
    fn load_identity_with(
        &self,
        keychain: impl FnOnce(&str) -> std::result::Result<String, String>,
    ) -> Result<DeviceIdentity> {
        let path = self.paths.identity_file();
        if let Some(payload) = crate::sharing::read_if_present(&path)? {
            let stored: StoredIdentity = serde_json::from_str(&payload)?;
            return DeviceIdentity::from_stored(&stored).ok_or(StorageError::NotInitialized);
        }
        if let Ok(payload) = keychain(&self.account("identity")) {
            let stored: StoredIdentity = serde_json::from_str(&payload)?;
            let identity =
                DeviceIdentity::from_stored(&stored).ok_or(StorageError::NotInitialized)?;
            // Restore the canonical file privately from creation; never write
            // through a pre-existing link or expose a partially written secret.
            if let Err(e) = write_private(&path, payload.as_bytes()) {
                warn!(error = %e, "could not cache identity file from keychain");
            }
            return Ok(identity);
        }
        Err(StorageError::NotInitialized)
    }

    pub fn save_named(&self, name: &str, value: &str) -> Result<()> {
        let path = self.paths.data_dir.join(format!("{name}.secret"));
        write_private(&path, value.as_bytes())?;
        if let Err(e) = self.set_keyring(&self.account(name), value) {
            warn!(error = %e, key = name, "keychain unavailable");
        }
        Ok(())
    }

    pub fn load_named(&self, name: &str) -> Result<String> {
        let path = self.paths.data_dir.join(format!("{name}.secret"));
        if let Some(value) = crate::sharing::read_if_present(&path)? {
            return Ok(value);
        }
        self.get_keyring(&self.account(name))
            .map_err(|_| StorageError::NotPaired)
    }

    pub fn delete_named(&self, name: &str) -> Result<()> {
        let _ = self.delete_keyring(&self.account(name));
        let path = self.paths.data_dir.join(format!("{name}.secret"));
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}

/// Create with private permissions before writing, then atomically replace.
/// A failed write leaves the previous credential available for retry/recovery.
pub(crate) fn write_private(path: &Path, value: &[u8]) -> std::io::Result<()> {
    write_private_pausing(path, value, crate::sharing::pause_before_retry)
}

/// [`write_private`] with the pause between replace attempts supplied, so
/// tests can release the file at exactly that point.
///
/// The replace is `MoveFileExW` on Windows, which refuses to replace a file
/// another handle has open, such as a status read in another process; it is
/// retried (see [`crate::sharing`]). The temporary file is handed back on
/// each failure and removed if the replace never succeeds.
fn write_private_pausing(path: &Path, value: &[u8], pause: impl FnMut()) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    set_private_file(tmp.path())?;
    tmp.write_all(value)?;
    tmp.as_file().sync_all()?;
    let mut pending = Some(tmp);
    crate::sharing::retry_access_denied(
        || {
            let tmp = pending
                .take()
                .expect("a failed replace hands the file back");
            tmp.persist(path).map(drop).map_err(|error| {
                pending = Some(error.file);
                error.error
            })
        },
        crate::sharing::ACCESS_DENIED_RETRIES,
        pause,
    )?;
    // Make credential renames durable before removing a legacy metadata copy.
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Why a store without a keychain service reports the mirror unavailable.
const NO_KEYCHAIN: &str = if PLATFORM_HAS_KEYCHAIN {
    "os keychain not used by test builds"
} else {
    "os keychain not enabled on this platform"
};

/// The keychain service a new store mirrors under.
fn keychain_service() -> Option<&'static str> {
    select_service(
        PLATFORM_HAS_KEYCHAIN,
        TEST_BUILD,
        test_native_keychain_requested(),
    )
}

/// Choose the keychain service from the build and the opt-in.
///
/// Test builds never reach the product service: every secret stays in its
/// private file, as on Linux, so the outcome cannot depend on keychain
/// contents, `securityd` latency or which test ran first. This is a pure
/// function compiled into every build, so its product branch is tested even
/// though no test build takes it.
const fn select_service(
    platform_has_keychain: bool,
    test_build: bool,
    native_opt_in: bool,
) -> Option<&'static str> {
    match (platform_has_keychain, test_build, native_opt_in) {
        (false, _, _) => None,
        (true, false, _) => Some(SERVICE),
        (true, true, true) => Some(TEST_KEYCHAIN_SERVICE),
        (true, true, false) => None,
    }
}

#[cfg(any(test, feature = "test-isolated-keychain"))]
fn test_native_keychain_requested() -> bool {
    std::env::var_os(TEST_NATIVE_KEYCHAIN_ENV)
        .is_some_and(|value| value == TEST_NATIVE_KEYCHAIN_OPT_IN)
}

#[cfg(not(any(test, feature = "test-isolated-keychain")))]
fn test_native_keychain_requested() -> bool {
    false
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn native_set(service: &str, user: &str, value: &str) -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(service, user).map_err(|e| e.to_string())?;
    entry.set_password(value).map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn native_set(_service: &str, _user: &str, _value: &str) -> std::result::Result<(), String> {
    Err("os keychain not enabled on this platform".into())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn native_get(service: &str, user: &str) -> std::result::Result<String, String> {
    let entry = keyring::Entry::new(service, user).map_err(|e| e.to_string())?;
    entry.get_password().map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn native_get(_service: &str, _user: &str) -> std::result::Result<String, String> {
    Err("os keychain not enabled on this platform".into())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn native_delete(service: &str, user: &str) -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(service, user).map_err(|e| e.to_string())?;
    entry.delete_credential().map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn native_delete(_service: &str, _user: &str) -> std::result::Result<(), String> {
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod file_failure_tests {
    use super::*;

    /// Files in `dir` other than `keep`: temporary files a write left behind.
    fn strays(dir: &Path, keep: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path != keep)
            .collect()
    }

    #[test]
    fn a_private_file_open_in_another_reader_is_replaced_once_the_reader_closes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path, b"old").unwrap();
        // A status read in another process holds the file open.
        let mut reader = Some(std::fs::File::open(&path).unwrap());
        let mut pauses = 0;
        write_private_pausing(&path, b"new", || {
            pauses += 1;
            drop(reader.take());
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        // Windows refuses the first replace while the reader is open; other
        // platforms replace at once.
        assert_eq!(pauses, u32::from(cfg!(windows)));
        assert!(strays(root.path(), &path).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn a_private_file_that_stays_open_keeps_its_contents_after_bounded_retries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path, b"old").unwrap();
        let reader = std::fs::File::open(&path).unwrap();
        let mut pauses = 0;
        let error = write_private_pausing(&path, b"new", || pauses += 1).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(pauses, crate::sharing::ACCESS_DENIED_RETRIES);
        drop(reader);
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(strays(root.path(), &path).is_empty());
    }

    #[test]
    fn malformed_file_identity_is_reported_without_native_keyring_fallback() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().into());
        paths.ensure().unwrap();
        let secrets = SecretStore::new(paths.clone());
        std::fs::create_dir(paths.identity_file()).unwrap();
        assert!(matches!(secrets.load_identity(), Err(StorageError::Io(_))));
        std::fs::remove_dir(paths.identity_file()).unwrap();
        std::fs::write(paths.identity_file(), b"malformed JSON").unwrap();
        assert!(matches!(
            secrets.load_identity(),
            Err(StorageError::Serde(_))
        ));
        let mut invalid = DeviceIdentity::from_bytes("fixture".into(), [1; 32]).to_stored();
        invalid.secret = "00".into();
        std::fs::write(paths.identity_file(), serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(matches!(
            secrets.load_identity(),
            Err(StorageError::NotInitialized)
        ));
    }

    #[test]
    fn failed_private_file_persistence_preserves_the_target_and_reports_missing_parent() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().into());
        paths.ensure().unwrap();
        let secrets = SecretStore::new(paths.clone());
        std::fs::create_dir(paths.identity_file()).unwrap();
        assert!(secrets
            .save_identity(&DeviceIdentity::from_bytes("fixture".into(), [1; 32]))
            .is_err());
        let secret = paths.data_dir.join("blocked.secret");
        std::fs::create_dir(&secret).unwrap();
        assert!(secrets.save_named("blocked", "must not publish").is_err());
        assert!(secrets.load_named("blocked").is_err());
        assert!(secret.is_dir());
        assert!(write_private(Path::new("/"), b"invalid destination").is_err());
    }
}

#[cfg(all(test, unix))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod identity_cache_tests {
    use super::*;
    use crate::paths::{PERMISSION_BITS, PRIVATE_FILE_MODE};
    use std::os::unix::fs::PermissionsExt;

    fn keychain_payload(identity: &DeviceIdentity) -> String {
        serde_json::to_string(&identity.to_stored()).unwrap()
    }

    #[test]
    fn keychain_restore_creates_a_private_regular_file_without_following_a_planted_link() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().into());
        paths.ensure().unwrap();
        let store = SecretStore::new(paths.clone());
        let identity = DeviceIdentity::from_bytes("fixture".into(), [7; 32]);
        let payload = keychain_payload(&identity);
        // A dangling link reads as "no identity file", so the keychain branch runs.
        let outside = root.path().join("outside-identity.json");
        std::os::unix::fs::symlink(&outside, paths.identity_file()).unwrap();

        let expected_account = store.account("identity");
        let loaded = store
            .load_identity_with(|account| {
                assert_eq!(account, expected_account);
                Ok(payload.clone())
            })
            .unwrap();

        assert_eq!(loaded.device_id, identity.device_id);
        assert!(
            !outside.exists(),
            "the restored secret must not be written through a link outside the private dir"
        );
        let metadata = std::fs::symlink_metadata(paths.identity_file()).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(
            metadata.permissions().mode() & PERMISSION_BITS,
            PRIVATE_FILE_MODE
        );
        assert_eq!(
            std::fs::read_to_string(paths.identity_file()).unwrap(),
            payload
        );
        // The restored cache now serves file-based readers without the keychain.
        let cached = store
            .load_identity_with(|_| Err("keychain must not be consulted".into()))
            .unwrap();
        assert_eq!(cached.device_id, identity.device_id);
    }

    #[test]
    fn missing_file_and_keychain_entry_reports_not_initialized() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().into());
        paths.ensure().unwrap();
        let store = SecretStore::new(paths);
        assert!(matches!(
            store.load_identity_with(|_| Err("no entry".into())),
            Err(StorageError::NotInitialized)
        ));
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod keychain_isolation_tests {
    use super::*;

    #[test]
    fn only_product_builds_on_keychain_platforms_mirror_under_the_product_service() {
        for opt_in in [false, true] {
            assert_eq!(select_service(false, false, opt_in), None);
            assert_eq!(select_service(false, true, opt_in), None);
            assert_eq!(select_service(true, false, opt_in), Some(SERVICE));
        }
        assert_eq!(select_service(true, true, false), None);
        assert_eq!(
            select_service(true, true, true),
            Some(TEST_KEYCHAIN_SERVICE)
        );
        assert_eq!(
            SERVICE, "dev.clipsync.cli",
            "existing entries use this name"
        );
        assert_ne!(TEST_KEYCHAIN_SERVICE, SERVICE);
        const { assert!(TEST_BUILD) };
    }

    #[test]
    fn test_builds_keep_secrets_out_of_the_os_keychain_unless_opted_in() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().into());
        paths.ensure().unwrap();
        let store = SecretStore::new(paths.clone());
        let opted_in = PLATFORM_HAS_KEYCHAIN && test_native_keychain_requested();
        store.save_named("keychain-probe", "synthetic").unwrap();
        let account = store.account("keychain-probe");
        // Read the native keychain directly, so a write that a read-side
        // gate would hide is still caught.
        let product_entry = native_get(SERVICE, &account).is_ok();
        let test_entry = native_get(TEST_KEYCHAIN_SERVICE, &account).is_ok();
        std::fs::remove_file(paths.data_dir.join("keychain-probe.secret")).unwrap();
        let restored = store.load_named("keychain-probe").is_ok();
        let _ = store.delete_keyring(&account);
        assert!(!product_entry, "a test build wrote the product service");
        assert_eq!(test_entry, opted_in, "only an opted-in run writes");
        assert_eq!(restored, opted_in, "only an opted-in run restores");
    }
}

/// The test that always uses the native keychain (an opted-in run also
/// reaches it above). It writes only under [`TEST_KEYCHAIN_SERVICE`], to an
/// account named after its own temporary directory, and removes the entry
/// even when an assertion fails.
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod regression_tests {
    use super::*;

    #[test]
    fn keyring_only_identity_is_cached_for_file_based_background_clients() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().to_path_buf());
        paths.ensure().unwrap();
        let store = SecretStore {
            keychain: Some(TEST_KEYCHAIN_SERVICE),
            ..SecretStore::new(paths.clone())
        };
        let identity = DeviceIdentity::generate();
        store.save_identity(&identity).unwrap();
        struct Cleanup<'a>(&'a SecretStore, String);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.delete_keyring(&self.1);
            }
        }
        let _cleanup = Cleanup(&store, store.account("identity"));
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
        store.delete_keyring(&store.account("identity")).unwrap();
        assert_eq!(store.load_identity().unwrap().device_id, identity.device_id);
        #[cfg(unix)]
        {
            use crate::paths::{PERMISSION_BITS, PRIVATE_FILE_MODE};
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(paths.identity_file())
                    .unwrap()
                    .permissions()
                    .mode()
                    & PERMISSION_BITS,
                PRIVATE_FILE_MODE
            );
        }
    }
}
