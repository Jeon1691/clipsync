// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use clipsync_protocol::{DeviceId, ItemKind, MessageId, Recipient, RoutingHeader, TransferId};

fn fixture() -> (
    RelayState,
    CreatePairingResponse,
    PairingSessionId,
    DeviceId,
) {
    let state = RelayState::new(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: vec![21; 32],
        webhook_url: None,
        webhook_secret: None,
    });
    let sid = PairingSessionId::new();
    let owner = DeviceId::new();
    let offer = state
        .create_pairing(CreatePairingRequest {
            protocol_version: clipsync_protocol::PROTOCOL_VERSION,
            pairing_session_id: sid.clone(),
            device_id: owner.clone(),
            device_name: "owner".into(),
            os: "test".into(),
            ttl_secs: None,
        })
        .unwrap();
    (state, offer, sid, owner)
}

fn join(code: &str, id: DeviceId) -> JoinPairingRequest {
    JoinPairingRequest {
        protocol_version: clipsync_protocol::PROTOCOL_VERSION,
        pairing_code: code.into(),
        device_id: id,
        device_name: "joiner".into(),
        os: "test".into(),
    }
}

#[test]
fn pairing_rejects_expiry_reuse_same_device_and_exhausted_attempts() {
    let (state, offer, sid, owner) = fixture();
    let ip = "127.0.0.1".parse().unwrap();
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, owner), ip),
        Err(JoinError::Other(_))
    ));
    state.pairing_mut(sid.as_str()).unwrap().attempts = PAIRING_MAX_ATTEMPTS;
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), ip),
        Err(JoinError::Attempts)
    ));
    {
        let mut session = state.pairing_mut(sid.as_str()).unwrap();
        session.attempts = 0;
        session.expires_at = Utc::now() - chrono::Duration::seconds(1);
    }
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), ip),
        Err(JoinError::Expired)
    ));
    state.pairing_mut(sid.as_str()).unwrap().expires_at =
        Utc::now() + chrono::Duration::seconds(60);
    state
        .join_pairing(join(&offer.pairing_code, DeviceId::new()), ip)
        .unwrap();
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), ip),
        Err(JoinError::Busy)
    ));
    assert!(state.complete_pairing(sid.as_str()).is_none());
    assert!(state.complete_pairing(sid.as_str()).is_some());
    assert!(state.complete_pairing(sid.as_str()).is_none());
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), ip),
        Err(JoinError::Busy)
    ));
    state.gc();
    assert!(state.pairing(&offer.pairing_code).is_none());
}

#[test]
fn failed_join_rate_limit_resets_after_its_window_and_abort_removes_the_code() {
    let (state, offer, sid, _) = fixture();
    let ip = "127.0.0.2".parse().unwrap();
    for _ in 0..30 {
        state.record_ip_fail(ip);
    }
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), ip),
        Err(JoinError::Attempts)
    ));
    state.ip_fails.get_mut(&ip).unwrap().1 = Instant::now() - Duration::from_secs(3601);
    assert!(state
        .join_pairing(join(&offer.pairing_code, DeviceId::new()), ip)
        .is_ok());
    assert_eq!(state.pairing(&offer.pairing_code).unwrap().session_id, sid);
    state.abort_pairing(sid.as_str());
    assert!(state.pairing(&offer.pairing_code).is_none());
    assert!(state.pairing(sid.as_str()).is_none());
    assert_eq!(state.pairing_fail.load(Ordering::Relaxed), 31);
}

#[test]
fn queue_expiry_discards_only_expired_envelopes_and_gc_counts_them() {
    let (state, offer, _, owner) = fixture();
    let now = Utc::now();
    let routing = RoutingHeader {
        version: clipsync_protocol::PROTOCOL_VERSION,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: offer.room_id,
        sender_device_id: owner,
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + chrono::Duration::seconds(60),
        item_kind: ItemKind::Text,
        seq: 1,
    };
    let envelope = QueuedEnvelope {
        routing,
        nonce: "opaque".into(),
        ciphertext: "opaque".into(),
    };
    state.queue_text(
        "expired-on-read",
        envelope.clone(),
        now - chrono::Duration::seconds(1),
    );
    assert!(state.take_queue("expired-on-read").is_none());
    state.queue_text(
        "expired-on-gc",
        envelope.clone(),
        now - chrono::Duration::seconds(1),
    );
    state.queue_text(
        "fresh",
        envelope.clone(),
        now + chrono::Duration::seconds(60),
    );
    state.gc();
    assert!(state.take_queue("expired-on-gc").is_none());
    assert_eq!(state.take_queue("fresh"), Some(envelope));
    assert_eq!(state.expired.load(Ordering::Relaxed), 2);
}
