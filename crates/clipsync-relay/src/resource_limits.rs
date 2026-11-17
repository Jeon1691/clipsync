// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Bounds on the relay's in-memory state and concurrent connections.
//!
//! `/v1/pairing/create` and `/v1/ws` are reachable without credentials, so
//! everything they allocate is bounded:
//!
//! - Pairing creation is rate limited per client and relay-wide (the same
//!   fixed-window [`Budget`](crate::rate_limit::Budget) as failed joins).
//! - At most [`MAX_ACTIVE_PAIRING_SESSIONS`] sessions are open at once, far
//!   below the 10^6 pairing codes, so allocating a fresh code never degrades.
//! - Rooms are capped at `max_rooms`, and a room is dropped once every device
//!   token issued for it has expired or every device in it has been revoked.
//! - Offline text is queued for at most [`MAX_QUEUED_TEXT_ENVELOPES`]
//!   recipients, each envelope for at most the protocol's hard TTL.
//! - WebSocket connections are capped per client and relay-wide, and a
//!   socket that does not authenticate promptly, or then goes silent, is
//!   closed so it cannot hold a slot indefinitely.

use std::time::Duration;

use anyhow::bail;
use clipsync_protocol::{HEARTBEAT_SECS, PAIRING_TTL_SECS};

use crate::join_limits::{PAIRING_CODE_SPACE, WINDOWS_PER_PAIRING_LIFETIME};
use crate::rate_limit::BudgetLimits;

/// Pairings one client may start per window: a user restarting a pairing
/// several times, or a few households behind one NAT.
pub(crate) const PAIRING_CREATE_CLIENT_LIMIT: u32 = 20;
/// Per-client creation window, matching the failed-join window.
pub(crate) const PAIRING_CREATE_CLIENT_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Longest configurable per-client creation window: one day.
const MAX_PAIRING_CREATE_CLIENT_WINDOW_SECS: u64 = 24 * 60 * 60;
/// Pairings the whole relay may start per pairing TTL. A self-hosted relay
/// rarely sees more than a few pairings a minute.
pub(crate) const PAIRING_CREATE_RELAY_LIMIT: u32 = 100;
/// One pairing TTL, so the session bound below holds if the TTL changes.
pub(crate) const PAIRING_CREATE_RELAY_WINDOW: Duration = Duration::from_secs(PAIRING_TTL_SECS);
/// Most pairing sessions open at once: 0.1% of the code space, so a random
/// code collides with an open one with probability at most 1e-3 and every one
/// of `alloc_code`'s draws collides with probability at most 1e-96.
pub(crate) const MAX_ACTIVE_PAIRING_SESSIONS: usize = 1_000;
/// Share of the code space [`MAX_ACTIVE_PAIRING_SESSIONS`] may occupy, as 1 in this many.
const MIN_FREE_CODES_PER_SESSION: u64 = 1_000;
/// Largest relay-wide creation limit: a session's lifetime overlaps at most
/// two windows, so creations that pass the rate limit alone never fill
/// [`MAX_ACTIVE_PAIRING_SESSIONS`] (500 with the values above).
pub(crate) const MAX_PAIRING_CREATE_RELAY_LIMIT: u32 =
    (MAX_ACTIVE_PAIRING_SESSIONS as u64 / WINDOWS_PER_PAIRING_LIFETIME) as u32;
/// Most rooms held at once. Each holds two devices of at most a few hundred
/// bytes each (see [`MAX_PAIRING_FIELD_BYTES`]), so this bounds rooms to tens
/// of megabytes; a self-hosted relay rarely serves more than a few hundred.
pub(crate) const MAX_ROOMS: usize = 10_000;
/// Longest accepted device ID, device name, OS name and pairing session ID,
/// in bytes: room for any DNS hostname (253 bytes), while a pairing session or
/// room cannot hold a request-sized string.
pub(crate) const MAX_PAIRING_FIELD_BYTES: usize = 256;
/// Most recipients with a queued offline text envelope. Each envelope is at
/// most one frame (384 KiB), so the queue holds at most 96 MiB.
pub(crate) const MAX_QUEUED_TEXT_ENVELOPES: usize = 256;
/// Most WebSocket connections at once. Below the common 1024 open-file soft
/// limit, leaving room for the listener, 128 TLS handshakes and webhooks.
pub(crate) const MAX_CONNECTIONS: usize = 512;
/// Most WebSocket connections from one client address (IPv6: one /64): one
/// per device for a household behind one NAT, plus reconnect overlap.
pub(crate) const MAX_CLIENT_CONNECTIONS: usize = 32;

/// Time a WebSocket has to send a valid `hello` after the upgrade, however
/// many other messages it sends. Clients send it immediately; this matches the
/// listener's ten-second limit on the first HTTP request.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest silence accepted on an authenticated WebSocket: three client
/// heartbeats. Clients themselves reconnect after two heartbeats plus 5 s.
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(3 * HEARTBEAT_SECS);

/// Deadlines that keep idle sockets from holding connection slots.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SocketDeadlines {
    pub(crate) hello: Duration,
    pub(crate) idle: Duration,
}

impl Default for SocketDeadlines {
    fn default() -> Self {
        Self {
            hello: HELLO_TIMEOUT,
            idle: IDLE_TIMEOUT,
        }
    }
}

const _: () = {
    assert!(MAX_ACTIVE_PAIRING_SESSIONS as u64 * MIN_FREE_CODES_PER_SESSION <= PAIRING_CODE_SPACE);
    assert!(PAIRING_CREATE_RELAY_LIMIT <= MAX_PAIRING_CREATE_RELAY_LIMIT);
    assert!(PAIRING_CREATE_CLIENT_LIMIT <= PAIRING_CREATE_RELAY_LIMIT);
    assert!(PAIRING_CREATE_CLIENT_WINDOW.as_secs() >= PAIRING_TTL_SECS);
    assert!(PAIRING_CREATE_CLIENT_WINDOW.as_secs() <= MAX_PAIRING_CREATE_CLIENT_WINDOW_SECS);
    assert!(MAX_CLIENT_CONNECTIONS <= MAX_CONNECTIONS);
};

/// Operator-tunable resource limits. Defaults are the constants above;
/// [`RelayResourceLimits::validate`] rejects values that cannot work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::Args)]
pub struct RelayResourceLimits {
    /// Pairings one client address (IPv6: one /64) may create per client
    /// creation window
    #[arg(
        long = "pairing-client-create-limit",
        env = "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_LIMIT",
        default_value_t = PAIRING_CREATE_CLIENT_LIMIT
    )]
    pub client_create_limit: u32,
    /// Length of the per-client creation window, in seconds
    #[arg(
        long = "pairing-client-create-window-secs",
        env = "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_WINDOW_SECS",
        default_value_t = PAIRING_CREATE_CLIENT_WINDOW.as_secs()
    )]
    pub client_create_window_secs: u64,
    /// Pairings the whole relay may create per pairing TTL; at most 500
    #[arg(
        long = "pairing-relay-create-limit",
        env = "CLIPSYNC_RELAY_PAIRING_RELAY_CREATE_LIMIT",
        default_value_t = PAIRING_CREATE_RELAY_LIMIT
    )]
    pub relay_create_limit: u32,
    /// Rooms the relay holds at once; pairing is refused while full
    #[arg(
        long = "max-rooms",
        env = "CLIPSYNC_RELAY_MAX_ROOMS",
        default_value_t = MAX_ROOMS
    )]
    pub max_rooms: usize,
    /// WebSocket connections the relay accepts at once
    #[arg(
        long = "max-connections",
        env = "CLIPSYNC_RELAY_MAX_CONNECTIONS",
        default_value_t = MAX_CONNECTIONS
    )]
    pub max_connections: usize,
    /// WebSocket connections one client address (IPv6: one /64) may hold at once
    #[arg(
        long = "max-client-connections",
        env = "CLIPSYNC_RELAY_MAX_CLIENT_CONNECTIONS",
        default_value_t = MAX_CLIENT_CONNECTIONS
    )]
    pub max_client_connections: usize,
}

impl Default for RelayResourceLimits {
    fn default() -> Self {
        Self {
            client_create_limit: PAIRING_CREATE_CLIENT_LIMIT,
            client_create_window_secs: PAIRING_CREATE_CLIENT_WINDOW.as_secs(),
            relay_create_limit: PAIRING_CREATE_RELAY_LIMIT,
            max_rooms: MAX_ROOMS,
            max_connections: MAX_CONNECTIONS,
            max_client_connections: MAX_CLIENT_CONNECTIONS,
        }
    }
}

impl RelayResourceLimits {
    /// Reject limits that cannot work or would let creation fill the session cap.
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(1..=MAX_PAIRING_CREATE_RELAY_LIMIT).contains(&self.relay_create_limit) {
            bail!(
                "CLIPSYNC_RELAY_PAIRING_RELAY_CREATE_LIMIT must be between 1 and {MAX_PAIRING_CREATE_RELAY_LIMIT}; the relay holds at most {MAX_ACTIVE_PAIRING_SESSIONS} open pairing sessions"
            );
        }
        if !(1..=self.relay_create_limit).contains(&self.client_create_limit) {
            bail!(
                "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_LIMIT must be between 1 and CLIPSYNC_RELAY_PAIRING_RELAY_CREATE_LIMIT ({})",
                self.relay_create_limit
            );
        }
        if !(PAIRING_TTL_SECS..=MAX_PAIRING_CREATE_CLIENT_WINDOW_SECS)
            .contains(&self.client_create_window_secs)
        {
            bail!(
                "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_WINDOW_SECS must be between {PAIRING_TTL_SECS} (one pairing TTL) and {MAX_PAIRING_CREATE_CLIENT_WINDOW_SECS}"
            );
        }
        if self.max_rooms == 0 {
            bail!("CLIPSYNC_RELAY_MAX_ROOMS must be at least 1");
        }
        if self.max_connections == 0 {
            bail!("CLIPSYNC_RELAY_MAX_CONNECTIONS must be at least 1");
        }
        if !(1..=self.max_connections).contains(&self.max_client_connections) {
            bail!(
                "CLIPSYNC_RELAY_MAX_CLIENT_CONNECTIONS must be between 1 and CLIPSYNC_RELAY_MAX_CONNECTIONS ({})",
                self.max_connections
            );
        }
        Ok(())
    }

    pub(crate) fn create_budget(&self) -> BudgetLimits {
        BudgetLimits {
            client_limit: self.client_create_limit,
            client_window: Duration::from_secs(self.client_create_window_secs),
            relay_limit: self.relay_create_limit,
            relay_window: PAIRING_CREATE_RELAY_WINDOW,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "resource_limits_tests.rs"]
mod tests;
