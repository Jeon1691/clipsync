use std::path::PathBuf;
use std::process::{Command, Stdio};

use clipsync_storage::AppPaths;

use crate::{DaemonError, Result};

pub fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| DaemonError::Message(e.to_string()))
}

pub fn is_service_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        launch_agent_path().exists()
    }
    #[cfg(target_os = "linux")]
    {
        systemd_unit_path().map(|p| p.exists()).unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

pub fn install_and_start(paths: &AppPaths) -> Result<()> {
    let exe = current_exe()?;
    #[cfg(target_os = "macos")]
    {
        install_launchd(&exe, paths)?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        install_systemd(&exe, paths)?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        spawn_detached(&exe, paths)
    }
}

pub fn stop_and_uninstall() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let path = launch_agent_path();
        if path.exists() {
            let plist = path.display().to_string();
            launchctl_quiet(&["bootout", &launch_label()]);
            launchctl_quiet(&["unload", "-w", &plist]);
            let _ = std::fs::remove_file(path);
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "clipsync.service"])
            .status();
        if let Ok(path) = systemd_unit_path() {
            let _ = std::fs::remove_file(path);
        }
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(())
    }
}

pub fn restart_service(paths: &AppPaths) -> Result<()> {
    stop_and_uninstall()?;
    install_and_start(paths)
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> PathBuf {
    dirs_home().join("Library/LaunchAgents/dev.clipsync.daemon.plist")
}

#[cfg(target_os = "macos")]
fn install_launchd(exe: &PathBuf, paths: &AppPaths) -> Result<()> {
    let plist_path = launch_agent_path();
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = paths.log_file();
    let home = std::env::var("CLIPSYNC_HOME").ok();
    let mut env_entries = String::new();
    if let Some(h) = home {
        env_entries.push_str(&format!(
            "  <key>CLIPSYNC_HOME</key>\n  <string>{}</string>\n",
            xml_escape(&h)
        ));
    }
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.clipsync.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>daemon</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
  <key>EnvironmentVariables</key>
  <dict>
{env_entries}  </dict>
</dict>
</plist>
"#,
        xml_escape(&exe.display().to_string()),
        xml_escape(&log.display().to_string()),
        xml_escape(&log.display().to_string()),
    );
    std::fs::write(&plist_path, plist)?;
    let label = launch_label();
    let domain = gui_domain();
    let plist_file = plist_path.display().to_string();
    launchctl_quiet(&["bootout", &label]);
    launchctl_quiet(&["unload", "-w", &plist_file]);
    let loaded = launchctl_quiet(&["bootstrap", &domain, &plist_file])
        || launchctl_quiet(&["load", "-w", &plist_file]);
    if loaded {
        launchctl_quiet(&["kickstart", "-k", &label]);
    } else {
        spawn_detached(exe, paths)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn gui_domain() -> String {
    format!("gui/{}", user_id())
}

#[cfg(target_os = "macos")]
fn launch_label() -> String {
    format!("{}/dev.clipsync.daemon", gui_domain())
}

#[cfg(target_os = "macos")]
fn user_id() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Run launchctl with stdio discarded. Missing/unloaded agents are not errors.
#[cfg(target_os = "macos")]
fn launchctl_quiet(args: &[&str]) -> bool {
    Command::new("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf> {
    let dir = dirs_home().join(".config/systemd/user");
    Ok(dir.join("clipsync.service"))
}

#[cfg(target_os = "linux")]
fn install_systemd(exe: &PathBuf, paths: &AppPaths) -> Result<()> {
    let unit_path = systemd_unit_path()?;
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let home = std::env::var("CLIPSYNC_HOME").unwrap_or_default();
    let unit = format!(
        "[Unit]\nDescription=ClipSync clipboard daemon\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} daemon run\nRestart=on-failure\nRestartSec=3\nEnvironment=CLIPSYNC_HOME={}\n\n[Install]\nWantedBy=default.target\n",
        exe.display(),
        home
    );
    std::fs::write(&unit_path, unit)?;
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let status = Command::new("systemctl")
        .args(["--user", "enable", "--now", "clipsync.service"])
        .status()?;
    if !status.success() {
        spawn_detached(exe, paths)?;
    }
    Ok(())
}

fn spawn_detached(exe: &PathBuf, _paths: &AppPaths) -> Result<()> {
    let mut cmd = Command::new(exe);
    cmd.args(["daemon", "run"]);
    if let Ok(home) = std::env::var("CLIPSYNC_HOME") {
        cmd.env("CLIPSYNC_HOME", home);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(unix)]
fn libc_setsid() {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        let _ = setsid();
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
