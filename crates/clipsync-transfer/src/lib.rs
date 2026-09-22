//! Chunked transfer, path sanitization, staging, and BLAKE3 commit.

mod sanitize;
mod staging;

pub use sanitize::safe_filename;
pub use staging::{prune_idle_transfers, IncomingTransfer, StagingArea};

use clipsync_crypto::{blake3_hex, derive_content_key, derive_nonce, open, seal};
use clipsync_protocol::{
    decode_chunk_frame, encode_chunk_frame, ChunkFrame, TransferId, CHUNK_SIZE, FRAME_HARD_CAP,
};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("{0}")]
    Message(String),
    #[error("protocol: {0}")]
    Protocol(#[from] clipsync_protocol::ProtocolError),
    #[error("crypto: {0}")]
    Crypto(#[from] clipsync_crypto::CryptoError),
    #[error("aead: {0}")]
    Aead(#[from] clipsync_crypto::AeadError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsafe filename: {0}")]
    UnsafeName(String),
    #[error("integrity check failed")]
    Integrity,
    #[error("chunk error: {0}")]
    Chunk(&'static str),
}

pub type Result<T> = std::result::Result<T, TransferError>;

pub struct OutgoingChunks {
    pub transfer_id: TransferId,
    pub frames: Vec<Vec<u8>>,
    pub content_hash: String,
    pub total_bytes: u64,
    pub chunk_count: u32,
}

pub fn split_and_encrypt(
    epoch_key: &[u8; 32],
    transfer_id: &TransferId,
    payload: &[u8],
) -> Result<OutgoingChunks> {
    let content_key = derive_content_key(epoch_key, transfer_id.as_str())?;
    let content_hash = blake3_hex(payload);
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![payload]
    } else {
        payload.chunks(CHUNK_SIZE).collect()
    };
    let chunk_count = chunks.len() as u32;
    let mut frames = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let nonce = derive_nonce(&content_key, i as u32)?;
        let aad = aad_bytes(transfer_id, i as u32);
        let mut ct = nonce.to_vec();
        ct.extend(seal(&content_key, &nonce, &aad, chunk)?);
        let frame = encode_chunk_frame(&ChunkFrame {
            transfer_id: transfer_id.clone(),
            chunk_index: i as u32,
            chunk_count,
            ciphertext: ct,
        })?;
        if frame.len() > FRAME_HARD_CAP {
            return Err(TransferError::Message(
                "encrypted chunk exceeds frame cap".into(),
            ));
        }
        frames.push(frame);
    }
    Ok(OutgoingChunks {
        transfer_id: transfer_id.clone(),
        frames,
        content_hash,
        total_bytes: payload.len() as u64,
        chunk_count,
    })
}

pub fn decrypt_chunk(
    epoch_key: &[u8; 32],
    frame: &[u8],
) -> Result<(TransferId, u32, u32, Vec<u8>)> {
    let decoded = decode_chunk_frame(frame)?;
    let content_key = derive_content_key(epoch_key, decoded.transfer_id.as_str())?;
    if decoded.ciphertext.len() < 12 {
        return Err(TransferError::Chunk("ciphertext too short"));
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&decoded.ciphertext[..12]);
    let aad = aad_bytes(&decoded.transfer_id, decoded.chunk_index);
    let pt = open(&content_key, &nonce, &aad, &decoded.ciphertext[12..])?;
    Ok((
        decoded.transfer_id,
        decoded.chunk_index,
        decoded.chunk_count,
        pt,
    ))
}

pub fn aad_bytes(transfer_id: &TransferId, chunk_index: u32) -> Vec<u8> {
    let mut aad = transfer_id.as_str().as_bytes().to_vec();
    aad.extend_from_slice(&chunk_index.to_be_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipsync_crypto::blake3_hex as hash_hex;

    #[test]
    fn chunk_roundtrip() {
        let epoch = [7u8; 32];
        let id = TransferId::new();
        let payload = vec![9u8; 300 * 1024];
        let out = split_and_encrypt(&epoch, &id, &payload).unwrap();
        assert!(out.chunk_count >= 2);
        let mut assembled = Vec::new();
        let mut seen = std::collections::BTreeMap::new();
        for frame in &out.frames {
            let (tid, idx, count, pt) = decrypt_chunk(&epoch, frame).unwrap();
            assert_eq!(tid, id);
            assert_eq!(count, out.chunk_count);
            seen.insert(idx, pt);
        }
        for i in 0..out.chunk_count {
            assembled.extend(seen.get(&i).unwrap());
        }
        assert_eq!(assembled, payload);
        assert_eq!(hash_hex(&assembled), out.content_hash);
    }
}
