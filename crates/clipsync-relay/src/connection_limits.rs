// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Per-client, per-site and relay-wide caps on concurrent WebSocket
//! connections.
//!
//! Every socket starts unauthenticated. Unauthenticated sockets may hold at
//! most `max_unauthenticated` of the relay-wide slots; once they do, a new
//! socket evicts the oldest of them instead of being refused, so a flood of
//! sockets that never send `hello` cannot lock out clients that do. The
//! remaining slots are kept for authenticated sockets and for evicted ones
//! that have not finished closing, so the relay-wide cap is never exceeded.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use dashmap::DashMap;
use tokio::sync::Notify;

use crate::rate_limit::{client_key, site_key};

/// Why a connection was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionRefused {
    /// The relay holds its maximum number of connections.
    RelayFull,
    /// This client address, or its IPv6 site, holds its maximum number of
    /// connections.
    ClientFull,
}

/// Caps applied by [`ConnectionCounts::acquire`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectionLimits {
    pub(crate) max_total: usize,
    /// Most slots held by sockets that have not authenticated yet; below
    /// `max_total`, except when `max_total` is 1.
    pub(crate) max_unauthenticated: usize,
    pub(crate) max_per_client: usize,
    /// Most slots per IPv6 /48; IPv4 clients have no site.
    pub(crate) max_per_site: usize,
}

#[derive(Default)]
pub(crate) struct ConnectionCounts {
    total: AtomicUsize,
    per_client: DashMap<IpAddr, usize>,
    per_site: DashMap<IpAddr, usize>,
    /// Unauthenticated sockets by admission order, oldest first, each with the
    /// signal that tells it to close.
    unauthenticated: Mutex<BTreeMap<u64, Arc<Notify>>>,
    next_id: AtomicU64,
}

/// Held for the lifetime of one accepted connection; dropping it frees the slot.
pub(crate) struct ConnectionSlot {
    counts: Arc<ConnectionCounts>,
    key: IpAddr,
    site: Option<IpAddr>,
    id: u64,
    evicted: Arc<Notify>,
}

impl ConnectionCounts {
    /// Take a slot for a connection from `ip`, or refuse without taking one.
    /// The client's own caps are checked first, so a client retrying at its
    /// cap never inflates the relay-wide count seen by others. When the
    /// unauthenticated share is full, the oldest unauthenticated socket is
    /// told to close and the new one is admitted in its place.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        ip: IpAddr,
        limits: ConnectionLimits,
    ) -> Result<ConnectionSlot, ConnectionRefused> {
        let key = client_key(ip);
        if !take(&self.per_client, key, limits.max_per_client) {
            return Err(ConnectionRefused::ClientFull);
        }
        let site = site_key(ip);
        if let Some(site) = site {
            if !take(&self.per_site, site, limits.max_per_site) {
                give_back(&self.per_client, key);
                return Err(ConnectionRefused::ClientFull);
            }
        }
        let mut unauthenticated = self
            .unauthenticated
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Under the same lock as the unauthenticated set, so the two stay
        // consistent: every unauthenticated socket holds a counted slot.
        if self.total.load(Ordering::Acquire) >= limits.max_total {
            drop(unauthenticated);
            self.release_client(key, site);
            return Err(ConnectionRefused::RelayFull);
        }
        self.total.fetch_add(1, Ordering::AcqRel);
        if unauthenticated.len() >= limits.max_unauthenticated {
            if let Some((_, oldest)) = unauthenticated.pop_first() {
                // The evicted socket keeps its slot until it has closed, which
                // the authenticated share leaves room for.
                oldest.notify_one();
            }
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let evicted = Arc::new(Notify::new());
        unauthenticated.insert(id, evicted.clone());
        drop(unauthenticated);
        Ok(ConnectionSlot {
            counts: self.clone(),
            key,
            site,
            id,
            evicted,
        })
    }

    fn release_client(&self, key: IpAddr, site: Option<IpAddr>) {
        give_back(&self.per_client, key);
        if let Some(site) = site {
            give_back(&self.per_site, site);
        }
    }

    fn release(&self, slot: &ConnectionSlot) {
        self.release_client(slot.key, slot.site);
        let mut unauthenticated = self
            .unauthenticated
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        unauthenticated.remove(&slot.id);
        self.total.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn total(&self) -> usize {
        self.total.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn unauthenticated(&self) -> usize {
        self.unauthenticated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[cfg(test)]
    pub(crate) fn clients(&self) -> usize {
        self.per_client.len() + self.per_site.len()
    }
}

impl ConnectionSlot {
    /// Record that the socket authenticated, so it can no longer be evicted
    /// to admit a newer one. Returns false when it was already evicted: the
    /// socket must then close.
    pub(crate) fn authenticate(&self) -> bool {
        self.counts
            .unauthenticated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id)
            .is_some()
    }

    /// Resolves once this unauthenticated socket has been evicted.
    pub(crate) async fn evicted(&self) {
        self.evicted.notified().await;
    }
}

/// Count one more connection for `key`, unless it already holds `max`.
fn take(counts: &DashMap<IpAddr, usize>, key: IpAddr, max: usize) -> bool {
    let mut held = counts.entry(key).or_insert(0);
    if *held >= max {
        // Never leave an entry for a refused first connection.
        let empty = *held == 0;
        drop(held);
        if empty {
            counts.remove_if(&key, |_, held| *held == 0);
        }
        return false;
    }
    *held += 1;
    true
}

/// Undo one count; an entry that reaches zero is removed so the map holds
/// only clients with open connections.
fn give_back(counts: &DashMap<IpAddr, usize>, key: IpAddr) {
    counts.remove_if_mut(&key, |_, held| {
        *held = held.saturating_sub(1);
        *held == 0
    });
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.counts.release(self);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "connection_limits_tests.rs"]
mod tests;
