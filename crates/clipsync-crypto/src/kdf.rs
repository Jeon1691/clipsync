use hkdf::Hkdf;
use sha2::Sha256;

use crate::{CryptoError, Result};

pub const KEY_LEN: usize = 32;
pub const CONTENT_HASH_LEN: usize = 32;

pub fn hkdf_sha256(ikm: &[u8], salt: Option<&[u8]>, info: &[u8], out: &mut [u8]) -> Result<()> {
    let hk = Hkdf::<Sha256>::new(salt, ikm);
    hk.expand(info, out).map_err(|_| CryptoError::Hkdf)
}

pub fn derive_labeled(ikm: &[u8], salt: &[u8], label: &[u8]) -> Result<[u8; KEY_LEN]> {
    let mut out = [0u8; KEY_LEN];
    hkdf_sha256(ikm, Some(salt), label, &mut out)?;
    Ok(out)
}

pub fn derive_content_key(epoch_key: &[u8; KEY_LEN], transfer_id: &str) -> Result<[u8; KEY_LEN]> {
    let info = format!("clipsync/v1/content/{transfer_id}");
    derive_labeled(epoch_key, b"clipsync-content", info.as_bytes())
}

pub fn derive_nonce(content_key: &[u8; KEY_LEN], chunk_index: u32) -> Result<[u8; 12]> {
    let mut okm = [0u8; 12];
    let info = format!("clipsync/v1/nonce/{chunk_index}");
    hkdf_sha256(
        content_key,
        Some(b"clipsync-nonce"),
        info.as_bytes(),
        &mut okm,
    )?;
    Ok(okm)
}

pub fn blake3_hash(data: &[u8]) -> [u8; CONTENT_HASH_LEN] {
    *blake3::hash(data).as_bytes()
}

pub fn blake3_hex(data: &[u8]) -> String {
    hex::encode(blake3_hash(data))
}
