// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! AEAD associated data for text envelopes, manifests, and chunk frames.
//!
//! Envelope version 1 authenticated only the message or transfer ID. Version 2
//! authenticates a canonical encoding of the whole [`RoutingHeader`], so a relay
//! cannot alter room, sender, recipient, timestamps, item kind, or sequence
//! number without the receiver's AEAD check failing.

use crate::messages::{ItemKind, Recipient, RoutingHeader};
use crate::{ProtocolError, Result};

/// Legacy envelope: AAD covers only the message ID (text) or transfer ID.
pub const ENVELOPE_VERSION_LEGACY: u16 = 1;
/// Authenticated envelope: AAD covers the canonical full routing header.
pub const ENVELOPE_VERSION_AUTHENTICATED: u16 = 2;

/// Domain separator so v2 AAD can never collide with another AAD format.
pub const ENVELOPE_AAD_LABEL: &[u8] = b"clipsync-envelope-v2";

// Canonical one-byte tags. Values are part of the v2 wire contract.
const RECIPIENT_BROADCAST: u8 = 0;
const RECIPIENT_DEVICE: u8 = 1;
const KIND_TEXT: u8 = 0;
const KIND_IMAGE: u8 = 1;
const KIND_FILES: u8 = 2;
const FRAME_TEXT: u8 = 0;
const FRAME_MANIFEST: u8 = 1;
const FRAME_CHUNK: u8 = 2;

/// Which encrypted object of an envelope the AAD is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeFrame {
    Text,
    Manifest,
    Chunk { index: u32, count: u32 },
}

/// AAD for `frame` under the envelope version named in `routing.version`.
pub fn envelope_aad(routing: &RoutingHeader, frame: EnvelopeFrame) -> Result<Vec<u8>> {
    match routing.version {
        ENVELOPE_VERSION_LEGACY => Ok(legacy_aad(routing, frame)),
        ENVELOPE_VERSION_AUTHENTICATED => Ok(authenticated_aad(routing, frame)),
        other => Err(ProtocolError::Version(other)),
    }
}

fn legacy_aad(routing: &RoutingHeader, frame: EnvelopeFrame) -> Vec<u8> {
    match frame {
        EnvelopeFrame::Text => format!("{}:text", routing.message_id).into_bytes(),
        EnvelopeFrame::Manifest => format!("{}:manifest", routing.transfer_id).into_bytes(),
        EnvelopeFrame::Chunk { index, .. } => {
            let mut aad = routing.transfer_id.as_str().as_bytes().to_vec();
            aad.extend_from_slice(&index.to_be_bytes());
            aad
        }
    }
}

fn authenticated_aad(routing: &RoutingHeader, frame: EnvelopeFrame) -> Vec<u8> {
    let mut aad = ENVELOPE_AAD_LABEL.to_vec();
    aad.extend_from_slice(&routing.version.to_be_bytes());
    put_field(&mut aad, routing.message_id.as_str());
    put_field(&mut aad, routing.transfer_id.as_str());
    put_field(&mut aad, routing.room_id.as_str());
    put_field(&mut aad, routing.sender_device_id.as_str());
    match &routing.recipient {
        Recipient::Broadcast => aad.push(RECIPIENT_BROADCAST),
        Recipient::Device(device) => {
            aad.push(RECIPIENT_DEVICE);
            put_field(&mut aad, device.as_str());
        }
    }
    for at in [routing.sent_at, routing.expires_at] {
        aad.extend_from_slice(&at.unix_timestamp().to_be_bytes());
        aad.extend_from_slice(&at.subsec_nanos().to_be_bytes());
    }
    aad.push(match routing.item_kind {
        ItemKind::Text => KIND_TEXT,
        ItemKind::Image => KIND_IMAGE,
        ItemKind::Files => KIND_FILES,
    });
    aad.extend_from_slice(&routing.seq.to_be_bytes());
    match frame {
        EnvelopeFrame::Text => aad.push(FRAME_TEXT),
        EnvelopeFrame::Manifest => aad.push(FRAME_MANIFEST),
        EnvelopeFrame::Chunk { index, count } => {
            aad.push(FRAME_CHUNK);
            aad.extend_from_slice(&index.to_be_bytes());
            aad.extend_from_slice(&count.to_be_bytes());
        }
    }
    aad
}

/// Length-prefixed so adjacent variable-length IDs cannot shift boundaries.
fn put_field(aad: &mut Vec<u8>, value: &str) {
    aad.extend_from_slice(&(value.len() as u64).to_be_bytes());
    aad.extend_from_slice(value.as_bytes());
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::{DeviceId, MessageId, RoomId, Timestamp, TransferId};
    use time::Duration;

    fn header(version: u16) -> RoutingHeader {
        let sent_at = Timestamp::from_unix(1_700_000_000, 123_456_789).unwrap();
        RoutingHeader {
            version,
            message_id: MessageId::from("message"),
            transfer_id: TransferId::from("transfer"),
            room_id: RoomId::from("room"),
            sender_device_id: DeviceId::from("sender"),
            recipient: Recipient::Broadcast,
            sent_at,
            expires_at: sent_at + Duration::seconds(60),
            item_kind: ItemKind::Text,
            seq: 7,
        }
    }

    #[test]
    fn legacy_aad_matches_the_v1_wire_contract() {
        let routing = header(ENVELOPE_VERSION_LEGACY);
        assert_eq!(
            envelope_aad(&routing, EnvelopeFrame::Text).unwrap(),
            b"message:text"
        );
        assert_eq!(
            envelope_aad(&routing, EnvelopeFrame::Manifest).unwrap(),
            b"transfer:manifest"
        );
        assert_eq!(
            envelope_aad(&routing, EnvelopeFrame::Chunk { index: 2, count: 9 }).unwrap(),
            b"transfer\0\0\0\x02"
        );
    }

    #[test]
    fn authenticated_aad_changes_with_every_routing_field_and_frame() {
        let base = header(ENVELOPE_VERSION_AUTHENTICATED);
        let reference = envelope_aad(&base, EnvelopeFrame::Text).unwrap();
        assert!(reference.starts_with(ENVELOPE_AAD_LABEL));
        let mut variants: Vec<RoutingHeader> = Vec::new();
        let mut push = |change: &dyn Fn(&mut RoutingHeader)| {
            let mut routing = base.clone();
            change(&mut routing);
            variants.push(routing);
        };
        push(&|r| r.message_id = MessageId::from("message2"));
        push(&|r| r.transfer_id = TransferId::from("transfer2"));
        push(&|r| r.room_id = RoomId::from("room2"));
        push(&|r| r.sender_device_id = DeviceId::from("sender2"));
        push(&|r| r.recipient = Recipient::Device(DeviceId::from("sender")));
        push(&|r| r.sent_at += Duration::nanoseconds(1));
        push(&|r| r.expires_at += Duration::seconds(1));
        push(&|r| r.item_kind = ItemKind::Image);
        push(&|r| r.item_kind = ItemKind::Files);
        push(&|r| r.seq += 1);
        // Shifting a byte between adjacent IDs must not produce the same AAD.
        push(&|r| {
            r.message_id = MessageId::from("messaget");
            r.transfer_id = TransferId::from("ransfer");
        });
        for routing in &variants {
            assert_ne!(
                envelope_aad(routing, EnvelopeFrame::Text).unwrap(),
                reference,
                "{routing:?}"
            );
        }
        let frames = [
            EnvelopeFrame::Text,
            EnvelopeFrame::Manifest,
            EnvelopeFrame::Chunk { index: 0, count: 1 },
            EnvelopeFrame::Chunk { index: 1, count: 1 },
            EnvelopeFrame::Chunk { index: 0, count: 2 },
        ];
        let encoded: std::collections::HashSet<_> = frames
            .iter()
            .map(|frame| envelope_aad(&base, *frame).unwrap())
            .collect();
        assert_eq!(encoded.len(), frames.len());
        assert_eq!(
            envelope_aad(&base, EnvelopeFrame::Text).unwrap(),
            reference,
            "canonical encoding must be deterministic"
        );
    }

    #[test]
    fn authenticated_aad_survives_the_json_wire_roundtrip() {
        let routing = header(ENVELOPE_VERSION_AUTHENTICATED);
        let decoded: RoutingHeader =
            serde_json::from_str(&serde_json::to_string(&routing).unwrap()).unwrap();
        assert_eq!(
            envelope_aad(&decoded, EnvelopeFrame::Manifest).unwrap(),
            envelope_aad(&routing, EnvelopeFrame::Manifest).unwrap()
        );
    }

    #[test]
    fn unknown_envelope_versions_have_no_aad() {
        for version in [0, ENVELOPE_VERSION_AUTHENTICATED + 1] {
            assert!(matches!(
                envelope_aad(&header(version), EnvelopeFrame::Text),
                Err(ProtocolError::Version(v)) if v == version
            ));
        }
    }
}
