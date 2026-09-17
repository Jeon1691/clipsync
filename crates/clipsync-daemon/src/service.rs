use std::path::{Path, PathBuf};
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
    let exe = stable_daemon_exe(&current_exe()?);
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
            let label = launch_label();
            launchctl_quiet(&["disable", &label]);
            launchctl_quiet(&["bootout", &label]);
            launchctl_quiet(&["unload", &plist]);
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
    let home = dirs_home();
    let mut env_entries = format!(
        "    <key>HOME</key>\n    <string>{}</string>\n    <key>PATH</key>\n    <string>{}</string>\n",
        xml_escape(&home.display().to_string()),
        xml_escape(&daemon_path_env(exe)),
    );
    if let Ok(h) = std::env::var("CLIPSYNC_HOME") {
        env_entries.push_str(&format!(
            "    <key>CLIPSYNC_HOME</key>\n    <string>{}</string>\n",
            xml_escape(&h)
        ));
    }
    if let Ok(ca) = std::env::var("CLIPSYNC_CA_FILE") {
        env_entries.push_str(&format!(
            "    <key>CLIPSYNC_CA_FILE</key>\n    <string>{}</string>\n",
            xml_escape(&ca)
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
  <dict>
    <key>Crashed</key>
    <true/>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>ProcessType</key>
  <string>Background</string>
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
    // Do not `unload -w`: that disables the agent so it will not start at next login.
    launchctl_quiet(&["bootout", &label]);
    launchctl_quiet(&["enable", &label]);
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
    let home = dirs_home();
    let mut env = format!(
        "Environment=HOME={}\nEnvironment=PATH={}\n",
        home.display(),
        daemon_path_env(exe)
    );
    if let Ok(h) = std::env::var("CLIPSYNC_HOME") {
        env.push_str(&format!("Environment=CLIPSYNC_HOME={h}\n"));
    }
    if let Ok(ca) = std::env::var("CLIPSYNC_CA_FILE") {
        env.push_str(&format!("Environment=CLIPSYNC_CA_FILE={ca}\n"));
    }
    let unit = format!(
        "[Unit]\nDescription=ClipSync clipboard daemon\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={} daemon run\nRestart=always\nRestartSec=3\n{env}\n[Install]\nWantedBy=default.target\n",
        exe.display(),
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

/// Prefer the Homebrew prefix shim over a versioned Cellar path so upgrades
/// and reboots keep working.
fn stable_daemon_exe(current: &Path) -> PathBuf {
    let s = current.to_string_lossy();
    if let Some(i) = s.find("/Cellar/clipsync/") {
        let linked = Path::new(&s[..i]).join("bin").join("clipsync");
        if linked.exists() {
            return linked;
        }
    }
    current.to_path_buf()
}

fn daemon_path_env(exe: &Path) -> String {
    let mut parts = Vec::new();
    if let Some(dir) = exe.parent() {
        parts.push(dir.display().to_string());
    }
    parts.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    );
    parts.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn stable_exe_rewrites_cellar_when_prefix_bin_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let cellar = prefix.join("Cellar/clipsync/0.1.8/bin");
        fs::create_dir_all(&cellar).unwrap();
        let versioned = cellar.join("clipsync");
        fs::write(&versioned, b"").unwrap();
        let linked_dir = prefix.join("bin");
        fs::create_dir_all(&linked_dir).unwrap();
        let linked = linked_dir.join("clipsync");
        fs::write(&linked, b"").unwrap();
        assert_eq!(stable_daemon_exe(&versioned), linked);
    }

    #[test]
    fn stable_exe_keeps_non_cellar_path() {
        let p = PathBuf::from("/usr/local/bin/clipsync");
        assert_eq!(stable_daemon_exe(&p), p);
    }
}
