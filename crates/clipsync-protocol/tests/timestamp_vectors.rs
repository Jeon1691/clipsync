// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Golden vectors for every RFC 3339 timestamp the protocol puts on the wire.
//!
//! Released peers, relays and state files exchange these strings, and the v2
//! envelope AAD binds the decoded instant. The vectors pin what the current
//! implementation emits and accepts so a change of date-time library keeps the
//! wire format byte-identical. They go through the public serde types only, so
//! they do not depend on which library backs the timestamp fields.

use clipsync_protocol::{
    envelope_aad, ControlMessage, CreatePairingResponse, EnvelopeFrame, JoinPairingResponse,
    RoutingHeader,
};
use serde_json::{json, Value};

/// Any valid value for the timestamp field that a vector does not exercise.
const FILLER: &str = "2023-11-14T22:13:20Z";

fn header_json(sent_at: Value, expires_at: Value) -> Value {
    json!({
        "version": 2,
        "message_id": "message",
        "transfer_id": "transfer",
        "room_id": "room",
        "sender_device_id": "sender",
        "recipient": "broadcast",
        "sent_at": sent_at,
        "expires_at": expires_at,
        "item_kind": "text",
        "seq": 7,
    })
}

fn decode_header(sent_at: &str, expires_at: &str) -> Option<RoutingHeader> {
    serde_json::from_value(header_json(json!(sent_at), json!(expires_at))).ok()
}

/// The string the protocol re-emits for `input` in both header timestamp
/// fields, or `None` when the input is rejected.
fn reencode(input: &str) -> Option<String> {
    let sent = decode_header(input, FILLER).map(|header| {
        let value = serde_json::to_value(&header).unwrap();
        value["sent_at"].as_str().unwrap().to_owned()
    });
    let expires = decode_header(FILLER, input).map(|header| {
        let value = serde_json::to_value(&header).unwrap();
        value["expires_at"].as_str().unwrap().to_owned()
    });
    assert_eq!(sent, expires, "both header fields must agree for {input:?}");
    sent
}

fn check(vectors: &[(&str, Option<&str>)]) {
    let mismatches: Vec<String> = vectors
        .iter()
        .filter_map(|(input, expected)| {
            let actual = reencode(input);
            (actual.as_deref() != *expected)
                .then(|| format!("{input:?}: expected {expected:?}, got {actual:?}"))
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Canonical output: `Z`, and 0, 3, 6 or 9 fractional digits, whichever is the
/// shortest exact rendering of the nanoseconds.
#[test]
fn canonical_output_uses_z_and_grouped_fraction_digits() {
    check(&[
        ("2023-11-14T22:13:20Z", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T22:13:20.000Z", Some("2023-11-14T22:13:20Z")),
        (
            "2023-11-14T22:13:20.000000000Z",
            Some("2023-11-14T22:13:20Z"),
        ),
        ("2023-11-14T22:13:20.1Z", Some("2023-11-14T22:13:20.100Z")),
        ("2023-11-14T22:13:20.12Z", Some("2023-11-14T22:13:20.120Z")),
        ("2023-11-14T22:13:20.123Z", Some("2023-11-14T22:13:20.123Z")),
        (
            "2023-11-14T22:13:20.1234Z",
            Some("2023-11-14T22:13:20.123400Z"),
        ),
        (
            "2023-11-14T22:13:20.123456Z",
            Some("2023-11-14T22:13:20.123456Z"),
        ),
        (
            "2023-11-14T22:13:20.1234567Z",
            Some("2023-11-14T22:13:20.123456700Z"),
        ),
        (
            "2023-11-14T22:13:20.123456789Z",
            Some("2023-11-14T22:13:20.123456789Z"),
        ),
        (
            "2023-11-14T22:13:20.000001Z",
            Some("2023-11-14T22:13:20.000001Z"),
        ),
        (
            "2023-11-14T22:13:20.000000001Z",
            Some("2023-11-14T22:13:20.000000001Z"),
        ),
        (
            "2023-11-14T22:13:20.999999999Z",
            Some("2023-11-14T22:13:20.999999999Z"),
        ),
        // Digits past nanoseconds are dropped, not rounded.
        (
            "2023-11-14T22:13:20.1234567891Z",
            Some("2023-11-14T22:13:20.123456789Z"),
        ),
        (
            "2023-11-14T22:13:20.999999999999Z",
            Some("2023-11-14T22:13:20.999999999Z"),
        ),
        ("1970-01-01T00:00:00Z", Some("1970-01-01T00:00:00Z")),
        ("1969-12-31T23:59:59.5Z", Some("1969-12-31T23:59:59.500Z")),
        ("0000-01-01T00:00:00Z", Some("0000-01-01T00:00:00Z")),
        ("9999-12-31T23:59:59Z", Some("9999-12-31T23:59:59Z")),
        ("2024-02-29T12:00:00Z", Some("2024-02-29T12:00:00Z")),
    ]);
}

/// Accepted spellings other than the canonical one, all normalised to UTC `Z`.
#[test]
fn accepted_input_variants_normalise_to_canonical_utc() {
    check(&[
        // Offsets convert to UTC; a zero offset in any spelling is UTC.
        ("2023-11-14T22:13:20+00:00", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T22:13:20-00:00", Some("2023-11-14T22:13:20Z")),
        ("2023-11-15T07:13:20+09:00", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T16:43:20-05:30", Some("2023-11-14T22:13:20Z")),
        ("2023-11-15T07:13:20+0900", Some("2023-11-14T22:13:20Z")),
        ("2023-11-15T07:13:20+09 00", Some("2023-11-14T22:13:20Z")),
        ("2023-11-15T07:13:20+09::00", Some("2023-11-14T22:13:20Z")),
        (
            "2023-11-14T16:43:20\u{2212}05:30",
            Some("2023-11-14T22:13:20Z"),
        ),
        ("2023-11-15T22:12:20+23:59", Some("2023-11-14T22:13:20Z")),
        ("2023-11-13T22:14:20-23:59", Some("2023-11-14T22:13:20Z")),
        (
            "2023-11-15T07:13:20.5+09:00",
            Some("2023-11-14T22:13:20.500Z"),
        ),
        // An offset can move the instant across a year boundary.
        ("2024-01-01T08:59:59+09:00", Some("2023-12-31T23:59:59Z")),
        // Separator and designator case; `UTC` in any case.
        ("2023-11-14t22:13:20z", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14 22:13:20Z", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T22:13:20UTC", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T22:13:20utc", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T22:13:20 Z", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T22:13:20 +00:00", Some("2023-11-14T22:13:20Z")),
        (
            "2023-11-14T22:13:20.5 UTC",
            Some("2023-11-14T22:13:20.500Z"),
        ),
        // Fields need not be zero padded.
        ("2023-1-4T2:3:4Z", Some("2023-01-04T02:03:04Z")),
        ("923-11-14T22:13:20Z", Some("0923-11-14T22:13:20Z")),
        // Whitespace around the value and around the date and time separators.
        (" 2023-11-14T22:13:20Z", Some("2023-11-14T22:13:20Z")),
        ("2023-11-14T22:13:20Z \n", Some("2023-11-14T22:13:20Z")),
        (
            "2023 - 11 - 14T 22 : 13 : 20Z",
            Some("2023-11-14T22:13:20Z"),
        ),
        ("\u{a0}2023-11-14T22:13:20Z", Some("2023-11-14T22:13:20Z")),
        // A leap second (`:60`) at any minute survives the round trip.
        ("2016-12-31T23:59:60Z", Some("2016-12-31T23:59:60Z")),
        ("2016-12-31T23:59:60.25Z", Some("2016-12-31T23:59:60.250Z")),
        (
            "2016-12-31T23:59:60.999999999Z",
            Some("2016-12-31T23:59:60.999999999Z"),
        ),
        ("2023-11-14T22:13:60Z", Some("2023-11-14T22:13:60Z")),
        ("2017-01-01T08:59:60+09:00", Some("2016-12-31T23:59:60Z")),
    ]);
}

/// Years outside 0000-9999 carry an explicit sign; the representable range is
/// -262143-01-01 through +262142-12-31T23:59:59.999999999.
#[test]
fn extended_years_and_range_limits() {
    check(&[
        ("+2023-11-14T22:13:20Z", Some("2023-11-14T22:13:20Z")),
        ("+12023-01-01T00:00:00Z", Some("+12023-01-01T00:00:00Z")),
        (
            "+0000012023-01-01T00:00:00Z",
            Some("+12023-01-01T00:00:00Z"),
        ),
        ("-0001-12-31T23:59:59Z", Some("-0001-12-31T23:59:59Z")),
        ("-1-12-31T23:59:59Z", Some("-0001-12-31T23:59:59Z")),
        ("-10000-01-01T00:00:00Z", Some("-10000-01-01T00:00:00Z")),
        ("+10000-01-01T00:00:00Z", Some("+10000-01-01T00:00:00Z")),
        ("10000-01-01T00:00:00Z", None),
        (
            "+262142-12-31T23:59:59.999999999Z",
            Some("+262142-12-31T23:59:59.999999999Z"),
        ),
        ("-262143-01-01T00:00:00Z", Some("-262143-01-01T00:00:00Z")),
        ("+262143-01-01T00:00:00Z", None),
        ("-262144-12-31T23:59:59.999999999Z", None),
        // An offset that would move the instant outside the range.
        ("+262142-12-31T23:59:59-00:01", None),
        ("-262143-01-01T00:00:00+00:01", None),
        (
            "+262142-12-31T23:59:59+00:01",
            Some("+262142-12-31T23:58:59Z"),
        ),
        ("+99999999999-01-01T00:00:00Z", None),
        ("+99999999999999999999-01-01T00:00:00Z", None),
    ]);
}

/// Rejected inputs: malformed text, impossible fields and out-of-range offsets.
#[test]
fn malformed_and_out_of_range_inputs_are_rejected() {
    check(&[
        ("", None),
        (" ", None),
        ("not a timestamp", None),
        ("2023-11-14", None),
        ("2023-11-14T", None),
        ("2023-11-14T22:13Z", None),
        ("2023-11-14T22:13:20", None),
        ("2023-11-14T22:13:20.Z", None),
        ("2023-11-14T22:13:20,5Z", None),
        ("2023-11-14T22:13:20 .5Z", None),
        ("2023-11-14T22:13:20.5.5Z", None),
        ("2023-11-14T22:13:20ZZ", None),
        ("2023-11-14T22:13:20Z junk", None),
        ("2023-11-14T22:13:20+09", None),
        ("2023-11-14T22:13:20+9:00", None),
        ("2023-11-14T22:13:20+09:0", None),
        ("2023-11-14T22:13:20+09:60", None),
        ("2023-11-14T22:13:20+24:00", None),
        ("2023-11-14T22:13:20+99:59", None),
        ("2023-11-14T22:13:20GMT", None),
        ("2023-11-14X22:13:20Z", None),
        ("2023-11-14TT22:13:20Z", None),
        ("2023/11/14T22:13:20Z", None),
        ("20231114T221320Z", None),
        ("2023-011-14T22:13:20Z", None),
        ("2023-13-01T00:00:00Z", None),
        ("2023-00-10T00:00:00Z", None),
        ("2023-11-00T00:00:00Z", None),
        ("2023-11-31T00:00:00Z", None),
        ("2023-02-29T00:00:00Z", None),
        ("2100-02-29T00:00:00Z", None),
        ("2023-11-14T24:00:00Z", None),
        ("2023-11-14T23:60:00Z", None),
        ("2023-11-14T23:59:61Z", None),
        ("\u{ff12}\u{ff10}23-11-14T22:13:20Z", None),
        ("1700000000", None),
    ]);
}

/// Non-string JSON values are rejected even when they look like an instant.
#[test]
fn non_string_json_values_are_rejected() {
    for value in [
        json!(1_700_000_000),
        json!(1_700_000_000.5),
        json!(null),
        json!(true),
        json!(["2023-11-14T22:13:20Z"]),
        json!({"secs": 1_700_000_000}),
    ] {
        let decoded: Result<RoutingHeader, _> =
            serde_json::from_value(header_json(value.clone(), json!(FILLER)));
        assert!(decoded.is_err(), "{value}");
    }
}

/// A full header as released senders emit it, byte for byte.
#[test]
fn routing_header_wire_bytes_are_stable() {
    const WIRE: &str = concat!(
        r#"{"version":2,"message_id":"message","transfer_id":"transfer","room_id":"room","#,
        r#""sender_device_id":"sender","recipient":"broadcast","#,
        r#""sent_at":"2023-11-14T22:13:20.123456Z","expires_at":"2023-11-14T22:14:20.123456Z","#,
        r#""item_kind":"text","seq":7}"#
    );
    let header: RoutingHeader = serde_json::from_str(WIRE).unwrap();
    assert_eq!(serde_json::to_string(&header).unwrap(), WIRE);

    let message = ControlMessage::TextEnvelope {
        routing: header,
        nonce: "bm9uY2U".into(),
        ciphertext: "Y2lwaGVy".into(),
    };
    let encoded = message.encode().unwrap();
    assert_eq!(
        encoded,
        format!(
            r#"{{"type":"text_envelope","routing":{WIRE},"nonce":"bm9uY2U","ciphertext":"Y2lwaGVy"}}"#
        )
    );
    assert_eq!(ControlMessage::decode(&encoded).unwrap(), message);
}

/// The relay's pairing responses round-trip their `expires_at` unchanged.
#[test]
fn pairing_response_expiry_wire_bytes_are_stable() {
    const PEER: &str =
        r#"{"device_id":"peer","device_name":"Peer","os":"linux","role":"owner","online":true}"#;
    let create = concat!(
        r#"{"room_id":"room","pairing_code":"000000","pairing_token":"token","#,
        r#""expires_at":"2023-11-14T22:23:20.123456789Z","ws_path":"/v1/ws"}"#
    );
    let decoded: CreatePairingResponse = serde_json::from_str(create).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), create);

    let join = format!(
        concat!(
            r#"{{"pairing_session_id":"session","room_id":"room","pairing_token":"token","#,
            r#""peer":{},"expires_at":"2023-11-14T22:23:20.500Z","ws_path":"/v1/ws"}}"#
        ),
        PEER
    );
    let decoded: JoinPairingResponse = serde_json::from_str(&join).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), join);

    // A non-canonical spelling from the relay is normalised on re-encode.
    let offset = create.replace(
        "2023-11-14T22:23:20.123456789Z",
        "2023-11-15T07:23:20.1+09:00",
    );
    let decoded: CreatePairingResponse = serde_json::from_str(&offset).unwrap();
    assert_eq!(
        serde_json::to_value(&decoded).unwrap()["expires_at"],
        "2023-11-14T22:23:20.100Z"
    );
    let bad = create.replace("2023-11-14T22:23:20.123456789Z", "2023-11-14");
    assert!(serde_json::from_str::<CreatePairingResponse>(&bad).is_err());
}

/// v2 AAD binds each timestamp as big-endian `i64` Unix seconds and `u32`
/// nanoseconds. Pre-epoch instants floor the seconds; a leap second keeps the
/// `:59` second and carries 1e9 or more nanoseconds. The expected bytes were
/// computed independently of the implementation.
#[test]
fn authenticated_aad_timestamp_bytes_are_stable() {
    let vectors = [
        (
            "2023-11-14T22:13:20.123456789Z",
            "2023-11-14T22:14:20Z",
            "000000006553f100075bcd15000000006553f13c00000000",
        ),
        (
            "1969-12-31T23:59:59.5Z",
            "1970-01-01T00:00:00.000001Z",
            "ffffffffffffffff1dcd65000000000000000000000003e8",
        ),
        (
            "2016-12-31T23:59:60.25Z",
            "2017-01-01T00:00:59Z",
            "000000005868467f4a817c8000000000586846bb00000000",
        ),
        (
            "-262143-01-01T00:00:00Z",
            "+262142-12-31T23:59:59.999999999Z",
            "fffff86b730dee0000000000000007779a0a6b7f3b9ac9ff",
        ),
    ];
    const PREFIX: &str = concat!(
        "636c697073796e632d656e76656c6f70652d7632", // "clipsync-envelope-v2"
        "0002",                                     // version
        "0000000000000007",
        "6d657373616765", // message_id
        "0000000000000008",
        "7472616e73666572", // transfer_id
        "0000000000000004",
        "726f6f6d", // room_id
        "0000000000000006",
        "73656e646572", // sender_device_id
        "00",           // broadcast
    );
    // Item kind text, seq 7, text frame.
    const SUFFIX: &str = "00000000000000000700";
    for (sent_at, expires_at, timestamps) in vectors {
        let header = decode_header(sent_at, expires_at).expect(sent_at);
        let aad = envelope_aad(&header, EnvelopeFrame::Text).unwrap();
        let expected = format!("{PREFIX}{timestamps}{SUFFIX}");
        assert_eq!(hex(&aad), expected, "{sent_at} / {expires_at}");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
