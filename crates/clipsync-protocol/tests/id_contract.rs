// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_protocol::{
    validate_id, DeviceId, IdError, MessageId, PairingSessionId, RoomId, TransferId, ID_MAX_LEN,
};

#[test]
fn wire_identifiers_keep_display_and_owned_string_conversion_consistent() {
    macro_rules! check_id {
        ($id:ty) => {{
            let generated = <$id>::default();
            let owned = <$id>::from(generated.to_string());
            assert_eq!(owned, generated);
            assert_eq!(
                serde_json::to_value(&generated).unwrap(),
                serde_json::Value::String(generated.to_string())
            );
            assert_eq!(owned.as_str(), generated.to_string());
            assert_eq!(
                <$id>::from("compatibility-id").to_string(),
                "compatibility-id"
            );
        }};
    }
    check_id!(DeviceId);
    check_id!(RoomId);
    check_id!(MessageId);
    check_id!(TransferId);
    check_id!(PairingSessionId);
}

#[test]
fn transfer_wire_ids_roundtrip_ulids_and_keep_the_legacy_byte_fallback() {
    let id = TransferId::new();
    assert_eq!(TransferId::from_bytes(id.to_bytes()), id);
    let legacy = TransferId::from("legacy-transfer-name-that-exceeds-sixteen-bytes");
    assert_eq!(&legacy.to_bytes(), b"legacy-transfer-");
    let short = TransferId::from("old").to_bytes();
    assert_eq!(&short[..3], b"old");
    assert!(short[3..].iter().all(|byte| *byte == 0));
}

#[test]
fn transfer_id_wire_vectors_preserve_big_endian_bytes_and_json_strings() {
    // These literal pairs come from 128-bit big-endian integers encoded with
    // Crockford's Base32 alphabet, calculated independently of the ULID crate.
    let vectors = [
        (
            [0; 16],
            "00000000000000000000000000",
            r#""00000000000000000000000000""#,
        ),
        (
            [
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ],
            "00041061050R3GG28A1C60T3GF",
            r#""00041061050R3GG28A1C60T3GF""#,
        ),
        (
            [
                0x01, 0x56, 0x3e, 0x3a, 0xb5, 0xd3, 0xd6, 0x76, 0x4c, 0x61, 0xef, 0xb9, 0x93, 0x02,
                0xbd, 0x5b,
            ],
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            r#""01ARZ3NDEKTSV4RRFFQ69G5FAV""#,
        ),
        (
            [0xff; 16],
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZ",
            r#""7ZZZZZZZZZZZZZZZZZZZZZZZZZ""#,
        ),
    ];

    for (bytes, encoded, json) in vectors {
        let id = TransferId::from_bytes(bytes);
        assert_eq!(id.as_str(), encoded);
        assert_eq!(TransferId::from(encoded).to_bytes(), bytes);
        assert_eq!(serde_json::to_string(&id).unwrap(), json);
        let decoded: TransferId = serde_json::from_str(json).unwrap();
        assert_eq!(decoded.as_str(), encoded);
        assert_eq!(decoded.to_bytes(), bytes);
    }
}

/// Longest identifier the protocol accepts. Pinned here because the client
/// check that relay-supplied room and device IDs are plain identifiers uses
/// the same bound, and the two must not drift apart.
const SHARED_ID_MAX_LEN: usize = 64;

#[test]
fn wire_identifiers_reject_path_and_control_characters_at_decode() {
    let overlong = "A".repeat(SHARED_ID_MAX_LEN + 1);
    let longest = "A".repeat(SHARED_ID_MAX_LEN);
    let rejected = [
        "",
        "a/b",
        "../escape",
        "a\\b",
        "..\\escape",
        "..",
        ".",
        "a..b",
        ".hidden",
        "trailing.",
        "nul\0byte",
        "line\nbreak",
        "carriage\rreturn",
        "esc\u{1b}[2J",
        "tab\there",
        "del\u{7f}",
        "space here",
        "colon:stream",
        "percent%2F",
        "\u{202e}bidi",
        "장치",
        overlong.as_str(),
    ];
    let accepted = [
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "7ZZZZZZZZZZZZZZZZZZZZZZZZZ",
        "dev-a",
        "compatibility-id",
        "legacy-transfer-name-that-exceeds-sixteen-bytes",
        "old",
        "chunk",
        "replacement-live",
        "desktop-room",
        "snake_case_id",
        "dotted.name.v1",
        longest.as_str(),
    ];

    macro_rules! check_id {
        ($id:ty) => {{
            for raw in rejected {
                let json = serde_json::to_string(raw).unwrap();
                assert!(
                    serde_json::from_str::<$id>(&json).is_err(),
                    "{} accepted {:?}",
                    stringify!($id),
                    raw
                );
            }
            for raw in accepted {
                let json = serde_json::to_string(raw).unwrap();
                let decoded: $id = serde_json::from_str(&json).unwrap();
                assert_eq!(decoded.as_str(), raw);
            }
            let generated = <$id>::new();
            let json = serde_json::to_string(&generated).unwrap();
            assert_eq!(serde_json::from_str::<$id>(&json).unwrap(), generated);
        }};
    }
    check_id!(DeviceId);
    check_id!(RoomId);
    check_id!(MessageId);
    check_id!(TransferId);
    check_id!(PairingSessionId);
}

#[test]
fn control_messages_carrying_unsafe_identifiers_do_not_decode() {
    use clipsync_protocol::ControlMessage;
    let valid = r#"{"type":"peer_left","device_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV"}"#;
    assert!(ControlMessage::decode(valid).is_ok());
    for hostile in [
        r#"{"type":"peer_left","device_id":"../../etc/passwd"}"#,
        r#"{"type":"peer_left","device_id":"evil\u001b]0;title\u0007"}"#,
        r#"{"type":"peer_left","device_id":""}"#,
    ] {
        assert!(ControlMessage::decode(hostile).is_err(), "{hostile}");
    }
}

#[test]
fn parsed_and_rechecked_identifiers_follow_the_decode_rule_without_echoing_input() {
    assert_eq!(ID_MAX_LEN, SHARED_ID_MAX_LEN);
    assert_eq!(validate_id(""), Err(IdError::Empty));
    assert_eq!(
        validate_id(&"A".repeat(ID_MAX_LEN + 1)),
        Err(IdError::TooLong)
    );
    assert_eq!(validate_id("a/b"), Err(IdError::Character));
    assert_eq!(validate_id("a\0b"), Err(IdError::Character));
    for dotted in ["..", ".", ".a", "a.", "a..b"] {
        assert_eq!(validate_id(dotted), Err(IdError::Dot), "{dotted:?}");
    }
    assert_eq!(validate_id("a.b"), Ok(()));

    macro_rules! check_id {
        ($id:ty) => {{
            let generated = <$id>::new();
            assert_eq!(generated.validate(), Ok(()));
            let parsed: $id = generated.as_str().parse().unwrap();
            assert_eq!(parsed, generated);
            assert_eq!("../escape".parse::<$id>(), Err(IdError::Character));
            // `From` is the unchecked constructor; `validate` catches it later.
            assert_eq!(<$id>::from("../escape").validate(), Err(IdError::Character));
            let hostile = serde_json::to_string("evil\u{1b}]0;x\u{7}").unwrap();
            let error = serde_json::from_str::<$id>(&hostile)
                .unwrap_err()
                .to_string();
            assert!(
                !error.contains("evil") && !error.contains('\u{1b}'),
                "{error}"
            );
        }};
    }
    check_id!(DeviceId);
    check_id!(RoomId);
    check_id!(MessageId);
    check_id!(TransferId);
    check_id!(PairingSessionId);
}

#[test]
fn generated_and_byte_decoded_transfer_ids_always_pass_validation() {
    for bytes in [[0; 16], [0xff; 16], [0x5a; 16]] {
        assert_eq!(TransferId::from_bytes(bytes).validate(), Ok(()));
    }
}
