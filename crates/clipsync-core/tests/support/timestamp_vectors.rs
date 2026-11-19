// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Golden vectors for the receiver's clock checks on header timestamps.
//!
//! Every instant is spelled as the RFC 3339 string a peer or relay sends and
//! decoded through the protocol's serde types, so the vectors pin behaviour,
//! not a date-time library. The boundaries are `CLOCK_SKEW_SECS` (30 s) and
//! `HARD_MAX_TTL_SECS` (300 s); each case sits exactly on, or one nanosecond
//! past, one of them.

use super::*;
use crate::envelope::{check_header, sequence_floor};
use crate::EnvelopeRejection;
use clipsync_protocol::{CLOCK_SKEW_SECS, HARD_MAX_TTL_SECS};
use serde_json::json;

/// Skew and lifetime the vectors below were written against.
const VECTOR_SKEW_SECS: i64 = 30;
const VECTOR_MAX_TTL_SECS: u64 = 300;

fn instant<T: serde::de::DeserializeOwned>(text: &str) -> T {
    serde_json::from_value(json!(text)).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// (case, now, sent_at, expires_at, v1 outcome, v2 outcome); `None` = admitted.
type Case = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<EnvelopeRejection>,
    Option<EnvelopeRejection>,
);

#[test]
fn header_clock_checks_match_the_pinned_boundaries() {
    assert_eq!(CLOCK_SKEW_SECS, VECTOR_SKEW_SECS);
    assert_eq!(HARD_MAX_TTL_SECS, VECTOR_MAX_TTL_SECS);
    const NOW: &str = "2023-11-14T22:13:20Z";
    let cases: Vec<Case> = vec![
        (
            "sent_at exactly at now + skew",
            NOW,
            "2023-11-14T22:13:50Z",
            "2023-11-14T22:14:50Z",
            None,
            None,
        ),
        (
            "sent_at 1 ns past now + skew",
            NOW,
            "2023-11-14T22:13:50.000000001Z",
            "2023-11-14T22:14:50Z",
            None,
            Some(EnvelopeRejection::NotYetValid),
        ),
        (
            "sent_at in the future through an offset",
            NOW,
            "2023-11-15T07:13:50.000000001+09:00",
            "2023-11-14T22:14:50Z",
            None,
            Some(EnvelopeRejection::NotYetValid),
        ),
        (
            "expires_at exactly at now - skew",
            NOW,
            "2023-11-14T22:12:00Z",
            "2023-11-14T22:12:50Z",
            None,
            None,
        ),
        (
            "expires_at 1 ns before now - skew",
            NOW,
            "2023-11-14T22:12:00Z",
            "2023-11-14T22:12:49.999999999Z",
            Some(EnvelopeRejection::Expired),
            Some(EnvelopeRejection::Expired),
        ),
        (
            "lifetime exactly the hard maximum",
            NOW,
            "2023-11-14T22:13:00Z",
            "2023-11-14T22:18:00Z",
            None,
            None,
        ),
        (
            "lifetime 1 ns past the hard maximum",
            NOW,
            "2023-11-14T22:13:00Z",
            "2023-11-14T22:18:00.000000001Z",
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            "expires_at before sent_at",
            NOW,
            "2023-11-14T22:13:20Z",
            "2023-11-14T22:13:19Z",
            None,
            None,
        ),
        (
            "pre-epoch header against a pre-epoch clock",
            "1969-12-31T23:59:59.5Z",
            "1969-12-31T23:59:59Z",
            "1970-01-01T00:00:59Z",
            None,
            None,
        ),
        // A leap second moved past by whole seconds counts as its `:59`
        // second: `23:59:60.5 + 30 s` is `00:00:29.5`.
        (
            "leap-second expires_at exactly at now - skew",
            "2017-01-01T00:00:29.5Z",
            "2016-12-31T23:59:00Z",
            "2016-12-31T23:59:60.5Z",
            None,
            None,
        ),
        (
            "leap-second expires_at 1 ns before now - skew",
            "2017-01-01T00:00:29.500000001Z",
            "2016-12-31T23:59:00Z",
            "2016-12-31T23:59:60.5Z",
            Some(EnvelopeRejection::Expired),
            Some(EnvelopeRejection::Expired),
        ),
        // Lifetimes measured from or to a leap second count it once.
        (
            "lifetime from a leap second, at the maximum",
            "2017-01-01T00:00:00Z",
            "2016-12-31T23:59:60Z",
            "2017-01-01T00:05:00Z",
            None,
            None,
        ),
        (
            "lifetime from a leap second, 1 ns past the maximum",
            "2017-01-01T00:00:00Z",
            "2016-12-31T23:59:60Z",
            "2017-01-01T00:05:00.000000001Z",
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            "lifetime to a leap second, at the maximum",
            "2016-12-31T23:56:00Z",
            "2016-12-31T23:55:00Z",
            "2016-12-31T23:59:60Z",
            None,
            None,
        ),
        (
            "lifetime to a leap second, 1 ns past the maximum",
            "2016-12-31T23:56:00Z",
            "2016-12-31T23:54:59.999999999Z",
            "2016-12-31T23:59:60Z",
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            "leap-second sent_at exactly at now + skew",
            "2016-12-31T23:59:29.5Z",
            "2016-12-31T23:59:59.5Z",
            "2017-01-01T00:00:30Z",
            None,
            None,
        ),
        (
            "leap-second sent_at is later than its :59 second",
            "2016-12-31T23:59:29.5Z",
            "2016-12-31T23:59:60Z",
            "2017-01-01T00:00:30Z",
            None,
            Some(EnvelopeRejection::NotYetValid),
        ),
        // The extremes of the representable range (see the protocol vectors).
        (
            "expires_at at the representable maximum",
            NOW,
            NOW,
            "+262142-12-31T23:59:59.999999999Z",
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            "sent_at at the representable maximum",
            NOW,
            "+262142-12-31T23:59:59.999999999Z",
            "2023-11-14T22:14:20Z",
            None,
            Some(EnvelopeRejection::NotYetValid),
        ),
        (
            "sent_at at the representable minimum",
            NOW,
            "-262143-01-01T00:00:00Z",
            "2023-11-14T22:14:20Z",
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            "both at the representable minimum",
            NOW,
            "-262143-01-01T00:00:00Z",
            "-262143-01-01T00:00:00Z",
            Some(EnvelopeRejection::Expired),
            Some(EnvelopeRejection::Expired),
        ),
        (
            "both at the representable maximum",
            NOW,
            "+262142-12-31T23:59:59.999999999Z",
            "+262142-12-31T23:59:59.999999999Z",
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::NotYetValid),
        ),
    ];
    let room = RoomId::new();
    let peer = scripted_peer("http://127.0.0.1:1", room.clone());
    let record = peer.app.store.current_room().unwrap();
    let mut mismatches = Vec::new();
    for (case, now, sent_at, expires_at, v1, v2) in &cases {
        for (version, expected) in [
            (ENVELOPE_VERSION_LEGACY, v1),
            (ENVELOPE_VERSION_AUTHENTICATED, v2),
        ] {
            let routing = RoutingHeader {
                version,
                sent_at: instant(sent_at),
                expires_at: instant(expires_at),
                ..routing_from(&room, SCRIPT_SENDER, ItemKind::Text)
            };
            let outcome = check_header(
                &routing,
                &record,
                &peer.identity.device_id,
                None,
                instant(now),
            )
            .err();
            if outcome != *expected {
                mismatches.push(format!(
                    "v{version} {case}: expected {expected:?}, got {outcome:?}"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// The outgoing `seq` floor is the clock in Unix microseconds, truncated, and
/// zero before the epoch.
#[test]
fn sequence_floor_vectors() {
    for (now, floor) in [
        ("2023-11-14T22:13:20Z", 1_700_000_000_000_000),
        ("2023-11-14T22:13:20.123456789Z", 1_700_000_000_123_456),
        ("2023-11-14T22:13:20.000000999Z", 1_700_000_000_000_000),
        ("1970-01-01T00:00:00.000001Z", 1),
        ("1970-01-01T00:00:00Z", 0),
        ("1969-12-31T23:59:59.999999Z", 0),
        ("-262143-01-01T00:00:00Z", 0),
        ("2016-12-31T23:59:60.25Z", 1_483_228_800_250_000),
        (
            "+262142-12-31T23:59:59.999999999Z",
            8_210_266_876_799_999_999,
        ),
    ] {
        assert_eq!(sequence_floor(instant(now)), floor, "{now}");
    }
}
