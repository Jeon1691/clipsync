// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Clients must not send pairing codes or device tokens to a relay over
//! plaintext unless the relay is on this machine or the user opted in.

use clipsync_transport::{
    relay_url_security, RelayConnection, RelaySecurity, RelayUrlError, TransportError,
    ALLOW_INSECURE_RELAY_ENV,
};

#[test]
fn tls_relay_urls_are_accepted_without_the_opt_in() {
    for url in [
        "https://relay.example.test",
        "https://relay.example.test:8443/base/",
        "wss://relay.example.test/v1/ws",
        "HTTPS://Relay.Example.Test",
        "https://203.0.113.7",
        "wss://[2001:db8::1]:7600/v1/ws",
    ] {
        assert_eq!(
            relay_url_security(url, false),
            Ok(RelaySecurity::Tls),
            "{url}"
        );
        assert_eq!(
            relay_url_security(url, true),
            Ok(RelaySecurity::Tls),
            "{url}"
        );
    }
}

#[test]
fn plaintext_to_loopback_hosts_is_accepted_without_the_opt_in() {
    for url in [
        "http://127.0.0.1:7600",
        "http://127.255.255.254/",
        "ws://127.0.0.1:7600/v1/ws",
        "http://[::1]:7600",
        "ws://[::1]/v1/ws",
        "http://localhost:7600",
        "ws://LOCALHOST/v1/ws",
    ] {
        assert_eq!(
            relay_url_security(url, false),
            Ok(RelaySecurity::Loopback),
            "{url}"
        );
    }
}

#[test]
fn plaintext_to_non_loopback_hosts_is_rejected_with_an_actionable_error() {
    for (url, scheme, host) in [
        ("http://relay.example.test", "http", "relay.example.test"),
        ("ws://relay.example.test/v1/ws", "ws", "relay.example.test"),
        ("http://192.168.1.20:7600", "http", "192.168.1.20"),
        ("http://10.0.0.1", "http", "10.0.0.1"),
        ("http://0.0.0.0:7600", "http", "0.0.0.0"),
        ("http://[2001:db8::1]:7600", "http", "[2001:db8::1]"),
        ("http://[::ffff:127.0.0.1]", "http", "[::ffff:7f00:1]"),
        (
            "http://localhost.example.test",
            "http",
            "localhost.example.test",
        ),
        (
            "http://127.0.0.1.example.test",
            "http",
            "127.0.0.1.example.test",
        ),
    ] {
        let err = relay_url_security(url, false).unwrap_err();
        assert_eq!(
            err,
            RelayUrlError::Plaintext {
                scheme: scheme.into(),
                host: host.into(),
            },
            "{url}"
        );
        let message = err.to_string();
        for hint in ["https://", ALLOW_INSECURE_RELAY_ENV, "loopback", host] {
            assert!(message.contains(hint), "{url}: {message} lacks {hint}");
        }
    }
}

#[test]
fn the_explicit_opt_in_allows_plaintext_to_non_loopback_hosts() {
    for url in ["http://relay.example.test", "ws://192.168.1.20:7600/v1/ws"] {
        assert_eq!(
            relay_url_security(url, true),
            Ok(RelaySecurity::InsecureOptIn),
            "{url}"
        );
    }
    assert_eq!(
        relay_url_security("http://127.0.0.1:7600", true),
        Ok(RelaySecurity::Loopback)
    );
}

#[test]
fn scheme_less_and_unsupported_relay_urls_are_rejected_even_for_loopback() {
    for url in [
        "relay.example.test",
        "relay.example.test:7600",
        "localhost:7600",
        "127.0.0.1:7600",
        "//relay.example.test",
        "",
    ] {
        for allow_insecure in [false, true] {
            let err = relay_url_security(url, allow_insecure).unwrap_err();
            assert_eq!(err, RelayUrlError::MissingScheme, "{url}");
            assert!(err.to_string().contains("https://"), "{err}");
        }
    }
    for (url, scheme) in [
        ("ftp://relay.example.test", "ftp"),
        ("file:///tmp/relay", "file"),
    ] {
        assert_eq!(
            relay_url_security(url, true),
            Err(RelayUrlError::UnsupportedScheme {
                scheme: scheme.into()
            }),
            "{url}"
        );
    }
    assert_eq!(
        relay_url_security("https://", false),
        Err(RelayUrlError::Invalid(url::ParseError::EmptyHost))
    );
}

#[test]
fn non_canonical_hosts_are_rejected_when_the_url_is_checked() {
    // `url` normalizes these to the canonical host, but the WebSocket client
    // dials the raw text; refusing them up front keeps `config set` from
    // saving a URL that every later connect would reject.
    for (url, written, canonical) in [
        ("http://0x7f.1:7600", "0x7f.1", "127.0.0.1"),
        ("http://127.0.0.1.:7600", "127.0.0.1.", "127.0.0.1"),
        ("http://2130706433", "2130706433", "127.0.0.1"),
        (
            "http://[0:0:0:0:0:0:0:1]:7600",
            "[0:0:0:0:0:0:0:1]",
            "[::1]",
        ),
        ("ws://[0::1]/v1/ws", "[0::1]", "[::1]"),
        ("https://0x7f.1", "0x7f.1", "127.0.0.1"),
        (
            "wss://[2001:0db8::1]/v1/ws",
            "[2001:0db8::1]",
            "[2001:db8::1]",
        ),
    ] {
        for allow_insecure in [false, true] {
            let err = relay_url_security(url, allow_insecure).unwrap_err();
            assert_eq!(
                err,
                RelayUrlError::NonCanonicalHost {
                    written: written.into(),
                    canonical: canonical.into(),
                },
                "{url}"
            );
            assert!(err.to_string().contains(canonical), "{err}");
        }
    }
}

#[test]
fn the_opt_in_is_only_enabled_by_its_documented_value() {
    assert_eq!(ALLOW_INSECURE_RELAY_ENV, "CLIPSYNC_ALLOW_INSECURE_RELAY");
    assert!(clipsync_transport::insecure_relay_opt_in(Some("1")));
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("yes"),
        Some(" 1"),
    ] {
        assert!(
            !clipsync_transport::insecure_relay_opt_in(value),
            "{value:?}"
        );
    }
}

#[tokio::test]
async fn connect_refuses_plaintext_before_opening_a_socket() {
    // Only this test reads the process environment; clear an ambient opt-in so
    // the result does not depend on the shell that runs the suite.
    std::env::remove_var(ALLOW_INSECURE_RELAY_ENV);
    // The documentation address block never routes; a policy failure must be
    // returned before any TCP attempt could leak the request.
    let err = match RelayConnection::connect("ws://192.0.2.1:7600/v1/ws").await {
        Ok(_) => panic!("plaintext relay connection was allowed"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            TransportError::RelayUrl(RelayUrlError::Plaintext { .. })
        ),
        "{err}"
    );
    // Both `host:port/...` (an unknown "scheme") and a bare host are refused
    // with the same actionable error.
    let err = match RelayConnection::connect("localhost:7600/v1/ws").await {
        Ok(_) => panic!("scheme-less relay connection was allowed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, TransportError::RelayUrl(RelayUrlError::MissingScheme)),
        "{err}"
    );
    let err = match RelayConnection::connect("relay.example.test/v1/ws").await {
        Ok(_) => panic!("scheme-less relay connection was allowed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, TransportError::RelayUrl(RelayUrlError::MissingScheme)),
        "{err}"
    );
}

#[tokio::test]
async fn connect_dials_only_the_host_the_policy_classified() {
    // `url` reads these as 127.0.0.1, but the socket layer would resolve the
    // raw text, possibly via DNS; they must not borrow the loopback exemption.
    for (url, written) in [
        ("ws://127.0.0.1.:7600/v1/ws", "127.0.0.1."),
        ("ws://0x7f.1:7600/v1/ws", "0x7f.1"),
    ] {
        let err = match RelayConnection::connect(url).await {
            Ok(_) => panic!("{url} was dialed"),
            Err(err) => err,
        };
        assert!(
            matches!(
                &err,
                TransportError::RelayUrl(RelayUrlError::NonCanonicalHost { written: w, canonical })
                    if w == written && canonical == "127.0.0.1"
            ),
            "{url}: {err}"
        );
    }
}
