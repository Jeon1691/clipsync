// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! ClipSync wire protocol: routing headers, control messages, binary chunks, limits.

mod capabilities;
mod chunk;
mod envelope;
mod ids;
mod limits;
mod messages;

pub use capabilities::{HandshakeCapabilities, LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES};
pub use chunk::{decode_chunk_frame, encode_chunk_frame, ChunkFrame, CHUNK_MAGIC};
pub use envelope::{
    envelope_aad, EnvelopeFrame, ENVELOPE_AAD_LABEL, ENVELOPE_VERSION_AUTHENTICATED,
    ENVELOPE_VERSION_LEGACY,
};
pub use ids::{
    validate_id, DeviceId, IdError, MessageId, PairingSessionId, RoomId, TransferId, ID_MAX_LEN,
};
pub use limits::*;
pub use messages::*;

/// Hello/pairing protocol version checked by the relay. Envelope AAD is
/// versioned separately in `RoutingHeader.version`, so v2 envelopes work with
/// existing relays.
pub const PROTOCOL_VERSION: u16 = 1;
pub const PROTOCOL_NAME: &str = "clipsync-v1";
pub const PAIRING_TRANSCRIPT_LABEL: &str = "clipsync-pairing-v1";

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid chunk frame: {0}")]
    Chunk(&'static str),
    #[error("frame exceeds hard cap ({FRAME_HARD_CAP} bytes)")]
    FrameTooLarge,
    #[error("unsupported protocol version {0}")]
    Version(u16),
    #[error("invalid identifier: {0}")]
    Id(String),
    #[error("capabilities: {0}")]
    Capabilities(&'static str),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
