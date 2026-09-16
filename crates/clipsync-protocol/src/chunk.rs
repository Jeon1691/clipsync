use crate::ids::TransferId;
use crate::{ProtocolError, Result, FRAME_HARD_CAP};

pub const CHUNK_MAGIC: &[u8; 4] = b"CSY1";
pub const CHUNK_HEADER_LEN: usize = 34;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkFrame {
    pub transfer_id: TransferId,
    pub chunk_index: u32,
    pub chunk_count: u32,
    pub ciphertext: Vec<u8>,
}

pub fn encode_chunk_frame(frame: &ChunkFrame) -> Result<Vec<u8>> {
    let total = CHUNK_HEADER_LEN + frame.ciphertext.len();
    if total > FRAME_HARD_CAP {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(CHUNK_MAGIC);
    out.push(1); // version
    out.push(2); // frame_type = chunk
    out.extend_from_slice(&frame.transfer_id.to_bytes());
    out.extend_from_slice(&frame.chunk_index.to_be_bytes());
    out.extend_from_slice(&frame.chunk_count.to_be_bytes());
    out.extend_from_slice(&(frame.ciphertext.len() as u32).to_be_bytes());
    out.extend_from_slice(&frame.ciphertext);
    Ok(out)
}

pub fn decode_chunk_frame(buf: &[u8]) -> Result<ChunkFrame> {
    if buf.len() > FRAME_HARD_CAP {
        return Err(ProtocolError::FrameTooLarge);
    }
    if buf.len() < CHUNK_HEADER_LEN {
        return Err(ProtocolError::Chunk("truncated header"));
    }
    if &buf[0..4] != CHUNK_MAGIC {
        return Err(ProtocolError::Chunk("bad magic"));
    }
    if buf[4] != 1 {
        return Err(ProtocolError::Chunk("unsupported version"));
    }
    if buf[5] != 2 {
        return Err(ProtocolError::Chunk("not a chunk frame"));
    }
    let mut id_bytes = [0u8; 16];
    id_bytes.copy_from_slice(&buf[6..22]);
    let chunk_index = u32::from_be_bytes(buf[22..26].try_into().unwrap());
    let chunk_count = u32::from_be_bytes(buf[26..30].try_into().unwrap());
    let ct_len = u32::from_be_bytes(buf[30..34].try_into().unwrap()) as usize;
    if buf.len() != CHUNK_HEADER_LEN + ct_len {
        return Err(ProtocolError::Chunk("length mismatch"));
    }
    Ok(ChunkFrame {
        transfer_id: TransferId::from_bytes(id_bytes),
        chunk_index,
        chunk_count,
        ciphertext: buf[CHUNK_HEADER_LEN..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_chunk() {
        let frame = ChunkFrame {
            transfer_id: TransferId::new(),
            chunk_index: 3,
            chunk_count: 8,
            ciphertext: vec![1, 2, 3, 4, 5],
        };
        let encoded = encode_chunk_frame(&frame).unwrap();
        let decoded = decode_chunk_frame(&encoded).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn rejects_oversized() {
        let frame = ChunkFrame {
            transfer_id: TransferId::new(),
            chunk_index: 0,
            chunk_count: 1,
            ciphertext: vec![0; FRAME_HARD_CAP],
        };
        assert!(matches!(
            encode_chunk_frame(&frame),
            Err(ProtocolError::FrameTooLarge)
        ));
    }
}
