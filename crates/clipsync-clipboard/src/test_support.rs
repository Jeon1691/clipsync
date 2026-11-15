// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::path::PathBuf;

pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "clipsync-clipboard-test-{}-{id}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create fixture: {e}"),
            }
        }
    }

    pub(crate) fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    pub(crate) fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[cfg(unix)]
    pub(crate) fn executable(&self, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.file(name, body.as_bytes());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    pub(crate) fn child(&self, exact_test: &str) -> std::process::Command {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", exact_test, "--nocapture"]);
        child
            .env_clear()
            .env("HOME", self.dir("home"))
            .env("PATH", &self.0)
            .env("CLIPSYNC_HOME", self.dir("app"))
            .env("CLIPSYNC_CONFIG_DIR", self.dir("app/config"))
            .env("CLIPSYNC_DATA_DIR", self.dir("app/data"))
            .env("TMPDIR", self.dir("tmp"));
        // Instrumented child processes must write into the same profile set.
        for name in ["LLVM_PROFILE_FILE", "ASAN_OPTIONS", "DYLD_LIBRARY_PATH"] {
            if let Some(value) = std::env::var_os(name) {
                child.env(name, value);
            }
        }
        child
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn assert_text(item: Option<crate::ClipboardItem>, expected: &str) {
    assert_eq!(
        item,
        Some(crate::ClipboardItem::Text {
            text: expected.into()
        })
    );
}
