// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! `clipsync room confirm` on a real terminal (a pseudo-terminal): what the
//! user sees next to the verification code, and that the prompt ends when
//! the pairing does.
#![cfg(unix)]

use std::fs::File;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipsync_storage::{AppPaths, LocalStore, PairingWait};
use tempfile::TempDir;

/// The code the simulated background worker asks the user to compare.
const SAS: &str = "482913";
/// Wall-clock budget for the CLI to start, print its prompt, or react to a
/// pairing-file change it polls every tenth of a second.
const REACTION: Duration = Duration::from_secs(10);
/// How long a hung worker stand-in may live; far beyond any test here.
const STAND_IN_SECS: &str = "300";
/// Relay-reported name that, printed raw, clears the line, returns the
/// cursor, moves it up, prints a fake code and reverses what follows.
const HOSTILE_NAME: &str =
    "Laptop\u{1b}[2K\r\u{1b}[1Averification code: 000 000\u{8}\u{9b}2J\u{202e}\u{2066}\u{200f}";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_clipsync")
}

/// A pseudo-terminal: what the CLI writes arrives in `screen`.
struct Terminal {
    device: File,
    screen: Arc<Mutex<Vec<u8>>>,
}

impl Terminal {
    fn open() -> Self {
        let (mut controller_fd, mut device_fd) = (-1, -1);
        // SAFETY: openpty writes two fresh descriptors into the pointers and
        // reads no name, termios or window size (all null).
        let opened = unsafe {
            libc::openpty(
                &mut controller_fd,
                &mut device_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: each descriptor is owned by exactly one `File` from here on.
        let (mut controller, device) = unsafe {
            (
                File::from_raw_fd(controller_fd),
                File::from_raw_fd(device_fd),
            )
        };
        let screen = Arc::new(Mutex::new(Vec::new()));
        let shown = screen.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 1024];
            // Ends with an error once every copy of the device side closed.
            while let Ok(read @ 1..) = controller.read(&mut chunk) {
                shown.lock().unwrap().extend_from_slice(&chunk[..read]);
            }
        });
        Self { device, screen }
    }

    /// Run `clipsync room confirm` with this terminal as stdin, stdout and stderr.
    fn run_confirm(&self, home: &Path) -> Child {
        self.run_confirm_with(home, &[], self.device.try_clone().unwrap().into())
    }

    /// Run `clipsync room confirm` plus `extra` arguments with this terminal
    /// as stdin and stderr, and `stdout` as standard output.
    fn run_confirm_with(&self, home: &Path, extra: &[&str], stdout: Stdio) -> Child {
        Command::new(bin())
            .args(["room", "confirm"])
            .args(extra)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("CLIPSYNC_HOME", home)
            .env("CLIPSYNC_NO_DAEMON", "1")
            .env("RUST_LOG", "error")
            .stdin(self.device.try_clone().unwrap())
            .stdout(stdout)
            .stderr(self.device.try_clone().unwrap())
            .spawn()
            .expect("run clipsync")
    }

    /// Everything shown so far, with the terminal's `\r\n` line endings
    /// turned back into the `\n` the program wrote.
    fn shown(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()).replace("\r\n", "\n")
    }

    fn wait_until_shown(&self, text: &str) -> String {
        let deadline = Instant::now() + REACTION;
        loop {
            let shown = self.shown();
            if shown.contains(text) {
                return shown;
            }
            assert!(
                Instant::now() < deadline,
                "never showed {text:?}: {shown:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A live process standing in for the background pairing worker.
fn worker_stand_in() -> Child {
    Command::new("sleep")
        .arg(STAND_IN_SECS)
        .spawn()
        .expect("run sleep")
}

fn home_waiting_for_confirmation(worker: u32, peer_device_name: &str) -> (TempDir, LocalStore) {
    let home = tempfile::tempdir().unwrap();
    let store = LocalStore::open_from(AppPaths::from_root(home.path().to_path_buf())).unwrap();
    store
        .save_pairing_wait(&PairingWait {
            status: "confirm_sas".into(),
            pairing_code: "000 000".into(),
            room_id: "room".into(),
            expires_at: "2026-11-17T00:00:00Z".into(),
            pid: worker,
            error: None,
            sas: Some(SAS.into()),
            peer_device_name: Some(peer_device_name.into()),
            sas_decision: None,
            sas_protection: None,
        })
        .unwrap();
    (home, store)
}

fn exit_within(child: &mut Child, budget: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// True for characters a terminal acts on instead of showing.
fn rewrites_terminal(c: char) -> bool {
    (c.is_control() && c != '\n')
        || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[test]
fn hostile_peer_name_is_shown_inert_and_never_shares_the_code_line() {
    let mut worker = worker_stand_in();
    let (home, _store) = home_waiting_for_confirmation(worker.id(), HOSTILE_NAME);
    let terminal = Terminal::open();
    let mut confirm = terminal.run_confirm(home.path());
    let shown = terminal.wait_until_shown("Type the code");
    let _ = confirm.kill();
    let _ = confirm.wait();
    let _ = worker.kill();
    let _ = worker.wait();

    let rewriting: Vec<char> = shown.chars().filter(|&c| rewrites_terminal(c)).collect();
    assert!(
        rewriting.is_empty(),
        "{rewriting:?} reached the terminal: {shown:?}"
    );
    // The escapes stay visible, so the user can see the name is odd.
    assert!(shown.contains("Laptop\\u{1b}[2K\\u{d}"), "{shown:?}");
    let lines: Vec<&str> = shown.lines().collect();
    let code = lines
        .iter()
        .position(|line| line.starts_with("verification code: "))
        .expect("no verification code line");
    // The code line holds the code and nothing else, and it comes after the
    // peer's name, so nothing untrusted is printed after it.
    assert_eq!(lines[code], "verification code: 482 913");
    let name = lines
        .iter()
        .position(|line| line.starts_with("other device: "))
        .expect("no peer name line");
    assert!(name < code, "{shown:?}");
    assert!(
        lines[code + 1..]
            .iter()
            .all(|line| !line.contains("Laptop")),
        "{shown:?}"
    );
}

#[test]
fn prompt_ends_without_input_when_the_pairing_ends() {
    type End = fn(&LocalStore, &mut Child);
    let endings: [(End, &str); 3] = [
        // The worker saw the other device reject, or the pairing expire.
        (
            |store, _| {
                store
                    .update_pairing_wait(|wait| {
                        let failed = wait.map(|wait| PairingWait {
                            status: "failed".into(),
                            error: Some("the other device rejected the verification code".into()),
                            ..wait
                        });
                        (failed, ())
                    })
                    .unwrap()
            },
            "the other device rejected the verification code",
        ),
        // `room leave` removed the request.
        (|store, _| store.clear_pairing_wait(), "cancelled"),
        // The worker stopped without writing an outcome.
        (
            |_, worker| {
                worker.kill().unwrap();
                worker.wait().unwrap();
            },
            "no longer running",
        ),
    ];
    for (end, reason) in endings {
        let mut worker = worker_stand_in();
        let (home, store) = home_waiting_for_confirmation(worker.id(), "Laptop");
        let terminal = Terminal::open();
        let mut confirm = terminal.run_confirm(home.path());
        terminal.wait_until_shown("Type the code");
        end(&store, &mut worker);
        let status = exit_within(&mut confirm, REACTION);
        let _ = worker.kill();
        let _ = worker.wait();
        let shown = terminal.shown();
        let status = status
            .unwrap_or_else(|| panic!("prompt kept waiting for input after {reason:?}: {shown:?}"));
        assert!(!status.success(), "{reason:?}: {shown:?}");
        assert!(shown.contains(reason), "{reason:?}: {shown:?}");
        // Nothing was answered on the user's behalf.
        if let Some(wait) = store.load_pairing_wait().unwrap() {
            assert_eq!(wait.sas_decision, None, "{reason:?}");
        }
    }
}

/// Under reduced protection (the pairing file records no commitment round)
/// a prompt answer counts only from a person who has just read the REDUCED
/// line. With `--json`, or with the text going to a file, nobody at this
/// terminal was shown it, so `room confirm` refuses before prompting, even
/// though stdin is a terminal, and leaves the request open.
#[test]
fn reduced_protection_is_refused_when_the_announcement_missed_the_terminal() {
    let output = tempfile::tempdir().unwrap();
    let redirected = output.path().join("stdout.txt");
    type Stdout = fn(&Terminal, &Path) -> Stdio;
    let cases: [(&str, &[&str], Stdout); 2] = [
        ("--json on a terminal", &["--json"], |terminal, _| {
            terminal.device.try_clone().unwrap().into()
        }),
        ("stdout redirected to a file", &[], |_, file| {
            File::create(file).unwrap().into()
        }),
    ];
    for (case, extra, stdout) in cases {
        let mut worker = worker_stand_in();
        let (home, store) = home_waiting_for_confirmation(worker.id(), "Laptop");
        let terminal = Terminal::open();
        let mut confirm =
            terminal.run_confirm_with(home.path(), extra, stdout(&terminal, &redirected));
        let status = exit_within(&mut confirm, REACTION);
        let _ = worker.kill();
        let _ = worker.wait();
        let shown = terminal.shown();
        let status =
            status.unwrap_or_else(|| panic!("{case}: prompted instead of refusing: {shown:?}"));
        assert!(!status.success(), "{case}: {shown:?}");
        assert!(
            shown.contains("--accept-reduced-protection"),
            "{case}: {shown:?}"
        );
        assert!(shown.contains("still open"), "{case}: {shown:?}");
        assert!(!shown.contains("Type the code"), "{case}: {shown:?}");
        // Nothing was answered, and the request can still be confirmed.
        let wait = store.load_pairing_wait().unwrap().expect("request removed");
        assert_eq!(wait.sas_decision, None, "{case}");
        assert_eq!(wait.status, "confirm_sas", "{case}");
    }
}
