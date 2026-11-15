// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::PathBuf;
use std::process::{Command, Output};

use clipsync_daemon::{install_and_start, is_service_installed, restart_service};
use clipsync_storage::AppPaths;

struct Fixture {
    root: tempfile::TempDir,
    home: PathBuf,
    app_root: PathBuf,
    bin: PathBuf,
    ca_file: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        #[cfg(target_os = "macos")]
        let home = root.path().join("home & \"quoted\"");
        #[cfg(target_os = "linux")]
        let home = root.path().join("home with spaces");
        let app_root = root.path().join("isolated app data");
        let bin = root.path().join("commands");
        let ca_file = root.path().join("corporate root.pem");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        // The real service manager is an external, persistent system service.
        // Only that boundary is replaced: real installer paths/config writes and
        // command spawning remain in use. The fake models its enabled state.
        let script = r#"#!/bin/sh
printf '%s\n' 'CLIPSYNC_SERVICE_STDOUT_MARKER'
printf '%s\n' 'CLIPSYNC_SERVICE_STDERR_MARKER' >&2
printf '%s\n' "$*" >> "$CLIPSYNC_TEST_CALLS"
case "$1" in
  enable)
    [ "$2" = "$CLIPSYNC_TEST_LABEL" ] || exit 79
    printf '%s' enabled > "$CLIPSYNC_TEST_ENABLED"
    ;;
  disable) printf '%s' disabled > "$CLIPSYNC_TEST_ENABLED" ;;
  unload)
    if [ "$2" = '-w' ]; then
      printf '%s' disabled > "$CLIPSYNC_TEST_ENABLED"
    fi
    ;;
esac
exit 0
"#;
        for name in ["launchctl", "systemctl"] {
            let path = bin.join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            root,
            home,
            app_root,
            bin,
            ca_file,
        }
    }

    fn run(&self, action: &str) -> Output {
        let uid = Command::new("/usr/bin/id").arg("-u").output().unwrap();
        assert!(uid.status.success());
        let uid = String::from_utf8(uid.stdout).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "service_install_child", "--nocapture"])
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("CLIPSYNC_HOME", &self.app_root)
            .env("CLIPSYNC_CA_FILE", &self.ca_file)
            .env("CLIPSYNC_TEST_ACTION", action)
            .env(
                "CLIPSYNC_TEST_LABEL",
                format!("gui/{}/dev.clipsync.daemon", uid.trim()),
            )
            .env("CLIPSYNC_TEST_APP_ROOT", &self.app_root)
            .env("CLIPSYNC_TEST_CALLS", self.root.path().join("calls"))
            .env("CLIPSYNC_TEST_ENABLED", self.root.path().join("enabled"))
            .env("DISPLAY", ":93")
            .env("WAYLAND_DISPLAY", "wayland-regression-1")
            .env("XDG_RUNTIME_DIR", "/run/user/4242")
            .env("XDG_SESSION_TYPE", "wayland")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/4242/bus");
        // Keep application configuration isolated while retaining the coverage
        // and sanitizer runtime settings for the instrumented child binary.
        for name in ["LLVM_PROFILE_FILE", "ASAN_OPTIONS", "LSAN_OPTIONS"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "installer child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    #[cfg(target_os = "macos")]
    fn plist_path(&self) -> PathBuf {
        self.home
            .join("Library/LaunchAgents/dev.clipsync.daemon.plist")
    }

    #[cfg(target_os = "macos")]
    fn plist(&self) -> serde_json::Value {
        let out = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "--"])
            .arg(self.plist_path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "invalid generated plist: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    #[cfg(target_os = "linux")]
    fn unit(&self) -> String {
        std::fs::read_to_string(self.home.join(".config/systemd/user/clipsync.service")).unwrap()
    }
}

// The parent launches exactly this test in a fresh process. Normal test runs
// leave it idle, so no test mutates global HOME/PATH or touches real services.
#[test]
fn service_install_child() {
    let Ok(action) = std::env::var("CLIPSYNC_TEST_ACTION") else {
        return;
    };
    let root = std::env::var_os("CLIPSYNC_TEST_APP_ROOT").unwrap();
    let paths = AppPaths::from_root(root.into());
    paths.ensure().unwrap();
    match action.as_str() {
        "install" => install_and_start(&paths).unwrap(),
        "restart" => restart_service(&paths).unwrap(),
        _ => panic!("unknown child action"),
    }
    assert!(
        is_service_installed(),
        "public installation status must reflect the written service"
    );
}

// F15: omitting any of these values breaks background identity/CA/command lookup.
#[cfg(target_os = "macos")]
#[test]
fn launchd_install_preserves_complete_runtime_environment() {
    let f = Fixture::new();
    f.run("install");
    let plist = f.plist();
    let env = &plist["EnvironmentVariables"];
    assert_eq!(env["HOME"].as_str(), f.home.to_str());
    assert_eq!(env["CLIPSYNC_HOME"].as_str(), f.app_root.to_str());
    assert_eq!(env["CLIPSYNC_CA_FILE"].as_str(), f.ca_file.to_str());
    let executable = std::env::current_exe().unwrap();
    let path = env["PATH"].as_str().unwrap();
    assert_eq!(
        path.split(':').next(),
        executable.parent().unwrap().to_str()
    );
    assert!(path.split(':').any(|entry| entry == "/usr/bin"));
    assert!(path.split(':').any(|entry| entry == "/opt/homebrew/bin"));
    assert_eq!(plist["ProgramArguments"][1], "daemon");
    assert_eq!(plist["ProgramArguments"][2], "run");
}

// F12: restoring launchctl's inherited stdio leaks these external-tool messages
// into the caller's captured CLI output, including otherwise successful setup.
#[cfg(target_os = "macos")]
#[test]
fn successful_service_install_does_not_leak_launchctl_output() {
    let f = Fixture::new();
    let out = f.run("install");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("CLIPSYNC_SERVICE_STDOUT_MARKER"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("CLIPSYNC_SERVICE_STDERR_MARKER"));
    assert!(
        !std::fs::read_to_string(f.root.path().join("calls"))
            .unwrap()
            .is_empty(),
        "fixture must actually execute the external tool"
    );
}

// F13: after replacement the persisted service-manager state must be enabled,
// otherwise a currently running daemon will disappear at the next login.
#[cfg(target_os = "macos")]
#[test]
fn replacing_launch_agent_leaves_it_enabled_for_the_next_login() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.plist_path().parent().unwrap()).unwrap();
    std::fs::write(f.plist_path(), "old agent to replace").unwrap();
    std::fs::write(f.root.path().join("enabled"), "disabled").unwrap();
    f.run("restart");
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("enabled")).unwrap(),
        "enabled"
    );
    assert_eq!(f.plist()["RunAtLoad"], true);
}

#[cfg(target_os = "linux")]
fn environment(unit: &str) -> std::collections::BTreeMap<&str, &str> {
    unit.lines()
        .filter_map(|line| {
            line.strip_prefix("Environment=\"")?
                .strip_suffix('"')?
                .split_once('=')
        })
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn systemd_install_preserves_complete_runtime_environment() {
    let f = Fixture::new();
    f.run("install");
    let unit = f.unit();
    let env = environment(&unit);
    assert_eq!(env.get("HOME").copied(), f.home.to_str());
    assert_eq!(env.get("CLIPSYNC_HOME").copied(), f.app_root.to_str());
    assert_eq!(env.get("CLIPSYNC_CA_FILE").copied(), f.ca_file.to_str());
    let executable = std::env::current_exe().unwrap();
    let path = env.get("PATH").unwrap();
    assert_eq!(
        path.split(':').next(),
        executable.parent().unwrap().to_str()
    );
    assert!(path.split(':').any(|entry| entry == "/usr/bin"));
}

// F17: the generated user service must retain the actual graphical-session
// inputs. Testing the quoting helper alone cannot catch a removed pass-through.
#[cfg(target_os = "linux")]
#[test]
fn systemd_install_passes_the_graphical_session_to_the_daemon() {
    let f = Fixture::new();
    f.run("install");
    let unit = f.unit();
    let env = environment(&unit);
    for (key, expected) in [
        ("DISPLAY", ":93"),
        ("WAYLAND_DISPLAY", "wayland-regression-1"),
        ("XDG_RUNTIME_DIR", "/run/user/4242"),
        ("XDG_SESSION_TYPE", "wayland"),
        ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/4242/bus"),
    ] {
        assert_eq!(
            env.get(key).copied(),
            Some(expected),
            "lost session variable {key}"
        );
    }
}

// F16: systemd consumes this generated policy to restart after clean exits too,
// so regressing to on-failure revives the historical stop-after-success bug.
#[cfg(target_os = "linux")]
#[test]
fn systemd_install_restarts_the_daemon_after_successful_exit() {
    let f = Fixture::new();
    f.run("install");
    let unit = f.unit();
    let restart: Vec<_> = unit
        .lines()
        .filter_map(|line| line.strip_prefix("Restart="))
        .collect();
    assert_eq!(restart, ["always"]);
}
