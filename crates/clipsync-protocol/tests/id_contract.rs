// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_protocol::{DeviceId, MessageId, PairingSessionId, RoomId, TransferId};

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
