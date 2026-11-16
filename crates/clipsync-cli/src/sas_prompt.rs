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
mod tests {
    use super::*;

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
}
