#![no_main]
// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT

use clipsync_protocol::{
    decode_chunk_frame, encode_chunk_frame, envelope_aad, ControlMessage, EnvelopeFrame,
    RoutingHeader,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(frame) = decode_chunk_frame(data) {
        let encoded = encode_chunk_frame(&frame).expect("decoded frame must encode");
        assert_eq!(encoded, data);
    }

    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(message) = ControlMessage::decode(text) {
            let encoded = message.encode().expect("decoded message must encode");
            let decoded = ControlMessage::decode(&encoded).expect("encoded message must decode");
            assert_eq!(message, decoded);
            if let Some(routing) = routing_of(&message) {
                check_aad(routing);
            }
        }
    }
});

fn routing_of(message: &ControlMessage) -> Option<&RoutingHeader> {
    match message {
        ControlMessage::TextEnvelope { routing, .. } | ControlMessage::Manifest { routing, .. } => {
            Some(routing)
        }
        ControlMessage::HelloOk {
            queued_text: Some(queued),
            ..
        } => Some(&queued.routing),
        _ => None,
    }
}

/// Untrusted headers must yield deterministic AAD (or a version error), never panic.
fn check_aad(routing: &RoutingHeader) {
    for frame in [
        EnvelopeFrame::Text,
        EnvelopeFrame::Manifest,
        EnvelopeFrame::Chunk {
            index: routing.seq as u32,
            count: u32::MAX,
        },
    ] {
        let first = envelope_aad(routing, frame).ok();
        assert_eq!(first, envelope_aad(routing, frame).ok());
    }
}
