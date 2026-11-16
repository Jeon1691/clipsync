// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! Chunked transfer, path sanitization, staging, and BLAKE3 commit.

mod sanitize;
mod staging;

pub use sanitize::{safe_filename, safe_transfer_id};
pub use staging::{prune_idle_transfers, IncomingTransfer, StagingArea};

use clipsync_crypto::{blake3_hex, derive_content_key, derive_nonce, open, seal};
use clipsync_protocol::{
    decode_chunk_frame, encode_chunk_frame, envelope_aad, ChunkFrame, EnvelopeFrame, RoutingHeader,
    TransferId, AEAD_NONCE_LEN, CHUNK_SIZE, FRAME_HARD_CAP,
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

/// Encrypt `payload` as chunk frames bound to `routing` under its envelope version.
pub fn split_and_encrypt(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    payload: &[u8],
) -> Result<OutgoingChunks> {
    let transfer_id = &routing.transfer_id;
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
        let index = i as u32;
        let nonce = derive_nonce(&content_key, index)?;
        let aad = envelope_aad(
            routing,
            EnvelopeFrame::Chunk {
                index,
                count: chunk_count,
            },
        )?;
        let mut ct = nonce.to_vec();
        ct.extend(seal(&content_key, &nonce, &aad, chunk)?);
        let frame = encode_chunk_frame(&ChunkFrame {
            transfer_id: transfer_id.clone(),
            chunk_index: index,
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

/// Decode and decrypt one chunk frame of the transfer announced by `routing`.
pub fn decrypt_chunk(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    frame: &[u8],
) -> Result<(TransferId, u32, u32, Vec<u8>)> {
    let decoded = decode_chunk_frame(frame)?;
    let pt = open_chunk(epoch_key, routing, &decoded)?;
    Ok((
        decoded.transfer_id,
        decoded.chunk_index,
        decoded.chunk_count,
        pt,
    ))
}

/// Decrypt a decoded chunk frame. The AAD binds the frame's index and count to
/// the manifest's routing header (v2) or to its transfer ID (legacy v1).
pub fn open_chunk(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    frame: &ChunkFrame,
) -> Result<Vec<u8>> {
    if frame.transfer_id != routing.transfer_id {
        return Err(TransferError::Chunk("chunk belongs to another transfer"));
    }
    let content_key = derive_content_key(epoch_key, frame.transfer_id.as_str())?;
    if frame.ciphertext.len() < AEAD_NONCE_LEN {
        return Err(TransferError::Chunk("ciphertext too short"));
    }
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&frame.ciphertext[..AEAD_NONCE_LEN]);
    let aad = envelope_aad(
        routing,
        EnvelopeFrame::Chunk {
            index: frame.chunk_index,
            count: frame.chunk_count,
        },
    )?;
    Ok(open(
        &content_key,
        &nonce,
        &aad,
        &frame.ciphertext[AEAD_NONCE_LEN..],
    )?)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use clipsync_crypto::blake3_hex as hash_hex;
    use clipsync_protocol::{
        DeviceId, ItemKind, MessageId, Recipient, RoomId, ENVELOPE_VERSION_AUTHENTICATED,
        ENVELOPE_VERSION_LEGACY,
    };

    fn routing(version: u16) -> RoutingHeader {
        let now = Utc::now();
        RoutingHeader {
            version,
            message_id: MessageId::new(),
            transfer_id: TransferId::new(),
            room_id: RoomId::new(),
            sender_device_id: DeviceId::new(),
            recipient: Recipient::Broadcast,
            sent_at: now,
            expires_at: now + Duration::seconds(60),
            item_kind: ItemKind::Image,
            seq: 1,
        }
    }

    #[test]
    fn chunk_roundtrip() {
        let epoch = [7u8; 32];
        for version in [ENVELOPE_VERSION_LEGACY, ENVELOPE_VERSION_AUTHENTICATED] {
            let routing = routing(version);
            let payload = vec![9u8; 300 * 1024];
            let out = split_and_encrypt(&epoch, &routing, &payload).unwrap();
            assert!(out.chunk_count >= 2);
            let mut assembled = Vec::new();
            let mut seen = std::collections::BTreeMap::new();
            for frame in &out.frames {
                let (tid, idx, count, pt) = decrypt_chunk(&epoch, &routing, frame).unwrap();
                assert_eq!(tid, routing.transfer_id);
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

    #[test]
    fn legacy_chunks_keep_the_v1_aad() {
        let epoch = [3u8; 32];
        let routing = routing(ENVELOPE_VERSION_LEGACY);
        let key = derive_content_key(&epoch, routing.transfer_id.as_str()).unwrap();
        let nonce = derive_nonce(&key, 0).unwrap();
        let mut aad = routing.transfer_id.as_str().as_bytes().to_vec();
        aad.extend_from_slice(&0u32.to_be_bytes());
        let mut ciphertext = nonce.to_vec();
        ciphertext.extend(seal(&key, &nonce, &aad, b"legacy").unwrap());
        let frame = encode_chunk_frame(&ChunkFrame {
            transfer_id: routing.transfer_id.clone(),
            chunk_index: 0,
            chunk_count: 1,
            ciphertext,
        })
        .unwrap();
        assert_eq!(
            decrypt_chunk(&epoch, &routing, &frame).unwrap().3,
            b"legacy"
        );
    }

    #[test]
    fn authenticated_chunks_reject_any_routing_or_frame_header_change() {
        let epoch = [5u8; 32];
        let honest = routing(ENVELOPE_VERSION_AUTHENTICATED);
        let out = split_and_encrypt(&epoch, &honest, &vec![1u8; CHUNK_SIZE + 1]).unwrap();
        let mut changed: Vec<RoutingHeader> = Vec::new();
        for change in [
            (|r: &mut RoutingHeader| r.room_id = RoomId::new()) as fn(&mut RoutingHeader),
            |r| r.sender_device_id = DeviceId::new(),
            |r| r.recipient = Recipient::Device(DeviceId::new()),
            |r| r.message_id = MessageId::new(),
            |r| r.sent_at -= Duration::seconds(1),
            |r| r.expires_at += Duration::seconds(1),
            |r| r.item_kind = ItemKind::Files,
            |r| r.seq += 1,
            |r| r.version = ENVELOPE_VERSION_LEGACY,
        ] {
            let mut routing = honest.clone();
            change(&mut routing);
            changed.push(routing);
        }
        for routing in &changed {
            assert!(
                decrypt_chunk(&epoch, routing, &out.frames[0]).is_err(),
                "{routing:?}"
            );
        }
        let mut other_transfer = honest.clone();
        other_transfer.transfer_id = TransferId::new();
        assert!(matches!(
            decrypt_chunk(&epoch, &other_transfer, &out.frames[0]),
            Err(TransferError::Chunk("chunk belongs to another transfer"))
        ));
        let mut frame = decode_chunk_frame(&out.frames[1]).unwrap();
        frame.chunk_index = 0;
        assert!(open_chunk(&epoch, &honest, &frame).is_err(), "reordered");
        let mut frame = decode_chunk_frame(&out.frames[1]).unwrap();
        frame.chunk_count = 3;
        assert!(open_chunk(&epoch, &honest, &frame).is_err(), "recounted");
        let mut unknown = honest;
        unknown.version = ENVELOPE_VERSION_AUTHENTICATED + 1;
        assert!(matches!(
            decrypt_chunk(&epoch, &unknown, &out.frames[0]),
            Err(TransferError::Protocol(_))
        ));
    }

    #[test]
    fn empty_transfers_have_one_authenticated_chunk_and_short_or_tampered_chunks_fail() {
        let epoch = [19; 32];
        let routing = routing(ENVELOPE_VERSION_AUTHENTICATED);
        let id = routing.transfer_id.clone();
        let outgoing = split_and_encrypt(&epoch, &routing, &[]).unwrap();
        assert_eq!(outgoing.chunk_count, 1);
        assert_eq!(outgoing.total_bytes, 0);
        assert_eq!(
            decrypt_chunk(&epoch, &routing, &outgoing.frames[0]).unwrap(),
            (id.clone(), 0, 1, vec![])
        );
        let short = encode_chunk_frame(&ChunkFrame {
            transfer_id: id,
            chunk_index: 0,
            chunk_count: 1,
            ciphertext: vec![0; AEAD_NONCE_LEN - 1],
        })
        .unwrap();
        assert!(matches!(
            decrypt_chunk(&epoch, &routing, &short),
            Err(TransferError::Chunk("ciphertext too short"))
        ));
        let mut changed = outgoing.frames[0].clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(matches!(
            decrypt_chunk(&epoch, &routing, &changed),
            Err(TransferError::Aead(_))
        ));
    }
}
