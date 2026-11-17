// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! `clipsync room confirm` states this device's protection mode on every
//! path, and accepts a code under reduced protection only when the user
//! acknowledged it: a match counts only if both devices show full
//! protection, so a relay that downgraded the creator must not get a match
//! approved without that warning reaching the user.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clipsync_storage::{AppPaths, LocalStore, PairingWait};
use tempfile::TempDir;

/// The code the simulated background worker asks the user to compare.
const SAS: &str = "482913";
/// Pairing-file names of the two modes (`clipsync_core::protection_name`).
const COMMITTED: &str = "committed";
const UNCOMMITTED: &str = "uncommitted";
/// The fixed local lines the CLI prints for each mode
/// (`clipsync_core::protection_notice`).
const FULL_LINE: &str = "protection: full on this device";
const REDUCED_LINE: &str = "protection: REDUCED";
const ACKNOWLEDGE_FLAG: &str = "--accept-reduced-protection";
/// Wall-clock budget for one `room confirm` run. The worker stand-in reacts
/// within a poll interval, so only a hang reaches it.
const RUN_BUDGET: Duration = Duration::from_secs(30);
/// How often the worker stand-in re-reads the pairing file.
const WORKER_POLL: Duration = Duration::from_millis(20);

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_clipsync")
}

/// A home whose background worker (this test process, which is alive) asks
/// the user to confirm [`SAS`] under `protection`.
fn home_waiting_for_confirmation(protection: &str) -> (TempDir, LocalStore) {
    let home = tempfile::tempdir().unwrap();
    let store = LocalStore::open_from(AppPaths::from_root(home.path().to_path_buf())).unwrap();
    store
        .save_pairing_wait(&PairingWait {
            status: "confirm_sas".into(),
            pairing_code: "000 000".into(),
            room_id: "room".into(),
            expires_at: "2026-11-17T00:00:00Z".into(),
            pid: std::process::id(),
            error: None,
            sas: Some(SAS.into()),
            peer_device_name: Some("Laptop".into()),
            sas_decision: None,
            sas_protection: Some(protection.into()),
        })
        .unwrap();
    (home, store)
}

/// Stands in for the background worker: once a confirmation is recorded it
/// reports the pairing as completed, as the real worker does after commit.
struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn start(store: &LocalStore) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (store, stopped) = (store.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                store
                    .update_pairing_wait(|wait| match wait {
                        Some(wait) if wait.sas_decision.as_deref() == Some("confirmed") => (
                            Some(PairingWait {
                                status: "paired".into(),
                                ..wait
                            }),
                            (),
                        ),
                        _ => (None, ()),
                    })
                    .unwrap();
                std::thread::sleep(WORKER_POLL);
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Run `clipsync ARGS` in `home` with `answer` (if any) piped to stdin.
fn clipsync(home: &Path, args: &[&str], answer: Option<&str>) -> Output {
    let mut child = Command::new(bin())
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CLIPSYNC_HOME", home)
        .env("CLIPSYNC_NO_DAEMON", "1")
        .env("RUST_LOG", "error")
        .stdin(if answer.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run clipsync");
    if let Some(answer) = answer {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(answer.as_bytes()).unwrap();
    }
    let deadline = Instant::now() + RUN_BUDGET;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("clipsync {args:?} did not finish");
        }
        std::thread::sleep(WORKER_POLL);
    }
    child.wait_with_output().unwrap()
}

fn shown(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn decision(store: &LocalStore) -> Option<String> {
    store.load_pairing_wait().unwrap().unwrap().sas_decision
}

#[test]
fn positional_confirm_under_reduced_protection_is_refused_without_acknowledgement() {
    for json in [false, true] {
        let (home, store) = home_waiting_for_confirmation(UNCOMMITTED);
        let _worker = Worker::start(&store);
        let mut args = vec!["room", "confirm", SAS];
        if json {
            args.insert(0, "--json");
        }
        let output = clipsync(home.path(), &args, None);
        let shown = shown(&output);
        assert!(!output.status.success(), "{shown}");
        if json {
            assert!(shown.contains(r#""protection":"uncommitted""#), "{shown}");
        } else {
            assert!(shown.contains(REDUCED_LINE), "{shown}");
        }
        assert!(shown.contains(ACKNOWLEDGE_FLAG), "{shown}");
        // Nothing was recorded: the request can still be confirmed with
        // the acknowledgement, or rejected.
        assert_eq!(decision(&store), None, "{shown}");
    }
}

#[test]
fn positional_confirm_under_reduced_protection_pairs_once_acknowledged() {
    let (home, store) = home_waiting_for_confirmation(UNCOMMITTED);
    let _worker = Worker::start(&store);
    let output = clipsync(
        home.path(),
        &["room", "confirm", ACKNOWLEDGE_FLAG, SAS],
        None,
    );
    let shown = shown(&output);
    assert!(output.status.success(), "{shown}");
    assert!(shown.contains(REDUCED_LINE), "{shown}");
    assert!(shown.contains("paired"), "{shown}");
}

#[test]
fn positional_confirm_under_full_protection_states_it_and_pairs() {
    for json in [false, true] {
        let (home, store) = home_waiting_for_confirmation(COMMITTED);
        let _worker = Worker::start(&store);
        let mut args = vec!["room", "confirm", SAS];
        if json {
            args.insert(0, "--json");
        }
        let output = clipsync(home.path(), &args, None);
        let shown = shown(&output);
        assert!(output.status.success(), "{shown}");
        if json {
            assert!(shown.contains(r#""protection":"committed""#), "{shown}");
        } else {
            assert!(shown.contains(FULL_LINE), "{shown}");
            assert!(!shown.contains(REDUCED_LINE), "{shown}");
        }
        // The positional path compares the argument; it never shows the code.
        assert!(!shown.contains("482 913"), "{shown}");
    }
}

#[test]
fn piped_prompt_answer_under_reduced_protection_needs_acknowledgement() {
    let (home, store) = home_waiting_for_confirmation(UNCOMMITTED);
    let _worker = Worker::start(&store);
    let answer = format!("{SAS}\n");
    let refused = clipsync(home.path(), &["room", "confirm"], Some(&answer));
    let shown_refused = shown(&refused);
    assert!(!refused.status.success(), "{shown_refused}");
    assert!(shown_refused.contains(REDUCED_LINE), "{shown_refused}");
    assert!(shown_refused.contains(ACKNOWLEDGE_FLAG), "{shown_refused}");
    assert_eq!(decision(&store), None, "{shown_refused}");

    let accepted = clipsync(
        home.path(),
        &["room", "confirm", ACKNOWLEDGE_FLAG],
        Some(&answer),
    );
    let shown_accepted = shown(&accepted);
    assert!(accepted.status.success(), "{shown_accepted}");
    assert!(shown_accepted.contains(REDUCED_LINE), "{shown_accepted}");
}

#[test]
fn piped_prompt_answer_under_full_protection_pairs_without_acknowledgement() {
    let (home, store) = home_waiting_for_confirmation(COMMITTED);
    let _worker = Worker::start(&store);
    let output = clipsync(home.path(), &["room", "confirm"], Some(&format!("{SAS}\n")));
    let shown = shown(&output);
    assert!(output.status.success(), "{shown}");
    assert!(shown.contains(FULL_LINE), "{shown}");
}
