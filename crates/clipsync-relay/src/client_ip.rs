// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Client address resolution for per-client limits.
//!
//! The TCP peer address is authoritative by default. `X-Forwarded-For` is read
//! only when the peer itself is an operator-configured trusted proxy, and then
//! only up to the rightmost hop that is not a trusted proxy: every entry to the
//! left of that hop was written by the client and can be forged.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use ipnet::IpNet;

const X_FORWARDED_FOR: &str = "x-forwarded-for";
/// Separates hops within one `X-Forwarded-For` value (comma-separated list).
const HOP_SEPARATOR: u8 = b',';

#[derive(Clone, Debug, Default)]
pub(crate) struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    pub(crate) fn new(proxies: Vec<IpNet>) -> Self {
        Self(proxies.into_iter().map(|net| net.trunc()).collect())
    }

    fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|net| net.contains(&ip))
    }

    /// Resolve the client address of a request received from `peer`.
    pub(crate) fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let peer = peer.to_canonical();
        if !self.contains(peer) {
            return peer;
        }
        // Each proxy appends the address it received the request from, so walk
        // from the right. A malformed hop ends the walk at the nearest trusted
        // proxy: nothing to its left can be attributed reliably. Split the raw
        // bytes and decode each hop on its own, so an opaque client-written
        // hop cannot hide the hop a proxy appended to the same header value.
        let hops: Vec<&[u8]> = headers
            .get_all(X_FORWARDED_FOR)
            .iter()
            .flat_map(|value| value.as_bytes().split(|byte| *byte == HOP_SEPARATOR))
            .collect();
        let mut client = peer;
        for hop in hops.into_iter().rev() {
            let Some(ip) = std::str::from_utf8(hop).ok().and_then(parse_hop) else {
                break;
            };
            client = ip.to_canonical();
            if !self.contains(client) {
                break;
            }
        }
        client
    }
}

fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    hop.parse::<IpAddr>()
        .ok()
        .or_else(|| hop.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
}

/// Parse one `CLIPSYNC_RELAY_TRUSTED_PROXIES` entry: a CIDR range or a single address.
pub(crate) fn parse_trusted_proxy(value: &str) -> Result<IpNet, String> {
    let value = value.trim();
    value
        .parse::<IpNet>()
        .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
        .map_err(|_| format!("`{value}` is not an IP address or CIDR range"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_ip_tests.rs"]
mod tests;
