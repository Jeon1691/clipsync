// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::fs::OpenOptions;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clipsync_core::{App, CoreError};
use clipsync_storage::PairingWait;

use crate::output::emit;

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub async fn spawn_background_create(
    app: &App,
    ttl: Option<&str>,
    no_auto_sync: bool,
    insecure_skip_sas: bool,
    json: bool,
) -> Result<(), CoreError> {
    if let Ok(Some(existing)) = app.store.load_pairing_wait() {
        let active = existing.status == "waiting"
            || existing.status == crate::sas_prompt::STATUS_CONFIRM_SAS;
        if active && !crate::sas_prompt::replaceable(&existing) {
            let next = if existing.status == "waiting" {
                format!("join with: clipsync room join {}", existing.pairing_code)
            } else {
                "a device joined; compare codes with: clipsync room confirm (or clipsync room reject)"
                    .into()
            };
            return Err(CoreError::Message(format!(
                "pairing already waiting in background (pid {}); {next}",
                existing.pid
            )));
        }
    }
    app.store.clear_pairing_wait();

    let exe = std::env::current_exe().map_err(|e| CoreError::Message(e.to_string()))?;
    let log_path = app.store.paths.log_file();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let mut cmd = Command::new(exe);
    cmd.arg("room").arg("create").arg("--background-worker");
    if let Some(ttl) = ttl {
        cmd.arg("--ttl").arg(ttl);
    }
    if no_auto_sync {
        cmd.arg("--no-auto-sync");
    }
    if insecure_skip_sas {
        cmd.arg("--insecure-skip-sas");
    }
    if let Ok(home) = std::env::var("CLIPSYNC_HOME") {
        cmd.env("CLIPSYNC_HOME", home);
    }
    if let Ok(relay) = std::env::var("CLIPSYNC_RELAY_URL") {
        cmd.env("CLIPSYNC_RELAY_URL", relay);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));

    let mut child = clipsync_daemon::spawn_isolated(&mut cmd)?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait()? {
            let from_state = app
                .store
                .load_pairing_wait()
                .ok()
                .flatten()
                .and_then(|w| w.error);
            let log_tail = std::fs::read_to_string(&log_path).unwrap_or_default();
            let detail = from_state
                .or_else(|| extract_error_line(&log_tail))
                .unwrap_or_else(|| format!("background worker exited ({status})"));
            return Err(CoreError::Message(detail));
        }
        if let Ok(Some(wait)) = app.store.load_pairing_wait() {
            if wait.status == "waiting" && !wait.pairing_code.is_empty() && wait.pid == pid {
                print_offer(&wait, pid, json);
                return Ok(());
            }
            if wait.status == "failed" {
                return Err(CoreError::Message(
                    wait.error
                        .unwrap_or_else(|| "background pairing failed".into()),
                ));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(CoreError::Message(
                "timed out waiting for background pairing code".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn print_offer(wait: &PairingWait, pid: u32, json: bool) {
    emit(
        json,
        serde_json::json!({
            "event": "pairing_code",
            "pairing_code": wait.pairing_code,
            "room_id": wait.room_id,
            "expires_at": wait.expires_at,
            "background": true,
            "pid": pid,
        }),
    );
    if !json {
        println!("pairing code: {}", wait.pairing_code);
        println!("room: {}", wait.room_id);
        println!("expires: {}", wait.expires_at);
        println!("waiting in background (pid {pid})");
        println!(
            "join from another device: clipsync room join {}",
            wait.pairing_code
        );
        println!("then compare verification codes here: clipsync room confirm");
    }
}

fn extract_error_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .find(|l| l.starts_with("error:") || l.contains("websocket") || l.contains("relay "))
        .map(|l| l.trim_start_matches("error:").trim().to_string())
}

#[cfg(unix)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            false
        } else {
            CloseHandle(handle);
            true
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
pub fn ignore_sighup() {
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    const SIGHUP: i32 = 1;
    const SIG_IGN: usize = 1;
    unsafe {
        let _ = signal(SIGHUP, SIG_IGN);
    }
}
