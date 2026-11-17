// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_transport::{http_to_ws, join_ws};

#[test]
fn relay_urls_preserve_transport_security_and_join_one_path_separator() {
    for (base, expected) in [
        ("https://relay.example.test", "wss://relay.example.test"),
        ("http://127.0.0.1:7600", "ws://127.0.0.1:7600"),
        ("wss://relay.example.test", "wss://relay.example.test"),
        ("ws://127.0.0.1:7600", "ws://127.0.0.1:7600"),
    ] {
        assert_eq!(http_to_ws(base), expected);
        assert_eq!(
            join_ws(&format!("{base}/"), "/v1/ws"),
            format!("{expected}/v1/ws")
        );
        assert_eq!(join_ws(base, "v1/ws"), format!("{expected}/v1/ws"));
    }
}

#[test]
fn scheme_less_relay_urls_are_never_downgraded_to_plaintext_websockets() {
    // Inventing `ws://` would silently send credentials in clear; leave the
    // input unchanged so connection-time validation rejects it.
    for base in ["localhost:7600", "relay.example.test"] {
        assert_eq!(http_to_ws(base), base);
        assert_eq!(join_ws(base, "/v1/ws"), format!("{base}/v1/ws"));
    }
}
