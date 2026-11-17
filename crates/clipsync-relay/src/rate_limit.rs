// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Fixed-window budgets shared by the pairing limits.
//!
//! A [`Budget`] counts events per client, per IPv6 site when configured, and
//! relay-wide. An event is reserved in every count before the guarded work
//! runs, so concurrent requests cannot overshoot any limit, and it can be
//! refunded afterwards. Pairing joins refund successes (only failures count);
//! pairing creation keeps every accepted request.

use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// IPv6 sites are normally delegated at least a /64, so one client controls
/// every address in it; count them as one client.
const IPV6_CLIENT_PREFIX_LEN: u32 = 64;
/// A /48 is the largest prefix routinely delegated to one end site (RFC 6177;
/// ISPs often hand out a /56, which a /48 contains). One site can spread over
/// many /64s, so per-site limits stop it from multiplying its client limits.
const IPV6_SITE_PREFIX_LEN: u32 = 48;
/// Clients' worth of a limit one IPv6 site may use: a few devices or
/// households behind one site router each get their own /64.
pub(crate) const IPV6_SITE_CLIENT_SHARE: usize = 3;

fn ipv6_prefix(v6: Ipv6Addr, len: u32) -> IpAddr {
    let mask = u128::MAX << (u128::BITS - len);
    IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
}

/// Key for a client address: IPv4 as is, IPv6 by its /64 prefix.
pub(crate) fn client_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => ipv6_prefix(v6, IPV6_CLIENT_PREFIX_LEN),
        v4 => v4,
    }
}

/// Key for the IPv6 site (/48) of a client address; `None` for IPv4, where
/// the client key is already the narrowest grouping the relay can make.
pub(crate) fn site_key(ip: IpAddr) -> Option<IpAddr> {
    match ip.to_canonical() {
        IpAddr::V6(v6) => Some(ipv6_prefix(v6, IPV6_SITE_PREFIX_LEN)),
        IpAddr::V4(_) => None,
    }
}

/// The per-site allowance for a per-client limit, never above `relay_limit`.
pub(crate) fn site_limit(client_limit: usize, relay_limit: usize) -> usize {
    client_limit
        .saturating_mul(IPV6_SITE_CLIENT_SHARE)
        .min(relay_limit)
}

/// Limits of one [`Budget`] check.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BudgetLimits {
    pub(crate) client_limit: u32,
    pub(crate) client_window: Duration,
    /// Limit per IPv6 /48 in the client window; `None` counts no sites.
    pub(crate) site_limit: Option<u32>,
    pub(crate) relay_limit: u32,
    pub(crate) relay_window: Duration,
}

/// Per-client counts and the relay-wide count, each in a fixed window.
#[derive(Default)]
pub(crate) struct Budget {
    /// Count and window start per client key (see [`client_key`]).
    pub(crate) clients: DashMap<IpAddr, (u32, Instant)>,
    /// Count and window start per IPv6 site (see [`site_key`]).
    pub(crate) sites: DashMap<IpAddr, (u32, Instant)>,
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
    site: Option<(IpAddr, Instant)>,
    relay_window: Instant,
}

/// Count one event for `key` in its fixed window, or refuse without counting.
fn reserve_in(
    counts: &DashMap<IpAddr, (u32, Instant)>,
    key: IpAddr,
    limit: u32,
    window: Duration,
) -> Option<Instant> {
    let mut entry = counts.entry(key).or_insert_with(|| (0, Instant::now()));
    // Read the clock while holding the entry so a reset never back-dates a
    // window past a concurrent update.
    let now = Instant::now();
    if now.duration_since(entry.1) > window {
        *entry = (0, now);
    }
    if entry.0 >= limit {
        return None;
    }
    entry.0 += 1;
    Some(entry.1)
}

/// Refund one event for `key`; a refund for an already-reset window is void.
/// An entry that reaches zero is removed.
fn release_in(counts: &DashMap<IpAddr, (u32, Instant)>, key: IpAddr, window: Instant) {
    counts.remove_if_mut(&key, |_, entry| {
        if entry.1 == window {
            entry.0 = entry.0.saturating_sub(1);
        }
        entry.0 == 0
    });
}

impl Budget {
    /// Count one event for the client at `ip` and relay-wide, or refuse it
    /// without counting when either limit is already reached.
    pub(crate) fn reserve(&self, ip: IpAddr, limits: BudgetLimits) -> Option<Reservation> {
        let key = client_key(ip);
        let client_window = reserve_in(
            &self.clients,
            key,
            limits.client_limit,
            limits.client_window,
        )?;
        let site = match (limits.site_limit, site_key(ip)) {
            (Some(limit), Some(site)) => {
                let Some(window) = reserve_in(&self.sites, site, limit, limits.client_window)
                else {
                    release_in(&self.clients, key, client_window);
                    return None;
                };
                Some((site, window))
            }
            _ => None,
        };
        let mut relay = self.relay.0.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if now.duration_since(relay.1) > limits.relay_window {
            *relay = (0, now);
        }
        if relay.0 >= limits.relay_limit {
            drop(relay);
            self.release_local(key, client_window, site);
            return None;
        }
        relay.0 += 1;
        Some(Reservation {
            key,
            client_window,
            site,
            relay_window: relay.1,
        })
    }

    /// Refund a reserved event; a refund for an already-reset window is void.
    pub(crate) fn release(&self, reservation: Reservation) {
        self.release_local(reservation.key, reservation.client_window, reservation.site);
        let mut relay = self.relay.0.lock().unwrap_or_else(PoisonError::into_inner);
        if relay.1 == reservation.relay_window {
            relay.0 = relay.0.saturating_sub(1);
        }
    }

    fn release_local(&self, key: IpAddr, client_window: Instant, site: Option<(IpAddr, Instant)>) {
        release_in(&self.clients, key, client_window);
        if let Some((site, window)) = site {
            release_in(&self.sites, site, window);
        }
    }

    /// Drop client and site entries whose window has ended; they would be
    /// reset anyway.
    pub(crate) fn prune(&self, client_window: Duration) {
        self.clients
            .retain(|_, (_, window)| window.elapsed() <= client_window);
        self.sites
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
