// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use crate::join_limits::{GLOBAL_FAILED_JOIN_LIMIT, IP_FAILED_JOIN_LIMIT, IP_FAILED_JOIN_WINDOW};
use clipsync_protocol::{DeviceId, ItemKind, MessageId, Recipient, RoutingHeader, TransferId};
use std::time::Duration;

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
    for _ in 0..IP_FAILED_JOIN_LIMIT {
        state.record_ip_fail(ip);
    }
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), ip),
        Err(JoinError::Attempts)
    ));
    state.ip_fails.get_mut(&ip).unwrap().1 =
        Instant::now() - IP_FAILED_JOIN_WINDOW - Duration::from_secs(1);
    assert!(state
        .join_pairing(join(&offer.pairing_code, DeviceId::new()), ip)
        .is_ok());
    assert_eq!(state.pairing(&offer.pairing_code).unwrap().session_id, sid);
    state.abort_pairing(sid.as_str());
    assert!(state.pairing(&offer.pairing_code).is_none());
    assert!(state.pairing(sid.as_str()).is_none());
    // Recorded failures plus the aborted session; the refused join is throttled.
    assert_eq!(
        state.pairing_fail.load(Ordering::Relaxed),
        u64::from(IP_FAILED_JOIN_LIMIT) + 1
    );
    assert_eq!(state.pairing_throttled.load(Ordering::Relaxed), 1);
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

#[test]
fn gc_drops_expired_failed_join_entries() {
    let (state, _, _, _) = fixture();
    let stale: IpAddr = "192.0.2.10".parse().unwrap();
    let fresh: IpAddr = "192.0.2.11".parse().unwrap();
    state.record_ip_fail(stale);
    state.record_ip_fail(fresh);
    state.ip_fails.get_mut(&stale).unwrap().1 =
        Instant::now() - IP_FAILED_JOIN_WINDOW - Duration::from_secs(1);
    state.gc();
    assert!(state.ip_fails.get(&stale).is_none());
    assert!(state.ip_fails.get(&fresh).is_some());
}

/// A code of the same length that is never `offer`'s: the real code advanced
/// by `offset + 1` modulo the code space (wrapping past the real code is skipped).
fn wrong_code(offer: &CreatePairingResponse, offset: u32) -> String {
    let real: u32 = offer.pairing_code.parse().unwrap();
    let digits = offer.pairing_code.len();
    let modulus = 10u32.pow(digits as u32);
    let code = (real + 1 + offset % (modulus - 1)) % modulus;
    format!("{code:0digits$}")
}

#[test]
fn wrong_codes_from_one_client_exhaust_its_budget_and_refuse_its_correct_code() {
    let (state, offer, _, owner) = fixture();
    let attacker: IpAddr = "198.51.100.1".parse().unwrap();
    // Every failure kind counts, not only unknown codes.
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, owner), attacker),
        Err(JoinError::Other(_))
    ));
    for offset in 1..IP_FAILED_JOIN_LIMIT {
        assert!(matches!(
            state.join_pairing(join(&wrong_code(&offer, offset), DeviceId::new()), attacker),
            Err(JoinError::NotFound)
        ));
    }
    assert!(matches!(
        state.join_pairing(join(&wrong_code(&offer, 0), DeviceId::new()), attacker),
        Err(JoinError::Attempts)
    ));
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), attacker),
        Err(JoinError::Attempts)
    ));
    assert_eq!(
        state.ip_fails.get(&attacker).unwrap().0,
        IP_FAILED_JOIN_LIMIT,
        "refused attempts must not extend the count"
    );
    let other: IpAddr = "198.51.100.2".parse().unwrap();
    state
        .join_pairing(join(&offer.pairing_code, DeviceId::new()), other)
        .unwrap();
    assert!(state.ip_fails.get(&other).is_none(), "success is refunded");
    assert_eq!(
        state.pairing_fail.load(Ordering::Relaxed),
        u64::from(IP_FAILED_JOIN_LIMIT)
    );
}

#[test]
fn successful_joins_do_not_consume_failure_budgets() {
    let state = fixture().0;
    let client: IpAddr = "198.51.100.3".parse().unwrap();
    for _ in 0..(GLOBAL_FAILED_JOIN_LIMIT + IP_FAILED_JOIN_LIMIT) {
        let offer = state
            .create_pairing(CreatePairingRequest {
                protocol_version: clipsync_protocol::PROTOCOL_VERSION,
                pairing_session_id: PairingSessionId::new(),
                device_id: DeviceId::new(),
                device_name: "owner".into(),
                os: "test".into(),
                ttl_secs: None,
            })
            .unwrap();
        state
            .join_pairing(join(&offer.pairing_code, DeviceId::new()), client)
            .unwrap();
    }
    assert_eq!(state.global_join_fails.lock().unwrap().0, 0);
}

#[test]
fn distributed_wrong_codes_exhaust_the_relay_wide_budget_without_revealing_codes() {
    let (state, offer, _, _) = fixture();
    for n in 0..GLOBAL_FAILED_JOIN_LIMIT {
        let ip = IpAddr::from([203, 0, 113, n as u8]);
        assert!(matches!(
            state.join_pairing(join(&wrong_code(&offer, n), DeviceId::new()), ip),
            Err(JoinError::NotFound)
        ));
    }
    let fresh: IpAddr = "192.0.2.200".parse().unwrap();
    // Unknown and correct codes are refused identically once the budget is spent.
    assert!(matches!(
        state.join_pairing(join(&wrong_code(&offer, 0), DeviceId::new()), fresh),
        Err(JoinError::Attempts)
    ));
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), fresh),
        Err(JoinError::Attempts)
    ));
    assert!(
        state.ip_fails.get(&fresh).is_none(),
        "refused attempts must not allocate per-client entries"
    );
    state.global_join_fails.lock().unwrap().1 =
        Instant::now() - GLOBAL_FAILED_JOIN_WINDOW - Duration::from_secs(1);
    state
        .join_pairing(join(&offer.pairing_code, DeviceId::new()), fresh)
        .unwrap();
}

#[test]
fn ipv6_clients_are_limited_per_64_prefix_and_mapped_ipv4_as_ipv4() {
    let (state, offer, _, _) = fixture();
    for n in 0..IP_FAILED_JOIN_LIMIT {
        let ip = IpAddr::from([0x2001, 0xdb8, 0, 0x7, 0, 0, 0, n as u16 + 1]);
        assert!(matches!(
            state.join_pairing(join(&wrong_code(&offer, n), DeviceId::new()), ip),
            Err(JoinError::NotFound)
        ));
    }
    let same_prefix = IpAddr::from([0x2001, 0xdb8, 0, 0x7, 0xffff, 0, 0, 1]);
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), same_prefix),
        Err(JoinError::Attempts)
    ));
    assert_eq!(
        rate_limit_key("::ffff:192.0.2.1".parse().unwrap()),
        "192.0.2.1".parse::<IpAddr>().unwrap()
    );
    let next_prefix = IpAddr::from([0x2001, 0xdb8, 0, 0x8, 0, 0, 0, 1]);
    state
        .join_pairing(join(&offer.pairing_code, DeviceId::new()), next_prefix)
        .unwrap();
}

fn limited_state(limits: PairingJoinLimits) -> RelayState {
    RelayState::with_join_policy(
        RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "http://127.0.0.1".into(),
            token_secret: rand::random::<[u8; 32]>().to_vec(),
            webhook_url: None,
            webhook_secret: None,
        },
        Vec::new(),
        limits,
    )
    .unwrap()
}

#[test]
fn configured_join_limits_replace_the_defaults_and_refusals_are_counted() {
    const CLIENT_LIMIT: u32 = 3;
    const RELAY_LIMIT: u32 = 5;
    let state = limited_state(PairingJoinLimits {
        client_fail_limit: CLIENT_LIMIT,
        client_fail_window_secs: PAIRING_TTL_SECS,
        relay_fail_limit: RELAY_LIMIT,
    });
    let offer = state
        .create_pairing(CreatePairingRequest {
            protocol_version: clipsync_protocol::PROTOCOL_VERSION,
            pairing_session_id: PairingSessionId::new(),
            device_id: DeviceId::new(),
            device_name: "owner".into(),
            os: "test".into(),
            ttl_secs: None,
        })
        .unwrap();
    let first: IpAddr = "198.51.100.30".parse().unwrap();
    let second: IpAddr = "198.51.100.31".parse().unwrap();
    let mut offset = 0;
    let mut attempt = |ip| {
        offset += 1;
        state.join_pairing(join(&wrong_code(&offer, offset), DeviceId::new()), ip)
    };
    for _ in 0..CLIENT_LIMIT {
        assert!(matches!(attempt(first), Err(JoinError::NotFound)));
    }
    assert!(matches!(attempt(first), Err(JoinError::Attempts)));
    for _ in CLIENT_LIMIT..RELAY_LIMIT {
        assert!(matches!(attempt(second), Err(JoinError::NotFound)));
    }
    assert!(matches!(attempt(second), Err(JoinError::Attempts)));
    let metrics = state.metrics_text();
    assert!(
        metrics
            .lines()
            .any(|line| line == "clipsync_pairing_throttled 2"),
        "{metrics}"
    );
    assert!(metrics
        .lines()
        .any(|line| line == format!("clipsync_pairing_fail {RELAY_LIMIT}")));
    // GC follows the configured client window, not the default one.
    state.ip_fails.get_mut(&rate_limit_key(first)).unwrap().1 =
        Instant::now() - Duration::from_secs(PAIRING_TTL_SECS + 1);
    state.gc();
    assert!(state.ip_fails.get(&rate_limit_key(first)).is_none());
    assert!(state.ip_fails.get(&rate_limit_key(second)).is_some());
}

#[test]
fn join_limits_that_weaken_the_guessing_bound_are_refused() {
    let config = RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: rand::random::<[u8; 32]>().to_vec(),
        webhook_url: None,
        webhook_secret: None,
    };
    let weakened = PairingJoinLimits {
        relay_fail_limit: crate::join_limits::MAX_GLOBAL_FAILED_JOIN_LIMIT + 1,
        ..Default::default()
    };
    assert!(RelayState::with_join_policy(config, Vec::new(), weakened).is_err());
}
