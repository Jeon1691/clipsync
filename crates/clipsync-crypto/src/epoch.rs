use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::aead::{open, seal};
use crate::kdf::{derive_labeled, KEY_LEN};
use crate::Result;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WrappedEpochKey {
    pub eph_public: String,
    pub nonce: String,
    pub ciphertext: String,
}

pub fn random_epoch_key() -> [u8; KEY_LEN] {
    use rand::RngCore;
    let mut key = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut key);
    key
}

pub fn wrap_epoch_key(
    recipient_pk: &PublicKey,
    epoch_key: &[u8; KEY_LEN],
) -> Result<WrappedEpochKey> {
    let eph = StaticSecret::random_from_rng(OsRng);
    let eph_pub = PublicKey::from(&eph);
    let shared = eph.diffie_hellman(recipient_pk);
    let wrap_key = derive_labeled(
        shared.as_bytes(),
        b"clipsync-epoch",
        b"clipsync/v1/epoch-wrap",
    )?;
    let nonce = crate::kdf::derive_nonce(&wrap_key, 0)?;
    let ct = seal(&wrap_key, &nonce, b"epoch", epoch_key)?;
    Ok(WrappedEpochKey {
        eph_public: hex::encode(eph_pub.as_bytes()),
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ct),
    })
}

pub fn unwrap_epoch_key(local: &StaticSecret, wrapped: &WrappedEpochKey) -> Result<[u8; KEY_LEN]> {
    let pk_bytes = hex::decode(&wrapped.eph_public).map_err(|_| crate::CryptoError::Key)?;
    if pk_bytes.len() != 32 {
        return Err(crate::CryptoError::Key);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&pk_bytes);
    let eph_pub = PublicKey::from(arr);
    let shared = local.diffie_hellman(&eph_pub);
    let wrap_key = derive_labeled(
        shared.as_bytes(),
        b"clipsync-epoch",
        b"clipsync/v1/epoch-wrap",
    )?;
    let nonce_bytes = hex::decode(&wrapped.nonce).map_err(|_| crate::CryptoError::Key)?;
    if nonce_bytes.len() != 12 {
        return Err(crate::CryptoError::Key);
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes);
    let ct = hex::decode(&wrapped.ciphertext).map_err(|_| crate::CryptoError::Key)?;
    let pt = open(&wrap_key, &nonce, b"epoch", &ct)?;
    if pt.len() != KEY_LEN {
        return Err(crate::CryptoError::Key);
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&pt);
    Ok(key)
}

/// First-epoch key derived from the PAKE room root so both peers agree without extra wrap.
pub fn initial_epoch_key(room_root_key: &[u8; KEY_LEN]) -> Result<[u8; KEY_LEN]> {
    derive_labeled(room_root_key, b"clipsync-epoch", b"clipsync/v1/epoch/1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn wrap_roundtrip() {
        let sk = StaticSecret::random_from_rng(OsRng);
        let pk = PublicKey::from(&sk);
        let epoch = random_epoch_key();
        let wrapped = wrap_epoch_key(&pk, &epoch).unwrap();
        let opened = unwrap_epoch_key(&sk, &wrapped).unwrap();
        assert_eq!(epoch, opened);
    }
}
