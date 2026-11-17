// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use snow::{Builder, TransportState};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::{CryptoError, Result};

pub const NOISE_PATTERN: &str = "Noise_KK_25519_AESGCM_SHA256";

/// Maximum Noise message length (Noise specification, section 3).
const NOISE_MAX_MESSAGE_LEN: usize = 65_535;

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

    /// Next handshake message with an empty payload, as released clients send.
    pub fn write_message(&mut self) -> Result<Vec<u8>> {
        self.write_message_with_payload(&[])
    }

    /// Next handshake message carrying `payload`. In `KK` both messages are
    /// encrypted and authenticated under the static keys of both devices.
    pub fn write_message_with_payload(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; NOISE_MAX_MESSAGE_LEN];
        let len = self
            .inner
            .write_message(payload, &mut buf)
            .map_err(noise_err)?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Read the peer's next handshake message and return its decrypted payload.
    /// On failure the handshake state is left as it was before the call.
    pub fn read_message(&mut self, msg: &[u8]) -> Result<Vec<u8>> {
        // The payload is never longer than the message that carries it.
        let mut buf = vec![0u8; msg.len().min(NOISE_MAX_MESSAGE_LEN)];
        let len = self.inner.read_message(msg, &mut buf).map_err(noise_err)?;
        buf.truncate(len);
        Ok(buf)
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// The read path of released clients (v0.1.0 through v0.2.0): the payload
    /// is decrypted into a fixed 1024-byte buffer and then discarded.
    fn legacy_read(state: &mut snow::HandshakeState, msg: &[u8]) -> bool {
        let mut buf = vec![0u8; clipsync_protocol::LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES];
        state.read_message(msg, &mut buf).is_ok()
    }

    fn legacy_state(
        local: &StaticSecret,
        remote: &PublicKey,
        initiator: bool,
    ) -> snow::HandshakeState {
        let local = local.to_bytes();
        let builder = Builder::new(NOISE_PATTERN.parse().unwrap())
            .local_private_key(&local)
            .unwrap()
            .remote_public_key(remote.as_bytes())
            .unwrap();
        if initiator {
            builder.build_initiator().unwrap()
        } else {
            builder.build_responder().unwrap()
        }
    }

    #[test]
    fn released_clients_complete_handshakes_that_carry_a_payload_up_to_their_limit() {
        let new = StaticSecret::from([61u8; 32]);
        let old = StaticSecret::from([62u8; 32]);
        let limit = clipsync_protocol::LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES;
        let capabilities = clipsync_protocol::HandshakeCapabilities::accepting(&[1, 2])
            .encode()
            .unwrap();
        for payload in [capabilities, vec![b'x'; limit]] {
            // New initiator, released responder.
            let mut init = NoiseHandshake::initiator(&new, &PublicKey::from(&old)).unwrap();
            let mut resp = legacy_state(&old, &PublicKey::from(&new), false);
            assert!(legacy_read(
                &mut resp,
                &init.write_message_with_payload(&payload).unwrap()
            ));
            let mut reply = vec![0u8; NOISE_MAX_MESSAGE_LEN];
            let len = resp.write_message(&[], &mut reply).unwrap();
            assert!(init.read_message(&reply[..len]).unwrap().is_empty());
            assert!(init.is_finished() && resp.is_handshake_finished());

            // Released initiator, new responder.
            let mut init = legacy_state(&old, &PublicKey::from(&new), true);
            let mut resp = NoiseHandshake::responder(&new, &PublicKey::from(&old)).unwrap();
            let mut first = vec![0u8; NOISE_MAX_MESSAGE_LEN];
            let len = init.write_message(&[], &mut first).unwrap();
            assert!(resp.read_message(&first[..len]).unwrap().is_empty());
            assert!(legacy_read(
                &mut init,
                &resp.write_message_with_payload(&payload).unwrap()
            ));
            assert!(init.is_handshake_finished() && resp.is_finished());
        }
        // One byte more and the released client fails; encode() refuses that.
        let mut init = NoiseHandshake::initiator(&new, &PublicKey::from(&old)).unwrap();
        let mut resp = legacy_state(&old, &PublicKey::from(&new), false);
        let message = init
            .write_message_with_payload(&vec![b'x'; limit + 1])
            .unwrap();
        assert!(!legacy_read(&mut resp, &message));
    }

    #[test]
    fn payloads_are_bound_to_the_pinned_static_keys() {
        let a = StaticSecret::from([71u8; 32]);
        let b = StaticSecret::from([72u8; 32]);
        let impostor = StaticSecret::from([73u8; 32]);
        let mut init = NoiseHandshake::initiator(&impostor, &PublicKey::from(&b)).unwrap();
        let mut resp = NoiseHandshake::responder(&b, &PublicKey::from(&a)).unwrap();
        let forged = init.write_message_with_payload(b"caps").unwrap();
        assert!(resp.read_message(&forged).is_err(), "impostor accepted");
        // The failed read left the responder usable for the genuine peer.
        let mut genuine = NoiseHandshake::initiator(&a, &PublicKey::from(&b)).unwrap();
        let first = genuine.write_message_with_payload(b"caps-a").unwrap();
        assert_eq!(resp.read_message(&first).unwrap(), b"caps-a");
        let second = resp.write_message_with_payload(b"caps-b").unwrap();
        let mut tampered = second.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(
            genuine.read_message(&tampered).is_err(),
            "tampered reply accepted"
        );
        assert_eq!(genuine.read_message(&second).unwrap(), b"caps-b");
        assert!(genuine.is_finished() && resp.is_finished());
    }

    #[test]
    fn noise_kk_handshake() {
        let a = StaticSecret::random();
        let b = StaticSecret::random();
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

    fn established_sessions() -> (NoiseSession, NoiseSession) {
        let a = StaticSecret::from([41u8; 32]);
        let b = StaticSecret::from([42u8; 32]);
        let mut initiator = NoiseHandshake::initiator(&a, &PublicKey::from(&b)).unwrap();
        let mut responder = NoiseHandshake::responder(&b, &PublicKey::from(&a)).unwrap();
        responder
            .read_message(&initiator.write_message().unwrap())
            .unwrap();
        initiator
            .read_message(&responder.write_message().unwrap())
            .unwrap();
        assert!(
            initiator.write_message().is_err(),
            "a completed handshake cannot emit another message"
        );
        (
            initiator.into_transport().unwrap(),
            responder.into_transport().unwrap(),
        )
    }

    #[test]
    fn incomplete_or_out_of_order_noise_handshakes_fail_closed() {
        let a = StaticSecret::from([41u8; 32]);
        let b = StaticSecret::from([42u8; 32]);
        let mut responder = NoiseHandshake::responder(&b, &PublicKey::from(&a)).unwrap();
        assert!(!responder.is_finished());
        assert!(matches!(
            responder.write_message(),
            Err(CryptoError::Noise(_))
        ));
        assert!(matches!(
            responder.into_transport(),
            Err(CryptoError::Noise(_))
        ));
        let mut initiator = NoiseHandshake::initiator(&a, &PublicKey::from(&b)).unwrap();
        assert!(matches!(
            initiator.read_message(&[]),
            Err(CryptoError::Noise(_))
        ));
        assert!(matches!(
            initiator.into_transport(),
            Err(CryptoError::Noise(_))
        ));
    }

    #[test]
    fn noise_handshake_authenticates_the_pinned_static_identity_and_transcript() {
        let a = StaticSecret::from([41u8; 32]);
        let b = StaticSecret::from([42u8; 32]);
        let unexpected = StaticSecret::from([43u8; 32]);
        let mut initiator = NoiseHandshake::initiator(&a, &PublicKey::from(&b)).unwrap();
        let message = initiator.write_message().unwrap();
        let mut wrong_peer = NoiseHandshake::responder(&b, &PublicKey::from(&unexpected)).unwrap();
        assert!(matches!(
            wrong_peer.read_message(&message),
            Err(CryptoError::Noise(_))
        ));
        for malformed in [&[][..], &message[..message.len() - 1]] {
            let mut responder = NoiseHandshake::responder(&b, &PublicKey::from(&a)).unwrap();
            assert!(matches!(
                responder.read_message(malformed),
                Err(CryptoError::Noise(_))
            ));
            assert!(!responder.is_finished());
        }
        let mut damaged = message;
        *damaged.last_mut().unwrap() ^= 1;
        let mut responder = NoiseHandshake::responder(&b, &PublicKey::from(&a)).unwrap();
        assert!(matches!(
            responder.read_message(&damaged),
            Err(CryptoError::Noise(_))
        ));
    }

    #[test]
    fn noise_transport_rejects_bad_tags_replays_and_oversized_messages() {
        let (mut sender, mut receiver) = established_sessions();
        let ciphertext = sender.encrypt(b"authenticated payload").unwrap();
        let mut damaged = ciphertext.clone();
        damaged[0] ^= 1;
        assert!(matches!(
            receiver.decrypt(&damaged),
            Err(CryptoError::Noise(_))
        ));
        assert_eq!(
            receiver.decrypt(&ciphertext).unwrap(),
            b"authenticated payload"
        );
        assert!(matches!(
            receiver.decrypt(&ciphertext),
            Err(CryptoError::Noise(_))
        ));
        assert!(matches!(receiver.decrypt(&[]), Err(CryptoError::Noise(_))));
        assert!(matches!(
            receiver.decrypt(&vec![0; 65_536]),
            Err(CryptoError::Noise(_))
        ));
        assert!(matches!(
            sender.encrypt(&vec![0; 65_536]),
            Err(CryptoError::Noise(_))
        ));
        let next = sender.encrypt(b"next authenticated payload").unwrap();
        assert_eq!(
            receiver.decrypt(&next).unwrap(),
            b"next authenticated payload"
        );
        let reverse = receiver.encrypt(b"reverse direction").unwrap();
        assert_eq!(sender.decrypt(&reverse).unwrap(), b"reverse direction");
    }
}
