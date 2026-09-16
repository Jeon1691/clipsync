use snow::{Builder, TransportState};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::{CryptoError, Result};

pub const NOISE_PATTERN: &str = "Noise_KK_25519_AESGCM_SHA256";

pub struct NoiseSession {
    transport: TransportState,
}

fn noise_err(e: impl std::fmt::Display) -> CryptoError {
    CryptoError::Noise(e.to_string())
}

pub struct NoiseHandshake {
    inner: snow::HandshakeState,
}

impl NoiseHandshake {
    pub fn initiator(local: &StaticSecret, remote: &PublicKey) -> Result<Self> {
        Self::build(local, remote, true)
    }

    pub fn responder(local: &StaticSecret, remote: &PublicKey) -> Result<Self> {
        Self::build(local, remote, false)
    }

    fn build(local: &StaticSecret, remote: &PublicKey, initiator: bool) -> Result<Self> {
        let local_bytes = local.to_bytes();
        let remote_bytes = *remote.as_bytes();
        let params = NOISE_PATTERN.parse().map_err(noise_err)?;
        let builder = Builder::new(params)
            .local_private_key(&local_bytes)
            .map_err(noise_err)?
            .remote_public_key(&remote_bytes)
            .map_err(noise_err)?;
        let inner = if initiator {
            builder.build_initiator().map_err(noise_err)?
        } else {
            builder.build_responder().map_err(noise_err)?
        };
        Ok(Self { inner })
    }

    pub fn write_message(&mut self) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; 1024];
        let len = self.inner.write_message(&[], &mut buf).map_err(noise_err)?;
        Ok(buf[..len].to_vec())
    }

    pub fn read_message(&mut self, msg: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; 1024];
        self.inner.read_message(msg, &mut buf).map_err(noise_err)?;
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        self.inner.is_handshake_finished()
    }

    pub fn into_transport(self) -> Result<NoiseSession> {
        let transport = self.inner.into_transport_mode().map_err(noise_err)?;
        Ok(NoiseSession { transport })
    }
}

impl NoiseSession {
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; plaintext.len() + 32];
        let len = self
            .transport
            .write_message(plaintext, &mut buf)
            .map_err(noise_err)?;
        Ok(buf[..len].to_vec())
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; ciphertext.len()];
        let len = self
            .transport
            .read_message(ciphertext, &mut buf)
            .map_err(noise_err)?;
        Ok(buf[..len].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn noise_kk_handshake() {
        let a = StaticSecret::random_from_rng(OsRng);
        let b = StaticSecret::random_from_rng(OsRng);
        let a_pub = PublicKey::from(&a);
        let b_pub = PublicKey::from(&b);
        let mut init = NoiseHandshake::initiator(&a, &b_pub).unwrap();
        let mut resp = NoiseHandshake::responder(&b, &a_pub).unwrap();
        let m1 = init.write_message().unwrap();
        resp.read_message(&m1).unwrap();
        let m2 = resp.write_message().unwrap();
        init.read_message(&m2).unwrap();
        assert!(init.is_finished());
        assert!(resp.is_finished());
        let mut ta = init.into_transport().unwrap();
        let mut tb = resp.into_transport().unwrap();
        let ct = ta.encrypt(b"ping").unwrap();
        let pt = tb.decrypt(&ct).unwrap();
        assert_eq!(pt, b"ping");
    }
}
