// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use rand::rngs::SysRng;
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
    use rand::TryRng;
    let mut key = [0u8; KEY_LEN];
    SysRng
        .try_fill_bytes(&mut key)
        .expect("operating system randomness unavailable");
    key
}

pub fn wrap_epoch_key(
    recipient_pk: &PublicKey,
    epoch_key: &[u8; KEY_LEN],
) -> Result<WrappedEpochKey> {
    let eph = StaticSecret::random();
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn wrap_roundtrip() {
        let sk = StaticSecret::random();
        let pk = PublicKey::from(&sk);
        let epoch = random_epoch_key();
        let wrapped = wrap_epoch_key(&pk, &epoch).unwrap();
        let opened = unwrap_epoch_key(&sk, &wrapped).unwrap();
        assert_eq!(epoch, opened);
    }

    #[test]
    fn random_epoch_keys_are_fresh_os_entropy() {
        let first = random_epoch_key();
        let second = random_epoch_key();
        assert_ne!(first, [0u8; KEY_LEN], "SysRng must overwrite the buffer");
        assert_ne!(first, second, "each epoch key must be drawn afresh");
        let sk = StaticSecret::random();
        let wrapped = wrap_epoch_key(&PublicKey::from(&sk), &second).unwrap();
        assert_eq!(unwrap_epoch_key(&sk, &wrapped).unwrap(), second);
    }

    #[test]
    fn malformed_wrapped_fields_never_reach_key_material() {
        let secret = StaticSecret::from([31u8; 32]);
        let valid = wrap_epoch_key(&PublicKey::from(&secret), &[19; KEY_LEN]).unwrap();
        for value in [
            "not hex".to_owned(),
            "".to_owned(),
            hex::encode([0u8; 31]),
            hex::encode([0u8; 33]),
        ] {
            let mut wrapped = valid.clone();
            wrapped.eph_public = value;
            assert!(matches!(
                unwrap_epoch_key(&secret, &wrapped),
                Err(crate::CryptoError::Key)
            ));
        }
        for value in [
            "not hex".to_owned(),
            "".to_owned(),
            hex::encode([0u8; 11]),
            hex::encode([0u8; 13]),
        ] {
            let mut wrapped = valid.clone();
            wrapped.nonce = value;
            assert!(matches!(
                unwrap_epoch_key(&secret, &wrapped),
                Err(crate::CryptoError::Key)
            ));
        }
        let mut wrapped = valid;
        wrapped.ciphertext = "not hex".into();
        assert!(matches!(
            unwrap_epoch_key(&secret, &wrapped),
            Err(crate::CryptoError::Key)
        ));
    }

    #[test]
    fn wrapped_epoch_authenticates_recipient_nonce_and_ciphertext() {
        let secret = StaticSecret::from([31u8; 32]);
        let epoch = [19; KEY_LEN];
        let valid = wrap_epoch_key(&PublicKey::from(&secret), &epoch).unwrap();
        let wrong_recipient = StaticSecret::from([32u8; 32]);
        assert!(matches!(
            unwrap_epoch_key(&wrong_recipient, &valid),
            Err(crate::CryptoError::Aead(_))
        ));
        for field in ["nonce", "ciphertext"] {
            let mut wrapped = valid.clone();
            let encoded = if field == "nonce" {
                &mut wrapped.nonce
            } else {
                &mut wrapped.ciphertext
            };
            let mut bytes = hex::decode(&*encoded).unwrap();
            bytes[0] ^= 1;
            *encoded = hex::encode(bytes);
            assert!(matches!(
                unwrap_epoch_key(&secret, &wrapped),
                Err(crate::CryptoError::Aead(_))
            ));
        }
        for length in [0, 1, 15] {
            let mut wrapped = valid.clone();
            wrapped.ciphertext = hex::encode(vec![0; length]);
            assert!(matches!(
                unwrap_epoch_key(&secret, &wrapped),
                Err(crate::CryptoError::Aead(_))
            ));
        }
        assert_eq!(unwrap_epoch_key(&secret, &valid).unwrap(), epoch);
        let second = wrap_epoch_key(&PublicKey::from(&secret), &epoch).unwrap();
        assert_ne!(valid.eph_public, second.eph_public);
        assert_ne!(valid.ciphertext, second.ciphertext);
        assert_eq!(unwrap_epoch_key(&secret, &second).unwrap(), epoch);
    }

    #[test]
    fn authenticated_payload_must_still_have_the_exact_epoch_key_length() {
        let secret = StaticSecret::from([33u8; 32]);
        let ephemeral = StaticSecret::from([34u8; 32]);
        let shared = ephemeral.diffie_hellman(&PublicKey::from(&secret));
        let wrapping = derive_labeled(
            shared.as_bytes(),
            b"clipsync-epoch",
            b"clipsync/v1/epoch-wrap",
        )
        .unwrap();
        let nonce = crate::derive_nonce(&wrapping, 0).unwrap();
        for length in [0, KEY_LEN - 1, KEY_LEN + 1] {
            let wrapped = WrappedEpochKey {
                eph_public: hex::encode(PublicKey::from(&ephemeral).as_bytes()),
                nonce: hex::encode(nonce),
                ciphertext: hex::encode(
                    seal(&wrapping, &nonce, b"epoch", &vec![7; length]).unwrap(),
                ),
            };
            assert!(matches!(
                unwrap_epoch_key(&secret, &wrapped),
                Err(crate::CryptoError::Key)
            ));
        }
    }
}
