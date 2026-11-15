// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::process::Command;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use clipsync_protocol::NOTIFY_COOLDOWN_SECS;
use clipsync_storage::NotifyConfig;

pub struct Notifier {
    last: Mutex<Option<Instant>>,
    cfg: NotifyConfig,
}

impl Notifier {
    pub fn new(cfg: NotifyConfig) -> Self {
        Self {
            last: Mutex::new(None),
            cfg,
        }
    }

    pub fn received(&self, kind: &str, from: &str, summary: &str) {
        if !self.cfg.enabled {
            return;
        }
        {
            let mut last = self.last.lock();
            if let Some(t) = *last {
                if t.elapsed() < Duration::from_secs(NOTIFY_COOLDOWN_SECS) {
                    return;
                }
            }
            *last = Some(Instant::now());
        }
        let body = if self.cfg.preview {
            format!("{kind} from {from}: {summary}")
        } else {
            format!("received {kind} from {from}")
        };
        send("ClipSync", &body);
    }
}

fn send(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            escape_as(body),
            escape_as(title)
        );
        let _ = Command::new("osascript").args(["-e", &script]).status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("notify-send").args([title, body]).status();
    }
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $n = New-Object System.Windows.Forms.NotifyIcon; $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; $n.ShowBalloonTip(3000, '{title}', '{body}', [System.Windows.Forms.ToolTipIcon]::Info)",
            title = title.replace('\'', "''"),
            body = body.replace('\'', "''"),
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .status();
    }
}

#[cfg(target_os = "macos")]
fn escape_as(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(all(test, unix))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod behavior_tests {
    use super::*;

    #[test]
    fn notifications_observe_privacy_escaping_and_cooldown_without_using_the_desktop() {
        use std::os::unix::fs::PermissionsExt;
        const NAME: &str = "notify::behavior_tests::notifications_observe_privacy_escaping_and_cooldown_without_using_the_desktop";
        if std::env::var("CLIPSYNC_NOTIFICATION_WORKER").as_deref() != Ok(NAME) {
            let directory = tempfile::TempDir::new().unwrap();
            let log = directory.path().join("notifications");
            for program in ["osascript", "notify-send"] {
                let path = directory.path().join(program);
                std::fs::write(&path, "#!/bin/sh\nprintf '%s\\0' \"$@\" >> \"$CLIPSYNC_NOTIFICATION_LOG\"\nprintf '\\377' >> \"$CLIPSYNC_NOTIFICATION_LOG\"\n").unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env("CLIPSYNC_NOTIFICATION_WORKER", NAME)
                .env("CLIPSYNC_NOTIFICATION_LOG", &log)
                .env("PATH", directory.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let path = std::env::var_os("CLIPSYNC_NOTIFICATION_LOG").unwrap();
        let disabled = Notifier::new(NotifyConfig {
            enabled: false,
            preview: true,
        });
        disabled.received("text", "peer", "PRIVATE");
        assert!(!std::path::Path::new(&path).exists());
        let preview = Notifier::new(NotifyConfig {
            enabled: true,
            preview: true,
        });
        preview.received("text", "peer\"\\", "PRIVATE\"\\");
        let first = std::fs::read(&path).unwrap();
        preview.received("image", "another", "must be throttled");
        assert_eq!(std::fs::read(&path).unwrap(), first);
        *preview.last.lock() =
            Instant::now().checked_sub(Duration::from_secs(NOTIFY_COOLDOWN_SECS + 1));
        preview.received("text", "peer", "after cooldown");
        let private = Notifier::new(NotifyConfig {
            enabled: true,
            preview: false,
        });
        private.received("image", "remote", "SECRET SHOULD NOT APPEAR");
        let data = std::fs::read(&path).unwrap();
        let calls: Vec<_> = data
            .split(|byte| *byte == 0xff)
            .filter(|call| !call.is_empty())
            .collect();
        assert_eq!(calls.len(), 3);
        let arguments: Vec<_> = calls[0]
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .collect();
        #[cfg(target_os = "macos")]
        assert_eq!(arguments, [b"-e".as_slice(), b"display notification \"text from peer\\\"\\\\: PRIVATE\\\"\\\\\" with title \"ClipSync\"".as_slice()]);
        #[cfg(target_os = "linux")]
        assert_eq!(
            arguments,
            [
                b"ClipSync".as_slice(),
                b"text from peer\"\\: PRIVATE\"\\".as_slice()
            ]
        );
        let private_call = String::from_utf8_lossy(calls[2]);
        assert!(private_call.contains("received image from remote"));
        assert!(!private_call.contains("SECRET"));
    }
}
