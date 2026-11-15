// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_storage::AppPaths;

#[test]
fn path_overrides_are_isolated_and_home_override_has_precedence() {
    const CHILD: &str = "CLIPSYNC_PATH_TEST_CHILD";
    if let Some(mode) = std::env::var_os(CHILD) {
        let root = std::path::PathBuf::from(std::env::var_os("CLIPSYNC_PATH_TEST_ROOT").unwrap());
        let paths = AppPaths::from_env();
        if mode == "home" {
            assert_eq!(paths.config_dir, root.join("config"));
            assert_eq!(paths.data_dir, root.join("data"));
            assert_eq!(paths.runtime_dir, root.join("run"));
        } else {
            assert_eq!(paths.config_dir, root.join("custom-config"));
            assert_eq!(paths.data_dir, root.join("custom-data"));
            assert_eq!(paths.runtime_dir, root.join("custom-runtime"));
            assert_eq!(paths.log_dir, root.join("custom-data/logs"));
        }
        paths.ensure().unwrap();
        assert!(paths.log_dir.is_dir());
        assert!(paths.inbox_dir.is_dir());
        assert_eq!(paths.pairing_file().file_name().unwrap(), "pairing.json");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for mode in ["home", "overrides"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "path_overrides_are_isolated_and_home_override_has_precedence",
                "--nocapture",
            ])
            .env(CHILD, mode)
            .env("CLIPSYNC_PATH_TEST_ROOT", root.path())
            .env("CLIPSYNC_CONFIG_DIR", root.path().join("custom-config"))
            .env("CLIPSYNC_DATA_DIR", root.path().join("custom-data"))
            .env("XDG_RUNTIME_DIR", root.path().join("custom-runtime"));
        if mode == "home" {
            command.env("CLIPSYNC_HOME", root.path());
        } else {
            command.env_remove("CLIPSYNC_HOME");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn directories_and_private_files_use_restrictive_permissions() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_root(root.path().to_path_buf());
    paths.ensure().unwrap();
    let store = clipsync_storage::LocalStore::open_from(paths.clone()).unwrap();
    store
        .secrets
        .save_named("permissions", "synthetic")
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in [&paths.data_dir, &paths.inbox_dir] {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            std::fs::metadata(paths.data_dir.join("permissions.secret"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    store.secrets.delete_named("permissions").unwrap();
    let blocked = root.path().join("regular-file");
    std::fs::write(&blocked, "not a directory").unwrap();
    assert!(AppPaths::from_root(blocked).ensure().is_err());
}
