// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use axum::http::HeaderValue;

fn proxies(entries: &[&str]) -> TrustedProxies {
    TrustedProxies::new(
        entries
            .iter()
            .map(|entry| parse_trusted_proxy(entry).unwrap())
            .collect(),
    )
}

fn forwarded(values: &[&[u8]]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append(X_FORWARDED_FOR, HeaderValue::from_bytes(value).unwrap());
    }
    headers
}

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

#[test]
fn forwarded_header_is_ignored_without_trusted_proxy_configuration() {
    let headers = forwarded(&[b"203.0.113.9"]);
    assert_eq!(
        TrustedProxies::default().client_ip(ip("127.0.0.1"), &headers),
        ip("127.0.0.1")
    );
}

#[test]
fn forwarded_header_is_ignored_from_an_untrusted_peer() {
    let trusted = proxies(&["10.0.0.0/8"]);
    let headers = forwarded(&[b"203.0.113.9"]);
    assert_eq!(
        trusted.client_ip(ip("198.51.100.4"), &headers),
        ip("198.51.100.4")
    );
}

#[test]
fn trusted_proxy_yields_the_rightmost_untrusted_hop_not_client_supplied_entries() {
    let trusted = proxies(&["10.0.0.0/8", "127.0.0.1"]);
    // The client forged the leftmost entry; the edge proxy (10.0.0.2) appended
    // the real client address, and the local proxy appended the edge proxy.
    let headers = forwarded(&[b"192.0.2.66, 203.0.113.9", b"10.0.0.2"]);
    assert_eq!(
        trusted.client_ip(ip("127.0.0.1"), &headers),
        ip("203.0.113.9")
    );
    let single = forwarded(&[b"203.0.113.9:51234"]);
    assert_eq!(
        trusted.client_ip(ip("127.0.0.1"), &single),
        ip("203.0.113.9")
    );
    let v6 = forwarded(&[b"[2001:db8::7]:443"]);
    assert_eq!(
        trusted.client_ip(ip("::ffff:127.0.0.1"), &v6),
        ip("2001:db8::7")
    );
}

#[test]
fn missing_or_malformed_forwarded_hops_fall_back_to_the_nearest_trusted_proxy() {
    let trusted = proxies(&["10.0.0.0/8"]);
    assert_eq!(
        trusted.client_ip(ip("10.0.0.1"), &HeaderMap::new()),
        ip("10.0.0.1")
    );
    let garbage = forwarded(&[b"203.0.113.9, not-an-ip, 10.0.0.3"]);
    assert_eq!(trusted.client_ip(ip("10.0.0.1"), &garbage), ip("10.0.0.3"));
    let opaque = forwarded(&[b"203.0.113.9", b"\xff"]);
    assert_eq!(trusted.client_ip(ip("10.0.0.1"), &opaque), ip("10.0.0.1"));
    let all_trusted = forwarded(&[b"10.0.0.9, 10.0.0.3"]);
    assert_eq!(
        trusted.client_ip(ip("10.0.0.1"), &all_trusted),
        ip("10.0.0.9")
    );
}

#[test]
fn trusted_proxy_entries_accept_cidrs_and_single_addresses_only() {
    let range = parse_trusted_proxy(" 10.1.2.3/8 ").unwrap();
    assert_eq!(range, "10.1.2.3/8".parse::<IpNet>().unwrap());
    assert_eq!(
        parse_trusted_proxy("::1").unwrap(),
        "::1/128".parse::<IpNet>().unwrap()
    );
    assert!(parse_trusted_proxy("proxy.example").is_err());
    assert!(parse_trusted_proxy("10.0.0.0/33").is_err());
    let trusted = TrustedProxies::new(vec![range]);
    assert!(trusted.contains(ip("10.200.0.1")));
    assert!(!trusted.contains(ip("11.0.0.1")));
}

/// Regression: a client-written non-UTF-8 byte in `X-Forwarded-For` used to
/// discard the whole header value, including the hop a trusted proxy appended
/// to it, so the client was charged to the proxy's address instead of its own.
#[test]
fn opaque_client_written_hop_does_not_hide_the_hop_its_proxy_appended() {
    let trusted = proxies(&["127.0.0.1"]);
    // nginx's `$proxy_add_x_forwarded_for` appends to the client's own value.
    let appended = forwarded(&[b"\xff\xfe, 203.0.113.9"]);
    assert_eq!(
        trusted.client_ip(ip("127.0.0.1"), &appended),
        ip("203.0.113.9")
    );
    // Only the opaque hop itself is malformed: the walk still stops there.
    let behind_two = proxies(&["127.0.0.1", "10.0.0.0/8"]);
    let opaque_middle = forwarded(&[b"203.0.113.9, \xff, 10.0.0.3"]);
    assert_eq!(
        behind_two.client_ip(ip("127.0.0.1"), &opaque_middle),
        ip("10.0.0.3")
    );
}
