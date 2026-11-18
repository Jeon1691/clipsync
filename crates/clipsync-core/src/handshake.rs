// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Per-peer `Noise_KK` handshakes that carry envelope capabilities.
//!
//! The handshake payload is the only capability signal this device trusts: a
//! payload that decrypts proves it came from the holder of the static key the
//! room pinned for that peer. Released clients send an empty payload and ignore
//! the one they receive (see `LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES`), so they
//! keep completing the handshake and simply never advertise v2.

use std::collections::{HashMap, HashSet};

use clipsync_crypto::{parse_public, DeviceIdentity, NoiseHandshake};
use clipsync_protocol::{DeviceId, HandshakeCapabilities};
use clipsync_storage::{PeerRecord, RoomRecord};

/// What one received handshake message produced.
#[derive(Debug, Default)]
pub(crate) struct HandshakeOutcome {
    /// Message to send back (the responder's reply).
    pub(crate) reply: Option<Vec<u8>>,
    /// Peer whose handshake just completed and what it advertised.
    pub(crate) completed: Option<(DeviceId, HandshakeCapabilities)>,
}

/// Handshake state of one relay session, keyed by pinned peer device ID.
pub(crate) struct HandshakeTracker {
    /// Our encoded capabilities, sent in every handshake message we write.
    payload: Vec<u8>,
    /// Initiator handshakes waiting for the peer's reply.
    pending: HashMap<String, NoiseHandshake>,
    /// Peers whose handshake completed in this session.
    done: HashSet<String>,
}

/// The lexicographically smaller device ID initiates, so exactly one side of a
/// pair starts the handshake (same rule as released clients).
fn is_initiator(local: &DeviceId, peer: &DeviceId) -> bool {
    local.as_str() < peer.as_str()
}

impl HandshakeTracker {
    pub(crate) fn new(payload: Vec<u8>) -> Self {
        Self {
            payload,
            pending: HashMap::new(),
            done: HashSet::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_done(&self, peer: &DeviceId) -> bool {
        self.done.contains(peer.as_str())
    }

    #[cfg(test)]
    pub(crate) fn is_pending(&self, peer: &DeviceId) -> bool {
        self.pending.contains_key(peer.as_str())
    }

    /// First handshake message towards `peer`, if this device initiates for
    /// the pair and the peer is pinned with a valid key. Without `restart`, a
    /// peer whose handshake is running or finished is left alone (a repeated
    /// presence announcement); with it (the peer just reconnected) the
    /// handshake starts over.
    pub(crate) fn initiate(
        &mut self,
        identity: &DeviceIdentity,
        room: &RoomRecord,
        peer: &DeviceId,
        restart: bool,
    ) -> Option<Vec<u8>> {
        if !is_initiator(&identity.device_id, peer) {
            return None;
        }
        if !restart
            && (self.done.contains(peer.as_str()) || self.pending.contains_key(peer.as_str()))
        {
            return None;
        }
        let record = room.peers.iter().find(|record| record.device_id == *peer)?;
        let public = parse_public(&record.public_key_hex)?;
        let mut handshake = match NoiseHandshake::initiator(&identity.static_secret(), &public) {
            Ok(handshake) => handshake,
            Err(e) => {
                tracing::warn!(error = %e, "noise init");
                return None;
            }
        };
        let message = handshake.write_message_with_payload(&self.payload).ok()?;
        self.done.remove(peer.as_str());
        self.pending.insert(peer.to_string(), handshake);
        Some(message)
    }

    /// Process one handshake message from the relay. It is either the reply to
    /// one of our pending handshakes, or a first message from a pinned peer;
    /// trial decryption against each pinned static key tells which peer sent
    /// it. Anything else (unpinned key, tampered, garbage) is ignored.
    pub(crate) fn receive(
        &mut self,
        identity: &DeviceIdentity,
        room: &RoomRecord,
        message: &[u8],
    ) -> HandshakeOutcome {
        let replied = self.pending.iter_mut().find_map(|(peer, handshake)| {
            let payload = handshake.read_message(message).ok()?;
            handshake.is_finished().then(|| (peer.clone(), payload))
        });
        if let Some((peer, payload)) = replied {
            self.pending.remove(&peer);
            self.done.insert(peer.clone());
            return HandshakeOutcome {
                reply: None,
                completed: Some((peer.into(), HandshakeCapabilities::decode(&payload))),
            };
        }
        for record in room.peers.iter().filter(|record| {
            record.device_id != identity.device_id
                && !is_initiator(&identity.device_id, &record.device_id)
        }) {
            if let Some((reply, payload)) = self.respond(identity, record, message) {
                self.pending.remove(record.device_id.as_str());
                self.done.insert(record.device_id.to_string());
                return HandshakeOutcome {
                    reply: Some(reply),
                    completed: Some((
                        record.device_id.clone(),
                        HandshakeCapabilities::decode(&payload),
                    )),
                };
            }
        }
        HandshakeOutcome::default()
    }

    fn respond(
        &self,
        identity: &DeviceIdentity,
        record: &PeerRecord,
        message: &[u8],
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        let public = parse_public(&record.public_key_hex)?;
        let mut handshake = NoiseHandshake::responder(&identity.static_secret(), &public).ok()?;
        let payload = handshake.read_message(message).ok()?;
        let reply = handshake.write_message_with_payload(&self.payload).ok()?;
        handshake.is_finished().then_some((reply, payload))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use clipsync_protocol::Timestamp;
    use clipsync_protocol::{DeviceRole, RoomId};

    const CAPS: &[u8] = br#"{"envelope_versions":[1,2]}"#;

    fn identity(name: &str, secret: u8) -> DeviceIdentity {
        DeviceIdentity::from_bytes(name.into(), [secret; 32])
    }

    fn room_of(peers: &[&DeviceIdentity]) -> RoomRecord {
        RoomRecord {
            room_id: RoomId::new(),
            epoch: 1,
            role: DeviceRole::Owner,
            created_at: Timestamp::now(),
            device_token: "token".into(),
            peers: peers
                .iter()
                .map(|peer| PeerRecord {
                    device_id: peer.device_id.clone(),
                    device_name: "peer".into(),
                    os: "test".into(),
                    role: DeviceRole::Member,
                    public_key_hex: hex::encode(peer.public_bytes()),
                    fingerprint: peer.fingerprint(),
                })
                .collect(),
        }
    }

    #[test]
    fn two_trackers_exchange_capabilities_and_identify_the_sender_by_its_pinned_key() {
        let a = identity("device-a", 11);
        let z = identity("device-z", 12);
        let m = identity("device-m", 13);
        // z pins two peers; trial decryption must attribute the message to a.
        let room_a = room_of(&[&z]);
        let room_z = room_of(&[&m, &a]);
        let mut tracker_a = HandshakeTracker::new(CAPS.to_vec());
        let mut tracker_z = HandshakeTracker::new(Vec::new());
        assert!(
            tracker_z
                .initiate(&z, &room_z, &a.device_id, false)
                .is_none(),
            "z must respond"
        );
        let first = tracker_a
            .initiate(&a, &room_a, &z.device_id, false)
            .unwrap();
        assert!(
            tracker_a
                .initiate(&a, &room_a, &z.device_id, false)
                .is_none(),
            "already pending"
        );

        let at_z = tracker_z.receive(&z, &room_z, &first);
        let (peer, caps) = at_z.completed.unwrap();
        assert_eq!(peer, a.device_id);
        assert!(caps.accepts_authenticated());
        assert!(tracker_z.is_done(&a.device_id));

        let at_a = tracker_a.receive(&a, &room_a, &at_z.reply.unwrap());
        assert!(at_a.reply.is_none());
        let (peer, caps) = at_a.completed.unwrap();
        assert_eq!(peer, z.device_id);
        assert!(
            !caps.accepts_authenticated(),
            "z advertised nothing, like a released client"
        );
        assert!(tracker_a.is_done(&z.device_id));
        assert!(
            tracker_a
                .initiate(&a, &room_a, &z.device_id, false)
                .is_none(),
            "finished"
        );
        assert!(
            tracker_a
                .initiate(&a, &room_a, &z.device_id, true)
                .is_some(),
            "peer reconnected"
        );
    }

    #[test]
    fn unpinned_tampered_or_garbage_handshakes_complete_nothing() {
        let a = identity("device-a", 21);
        let z = identity("device-z", 22);
        let impostor = identity("device-a", 23);
        let room_z = room_of(&[&a]);
        let mut tracker_z = HandshakeTracker::new(CAPS.to_vec());
        // Same device ID, different static key: not the pinned peer.
        let mut forged = HandshakeTracker::new(CAPS.to_vec());
        let forged_first = forged
            .initiate(&impostor, &room_of(&[&z]), &z.device_id, false)
            .unwrap();
        let mut genuine = HandshakeTracker::new(CAPS.to_vec());
        let mut tampered = genuine
            .initiate(&a, &room_of(&[&z]), &z.device_id, false)
            .unwrap();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        for message in [forged_first, tampered, b"garbage".to_vec(), Vec::new()] {
            let outcome = tracker_z.receive(&z, &room_z, &message);
            assert!(outcome.reply.is_none() && outcome.completed.is_none());
        }
        assert!(!tracker_z.is_done(&a.device_id));
        // Peers with an unusable key are skipped, not trusted.
        let mut broken = room_of(&[&z]);
        broken.peers[0].public_key_hex = "not a key".into();
        assert!(genuine.initiate(&a, &broken, &z.device_id, true).is_none());
        assert!(genuine
            .initiate(&a, &room_of(&[&z]), &"unpinned".into(), false)
            .is_none());
    }
}
