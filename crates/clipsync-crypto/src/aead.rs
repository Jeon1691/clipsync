use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use thiserror::Error;

use crate::kdf::KEY_LEN;

#[derive(Debug, Error)]
pub enum AeadError {
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
    #[error("invalid nonce length")]
    Nonce,
}

pub fn seal(
    key: &[u8; KEY_LEN],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, AeadError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| AeadError::Encrypt)?;
    let nonce = Nonce::from_slice(nonce);
    cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| AeadError::Encrypt)
}

pub fn open(
    key: &[u8; KEY_LEN],
    nonce: &[u8; 12],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, AeadError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| AeadError::Decrypt)?;
    let nonce = Nonce::from_slice(nonce);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| AeadError::Decrypt)
}

/// Ciphertext on the wire is 12-byte nonce || AES-GCM(ct||tag).
pub fn seal_prefixed(
    key: &[u8; KEY_LEN],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, AeadError> {
    let mut out = nonce.to_vec();
    out.extend(seal(key, nonce, aad, plaintext)?);
    Ok(out)
}

pub fn open_prefixed(key: &[u8; KEY_LEN], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, AeadError> {
    if data.len() < 12 {
        return Err(AeadError::Nonce);
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&data[..12]);
    open(key, &nonce, aad, &data[12..])
}
