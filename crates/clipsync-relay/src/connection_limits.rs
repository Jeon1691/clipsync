// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Per-client and relay-wide caps on concurrent WebSocket connections.

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::rate_limit::client_key;

/// Why a connection was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionRefused {
    /// The relay holds its maximum number of connections.
    RelayFull,
    /// This client address holds its maximum number of connections.
    ClientFull,
}

#[derive(Default)]
pub(crate) struct ConnectionCounts {
    total: AtomicUsize,
    per_client: DashMap<IpAddr, usize>,
}

/// Held for the lifetime of one accepted connection; dropping it frees the slot.
pub(crate) struct ConnectionSlot {
    counts: Arc<ConnectionCounts>,
    key: IpAddr,
}

impl ConnectionCounts {
    /// Take a slot for a connection from `ip`, or refuse without taking one.
    /// The client's own cap is checked first, so a client retrying at its cap
    /// never inflates the relay-wide count seen by others.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        ip: IpAddr,
        max_total: usize,
        max_per_client: usize,
    ) -> Result<ConnectionSlot, ConnectionRefused> {
        let key = client_key(ip);
        {
            let mut held = self.per_client.entry(key).or_insert(0);
            if *held >= max_per_client {
                return Err(ConnectionRefused::ClientFull);
            }
            *held += 1;
        }
        let mut total = self.total.load(Ordering::Acquire);
        loop {
            if total >= max_total {
                self.release_client(key);
                return Err(ConnectionRefused::RelayFull);
            }
            match self.total.compare_exchange_weak(
                total,
                total + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => total = current,
            }
        }
        Ok(ConnectionSlot {
            counts: self.clone(),
            key,
        })
    }

    /// Undo a client count; an entry that reaches zero is removed so the map
    /// holds only clients with open connections.
    fn release_client(&self, key: IpAddr) {
        self.per_client.remove_if_mut(&key, |_, held| {
            *held = held.saturating_sub(1);
            *held == 0
        });
    }

    fn release(&self, key: IpAddr) {
        self.release_client(key);
        self.total.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn total(&self) -> usize {
        self.total.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn clients(&self) -> usize {
        self.per_client.len()
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.counts.release(self.key);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "connection_limits_tests.rs"]
mod tests;
