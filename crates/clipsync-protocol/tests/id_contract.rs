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
