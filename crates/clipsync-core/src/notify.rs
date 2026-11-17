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
    let _ = macos_command(title, body).status();
    #[cfg(target_os = "linux")]
    let _ = linux_command(title, body).status();
    #[cfg(target_os = "windows")]
    let _ = windows_command(title, body).status();
}

// Notification text can carry peer-controlled data (device names, text
// previews), so it must never become part of interpreted script source. Each
// platform runs a constant script and hands the text over out of band: as argv
// to an AppleScript run handler, as positional arguments after `--`, or as
// environment variables read by PowerShell.

/// AppleScript statements, one per `osascript -e`. The run handler receives the
/// arguments after `--` as `argv`: item 1 is the body, item 2 the title.
#[cfg(any(target_os = "macos", test))]
const MACOS_SCRIPT: [&str; 3] = [
    "on run argv",
    "display notification (item 1 of argv) with title (item 2 of argv)",
    "end run",
];

/// Environment variables that carry the notification text to PowerShell.
#[cfg(any(target_os = "windows", test))]
const WINDOWS_TITLE_ENV: &str = "CLIPSYNC_NOTIFY_TITLE";
#[cfg(any(target_os = "windows", test))]
const WINDOWS_BODY_ENV: &str = "CLIPSYNC_NOTIFY_BODY";

/// Constant PowerShell source that reads [`WINDOWS_TITLE_ENV`] and
/// [`WINDOWS_BODY_ENV`]. `3000` is the balloon timeout in milliseconds, kept
/// from the previous implementation (Windows may clamp it to accessibility
/// settings).
#[cfg(any(target_os = "windows", test))]
const WINDOWS_SCRIPT: &str = "Add-Type -AssemblyName System.Windows.Forms; $n = New-Object System.Windows.Forms.NotifyIcon; $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; $n.ShowBalloonTip(3000, $env:CLIPSYNC_NOTIFY_TITLE, $env:CLIPSYNC_NOTIFY_BODY, [System.Windows.Forms.ToolTipIcon]::Info)";

#[cfg(any(target_os = "macos", test))]
fn macos_command(title: &str, body: &str) -> Command {
    let mut command = Command::new("osascript");
    for statement in MACOS_SCRIPT {
        command.args(["-e", statement]);
    }
    // `--` stops osascript option parsing so a body such as `-e ...` stays data.
    command.args(["--", body, title]);
    command
}

#[cfg(any(target_os = "linux", test))]
fn linux_command(title: &str, body: &str) -> Command {
    let mut command = Command::new("notify-send");
    // `--` stops option parsing so text starting with `-` is not an option.
    command.args(["--", title, body]);
    command
}

#[cfg(any(target_os = "windows", test))]
fn windows_command(title: &str, body: &str) -> Command {
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-Command",
            WINDOWS_SCRIPT,
        ])
        .env(WINDOWS_TITLE_ENV, title)
        .env(WINDOWS_BODY_ENV, body);
    command
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod command_tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    /// Strings a hostile peer could place in a notification (device name, text
    /// preview, file name). Each carries a marker so "does not appear in the
    /// script" cannot pass by coincidence of a short substring.
    const HOSTILE: [&str; 9] = [
        "HOSTILE-dq\" & (do shell script \"id\") & \"",
        "HOSTILE-sq'; Start-Process calc; '",
        "HOSTILE-backtick `id` `$(id)`",
        "HOSTILE-subst $(id) ${env:PATH} $env:CLIPSYNC_NOTIFY_BODY",
        "HOSTILE-newline\nend run\non run argv\ndo shell script \"id\"",
        "-HOSTILE-leading-dash",
        "--HOSTILE-long-option=--urgency=critical",
        "HOSTILE-backslash \\\" \\\\ \\",
        "HOSTILE-applescript \u{ab}event sysoexec\u{bb} -- \"id\"",
    ];

    fn args(command: &Command) -> Vec<&OsStr> {
        command.get_args().collect()
    }

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn pairs() -> impl Iterator<Item = (&'static str, &'static str)> {
        HOSTILE
            .iter()
            .flat_map(|title| HOSTILE.iter().map(move |body| (*title, *body)))
            .chain(HOSTILE.iter().map(|body| ("ClipSync", *body)))
    }

    #[test]
    fn macos_notification_passes_text_as_argv_to_a_constant_applescript() {
        const SCRIPT: [&str; 3] = [
            "on run argv",
            "display notification (item 1 of argv) with title (item 2 of argv)",
            "end run",
        ];
        for (title, body) in pairs() {
            let command = macos_command(title, body);
            assert_eq!(command.get_program(), "osascript");
            let actual = args(&command);
            assert_eq!(
                actual,
                os(&["-e", SCRIPT[0], "-e", SCRIPT[1], "-e", SCRIPT[2], "--", body, title]),
                "title={title:?} body={body:?}"
            );
            for statement in actual.chunks(2).take(SCRIPT.len()) {
                let source = statement[1].to_str().unwrap();
                assert!(
                    !source.contains("HOSTILE"),
                    "user text in script: {source:?}"
                );
            }
            assert_eq!(command.get_envs().count(), 0);
        }
    }

    #[test]
    fn linux_notification_ends_option_parsing_before_user_text() {
        for (title, body) in pairs() {
            let command = linux_command(title, body);
            assert_eq!(command.get_program(), "notify-send");
            assert_eq!(
                args(&command),
                os(&["--", title, body]),
                "title={title:?} body={body:?}"
            );
            assert_eq!(command.get_envs().count(), 0);
        }
    }

    #[test]
    fn windows_notification_reads_text_from_environment_in_a_constant_script() {
        const SCRIPT: &str = "Add-Type -AssemblyName System.Windows.Forms; $n = New-Object System.Windows.Forms.NotifyIcon; $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; $n.ShowBalloonTip(3000, $env:CLIPSYNC_NOTIFY_TITLE, $env:CLIPSYNC_NOTIFY_BODY, [System.Windows.Forms.ToolTipIcon]::Info)";
        for (title, body) in pairs() {
            let command = windows_command(title, body);
            assert_eq!(command.get_program(), "powershell");
            assert_eq!(
                args(&command),
                os(&["-NoProfile", "-WindowStyle", "Hidden", "-Command", SCRIPT]),
                "title={title:?} body={body:?}"
            );
            let mut envs: Vec<_> = command.get_envs().collect();
            envs.sort();
            assert_eq!(
                envs,
                [
                    (OsStr::new("CLIPSYNC_NOTIFY_BODY"), Some(OsStr::new(body))),
                    (OsStr::new("CLIPSYNC_NOTIFY_TITLE"), Some(OsStr::new(title))),
                ],
                "title={title:?} body={body:?}"
            );
        }
    }
}

#[cfg(all(test, unix))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod behavior_tests {
    use super::*;

    #[test]
    fn notifications_observe_privacy_argv_passing_and_cooldown_without_using_the_desktop() {
        use std::os::unix::fs::PermissionsExt;
        const NAME: &str = "notify::behavior_tests::notifications_observe_privacy_argv_passing_and_cooldown_without_using_the_desktop";
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
        preview.received("text", "-peer\"\\", "-e PRIVATE\"\\ $(id) `id`");
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
        assert_eq!(
            arguments,
            [
                b"-e".as_slice(),
                b"on run argv",
                b"-e",
                b"display notification (item 1 of argv) with title (item 2 of argv)",
                b"-e",
                b"end run",
                b"--",
                b"text from -peer\"\\: -e PRIVATE\"\\ $(id) `id`",
                b"ClipSync",
            ]
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            arguments,
            [
                b"--".as_slice(),
                b"ClipSync",
                b"text from -peer\"\\: -e PRIVATE\"\\ $(id) `id`",
            ]
        );
        let private_call = String::from_utf8_lossy(calls[2]);
        assert!(private_call.contains("received image from remote"));
        assert!(!private_call.contains("SECRET"));
    }
}
