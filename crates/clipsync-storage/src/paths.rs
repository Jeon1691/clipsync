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
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    /// Private envelope sequence/replay state; see `LocalStore::next_send_seq`.
    pub fn replay_state_file(&self) -> PathBuf {
        self.data_dir.join("replay-state.json")
    }

    /// Advisory lock serializing every read-modify-write of
    /// [`Self::replay_state_file`] across processes. A separate file, because
    /// the state file itself is replaced by rename on each write.
    pub fn replay_state_lock_file(&self) -> PathBuf {
        self.data_dir.join("replay-state.lock")
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

    /// Lock that serializes writers of [`Self::pairing_file`].
    pub fn pairing_lock_file(&self) -> PathBuf {
        self.runtime_dir.join("pairing.lock")
    }
}

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    let _ = path;
    Ok(())
}

/// Owner read/write only, for every file holding keys or replay state.
#[cfg(unix)]
pub(crate) const PRIVATE_FILE_MODE: u32 = 0o600;

pub fn set_private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    }
    let _ = path;
    Ok(())
}
