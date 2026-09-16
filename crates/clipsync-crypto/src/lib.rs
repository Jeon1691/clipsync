//! Pairing (SPAKE2), key derivation, AEAD, Noise session handshake, epoch wrapping.

mod aead;
mod epoch;
mod identity;
mod kdf;
mod noise;
mod pairing;

pub use aead::{open, open_prefixed, seal, seal_prefixed, AeadError};
pub use epoch::{
    initial_epoch_key, random_epoch_key, unwrap_epoch_key, wrap_epoch_key, WrappedEpochKey,
};
pub use identity::{fingerprint, parse_public, DeviceIdentity, StoredIdentity};
pub use kdf::{
    blake3_hash, blake3_hex, derive_content_key, derive_labeled, derive_nonce, hkdf_sha256,
    CONTENT_HASH_LEN, KEY_LEN,
};
pub use noise::{NoiseHandshake, NoiseSession, NOISE_PATTERN};
pub use pairing::{
    confirm_mac, finish_pairing, pairing_transcript, start_pairing_a, start_pairing_b,
    verify_confirm_mac, PairingKeys, PairingRole, PairingState,
};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("aead error: {0}")]
    Aead(#[from] AeadError),
    #[error("pairing failed: {0}")]
    Pairing(&'static str),
    #[error("noise handshake: {0}")]
    Noise(String),
    #[error("hkdf expand failed")]
    Hkdf,
    #[error("invalid key material")]
    Key,
    #[error("confirmation mismatch")]
    Confirm,
}

pub type Result<T> = std::result::Result<T, CryptoError>;
