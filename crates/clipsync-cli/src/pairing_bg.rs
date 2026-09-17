use std::fs::OpenOptions;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clipsync_core::{App, CoreError};
use clipsync_storage::PairingWait;

use crate::output::emit;

pub async fn spawn_background_create(
    app: &App,
    ttl: Option<&str>,
    no_auto_sync: bool,
    json: bool,
) -> Result<(), CoreError> {
    #[cfg(not(unix))]
    {
        let _ = (app, ttl, no_auto_sync, json);
        return Err(CoreError::Message(
            "background pairing is only supported on macOS and Linux".into(),
        ));
    }

    #[cfg(unix)]
    {
        if let Ok(Some(existing)) = app.store.load_pairing_wait() {
            if existing.status == "waiting" && pid_alive(existing.pid) {
                return Err(CoreError::Message(format!(
                    "pairing already waiting in background (pid {}); join with: clipsync room join {}",
                    existing.pid, existing.pairing_code
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
        if let Ok(home) = std::env::var("CLIPSYNC_HOME") {
            cmd.env("CLIPSYNC_HOME", home);
        }
        if let Ok(relay) = std::env::var("CLIPSYNC_RELAY_URL") {
            cmd.env("CLIPSYNC_RELAY_URL", relay);
        }
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));

        let mut child = cmd.spawn()?;
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
fn pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
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
