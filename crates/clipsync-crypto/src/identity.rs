use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use clipsync_protocol::DeviceId;

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DeviceIdentity {
    #[zeroize(skip)]
    pub device_id: DeviceId,
    secret: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredIdentity {
    pub device_id: DeviceId,
    pub secret: String,
    pub public: String,
    pub fingerprint: String,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        Self {
            device_id: DeviceId::new(),
            secret: secret.to_bytes(),
        }
    }

    pub fn from_bytes(device_id: DeviceId, secret: [u8; 32]) -> Self {
        Self { device_id, secret }
    }

    pub fn static_secret(&self) -> StaticSecret {
        StaticSecret::from(self.secret)
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey::from(&self.static_secret())
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        *self.public_key().as_bytes()
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_bytes())
    }

    pub fn to_stored(&self) -> StoredIdentity {
        StoredIdentity {
            device_id: self.device_id.clone(),
            secret: hex::encode(self.secret),
            public: hex::encode(self.public_bytes()),
            fingerprint: self.fingerprint(),
        }
    }

    pub fn from_stored(stored: &StoredIdentity) -> Option<Self> {
        let bytes = hex::decode(&stored.secret).ok()?;
        if bytes.len() != 32 {
            return None;
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&bytes);
        Some(Self {
            device_id: stored.device_id.clone(),
            secret,
        })
    }
}

pub fn fingerprint(public: &[u8; 32]) -> String {
    let digest = Sha256::digest(public);
    hex::encode(digest)
}

pub fn parse_public(hex_str: &str) -> Option<PublicKey> {
    let bytes = hex::decode(hex_str).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(PublicKey::from(arr))
}
