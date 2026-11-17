// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Fixed-window budgets shared by the pairing limits.
//!
//! A [`Budget`] counts events per client and relay-wide. An event is reserved
//! in both before the guarded work runs, so concurrent requests cannot
//! overshoot either limit, and it can be refunded afterwards. Pairing joins
//! refund successes (only failures count); pairing creation keeps every
//! accepted request.

use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// IPv6 sites are normally delegated at least a /64, so one client controls
/// every address in it; count them as one client.
const IPV6_CLIENT_PREFIX_LEN: u32 = 64;

/// Key for a client address: IPv4 as is, IPv6 by its /64 prefix.
pub(crate) fn client_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mask = u128::MAX << (u128::BITS - IPV6_CLIENT_PREFIX_LEN);
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
        }
        v4 => v4,
    }
}

/// Limits of one [`Budget`] check.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BudgetLimits {
    pub(crate) client_limit: u32,
    pub(crate) client_window: Duration,
    pub(crate) relay_limit: u32,
    pub(crate) relay_window: Duration,
}

/// Per-client counts and the relay-wide count, each in a fixed window.
#[derive(Default)]
pub(crate) struct Budget {
    /// Count and window start per client key (see [`client_key`]).
    pub(crate) clients: DashMap<IpAddr, (u32, Instant)>,
    /// Relay-wide count and window start.
    pub(crate) relay: RelayCount,
}

pub(crate) struct RelayCount(pub(crate) Mutex<(u32, Instant)>);

impl Default for RelayCount {
    fn default() -> Self {
        Self(Mutex::new((0, Instant::now())))
    }
}

/// An event counted by [`Budget::reserve`]; pass it to [`Budget::release`]
/// to refund it.
pub(crate) struct Reservation {
    key: IpAddr,
    client_window: Instant,
    relay_window: Instant,
}

impl Budget {
    /// Count one event for the client at `ip` and relay-wide, or refuse it
    /// without counting when either limit is already reached.
    pub(crate) fn reserve(&self, ip: IpAddr, limits: BudgetLimits) -> Option<Reservation> {
        let key = client_key(ip);
        let client_window = {
            let mut entry = self
                .clients
                .entry(key)
                .or_insert_with(|| (0, Instant::now()));
            // Read the clock while holding the entry so a reset never
            // back-dates a window past a concurrent update.
            let now = Instant::now();
            if now.duration_since(entry.1) > limits.client_window {
                *entry = (0, now);
            }
            if entry.0 >= limits.client_limit {
                return None;
            }
            entry.0 += 1;
            entry.1
        };
        let mut relay = self.relay.0.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if now.duration_since(relay.1) > limits.relay_window {
            *relay = (0, now);
        }
        if relay.0 >= limits.relay_limit {
            drop(relay);
            self.release_client(key, client_window);
            return None;
        }
        relay.0 += 1;
        Some(Reservation {
            key,
            client_window,
            relay_window: relay.1,
        })
    }

    /// Refund a reserved event; a refund for an already-reset window is void.
    pub(crate) fn release(&self, reservation: Reservation) {
        self.release_client(reservation.key, reservation.client_window);
        let mut relay = self.relay.0.lock().unwrap_or_else(PoisonError::into_inner);
        if relay.1 == reservation.relay_window {
            relay.0 = relay.0.saturating_sub(1);
        }
    }

    fn release_client(&self, key: IpAddr, window: Instant) {
        self.clients.remove_if_mut(&key, |_, entry| {
            if entry.1 == window {
                entry.0 = entry.0.saturating_sub(1);
            }
            entry.0 == 0
        });
    }

    /// Drop client entries whose window has ended; they would be reset anyway.
    pub(crate) fn prune(&self, client_window: Duration) {
        self.clients
            .retain(|_, (_, window)| window.elapsed() <= client_window);
    }

    /// Test helper: count one event for a client without the relay-wide count.
    #[cfg(test)]
    pub(crate) fn charge_client(&self, ip: IpAddr, client_window: Duration) {
        let now = Instant::now();
        let mut entry = self.clients.entry(client_key(ip)).or_insert((0, now));
        if now.duration_since(entry.1) > client_window {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
    }
}
