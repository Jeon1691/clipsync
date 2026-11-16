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
    let room = offer.room_id.to_string();
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
        &room,
        "expired-on-read",
        envelope.clone(),
        now - chrono::Duration::seconds(1),
    );
    assert!(state.take_queue(&room, "expired-on-read").is_none());
    state.queue_text(
        &room,
        "expired-on-gc",
        envelope.clone(),
        now - chrono::Duration::seconds(1),
    );
    state.queue_text(
        &room,
        "fresh",
        envelope.clone(),
        now + chrono::Duration::seconds(60),
    );
    state.gc();
    assert!(state.take_queue(&room, "expired-on-gc").is_none());
    assert_eq!(state.take_queue(&room, "fresh"), Some(envelope));
    assert_eq!(state.expired.load(Ordering::Relaxed), 2);
}

fn paired_room() -> (RelayState, String, String, String) {
    let (state, offer, sid, owner) = fixture();
    let member = DeviceId::new();
    state
        .join_pairing(
            join(&offer.pairing_code, member.clone()),
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
    assert!(state.complete_pairing(sid.as_str()).is_none());
    let (_, _, room_id, _, _) = state.complete_pairing(sid.as_str()).unwrap();
    (
        state,
        room_id.to_string(),
        owner.to_string(),
        member.to_string(),
    )
}

#[test]
fn stale_connection_cleanup_cannot_remove_a_newer_member_route() {
    let (state, room, _, member) = paired_room();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let (new_tx, _new_rx) = mpsc::channel(1);
    let old = state.register_conn(&room, &member, old_tx).unwrap().id;
    let new = state
        .register_conn(&room, &member, new_tx.clone())
        .unwrap()
        .id;
    assert_ne!(old, new);

    assert!(!state.unregister_conn(&room, &member, old));
    assert!(state
        .member_sender(&room, &member)
        .unwrap()
        .same_channel(&new_tx));
    assert!(state.is_online(&room, &member));

    assert!(state.unregister_conn(&room, &member, new));
    assert!(!state.is_online(&room, &member));
    assert!(!state.unregister_conn(&room, &member, new));
}

#[tokio::test]
async fn a_replaced_member_socket_loses_its_route_and_is_evicted() {
    let (state, room, _, member) = paired_room();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let (new_tx, _new_rx) = mpsc::channel(1);
    let old = state.register_conn(&room, &member, old_tx).unwrap();
    assert!(state.owns_route(&room, &member, old.id));
    let new = state.register_conn(&room, &member, new_tx).unwrap();

    assert!(!state.owns_route(&room, &member, old.id));
    assert!(state.owns_route(&room, &member, new.id));
    // The eviction is retained until the old socket next waits for it.
    tokio::time::timeout(EVICTION_DEADLINE, old.evicted.notified())
        .await
        .expect("the replaced socket was not evicted");
    assert!(
        tokio::time::timeout(EVICTION_DEADLINE, new.evicted.notified())
            .await
            .is_err(),
        "the current socket must not be evicted"
    );
}

#[test]
fn member_routes_require_a_live_membership_and_are_scoped_per_room() {
    let (state, room, owner, member) = paired_room();
    let (tx, _rx) = mpsc::channel(1);
    assert_eq!(
        state
            .register_conn(&RoomId::new().to_string(), &member, tx.clone())
            .err(),
        Some("unknown room")
    );
    assert_eq!(
        state
            .register_conn(&room, &DeviceId::new().to_string(), tx.clone())
            .err(),
        Some("unknown device")
    );
    assert!(state.member_routes.is_empty());

    state.register_conn(&room, &member, tx.clone()).unwrap();
    assert!(state.is_online(&room, &member));
    assert!(!state.is_online(&RoomId::new().to_string(), &member));
    assert!(!state.is_online(&room, &owner));
}

#[tokio::test]
async fn revocation_drops_the_route_evicts_its_socket_and_blocks_reregistration() {
    let (state, room, _, member) = paired_room();
    let (tx, _rx) = mpsc::channel(1);
    let registration = state.register_conn(&room, &member, tx.clone()).unwrap();

    assert!(state.revoke(&room, &member));
    assert!(!state.is_online(&room, &member));
    assert!(!state.owns_route(&room, &member, registration.id));
    tokio::time::timeout(EVICTION_DEADLINE, registration.evicted.notified())
        .await
        .expect("the revoked socket was not evicted");
    assert_eq!(
        state.register_conn(&room, &member, tx).err(),
        Some("device revoked")
    );
    assert!(!state.revoke(&room, &DeviceId::new().to_string()));
    assert!(!state.revoke(&RoomId::new().to_string(), &member));
}

#[test]
fn queued_text_retention_is_capped_whatever_expiry_the_sender_chose() {
    let (state, room, _, member) = paired_room();
    let sent_at = Utc::now();
    // Far beyond the relay's retention cap, as a misbehaving sender may claim.
    let claimed_expiry = sent_at + chrono::Duration::days(SENDER_CLAIMED_EXPIRY_DAYS);
    state.queue_text(
        &room,
        &member,
        queued_envelope(&room, &member),
        claimed_expiry,
    );
    let (_, retained_until) = state
        .queue
        .get(&member_key(&room, &member))
        .map(|entry| entry.value().clone())
        .unwrap();
    let cap = chrono::Duration::seconds(QUEUE_TTL_SECS as i64);
    assert!(retained_until <= Utc::now() + cap);
    assert!(retained_until >= sent_at + cap);
}

// Evictions are signalled synchronously, so an absent one is decided quickly.
const EVICTION_DEADLINE: std::time::Duration = std::time::Duration::from_millis(50);
// Any claim well past the relay's retention cap exercises the clamp.
const SENDER_CLAIMED_EXPIRY_DAYS: i64 = 30;

fn queued_envelope(room: &str, sender: &str) -> QueuedEnvelope {
    let now = Utc::now();
    QueuedEnvelope {
        routing: RoutingHeader {
            version: clipsync_protocol::PROTOCOL_VERSION,
            message_id: MessageId::new(),
            transfer_id: TransferId::new(),
            room_id: RoomId::from(room),
            sender_device_id: DeviceId::from(sender),
            recipient: Recipient::Broadcast,
            sent_at: now,
            expires_at: now,
            item_kind: ItemKind::Text,
            seq: 1,
        },
        nonce: "opaque".into(),
        ciphertext: "opaque".into(),
    }
}
