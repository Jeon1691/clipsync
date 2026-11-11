#![no_main]

use clipsync_protocol::{decode_chunk_frame, encode_chunk_frame, ControlMessage};
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
        }
    }
});
