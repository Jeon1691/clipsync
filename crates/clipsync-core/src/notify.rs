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

    #[allow(dead_code)]
    pub fn error(&self, message: &str) {
        if !self.cfg.enabled {
            return;
        }
        send("ClipSync", message);
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
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("notify-send").args([title, body]).status();
    }
}

fn escape_as(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
