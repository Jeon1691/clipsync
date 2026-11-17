// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Client address resolution for per-client limits.
//!
//! The TCP peer address is authoritative by default. `X-Forwarded-For` is read
//! only when the peer itself is an operator-configured trusted proxy, and then
//! only up to the rightmost hop that is not a trusted proxy: every entry to the
//! left of that hop was written by the client and can be forged.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::http::HeaderMap;
use ipnet::{IpNet, Ipv4Net, Ipv6Net};

const X_FORWARDED_FOR: &str = "x-forwarded-for";
/// Separates hops within one `X-Forwarded-For` value (comma-separated list),
/// and entries within one `CLIPSYNC_RELAY_TRUSTED_PROXIES` value.
const HOP_SEPARATOR: u8 = b',';
/// Prefix length of the IPv4-mapped IPv6 block `::ffff:0:0/96` (RFC 4291
/// section 2.5.5.2: 80 zero bits and 16 one bits, then the IPv4 address).
const IPV4_MAPPED_PREFIX_LEN: u8 = 96;

#[derive(Clone, Debug, Default)]
pub(crate) struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    pub(crate) fn new(proxies: Vec<IpNet>) -> Self {
        Self(canonical_trusted_proxies(&proxies))
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

/// Canonicalize trusted networks the way [`TrustedProxies::client_ip`]
/// canonicalizes addresses, so membership does not depend on spelling.
///
/// Peers and hops are compared as IPv4 when they are IPv4-mapped IPv6
/// addresses, so a network inside `::ffff:0:0/96` becomes the equivalent IPv4
/// network. A wider IPv6 network that contains that whole block (such as
/// `::/0`) keeps its IPv6 part and also yields `0.0.0.0/0`, because it covers
/// every mapped and therefore every IPv4 address. Host bits are dropped.
pub(crate) fn canonical_trusted_proxies(proxies: &[IpNet]) -> Vec<IpNet> {
    let mapped_block = Ipv6Net::new(
        Ipv4Addr::UNSPECIFIED.to_ipv6_mapped(),
        IPV4_MAPPED_PREFIX_LEN,
    )
    .expect("the IPv4-mapped prefix length is a valid IPv6 prefix");
    let mut canonical = Vec::with_capacity(proxies.len());
    for net in proxies {
        match net.trunc() {
            IpNet::V6(net) if net.prefix_len() >= IPV4_MAPPED_PREFIX_LEN => {
                match net.network().to_ipv4_mapped() {
                    Some(v4) => canonical.push(IpNet::V4(
                        Ipv4Net::new(v4, net.prefix_len() - IPV4_MAPPED_PREFIX_LEN)
                            .expect("a mapped prefix minus 96 is a valid IPv4 prefix"),
                    )),
                    None => canonical.push(IpNet::V6(net)),
                }
            }
            IpNet::V6(net) if net.contains(&mapped_block) => {
                canonical.push(IpNet::V6(net));
                canonical.push(IpNet::V4(Ipv4Net::default()));
            }
            net => canonical.push(net),
        }
    }
    canonical
}

/// The canonical trusted networks that span a whole address family, for a
/// startup warning: with one of them every client can choose its own address.
pub(crate) fn trusts_every_address(proxies: &[IpNet]) -> impl Iterator<Item = &IpNet> {
    proxies.iter().filter(|net| net.prefix_len() == 0)
}

/// Parse a `CLIPSYNC_RELAY_TRUSTED_PROXIES` value: comma-separated CIDR ranges
/// or single addresses. Empty entries are skipped, so an empty value or a
/// trailing comma leaves the setting unset.
pub(crate) fn parse_trusted_proxies(value: &str) -> Result<Vec<IpNet>, String> {
    value
        .split(char::from(HOP_SEPARATOR))
        .filter(|entry| !entry.trim().is_empty())
        .map(parse_trusted_proxy)
        .collect()
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
