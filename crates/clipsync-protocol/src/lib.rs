//! ClipSync wire protocol: routing headers, control messages, binary chunks, limits.

mod chunk;
mod ids;
mod limits;
mod messages;

pub use chunk::{decode_chunk_frame, encode_chunk_frame, ChunkFrame, CHUNK_MAGIC};
pub use ids::{DeviceId, MessageId, PairingSessionId, RoomId, TransferId};
pub use limits::*;
pub use messages::*;

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
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
