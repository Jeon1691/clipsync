// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Failed pairing-join budgets and the guessing bound they must keep.
//!
//! Failed pairing joins are limited per client and relay-wide. The relay-wide
//! budget is what bounds guessing: with `PAIRING_TTL_SECS`-long fixed windows,
//! a pairing code's lifetime overlaps at most two windows, so at most
//! 2 * GLOBAL_FAILED_JOIN_LIMIT = 40 wrong codes are evaluated while any code
//! is live, however many addresses the attacker uses. Codes are uniform over
//! 10^6 values, so one session is guessed with probability <= 40 / 10^6 =
//! 4e-5 (4e-5 * S with S sessions open at once). This is below the 1e-4
//! implied by NIST SP 800-63B's ceiling of 100 failed attempts against a
//! 6-digit authenticator.
//!
//! Operators may change the budgets, but never past that 1e-4 bound: a
//! relay-wide limit above [`MAX_GLOBAL_FAILED_JOIN_LIMIT`] is rejected at startup.

use std::time::Duration;

use anyhow::bail;
use clipsync_protocol::PAIRING_TTL_SECS;

use crate::rate_limit::BudgetLimits;

/// Failed joins one client may make per window: enough to retype a mistyped
/// code a few times, while one address alone tests at most 10 of 10^6 codes.
pub(crate) const IP_FAILED_JOIN_LIMIT: u32 = 10;
/// Per-client window; longer than a pairing session so a code cannot outlive
/// one client's budget, short enough that a mistyping user is not locked out
/// for long.
pub(crate) const IP_FAILED_JOIN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Longest configurable per-client window: one day, so a user who mistyped a
/// code is never refused for longer than that.
const MAX_IP_FAILED_JOIN_WINDOW_SECS: u64 = 24 * 60 * 60;
/// Relay-wide failed joins per window; see the bound above. A self-hosted relay
/// rarely sees more than a handful of mistyped codes per pairing window.
pub(crate) const GLOBAL_FAILED_JOIN_LIMIT: u32 = 20;
/// One pairing TTL, so the per-session bound above holds if the TTL changes.
/// Not configurable: the bound depends on it.
pub(crate) const GLOBAL_FAILED_JOIN_WINDOW: Duration = Duration::from_secs(PAIRING_TTL_SECS);
/// Values a pairing code can take: six decimal digits (see `alloc_code`).
pub(crate) const PAIRING_CODE_SPACE: u64 = 1_000_000;
/// Largest acceptable guessing success per session, as 1 in this many (1e-4).
const NEGLIGIBLE_GUESS_ODDS: u64 = 10_000;
/// Fixed windows per pairing lifetime in the worst case (see the bound above).
pub(crate) const WINDOWS_PER_PAIRING_LIFETIME: u64 = 2;
/// Largest relay-wide limit that keeps one session's guessing chance at or
/// below 1 / `NEGLIGIBLE_GUESS_ODDS` (50 with the values above).
pub(crate) const MAX_GLOBAL_FAILED_JOIN_LIMIT: u32 =
    (PAIRING_CODE_SPACE / (WINDOWS_PER_PAIRING_LIFETIME * NEGLIGIBLE_GUESS_ODDS)) as u32;

// Keep the bound above true for the defaults if any of these values change.
const _: () = {
    assert!(GLOBAL_FAILED_JOIN_WINDOW.as_secs() >= PAIRING_TTL_SECS);
    assert!(IP_FAILED_JOIN_WINDOW.as_secs() >= PAIRING_TTL_SECS);
    assert!(IP_FAILED_JOIN_WINDOW.as_secs() <= MAX_IP_FAILED_JOIN_WINDOW_SECS);
    assert!(IP_FAILED_JOIN_LIMIT < GLOBAL_FAILED_JOIN_LIMIT);
    assert!(
        WINDOWS_PER_PAIRING_LIFETIME * GLOBAL_FAILED_JOIN_LIMIT as u64 * NEGLIGIBLE_GUESS_ODDS
            <= PAIRING_CODE_SPACE
    );
    assert!(
        WINDOWS_PER_PAIRING_LIFETIME * MAX_GLOBAL_FAILED_JOIN_LIMIT as u64 * NEGLIGIBLE_GUESS_ODDS
            <= PAIRING_CODE_SPACE
    );
};

/// Operator-tunable failed pairing-join budgets. Defaults are the constants
/// above; [`PairingJoinLimits::validate`] rejects values that break the
/// guessing bound or make no sense.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::Args)]
pub struct PairingJoinLimits {
    /// Failed pairing joins one client address (IPv6: one /64) may make per
    /// client window
    #[arg(
        long = "pairing-client-fail-limit",
        env = "CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_LIMIT",
        default_value_t = IP_FAILED_JOIN_LIMIT
    )]
    pub client_fail_limit: u32,
    /// Length of the per-client window, in seconds
    #[arg(
        long = "pairing-client-fail-window-secs",
        env = "CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_WINDOW_SECS",
        default_value_t = IP_FAILED_JOIN_WINDOW.as_secs()
    )]
    pub client_fail_window_secs: u64,
    /// Failed pairing joins the whole relay may make per pairing TTL; at most
    /// 50, which bounds a guess of one session at 1e-4
    #[arg(
        long = "pairing-relay-fail-limit",
        env = "CLIPSYNC_RELAY_PAIRING_RELAY_FAIL_LIMIT",
        default_value_t = GLOBAL_FAILED_JOIN_LIMIT
    )]
    pub relay_fail_limit: u32,
}

impl Default for PairingJoinLimits {
    fn default() -> Self {
        Self {
            client_fail_limit: IP_FAILED_JOIN_LIMIT,
            client_fail_window_secs: IP_FAILED_JOIN_WINDOW.as_secs(),
            relay_fail_limit: GLOBAL_FAILED_JOIN_LIMIT,
        }
    }
}

impl PairingJoinLimits {
    /// Reject budgets that weaken the guessing bound or cannot work.
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(1..=MAX_GLOBAL_FAILED_JOIN_LIMIT).contains(&self.relay_fail_limit) {
            bail!(
                "CLIPSYNC_RELAY_PAIRING_RELAY_FAIL_LIMIT must be between 1 and {MAX_GLOBAL_FAILED_JOIN_LIMIT}; larger values allow more than a 1e-4 chance of guessing a pairing code"
            );
        }
        if !(1..=self.relay_fail_limit).contains(&self.client_fail_limit) {
            bail!(
                "CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_LIMIT must be between 1 and CLIPSYNC_RELAY_PAIRING_RELAY_FAIL_LIMIT ({})",
                self.relay_fail_limit
            );
        }
        if !(PAIRING_TTL_SECS..=MAX_IP_FAILED_JOIN_WINDOW_SECS)
            .contains(&self.client_fail_window_secs)
        {
            bail!(
                "CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_WINDOW_SECS must be between {PAIRING_TTL_SECS} (one pairing TTL) and {MAX_IP_FAILED_JOIN_WINDOW_SECS}"
            );
        }
        Ok(())
    }

    pub(crate) fn client_fail_window(&self) -> Duration {
        Duration::from_secs(self.client_fail_window_secs)
    }

    pub(crate) fn budget(&self) -> BudgetLimits {
        BudgetLimits {
            client_limit: self.client_fail_limit,
            client_window: self.client_fail_window(),
            // Failed joins keep #26's per-client accounting.
            site_limit: None,
            relay_limit: self.relay_fail_limit,
            relay_window: GLOBAL_FAILED_JOIN_WINDOW,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "join_limits_tests.rs"]
mod tests;
