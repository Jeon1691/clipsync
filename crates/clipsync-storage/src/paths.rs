// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub inbox_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl AppPaths {
    pub fn from_env() -> Self {
        if let Ok(root) = std::env::var("CLIPSYNC_HOME") {
            return Self::from_root(PathBuf::from(root));
        }
        let dirs = ProjectDirs::from("dev", "clipsync", "clipsync").expect("home directory");
        let config_dir = std::env::var("CLIPSYNC_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs.config_dir().to_path_buf());
        let data_dir = std::env::var("CLIPSYNC_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs.data_dir().to_path_buf());
        let log_dir = data_dir.join("logs");
        let inbox_dir = data_dir.join("inbox");
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.clone());
        Self {
            config_dir,
            data_dir,
            log_dir,
            inbox_dir,
            runtime_dir,
        }
    }

    pub fn from_root(root: PathBuf) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            log_dir: root.join("logs"),
            inbox_dir: root.join("inbox"),
            runtime_dir: root.join("run"),
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        // The runtime dir may be a shared system location such as
        // `$XDG_RUNTIME_DIR`; only narrow it when this call creates it.
        let runtime_created = !self.runtime_dir.exists();
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.log_dir,
            &self.inbox_dir,
            &self.runtime_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        set_private_dir(&self.data_dir)?;
        set_private_dir(&self.inbox_dir)?;
        // The log dir is always ClipSync's own; it holds daemon and worker output.
        set_private_dir(&self.log_dir)?;
        if runtime_created {
            set_private_dir(&self.runtime_dir)?;
        }
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    pub fn identity_file(&self) -> PathBuf {
        self.data_dir.join("identity.json")
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.data_dir.join("credentials.json")
    }

    pub fn socket_file(&self) -> PathBuf {
        self.runtime_dir.join("clipsync.sock")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.runtime_dir.join("clipsync.pid")
    }

    pub fn log_file(&self) -> PathBuf {
        self.log_dir.join("clipsync.log")
    }

    pub fn pairing_file(&self) -> PathBuf {
        self.runtime_dir.join("pairing.json")
    }
}

/// Owner-only directory mode (`rwx------`) for ClipSync-owned directories.
#[cfg(unix)]
pub(crate) const PRIVATE_DIR_MODE: u32 = 0o700;
/// Owner read/write file mode (`rw-------`) for secrets, `pairing.json` and logs.
#[cfg(unix)]
pub(crate) const PRIVATE_FILE_MODE: u32 = 0o600;
/// Permission bits of `st_mode`, excluding file-type bits (test assertions).
#[cfg(all(test, unix))]
pub(crate) const PERMISSION_BITS: u32 = 0o777;

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE));
    }
    let _ = path;
    Ok(())
}

pub fn set_private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    }
    let _ = path;
    Ok(())
}

/// Opens `path` for appending, creating it owner-only (`0600`) on Unix and
/// narrowing an existing file that an older build left with a wider mode.
///
/// Windows relies on the user's profile-directory ACLs, as for every other
/// private file; the call is a plain create+append there.
pub fn open_private_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(PRIVATE_FILE_MODE);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mode` only applies on creation; fchmod the opened file to cover an
        // existing log without a path-based race.
        file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    }
    Ok(file)
}

#[cfg(all(test, unix))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod private_mode_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A typical umask-derived mode left by older builds.
    const LEGACY_FILE_MODE: u32 = 0o644;
    /// A typical umask-derived directory mode left by older builds.
    const LEGACY_DIR_MODE: u32 = 0o755;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS
    }

    #[test]
    fn private_append_creates_owner_only_and_narrows_an_existing_log() {
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join("fresh.log");
        drop(open_private_append(&fresh).unwrap());
        assert_eq!(mode(&fresh), PRIVATE_FILE_MODE);

        let legacy = root.path().join("legacy.log");
        std::fs::write(&legacy, b"kept\n").unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(LEGACY_FILE_MODE))
            .unwrap();
        let mut file = open_private_append(&legacy).unwrap();
        std::io::Write::write_all(&mut file, b"appended\n").unwrap();
        assert_eq!(mode(&legacy), PRIVATE_FILE_MODE);
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            "kept\nappended\n"
        );
    }

    #[test]
    fn ensure_makes_the_log_dir_and_a_newly_created_runtime_dir_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().join("home"));
        paths.ensure().unwrap();
        assert_eq!(mode(&paths.log_dir), PRIVATE_DIR_MODE);
        assert_eq!(mode(&paths.runtime_dir), PRIVATE_DIR_MODE);

        // An older build left the log dir wide: it is always ClipSync's own.
        std::fs::set_permissions(
            &paths.log_dir,
            std::fs::Permissions::from_mode(LEGACY_DIR_MODE),
        )
        .unwrap();
        // A pre-existing runtime dir may be shared (e.g. $XDG_RUNTIME_DIR): left as is.
        std::fs::set_permissions(
            &paths.runtime_dir,
            std::fs::Permissions::from_mode(LEGACY_DIR_MODE),
        )
        .unwrap();
        paths.ensure().unwrap();
        assert_eq!(mode(&paths.log_dir), PRIVATE_DIR_MODE);
        assert_eq!(mode(&paths.runtime_dir), LEGACY_DIR_MODE);
    }
}
