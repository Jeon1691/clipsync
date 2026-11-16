// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Short authentication string (SAS) confirmation for `room create` and
//! `room join`.
//!
//! Pairing is committed only after the local user confirms that the other
//! device shows the same verification code. A foreground command asks on
//! stdin. The background create worker publishes the code in the pairing
//! file and waits for `clipsync room confirm` or `clipsync room reject`.

use std::future::Future;
use std::io::{self, BufRead, IsTerminal, Write};
use std::pin::Pin;
use std::time::{Duration, Instant};

use clipsync_core::{display_sas, normalize_sas, App, CoreError, SasCheck, SasDecision};
use clipsync_protocol::PAIRING_TTL_SECS;
use clipsync_storage::{LocalStore, PairingWait};

use crate::output::emit;
use crate::pairing_bg::pid_alive;

/// Pairing-file status while the background worker waits for the SAS answer.
pub const STATUS_CONFIRM_SAS: &str = "confirm_sas";
const STATUS_WAITING: &str = "waiting";
const STATUS_PAIRED: &str = "paired";
const STATUS_FAILED: &str = "failed";
const DECISION_CONFIRMED: &str = "confirmed";
const DECISION_REJECTED: &str = "rejected";

/// How often the worker and `room confirm` re-read the pairing file: fast
/// enough to feel immediate, slow enough to keep both processes idle.
const PAIRING_FILE_POLL: Duration = Duration::from_millis(100);
/// Upper bound for `room confirm` to wait for a joiner and then for the
/// worker to finish. The worker gives up after the relay pairing TTL, so twice
/// that covers a joiner arriving late plus the commit.
const CONFIRM_COMMAND_DEADLINE: Duration = Duration::from_secs(PAIRING_TTL_SECS * 2);

pub type SasConfirm =
    Box<dyn FnOnce(SasCheck) -> Pin<Box<dyn Future<Output = SasDecision> + Send>> + Send>;

#[derive(Clone, Copy, Debug)]
pub struct SasMode {
    pub json: bool,
    pub non_interactive: bool,
    /// `--insecure-skip-sas`: accept without comparing.
    pub skip: bool,
}

/// Confirm on this terminal: show the SAS and read one answer line from
/// stdin. A non-terminal stdin lets scripts compare codes programmatically.
pub fn terminal_confirmer(mode: SasMode) -> SasConfirm {
    Box::new(move |check: SasCheck| {
        Box::pin(async move {
            if mode.skip {
                return skipped(mode, &check.sas, &check.peer_device_name);
            }
            announce(mode, &check.sas, &check.peer_device_name);
            if mode.non_interactive {
                eprintln!(
                    "error: the verification code must be confirmed; rerun without --non-interactive, or pass --insecure-skip-sas"
                );
                return SasDecision::Rejected;
            }
            prompt_line(mode);
            let sas = check.sas.clone();
            let answer = tokio::task::spawn_blocking(read_answer_line)
                .await
                .ok()
                .flatten();
            decide(&sas, answer.as_deref())
        })
    })
}

/// Background worker: publish the SAS in the pairing file and wait for
/// `clipsync room confirm` or `clipsync room reject` to answer there.
pub fn background_confirmer(store: LocalStore, skip: bool) -> SasConfirm {
    Box::new(move |check: SasCheck| {
        Box::pin(async move {
            if skip {
                eprintln!("warning: background pairing accepted without comparing verification codes (--insecure-skip-sas)");
                return SasDecision::Confirmed;
            }
            let worker = std::process::id();
            // Only answer through a pairing file this worker owns; a newer
            // `room create` may have replaced it.
            let owned = |wait: &PairingWait| wait.pid == worker;
            let Ok(Some(mut wait)) = store.load_pairing_wait() else {
                return SasDecision::Rejected;
            };
            if !owned(&wait) || rejected(&wait) {
                // A rejection recorded before the joiner arrived still applies.
                return SasDecision::Rejected;
            }
            wait.status = STATUS_CONFIRM_SAS.into();
            wait.sas = Some(check.sas.clone());
            wait.peer_device_name = Some(check.peer_device_name.clone());
            // A confirmation can only come from a user who saw this code.
            wait.sas_decision = None;
            // Re-read right before publishing so a rejection written since
            // the first read is not overwritten.
            match store.load_pairing_wait() {
                Ok(Some(latest)) if owned(&latest) && !rejected(&latest) => {}
                _ => return SasDecision::Rejected,
            }
            if store.save_pairing_wait(&wait).is_err() {
                return SasDecision::Rejected;
            }
            loop {
                tokio::time::sleep(PAIRING_FILE_POLL).await;
                match store.load_pairing_wait() {
                    Ok(Some(current)) if owned(&current) => match current.sas_decision.as_deref() {
                        Some(DECISION_REJECTED) => return SasDecision::Rejected,
                        Some(DECISION_CONFIRMED)
                            if current.sas.as_deref() == Some(check.sas.as_str()) =>
                        {
                            return SasDecision::Confirmed
                        }
                        _ => {}
                    },
                    // `room leave` removed the file, a newer `room create`
                    // replaced it, or it became unreadable.
                    _ => return SasDecision::Rejected,
                }
            }
        })
    })
}

fn rejected(wait: &PairingWait) -> bool {
    wait.sas_decision.as_deref() == Some(DECISION_REJECTED)
}

/// True when a background pairing file no longer blocks a new `room create`:
/// its worker exited, or the user already rejected it.
pub fn replaceable(wait: &PairingWait) -> bool {
    !pid_alive(wait.pid) || rejected(wait)
}

/// `clipsync room confirm [CODE]` and `clipsync room reject`.
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub async fn answer_background(
    app: &App,
    shown_on_other_device: Option<&str>,
    reject: bool,
    mode: SasMode,
) -> Result<(), CoreError> {
    let deadline = Instant::now() + CONFIRM_COMMAND_DEADLINE;
    let mut announced_wait = false;
    let wait = loop {
        let wait = load_live(app)?;
        match wait.status.as_str() {
            STATUS_CONFIRM_SAS if wait.sas.is_some() => break wait,
            // No joiner will ever be shown by a worker that already exited.
            STATUS_WAITING if !pid_alive(wait.pid) => return Err(not_answerable(&wait)),
            STATUS_WAITING if reject => break wait,
            STATUS_WAITING => {
                if !announced_wait && !mode.json {
                    println!("waiting for the other device to join…");
                    let _ = io::stdout().flush();
                    announced_wait = true;
                }
            }
            _ => return Err(not_answerable(&wait)),
        }
        if Instant::now() >= deadline {
            return Err(CoreError::Timeout);
        }
        tokio::time::sleep(PAIRING_FILE_POLL).await;
    };

    if reject {
        return reject_background(app, wait, deadline, mode).await;
    }
    let sas = wait.sas.clone().unwrap_or_default();
    let peer = wait.peer_device_name.clone().unwrap_or_default();
    let decision = if let Some(entered) = shown_on_other_device {
        decide(&sas, Some(entered))
    } else {
        announce(mode, &sas, &peer);
        if mode.non_interactive {
            return Err(CoreError::Message(
                "pass the code shown on the other device: clipsync room confirm <code>".into(),
            ));
        }
        prompt_line(mode);
        let answer = tokio::task::spawn_blocking(read_answer_line)
            .await
            .ok()
            .flatten();
        decide(&sas, answer.as_deref())
    };

    // The prompt may have taken a while: answer only the same live request.
    let mut answered = load_live(app)?;
    if answered.status != STATUS_CONFIRM_SAS
        || answered.pid != wait.pid
        || answered.sas.as_deref() != Some(sas.as_str())
    {
        return Err(not_answerable(&answered));
    }
    if rejected(&answered) {
        return Err(CoreError::Pairing(
            "pairing was already rejected; nothing was stored".into(),
        ));
    }
    answered.sas_decision = Some(
        match decision {
            SasDecision::Confirmed => DECISION_CONFIRMED,
            SasDecision::Rejected => DECISION_REJECTED,
        }
        .into(),
    );
    app.store.save_pairing_wait(&answered)?;
    if decision == SasDecision::Rejected {
        wait_for_worker_exit(app, &answered, deadline).await?;
        return Err(CoreError::Pairing(
            "verification code does not match the other device; pairing aborted and nothing was stored"
                .into(),
        ));
    }

    loop {
        let Some(done) = app.store.load_pairing_wait()? else {
            return Err(CoreError::Message("pairing was cancelled".into()));
        };
        match done.status.as_str() {
            STATUS_PAIRED => {
                emit(
                    mode.json,
                    serde_json::json!({"ok": true, "room_id": done.room_id}),
                );
                if !mode.json {
                    println!("paired");
                }
                return Ok(());
            }
            STATUS_FAILED => {
                return Err(CoreError::Message(
                    done.error
                        .unwrap_or_else(|| "background pairing failed".into()),
                ));
            }
            _ if !pid_alive(done.pid) => {
                return Err(CoreError::Message(
                    "background pairing stopped before completing".into(),
                ));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(CoreError::Timeout);
        }
        tokio::time::sleep(PAIRING_FILE_POLL).await;
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn reject_background(
    app: &App,
    wait: PairingWait,
    deadline: Instant,
    mode: SasMode,
) -> Result<(), CoreError> {
    if wait.sas_decision.as_deref() == Some(DECISION_CONFIRMED) {
        return Err(CoreError::Message(
            "the verification code was already confirmed and pairing is completing".into(),
        ));
    }
    let mut answered = wait;
    answered.sas_decision = Some(DECISION_REJECTED.into());
    app.store.save_pairing_wait(&answered)?;
    if answered.status == STATUS_WAITING {
        // No joiner yet: the worker rejects as soon as one arrives, and a new
        // `room create` may replace this pairing right away.
        emit(
            mode.json,
            serde_json::json!({"ok": true, "rejected": true, "joined": false}),
        );
        if !mode.json {
            println!(
                "pairing rejected; it will stop when a device joins, and nothing will be stored"
            );
        }
        return Ok(());
    }
    wait_for_worker_exit(app, &answered, deadline).await?;
    emit(mode.json, serde_json::json!({"ok": true, "rejected": true}));
    if !mode.json {
        println!("pairing rejected; nothing was stored");
    }
    Ok(())
}

/// Wait until the worker that owns `answered` gave up, re-asserting the
/// rejection if a concurrent write by the worker dropped it.
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn wait_for_worker_exit(
    app: &App,
    answered: &PairingWait,
    deadline: Instant,
) -> Result<(), CoreError> {
    loop {
        match app.store.load_pairing_wait()? {
            Some(current) if current.pid == answered.pid => match current.status.as_str() {
                STATUS_FAILED => return Ok(()),
                STATUS_PAIRED => {
                    return Err(CoreError::Message(
                        "pairing completed before the rejection was recorded".into(),
                    ))
                }
                _ if !pid_alive(current.pid) => return Ok(()),
                _ if !rejected(&current) => {
                    let mut again = current;
                    again.sas_decision = Some(DECISION_REJECTED.into());
                    app.store.save_pairing_wait(&again)?;
                }
                _ => {}
            },
            // Removed or replaced: that worker can no longer commit through it.
            _ => return Ok(()),
        }
        if Instant::now() >= deadline {
            return Err(CoreError::Timeout);
        }
        tokio::time::sleep(PAIRING_FILE_POLL).await;
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn load_live(app: &App) -> Result<PairingWait, CoreError> {
    app.store.load_pairing_wait()?.ok_or_else(|| {
        CoreError::Message("no pairing in progress; run `clipsync room create` first".into())
    })
}

/// Explain why a pairing file can no longer be answered.
fn not_answerable(wait: &PairingWait) -> CoreError {
    match wait.status.as_str() {
        STATUS_PAIRED => CoreError::Message("pairing already completed".into()),
        STATUS_FAILED => CoreError::Message(
            wait.error
                .clone()
                .unwrap_or_else(|| "background pairing failed".into()),
        ),
        _ if !pid_alive(wait.pid) => CoreError::Message(
            "background pairing is no longer running; run `clipsync room create` again".into(),
        ),
        _ => CoreError::Message(
            "the pairing request changed; run `clipsync room confirm` again".into(),
        ),
    }
}

fn announce(mode: SasMode, sas: &str, peer_device_name: &str) {
    emit(
        mode.json,
        serde_json::json!({
            "event": "sas_confirmation",
            "sas": sas,
            "peer_device_name": peer_device_name,
        }),
    );
    if !mode.json {
        println!("verification code: {}", display_sas(sas));
        println!("other device: {}", display_name(peer_device_name));
        println!("Check that the other device shows the same verification code.");
    }
    let _ = io::stdout().flush();
}

fn prompt_line(mode: SasMode) {
    if !mode.json {
        print!("Type the code shown on the other device, or \"yes\" if it matches: ");
        let _ = io::stdout().flush();
    }
}

fn skipped(mode: SasMode, sas: &str, peer_device_name: &str) -> SasDecision {
    eprintln!(
        "warning: verification code not compared (--insecure-skip-sas); a malicious relay or a guessed code would go unnoticed"
    );
    emit(
        mode.json,
        serde_json::json!({
            "event": "sas_skipped",
            "sas": sas,
            "peer_device_name": peer_device_name,
        }),
    );
    SasDecision::Confirmed
}

fn display_name(peer_device_name: &str) -> &str {
    if peer_device_name.is_empty() {
        "(unnamed device)"
    } else {
        peer_device_name
    }
}

fn read_answer_line() -> Option<String> {
    let stdin = io::stdin();
    let mut line = String::new();
    if stdin.is_terminal() {
        let _ = io::stdout().flush();
    }
    match stdin.lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// Accept "y"/"yes", or the code shown on the other device. Anything else,
/// including end of input, rejects.
pub fn decide(sas: &str, answer: Option<&str>) -> SasDecision {
    let Some(answer) = answer else {
        return SasDecision::Rejected;
    };
    let trimmed = answer.trim();
    let entered = normalize_sas(trimmed);
    let affirmed = trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes");
    if affirmed || (!entered.is_empty() && entered == sas) {
        SasDecision::Confirmed
    } else {
        SasDecision::Rejected
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::future::Future;

    const SAS: &str = "482913";
    const OTHER_SAS: &str = "482914";
    /// Wall-clock budget for a simulated worker to react to a pairing-file
    /// change; the poll interval is a tenth of a second.
    const REACTION: Duration = Duration::from_secs(5);
    const TEXT: SasMode = SasMode {
        json: false,
        non_interactive: true,
        skip: false,
    };
    const JSON: SasMode = SasMode {
        json: true,
        non_interactive: true,
        skip: false,
    };

    fn fixture() -> (tempfile::TempDir, App) {
        let root = tempfile::tempdir().unwrap();
        let app = App::open_from(clipsync_storage::AppPaths::from_root(
            root.path().to_path_buf(),
        ))
        .unwrap();
        (root, app)
    }

    fn check() -> SasCheck {
        SasCheck {
            sas: SAS.into(),
            peer_device_id: "joiner".into(),
            peer_device_name: "Laptop".into(),
        }
    }

    fn live() -> u32 {
        std::process::id()
    }

    /// A worker that is no longer running. A real exited child is not
    /// reliable here: Windows keeps its PID openable while any handle to it
    /// remains and reuses PIDs quickly. No process can have this one: it is
    /// above Linux's maximum `pid_max` (2^22) and macOS's 99 999, and far
    /// beyond the PIDs Windows assigns in practice.
    fn exited() -> u32 {
        const NO_SUCH_PID: u32 = i32::MAX as u32 - 1;
        assert!(!pid_alive(NO_SUCH_PID));
        NO_SUCH_PID
    }

    fn request(status: &str, pid: u32) -> PairingWait {
        PairingWait {
            status: status.into(),
            pairing_code: "123 456".into(),
            room_id: "room".into(),
            expires_at: "2026-11-17T00:00:00Z".into(),
            pid,
            error: None,
            sas: (status == STATUS_CONFIRM_SAS).then(|| SAS.into()),
            peer_device_name: (status == STATUS_CONFIRM_SAS).then(|| "Laptop".into()),
            sas_decision: None,
        }
    }

    fn decided(mut wait: PairingWait, decision: &str) -> PairingWait {
        wait.sas_decision = Some(decision.into());
        wait
    }

    /// Simulate the worker: once `ready` holds for the pairing file, apply
    /// `react` to it.
    fn worker(
        store: &LocalStore,
        ready: impl Fn(&PairingWait) -> bool + Send + 'static,
        react: impl FnOnce(&LocalStore, PairingWait) + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let store = store.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(Some(wait)) = store.load_pairing_wait() {
                    if ready(&wait) {
                        react(&store, wait);
                        return;
                    }
                }
                tokio::time::sleep(PAIRING_FILE_POLL).await;
            }
        })
    }

    async fn within<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(REACTION, future).await.unwrap()
    }

    async fn confirm_error(app: &App, code: Option<&str>) -> String {
        within(answer_background(app, code, false, TEXT))
            .await
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn answers_accept_yes_or_the_matching_code_and_reject_everything_else() {
        for accepted in ["y", "YES", " yes \n", "482913", "482 913\n", "482-913"] {
            assert_eq!(
                decide("482913", Some(accepted)),
                SasDecision::Confirmed,
                "{accepted:?}"
            );
        }
        for rejected in ["", "\n", "n", "no", "482914", "48291", "yess", "1"] {
            assert_eq!(
                decide("482913", Some(rejected)),
                SasDecision::Rejected,
                "{rejected:?}"
            );
        }
        assert_eq!(decide("482913", None), SasDecision::Rejected);
    }

    #[tokio::test]
    async fn terminal_confirmer_fails_closed_without_a_terminal_answer_unless_skipped() {
        for mode in [TEXT, JSON] {
            assert_eq!(
                terminal_confirmer(mode)(check()).await,
                SasDecision::Rejected
            );
            let skip = SasMode { skip: true, ..mode };
            assert_eq!(
                terminal_confirmer(skip)(check()).await,
                SasDecision::Confirmed
            );
        }
        assert_eq!(display_name(""), "(unnamed device)");
        assert_eq!(display_name("Laptop"), "Laptop");
    }

    #[tokio::test]
    async fn background_confirmer_rejects_requests_it_does_not_own() {
        let (_root, app) = fixture();
        let store = app.store.clone();
        let ask = || background_confirmer(store.clone(), false)(check());
        // No pairing file: `room leave` or a cleanup removed it.
        assert_eq!(ask().await, SasDecision::Rejected);
        // A newer `room create` replaced it.
        store
            .save_pairing_wait(&request(STATUS_WAITING, exited()))
            .unwrap();
        assert_eq!(ask().await, SasDecision::Rejected);
        // `room reject` ran before the joiner arrived.
        store
            .save_pairing_wait(&decided(request(STATUS_WAITING, live()), DECISION_REJECTED))
            .unwrap();
        assert_eq!(ask().await, SasDecision::Rejected);
        assert_eq!(
            background_confirmer(store.clone(), true)(check()).await,
            SasDecision::Confirmed
        );
    }

    #[tokio::test]
    async fn background_confirmer_publishes_the_code_and_accepts_only_its_confirmation() {
        let (_root, app) = fixture();
        let store = app.store.clone();
        store
            .save_pairing_wait(&decided(
                request(STATUS_WAITING, live()),
                DECISION_CONFIRMED,
            ))
            .unwrap();
        let answer = tokio::spawn(background_confirmer(store.clone(), false)(check()));
        let published = within(async {
            loop {
                if let Ok(Some(wait)) = store.load_pairing_wait() {
                    if wait.status == STATUS_CONFIRM_SAS {
                        break wait;
                    }
                }
                tokio::time::sleep(PAIRING_FILE_POLL).await;
            }
        })
        .await;
        assert_eq!(published.sas.as_deref(), Some(SAS));
        assert_eq!(published.peer_device_name.as_deref(), Some("Laptop"));
        // Only a user who saw this code can confirm it.
        assert_eq!(published.sas_decision, None);
        let mut stale = decided(published.clone(), DECISION_CONFIRMED);
        stale.sas = Some(OTHER_SAS.into());
        store.save_pairing_wait(&stale).unwrap();
        tokio::time::sleep(PAIRING_FILE_POLL * 3).await;
        assert!(
            !answer.is_finished(),
            "a confirmation of another code was accepted"
        );
        store
            .save_pairing_wait(&decided(published, DECISION_CONFIRMED))
            .unwrap();
        assert_eq!(within(answer).await.unwrap(), SasDecision::Confirmed);
    }

    #[tokio::test]
    async fn background_confirmer_stops_on_a_rejection_or_a_removed_request() {
        for remove in [false, true] {
            let (_root, app) = fixture();
            let store = app.store.clone();
            store
                .save_pairing_wait(&request(STATUS_WAITING, live()))
                .unwrap();
            let answer = tokio::spawn(background_confirmer(store.clone(), false)(check()));
            worker(
                &store,
                |wait| wait.status == STATUS_CONFIRM_SAS,
                move |store, wait| {
                    if remove {
                        store.clear_pairing_wait();
                    } else {
                        store
                            .save_pairing_wait(&decided(wait, DECISION_REJECTED))
                            .unwrap();
                    }
                },
            );
            assert_eq!(within(answer).await.unwrap(), SasDecision::Rejected);
        }
    }

    #[tokio::test]
    async fn room_confirm_explains_requests_it_cannot_answer() {
        let (_root, app) = fixture();
        assert!(confirm_error(&app, Some(SAS))
            .await
            .contains("no pairing in progress"));
        let cases = [
            (request(STATUS_PAIRED, live()), "pairing already completed"),
            (request(STATUS_FAILED, live()), "background pairing failed"),
            (
                PairingWait {
                    error: Some("relay closed during pairing".into()),
                    ..request(STATUS_FAILED, live())
                },
                "relay closed during pairing",
            ),
            (request(STATUS_WAITING, exited()), "no longer running"),
            (request("unknown", exited()), "no longer running"),
            (request("unknown", live()), "the pairing request changed"),
            (
                PairingWait {
                    sas: None,
                    ..request(STATUS_CONFIRM_SAS, live())
                },
                "the pairing request changed",
            ),
            (
                decided(request(STATUS_CONFIRM_SAS, live()), DECISION_REJECTED),
                "already rejected",
            ),
        ];
        for (wait, expected) in cases {
            app.store.save_pairing_wait(&wait).unwrap();
            let error = confirm_error(&app, Some(SAS)).await;
            assert!(error.contains(expected), "{expected:?}: {error}");
        }
        app.store
            .save_pairing_wait(&request(STATUS_CONFIRM_SAS, live()))
            .unwrap();
        let error = confirm_error(&app, None).await;
        assert!(error.contains("pass the code shown"), "{error}");
        assert_eq!(
            app.store.load_pairing_wait().unwrap().unwrap().sas_decision,
            None
        );
    }

    #[tokio::test]
    async fn room_confirm_records_a_mismatch_as_a_rejection() {
        let (_root, app) = fixture();
        app.store
            .save_pairing_wait(&request(STATUS_CONFIRM_SAS, exited()))
            .unwrap();
        let error = confirm_error(&app, Some(OTHER_SAS)).await;
        assert!(error.contains("does not match"), "{error}");
        assert_eq!(
            app.store
                .load_pairing_wait()
                .unwrap()
                .unwrap()
                .sas_decision
                .as_deref(),
            Some(DECISION_REJECTED)
        );
    }

    #[tokio::test]
    async fn room_confirm_waits_for_a_joiner_then_reports_the_worker_outcome() {
        for mode in [TEXT, JSON] {
            let (_root, app) = fixture();
            app.store
                .save_pairing_wait(&request(STATUS_WAITING, live()))
                .unwrap();
            // The joiner arrives after `room confirm` saw it waiting.
            let store = app.store.clone();
            tokio::spawn(async move {
                tokio::time::sleep(PAIRING_FILE_POLL * 3).await;
                store
                    .save_pairing_wait(&request(STATUS_CONFIRM_SAS, live()))
                    .unwrap();
            });
            worker(
                &app.store,
                |wait| wait.sas_decision.as_deref() == Some(DECISION_CONFIRMED),
                |store, wait| {
                    store
                        .save_pairing_wait(&PairingWait {
                            status: STATUS_PAIRED.into(),
                            ..wait
                        })
                        .unwrap()
                },
            );
            within(answer_background(&app, Some("482 913"), false, mode))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn room_confirm_reports_a_worker_that_fails_stops_or_is_cancelled() {
        type Finish = fn(&LocalStore, PairingWait);
        let outcomes: [(Finish, &str); 3] = [
            (
                |store, wait| {
                    store
                        .save_pairing_wait(&PairingWait {
                            status: STATUS_FAILED.into(),
                            error: Some("relay closed during pairing".into()),
                            ..wait
                        })
                        .unwrap()
                },
                "relay closed during pairing",
            ),
            (
                |store, wait| {
                    store
                        .save_pairing_wait(&PairingWait {
                            status: STATUS_FAILED.into(),
                            ..wait
                        })
                        .unwrap()
                },
                "background pairing failed",
            ),
            (|store, _| store.clear_pairing_wait(), "cancelled"),
        ];
        for (finish, expected) in outcomes {
            let (_root, app) = fixture();
            app.store
                .save_pairing_wait(&request(STATUS_CONFIRM_SAS, live()))
                .unwrap();
            worker(
                &app.store,
                |wait| wait.sas_decision.as_deref() == Some(DECISION_CONFIRMED),
                finish,
            );
            let error = confirm_error(&app, Some(SAS)).await;
            assert!(error.contains(expected), "{expected:?}: {error}");
        }
        // The worker exited without writing an outcome.
        let (_root, app) = fixture();
        app.store
            .save_pairing_wait(&request(STATUS_CONFIRM_SAS, exited()))
            .unwrap();
        let error = confirm_error(&app, Some(SAS)).await;
        assert!(error.contains("stopped before completing"), "{error}");
    }

    #[tokio::test]
    async fn room_reject_before_a_joiner_records_the_rejection_at_once() {
        for mode in [TEXT, JSON] {
            let (_root, app) = fixture();
            app.store
                .save_pairing_wait(&request(STATUS_WAITING, live()))
                .unwrap();
            within(answer_background(&app, None, true, mode))
                .await
                .unwrap();
            let wait = app.store.load_pairing_wait().unwrap().unwrap();
            assert!(rejected(&wait));
            assert!(replaceable(&wait), "a rejected request blocks a new room");
        }
        assert!(replaceable(&request(STATUS_WAITING, exited())));
        assert!(!replaceable(&request(STATUS_WAITING, live())));
        let (_root, app) = fixture();
        app.store
            .save_pairing_wait(&decided(
                request(STATUS_CONFIRM_SAS, live()),
                DECISION_CONFIRMED,
            ))
            .unwrap();
        let error = within(answer_background(&app, None, true, TEXT))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already confirmed"), "{error}");
    }

    #[tokio::test]
    async fn room_reject_waits_for_the_worker_and_reasserts_a_dropped_rejection() {
        for mode in [TEXT, JSON] {
            let (_root, app) = fixture();
            app.store
                .save_pairing_wait(&request(STATUS_CONFIRM_SAS, live()))
                .unwrap();
            // The worker overwrites the file once without the rejection,
            // as a concurrent write of its own would.
            let dropped = worker(&app.store, rejected, |store, wait| {
                store
                    .save_pairing_wait(&PairingWait {
                        sas_decision: None,
                        ..wait
                    })
                    .unwrap()
            });
            let store = app.store.clone();
            let gave_up = tokio::spawn(async move {
                dropped.await.unwrap();
                worker(&store, rejected, |store, wait| {
                    store
                        .save_pairing_wait(&PairingWait {
                            status: STATUS_FAILED.into(),
                            ..wait
                        })
                        .unwrap()
                })
                .await
                .unwrap();
            });
            within(answer_background(&app, None, true, mode))
                .await
                .unwrap();
            within(gave_up).await.unwrap();
        }
    }

    #[tokio::test]
    async fn room_reject_reports_a_pairing_that_completed_first_or_was_removed() {
        type Finish = fn(&LocalStore, PairingWait);
        let outcomes: [(Finish, bool); 3] = [
            (
                |store, wait| {
                    store
                        .save_pairing_wait(&PairingWait {
                            status: STATUS_PAIRED.into(),
                            ..wait
                        })
                        .unwrap()
                },
                false,
            ),
            (|store, _| store.clear_pairing_wait(), true),
            // A newer `room create` replaced the request.
            (
                |store, wait| {
                    store
                        .save_pairing_wait(&PairingWait {
                            pid: wait.pid.wrapping_add(1),
                            sas_decision: None,
                            ..wait
                        })
                        .unwrap()
                },
                true,
            ),
        ];
        for (finish, stopped) in outcomes {
            let (_root, app) = fixture();
            app.store
                .save_pairing_wait(&request(STATUS_CONFIRM_SAS, live()))
                .unwrap();
            worker(&app.store, rejected, finish);
            let result = within(answer_background(&app, None, true, TEXT)).await;
            if stopped {
                result.unwrap();
            } else {
                let error = result.unwrap_err().to_string();
                assert!(error.contains("completed before the rejection"), "{error}");
            }
        }
    }
}
