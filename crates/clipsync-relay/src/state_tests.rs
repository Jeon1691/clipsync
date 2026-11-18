// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use crate::join_limits::{
    GLOBAL_FAILED_JOIN_LIMIT, GLOBAL_FAILED_JOIN_WINDOW, IP_FAILED_JOIN_LIMIT,
    IP_FAILED_JOIN_WINDOW,
};
use crate::resource_limits::{
    MAX_ROOMS, PAIRING_CREATE_CLIENT_LIMIT, PAIRING_CREATE_CLIENT_WINDOW,
    PAIRING_CREATE_RELAY_LIMIT, PAIRING_CREATE_RELAY_WINDOW,
};
use clipsync_protocol::{DeviceId, ItemKind, MessageId, Recipient, RoutingHeader, TransferId};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

/// Sender of envelopes queued directly in tests.
const SENDER: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 30));

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
        metrics_token: None,
    });
    let sid = PairingSessionId::new();
    let owner = DeviceId::new();
    let offer = state
        .create_pairing(
            CreatePairingRequest {
                protocol_version: clipsync_protocol::PROTOCOL_VERSION,
                pairing_session_id: sid.clone(),
                device_id: owner.clone(),
                device_name: "owner".into(),
                os: "test".into(),
                ttl_secs: None,
            },
            std::net::Ipv4Addr::LOCALHOST.into(),
        )
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
        session.expires_at = Timestamp::now() - time::Duration::seconds(1);
    }
    assert!(matches!(
        state.join_pairing(join(&offer.pairing_code, DeviceId::new()), ip),
        Err(JoinError::Expired)
    ));
    state.pairing_mut(sid.as_str()).unwrap().expires_at =
        Timestamp::now() + time::Duration::seconds(60);
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
    state.join_budget.clients.get_mut(&ip).unwrap().1 =
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
    let room = offer.room_id.to_string();
    let now = Timestamp::now();
    let routing = RoutingHeader {
        version: clipsync_protocol::PROTOCOL_VERSION,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: offer.room_id,
        sender_device_id: owner,
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + time::Duration::seconds(60),
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
        now - time::Duration::seconds(1),
        SENDER,
    );
    assert!(state.take_queue(&room, "expired-on-read").is_none());
    state.queue_text(
        &room,
        "expired-on-gc",
        envelope.clone(),
        now - time::Duration::seconds(1),
        SENDER,
    );
    state.queue_text(
        &room,
        "fresh",
        envelope.clone(),
        now + time::Duration::seconds(60),
        SENDER,
    );
    state.gc();
    assert!(state.take_queue(&room, "expired-on-gc").is_none());
    assert_eq!(state.take_queue(&room, "fresh"), Some(envelope));
    assert_eq!(state.expired.load(Ordering::Relaxed), 2);
}

#[test]
fn gc_drops_expired_failed_join_entries() {
    let (state, _, _, _) = fixture();
    let stale: IpAddr = "192.0.2.10".parse().unwrap();
    let fresh: IpAddr = "192.0.2.11".parse().unwrap();
    state.record_ip_fail(stale);
    state.record_ip_fail(fresh);
    state.join_budget.clients.get_mut(&stale).unwrap().1 =
        Instant::now() - IP_FAILED_JOIN_WINDOW - Duration::from_secs(1);
    state.gc();
    assert!(state.join_budget.clients.get(&stale).is_none());
    assert!(state.join_budget.clients.get(&fresh).is_some());
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
        state.join_budget.clients.get(&attacker).unwrap().0,
        IP_FAILED_JOIN_LIMIT,
        "refused attempts must not extend the count"
    );
    let other: IpAddr = "198.51.100.2".parse().unwrap();
    state
        .join_pairing(join(&offer.pairing_code, DeviceId::new()), other)
        .unwrap();
    assert!(
        state.join_budget.clients.get(&other).is_none(),
        "success is refunded"
    );
    assert_eq!(
        state.pairing_fail.load(Ordering::Relaxed),
        u64::from(IP_FAILED_JOIN_LIMIT)
    );
}

#[test]
fn successful_joins_do_not_consume_failure_budgets() {
    let state = fixture().0;
    let client: IpAddr = "198.51.100.3".parse().unwrap();
    for n in 0..(GLOBAL_FAILED_JOIN_LIMIT + IP_FAILED_JOIN_LIMIT) {
        // Each offer from its own owner address, within the creation limits.
        let owner = IpAddr::from([192, 0, 2, n as u8]);
        let offer = state.create_pairing(create_request(), owner).unwrap();
        state
            .join_pairing(join(&offer.pairing_code, DeviceId::new()), client)
            .unwrap();
    }
    assert_eq!(state.join_budget.relay.0.lock().unwrap().0, 0);
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
        state.join_budget.clients.get(&fresh).is_none(),
        "refused attempts must not allocate per-client entries"
    );
    state.join_budget.relay.0.lock().unwrap().1 =
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
        crate::rate_limit::client_key("::ffff:192.0.2.1".parse().unwrap()),
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
            metrics_token: None,
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
        .create_pairing(
            CreatePairingRequest {
                protocol_version: clipsync_protocol::PROTOCOL_VERSION,
                pairing_session_id: PairingSessionId::new(),
                device_id: DeviceId::new(),
                device_name: "owner".into(),
                os: "test".into(),
                ttl_secs: None,
            },
            std::net::Ipv4Addr::LOCALHOST.into(),
        )
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
    state
        .join_budget
        .clients
        .get_mut(&crate::rate_limit::client_key(first))
        .unwrap()
        .1 = Instant::now() - Duration::from_secs(PAIRING_TTL_SECS + 1);
    state.gc();
    assert!(state
        .join_budget
        .clients
        .get(&crate::rate_limit::client_key(first))
        .is_none());
    assert!(state
        .join_budget
        .clients
        .get(&crate::rate_limit::client_key(second))
        .is_some());
}

#[test]
fn join_limits_that_weaken_the_guessing_bound_are_refused() {
    let config = RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: rand::random::<[u8; 32]>().to_vec(),
        webhook_url: None,
        webhook_secret: None,
        metrics_token: None,
    };
    let weakened = PairingJoinLimits {
        relay_fail_limit: crate::join_limits::MAX_GLOBAL_FAILED_JOIN_LIMIT + 1,
        ..Default::default()
    };
    assert!(RelayState::with_join_policy(config, Vec::new(), weakened).is_err());
}

fn create_request() -> CreatePairingRequest {
    CreatePairingRequest {
        protocol_version: clipsync_protocol::PROTOCOL_VERSION,
        pairing_session_id: PairingSessionId::new(),
        device_id: DeviceId::new(),
        device_name: "owner".into(),
        os: "test".into(),
        ttl_secs: None,
    }
}

fn resource_limited_state(limits: RelayResourceLimits) -> RelayState {
    RelayState::with_limits(
        RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "http://127.0.0.1".into(),
            token_secret: rand::random::<[u8; 32]>().to_vec(),
            webhook_url: None,
            webhook_secret: None,
            metrics_token: None,
        },
        Vec::new(),
        PairingJoinLimits::default(),
        limits,
    )
    .unwrap()
}

/// The `n`th of many distinct client addresses (10.0.0.0/8).
fn client(n: usize) -> IpAddr {
    let [_, a, b, c] = (n as u32).to_be_bytes();
    IpAddr::from([10, a, b, c])
}

/// Regression for unlimited pairing creation: one client, then many clients,
/// must run out of creation budget, and refusals must not allocate anything.
#[test]
fn pairing_creation_is_rate_limited_per_client_and_relay_wide() {
    let state = RelayState::new(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: rand::random::<[u8; 32]>().to_vec(),
        webhook_url: None,
        webhook_secret: None,
        metrics_token: None,
    });
    let attacker: IpAddr = "198.51.100.60".parse().unwrap();
    for _ in 0..PAIRING_CREATE_CLIENT_LIMIT {
        state.create_pairing(create_request(), attacker).unwrap();
    }
    assert!(matches!(
        state.create_pairing(create_request(), attacker),
        Err(CreateError::Throttled)
    ));
    assert_eq!(
        state.pairing_by_code.len(),
        PAIRING_CREATE_CLIENT_LIMIT as usize
    );
    // Every address in one IPv6 /64 is the same client; the next /64 is not.
    let v6 = |host: u16| IpAddr::from([0x2001, 0xdb8, 0, 0x60, host, 0, 0, 1]);
    for host in 0..PAIRING_CREATE_CLIENT_LIMIT {
        state
            .create_pairing(create_request(), v6(host as u16))
            .unwrap();
    }
    assert!(matches!(
        state.create_pairing(create_request(), v6(u16::MAX)),
        Err(CreateError::Throttled)
    ));
    let next_prefix = IpAddr::from([0x2001, 0xdb8, 0, 0x61, 0, 0, 0, 1]);
    state.create_pairing(create_request(), next_prefix).unwrap();

    // Distributed creation exhausts the relay-wide budget.
    let used = 2 * PAIRING_CREATE_CLIENT_LIMIT + 1;
    for n in 0..(PAIRING_CREATE_RELAY_LIMIT - used) as usize {
        state.create_pairing(create_request(), client(n)).unwrap();
    }
    let fresh = client(PAIRING_CREATE_RELAY_LIMIT as usize);
    assert!(matches!(
        state.create_pairing(create_request(), fresh),
        Err(CreateError::Throttled)
    ));
    assert_eq!(
        state.pairing_by_code.len(),
        PAIRING_CREATE_RELAY_LIMIT as usize
    );
    assert_eq!(state.pairing_create_throttled.load(Ordering::Relaxed), 3);
    assert!(state
        .metrics_text()
        .lines()
        .any(|line| line == "clipsync_pairing_create_throttled 3"));

    // Both budgets reset with their windows, and GC drops stale clients.
    state.create_budget.relay.0.lock().unwrap().1 =
        Instant::now() - PAIRING_CREATE_RELAY_WINDOW - Duration::from_secs(1);
    state.create_pairing(create_request(), fresh).unwrap();
    let key = crate::rate_limit::client_key(attacker);
    state.create_budget.clients.get_mut(&key).unwrap().1 =
        Instant::now() - PAIRING_CREATE_CLIENT_WINDOW - Duration::from_secs(1);
    state.gc();
    assert!(state.create_budget.clients.get(&key).is_none());
    state.create_pairing(create_request(), attacker).unwrap();
}

/// Let the next creations through both budgets, as if each came in a fresh window.
fn reset_create_budget(state: &RelayState) {
    state.create_budget.clients.clear();
    state.create_budget.relay.0.lock().unwrap().0 = 0;
}

/// Regression for pairing-code exhaustion: open sessions stop at a cap far
/// below the code space, with a distinct `Full` refusal, and expired sessions
/// never hold the relay full.
#[test]
fn open_pairing_sessions_are_capped_well_below_the_code_space() {
    let state = resource_limited_state(RelayResourceLimits {
        max_rooms: MAX_ACTIVE_PAIRING_SESSIONS * 2,
        ..RelayResourceLimits::default()
    });
    for n in 0..MAX_ACTIVE_PAIRING_SESSIONS {
        reset_create_budget(&state);
        state.create_pairing(create_request(), client(n)).unwrap();
    }
    reset_create_budget(&state);
    let next = client(MAX_ACTIVE_PAIRING_SESSIONS);
    assert!(matches!(
        state.create_pairing(create_request(), next),
        Err(CreateError::Full)
    ));
    assert_eq!(state.pairing_by_code.len(), MAX_ACTIVE_PAIRING_SESSIONS);
    assert_eq!(state.pairing_by_id.len(), MAX_ACTIVE_PAIRING_SESSIONS);
    assert_eq!(state.pairing_create_full.load(Ordering::Relaxed), 1);
    // A capacity refusal does not spend the client's budget.
    assert!(state
        .create_budget
        .clients
        .get(&crate::rate_limit::client_key(next))
        .is_none());

    // An expired session frees its slot at once, without waiting for GC.
    state.pairing_by_code.iter_mut().next().unwrap().expires_at =
        Timestamp::now() - time::Duration::seconds(1);
    state.create_pairing(create_request(), next).unwrap();
    assert_eq!(state.pairing_by_code.len(), MAX_ACTIVE_PAIRING_SESSIONS);
    assert_eq!(state.pairing_by_id.len(), MAX_ACTIVE_PAIRING_SESSIONS);
}

/// Join and bind both sides of `offer`; returns the issued device tokens.
fn complete(state: &RelayState, offer: &CreatePairingResponse, sid: &str) -> [String; 2] {
    state
        .join_pairing(
            join(&offer.pairing_code, DeviceId::new()),
            Ipv4Addr::LOCALHOST.into(),
        )
        .unwrap();
    assert!(state.complete_pairing(sid).is_none());
    let (owner, member, ..) = state.complete_pairing(sid).unwrap();
    [owner, member]
}

/// Rooms are capped too: open sessions that could still complete count
/// against the cap, so rooms never exceed it. Sessions alone fill it; a room
/// whose devices are offline makes way for a new session.
#[test]
fn open_sessions_count_against_the_room_cap_and_idle_rooms_make_way() {
    const MAX_ROOMS: usize = 2;
    let state = small_room_state(MAX_ROOMS);
    let ip: IpAddr = Ipv4Addr::LOCALHOST.into();
    let first = create_request();
    let sid = first.pairing_session_id.clone();
    let offer = state.create_pairing(first, ip).unwrap();
    state.create_pairing(create_request(), ip).unwrap();
    assert!(matches!(
        state.create_pairing(create_request(), ip),
        Err(CreateError::Full)
    ));
    complete(&state, &offer, sid.as_str());
    state.gc();
    assert_eq!(state.rooms.len(), 1);
    state.create_pairing(create_request(), ip).unwrap();
    assert_eq!(state.rooms.len(), 0);
    assert_eq!(state.pairing_by_code.len(), MAX_ROOMS);
    assert!(matches!(
        state.create_pairing(create_request(), ip),
        Err(CreateError::Full)
    ));
}

/// Regression for unbounded growth of the session-ID index: GC must remove
/// it together with the code index, for expired, completed and aborted
/// sessions alike, without touching a live session that reused the ID.
#[test]
fn gc_prunes_pairing_by_id_with_expired_completed_and_aborted_sessions() {
    let (state, _, expired_sid, _) = fixture();
    let ip: IpAddr = Ipv4Addr::LOCALHOST.into();
    state.pairing_mut(expired_sid.as_str()).unwrap().expires_at =
        Timestamp::now() - time::Duration::seconds(1);
    let completed = create_request();
    let completed_sid = completed.pairing_session_id.clone();
    let completed_offer = state.create_pairing(completed, ip).unwrap();
    complete(&state, &completed_offer, completed_sid.as_str());
    let aborted = create_request();
    let aborted_sid = aborted.pairing_session_id.clone();
    state.create_pairing(aborted, ip).unwrap();
    state.abort_pairing(aborted_sid.as_str());
    let live = create_request();
    let live_sid = live.pairing_session_id.clone();
    state.create_pairing(live, ip).unwrap();
    assert_eq!(state.pairing_by_id.len(), 3);

    state.gc();
    assert_eq!(state.pairing_by_code.len(), 1);
    assert_eq!(state.pairing_by_id.len(), 1);
    assert!(state.pairing(live_sid.as_str()).is_some());
    for sid in [&expired_sid, &completed_sid, &aborted_sid] {
        assert!(state.pairing_by_id.get(sid.as_str()).is_none());
    }

    // A session whose ID was reused by a newer one keeps the newer mapping
    // when the older session expires.
    let mut reused = create_request();
    reused.pairing_session_id = live_sid.clone();
    let newer = state.create_pairing(reused, ip).unwrap();
    let older_code = state
        .pairing_by_code
        .iter()
        .find(|entry| *entry.key() != newer.pairing_code)
        .map(|entry| entry.key().clone())
        .unwrap();
    state
        .pairing_by_code
        .get_mut(&older_code)
        .unwrap()
        .expires_at = Timestamp::now() - time::Duration::seconds(1);
    state.gc();
    assert_eq!(
        state.pairing(live_sid.as_str()).unwrap().code,
        newer.pairing_code
    );
}

#[test]
fn gc_drops_only_rooms_that_can_never_be_used_again() {
    let (state, _, _, _) = fixture();
    let state = Arc::new(state);
    let ip: IpAddr = Ipv4Addr::LOCALHOST.into();
    let mut rooms = Vec::new();
    for _ in 0..4 {
        let request = create_request();
        let sid = request.pairing_session_id.clone();
        let offer = state.create_pairing(request, ip).unwrap();
        complete(&state, &offer, sid.as_str());
        rooms.push(offer.room_id.as_str().to_string());
    }
    let [live, revoked, expired, expired_online] = rooms.try_into().unwrap();
    let devices =
        |room: &str| -> Vec<String> { state.room(room).unwrap().devices.keys().cloned().collect() };
    for device in devices(&revoked) {
        assert!(state.revoke(&revoked, &device));
    }
    for room in [&expired, &expired_online] {
        state.room_mut(room).unwrap().tokens_expire_at =
            Timestamp::now() - time::Duration::seconds(1);
    }
    let (tx, _rx) = mpsc::channel(1);
    let _online = state
        .register_conn(&expired_online, &devices(&expired_online)[0], tx)
        .unwrap();
    // One revoked device does not end a room.
    let live_devices = devices(&live);
    assert!(state.revoke(&live, &live_devices[0]));

    state.gc();
    assert!(state.room(&live).is_some());
    assert!(
        state.room(&expired_online).is_some(),
        "a connected device keeps its room"
    );
    assert!(state.room(&revoked).is_none());
    assert!(state.room(&expired).is_none());
    assert_eq!(state.rooms.len(), 2);
}

#[test]
fn completed_rooms_outlive_every_device_token_issued_for_them() {
    let (state, offer, sid, _) = fixture();
    let tokens = complete(&state, &offer, sid.as_str());
    let room = state.room(offer.room_id.as_str()).unwrap();
    let lifetime = room.tokens_expire_at - Timestamp::now();
    assert!(lifetime > time::Duration::seconds(DEVICE_TOKEN_TTL_SECS - 60));
    assert!(lifetime <= time::Duration::seconds(DEVICE_TOKEN_TTL_SECS + TOKEN_EXPIRY_GRACE_SECS));
    for token in tokens {
        let claims = crate::auth::verify_token(&state.config.token_secret, &token).unwrap();
        // A token is valid through its `exp` second.
        assert!(room.tokens_expire_at.unix_timestamp() > claims.exp);
    }
}

fn envelope(id: &str) -> QueuedEnvelope {
    let now = Timestamp::now();
    QueuedEnvelope {
        routing: RoutingHeader {
            version: clipsync_protocol::PROTOCOL_VERSION,
            message_id: MessageId::from(id),
            transfer_id: TransferId::new(),
            room_id: RoomId::new(),
            sender_device_id: DeviceId::new(),
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

fn routed(state: &RelayState, room: &str, member: &str) -> Vec<ConnectionId> {
    state
        .member_senders(room, member)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[test]
fn closing_one_socket_keeps_the_member_s_other_sockets_routed() {
    let (state, room, _, member) = paired_room();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let (new_tx, _new_rx) = mpsc::channel(1);
    let old = state.register_conn(&room, &member, old_tx).unwrap().id;
    let new = state
        .register_conn(&room, &member, new_tx.clone())
        .unwrap()
        .id;
    assert_ne!(old, new);
    assert_eq!(routed(&state, &room, &member), [old, new]);

    assert!(state.unregister_conn(&room, &member, old));
    assert!(!state.unregister_conn(&room, &member, old));
    let senders = state.member_senders(&room, &member);
    assert_eq!(senders.len(), 1);
    assert!(senders[0].1.same_channel(&new_tx));
    assert!(state.is_online(&room, &member));

    assert!(state.unregister_conn(&room, &member, new));
    assert!(!state.is_online(&room, &member));
    assert!(state.member_routes.is_empty());
    assert!(!state.unregister_conn(&room, &member, new));
}

#[tokio::test]
async fn concurrent_sockets_of_a_member_are_all_routed_and_none_is_evicted() {
    let (state, room, _, member) = paired_room();
    let (daemon_tx, _daemon_rx) = mpsc::channel(1);
    let (command_tx, _command_rx) = mpsc::channel(1);
    let daemon = state.register_conn(&room, &member, daemon_tx).unwrap();
    let command = state.register_conn(&room, &member, command_tx).unwrap();

    assert!(state.owns_route(&room, &member, daemon.id));
    assert!(state.owns_route(&room, &member, command.id));
    for registration in [&daemon, &command] {
        assert!(
            tokio::time::timeout(EVICTION_DEADLINE, registration.evicted.notified())
                .await
                .is_err(),
            "a concurrent socket of the same member must not be evicted"
        );
    }
}

#[tokio::test]
async fn registering_past_the_per_member_cap_evicts_only_the_oldest_socket() {
    let (state, room, _, member) = paired_room();
    let mut receivers = Vec::new();
    let mut registrations = Vec::new();
    for _ in 0..=MAX_MEMBER_SOCKETS {
        let (tx, rx) = mpsc::channel(1);
        receivers.push(rx);
        registrations.push(state.register_conn(&room, &member, tx).unwrap());
    }

    let oldest = registrations.remove(0);
    assert!(!state.owns_route(&room, &member, oldest.id));
    tokio::time::timeout(EVICTION_DEADLINE, oldest.evicted.notified())
        .await
        .expect("the oldest socket past the cap was not evicted");
    assert_eq!(
        routed(&state, &room, &member),
        registrations.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    for registration in &registrations {
        assert!(
            tokio::time::timeout(EVICTION_DEADLINE, registration.evicted.notified())
                .await
                .is_err(),
            "only the oldest socket may be evicted"
        );
    }
    // The evicted socket's own cleanup later finds nothing to remove.
    assert!(!state.unregister_conn(&room, &member, oldest.id));
    assert_eq!(routed(&state, &room, &member).len(), MAX_MEMBER_SOCKETS);
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
async fn revocation_drops_every_route_evicts_every_socket_and_blocks_reregistration() {
    let (state, room, owner, member) = paired_room();
    let (tx, _rx) = mpsc::channel(1);
    let registrations: Vec<_> = (0..MAX_MEMBER_SOCKETS)
        .map(|_| state.register_conn(&room, &member, tx.clone()).unwrap())
        .collect();
    let owner_registration = state.register_conn(&room, &owner, tx.clone()).unwrap();

    assert!(state.revoke(&room, &member));
    assert!(!state.is_online(&room, &member));
    for registration in &registrations {
        assert!(!state.owns_route(&room, &member, registration.id));
        tokio::time::timeout(EVICTION_DEADLINE, registration.evicted.notified())
            .await
            .expect("a revoked member's socket was not evicted");
    }
    assert!(state.owns_route(&room, &owner, owner_registration.id));
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
    let sent_at = Timestamp::now();
    // Far beyond the relay's retention cap, as a misbehaving sender may claim.
    let claimed_expiry = sent_at + time::Duration::days(SENDER_CLAIMED_EXPIRY_DAYS);
    assert!(state.queue_text(
        &room,
        &member,
        queued_envelope(&room, &member),
        claimed_expiry,
        SENDER,
    ));
    let retained_until = state
        .queue
        .get(&member_key(&room, &member))
        .map(|entry| entry.expires_at)
        .unwrap();
    let cap = time::Duration::seconds(QUEUE_TTL_SECS as i64);
    assert!(retained_until <= Timestamp::now() + cap);
    assert!(retained_until >= sent_at + cap);
}

// Evictions are signalled synchronously, so an absent one is decided quickly.
const EVICTION_DEADLINE: std::time::Duration = std::time::Duration::from_millis(50);
// Any claim well past the relay's retention cap exercises the clamp.
const SENDER_CLAIMED_EXPIRY_DAYS: i64 = 30;

fn queued_envelope(room: &str, sender: &str) -> QueuedEnvelope {
    let now = Timestamp::now();
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

/// The room the queue-cap tests queue into; the caps are relay-wide.
const QUEUE_ROOM: &str = "queue-room";

#[test]
fn the_offline_queue_is_capped_and_never_outlives_the_queue_ttl() {
    let (state, _, _, _) = fixture();
    let far_future = Timestamp::now() + time::Duration::days(365);
    // Spread over enough senders that no one reaches its own share.
    let sender = |n: usize| client(n / MAX_CLIENT_QUEUED_TEXT_ENVELOPES);
    for n in 0..MAX_QUEUED_TEXT_ENVELOPES {
        assert!(state.queue_text(
            QUEUE_ROOM,
            &format!("device-{n}"),
            envelope("m"),
            far_future,
            sender(n)
        ));
    }
    let fresh = client(MAX_QUEUED_TEXT_ENVELOPES);
    assert!(!state.queue_text(QUEUE_ROOM, "one-too-many", envelope("m"), far_future, fresh));
    assert_eq!(state.queue_full.load(Ordering::Relaxed), 1);
    assert_eq!(state.queue.len(), MAX_QUEUED_TEXT_ENVELOPES);
    // Replacing a recipient's envelope does not grow the queue.
    assert!(state.queue_text(
        QUEUE_ROOM,
        "device-0",
        envelope("newer"),
        far_future,
        sender(0)
    ));
    let retention = Timestamp::now() + time::Duration::seconds(QUEUE_TTL_SECS as i64);
    assert!(
        state
            .queue
            .get(&member_key(QUEUE_ROOM, "device-0"))
            .unwrap()
            .expires_at
            <= retention
    );
    // An expired envelope frees its slot without waiting for GC.
    state
        .queue
        .get_mut(&member_key(QUEUE_ROOM, "device-1"))
        .unwrap()
        .expires_at = Timestamp::now() - time::Duration::seconds(1);
    assert!(state.queue_text(QUEUE_ROOM, "one-too-many", envelope("m"), far_future, fresh));
    assert_eq!(state.queue.len(), MAX_QUEUED_TEXT_ENVELOPES);
    let charged: usize = state.queue_senders.iter().map(|held| *held).sum();
    assert_eq!(charged, state.queue.len(), "every envelope is charged once");
}

/// Regression for one client filling the relay-wide offline queue with
/// self-made rooms: its share is capped, replacing its own envelope stays
/// free, and other clients can still queue.
#[test]
fn regression_one_client_cannot_fill_the_relay_wide_offline_queue() {
    let (state, _, _, _) = fixture();
    let far_future = Timestamp::now() + time::Duration::days(365);
    let attacker: IpAddr = "2001:db8:91::1".parse().unwrap();
    let queued = (0..MAX_QUEUED_TEXT_ENVELOPES)
        .filter(|n| {
            // Any address in the attacker's /64 is the same sender.
            let mut octets = match attacker {
                IpAddr::V6(v6) => v6.octets(),
                IpAddr::V4(_) => unreachable!(),
            };
            octets[15] = *n as u8;
            state.queue_text(
                QUEUE_ROOM,
                &format!("attacker-peer-{n}"),
                envelope("m"),
                far_future,
                IpAddr::from(octets),
            )
        })
        .count();
    assert_eq!(queued, MAX_CLIENT_QUEUED_TEXT_ENVELOPES);
    assert!(state.queue_text(
        QUEUE_ROOM,
        "attacker-peer-0",
        envelope("newer"),
        far_future,
        attacker
    ));
    let victim: IpAddr = "203.0.113.91".parse().unwrap();
    assert!(state.queue_text(QUEUE_ROOM, "victim-peer", envelope("m"), far_future, victim));
    // A delivered envelope returns its sender's share.
    assert!(state.take_queue(QUEUE_ROOM, "attacker-peer-0").is_some());
    assert!(state.queue_text(
        QUEUE_ROOM,
        "attacker-peer-next",
        envelope("m"),
        far_future,
        attacker
    ));
    assert!(!state.queue_text(
        QUEUE_ROOM,
        "attacker-peer-over",
        envelope("m"),
        far_future,
        attacker
    ));
    // So does an envelope that expires.
    state
        .queue
        .get_mut(&member_key(QUEUE_ROOM, "attacker-peer-1"))
        .unwrap()
        .expires_at = Timestamp::now() - time::Duration::seconds(1);
    state.gc();
    assert!(state.queue_text(
        QUEUE_ROOM,
        "attacker-peer-over",
        envelope("m"),
        far_future,
        attacker
    ));
}

/// A state with `limits` applied without validation, for room caps far below
/// what a relay accepts at startup.
fn small_room_state(max_rooms: usize) -> Arc<RelayState> {
    Arc::new(RelayState::build(
        RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "http://127.0.0.1".into(),
            token_secret: rand::random::<[u8; 32]>().to_vec(),
            webhook_url: None,
            webhook_secret: None,
            metrics_token: None,
        },
        Vec::new(),
        PairingJoinLimits::default(),
        RelayResourceLimits {
            max_rooms,
            ..RelayResourceLimits::default()
        },
    ))
}

/// Pair two fresh devices into a new room; returns its ID and one device.
fn pair_room_in(state: &RelayState) -> (String, String) {
    reset_create_budget(state);
    let request = create_request();
    let sid = request.pairing_session_id.clone();
    let owner = request.device_id.as_str().to_string();
    let offer = state
        .create_pairing(request, Ipv4Addr::LOCALHOST.into())
        .unwrap();
    complete(state, &offer, sid.as_str());
    state.gc();
    (offer.room_id.as_str().to_string(), owner)
}

/// A room with a device online is never evicted, and a cap where every room
/// has one refuses with `Full`; the room whose devices left makes way, and
/// its devices then get `unknown room`.
#[test]
fn rooms_with_a_device_online_are_never_evicted() {
    const MAX_ROOMS: usize = 3;
    let state = small_room_state(MAX_ROOMS);
    let rooms: Vec<_> = (0..MAX_ROOMS).map(|_| pair_room_in(&state)).collect();
    let (tx, _rx) = mpsc::channel(1);
    let online: Vec<_> = rooms
        .iter()
        .map(|(room, device)| state.register_conn(room, device, tx.clone()).unwrap())
        .collect();
    reset_create_budget(&state);
    assert!(matches!(
        state.create_pairing(create_request(), Ipv4Addr::LOCALHOST.into()),
        Err(CreateError::Full)
    ));
    assert_eq!(state.rooms.len(), MAX_ROOMS);
    state.gc();
    assert_eq!(state.rooms.len(), MAX_ROOMS, "GC keeps online rooms");

    let (left, left_device) = &rooms[1];
    assert!(state.unregister_conn(left, left_device, online[1].id));
    reset_create_budget(&state);
    state
        .create_pairing(create_request(), Ipv4Addr::LOCALHOST.into())
        .unwrap();
    assert!(state.room(left).is_none());
    assert!(state.room(&rooms[0].0).is_some() && state.room(&rooms[2].0).is_some());
    assert_eq!(state.rooms_evicted.load(Ordering::Relaxed), 1);
    assert!(state
        .metrics_text()
        .lines()
        .any(|line| line == "clipsync_rooms_evicted 1"));
    assert_eq!(
        state.register_conn(left, left_device, tx).err(),
        Some("unknown room")
    );
}

/// A member may hold several routed sockets (daemon, CLI commands, reconnect
/// overlap). Its room stays online, and so is never evicted, until the last
/// of them has closed.
#[test]
fn a_room_stays_unevictable_until_its_members_last_socket_closes() {
    const MAX_ROOMS: usize = 1;
    let state = small_room_state(MAX_ROOMS);
    let (room, device) = pair_room_in(&state);
    let (tx, _rx) = mpsc::channel(1);
    let sockets: Vec<_> = (0..MAX_MEMBER_SOCKETS)
        .map(|_| state.register_conn(&room, &device, tx.clone()).unwrap())
        .collect();
    let (newest, older) = sockets.split_last().unwrap();
    for socket in older {
        assert!(state.unregister_conn(&room, &device, socket.id));
        reset_create_budget(&state);
        assert!(matches!(
            state.create_pairing(create_request(), Ipv4Addr::LOCALHOST.into()),
            Err(CreateError::Full)
        ));
        assert!(
            state.room(&room).is_some(),
            "a routed socket keeps its room"
        );
    }
    assert!(state.unregister_conn(&room, &device, newest.id));
    reset_create_budget(&state);
    state
        .create_pairing(create_request(), Ipv4Addr::LOCALHOST.into())
        .unwrap();
    assert!(state.room(&room).is_none());
    assert_eq!(state.rooms_evicted.load(Ordering::Relaxed), 1);
}

/// At the room cap the least recently used idle room goes first: activity
/// (a device attaching, sending or leaving) moves a room to the back.
#[test]
fn room_eviction_follows_least_recent_use() {
    const MAX_ROOMS: usize = 3;
    let state = small_room_state(MAX_ROOMS);
    let [(oldest, oldest_device), (middle, _), (newest, _)]: [(String, String); MAX_ROOMS] = (0
        ..MAX_ROOMS)
        .map(|_| pair_room_in(&state))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    // The oldest room is used again: a device registers, sends and leaves.
    let (tx, _rx) = mpsc::channel(1);
    let registration = state.register_conn(&oldest, &oldest_device, tx).unwrap();
    let presence = RoomPresence::new(&state, &oldest);
    presence.touch();
    assert!(state.unregister_conn(&oldest, &oldest_device, registration.id));
    drop(presence);

    let evicted_in_order: Vec<_> = (0..MAX_ROOMS)
        .map(|_| {
            let before: Vec<String> = [&oldest, &middle, &newest]
                .into_iter()
                .filter(|room| state.room(room).is_some())
                .cloned()
                .collect();
            pair_room_in(&state);
            before
                .into_iter()
                .find(|room| state.room(room).is_none())
                .expect("each new room evicts one idle room")
        })
        .collect();
    assert_eq!(evicted_in_order, [middle, newest, oldest]);
    assert_eq!(state.rooms.len(), MAX_ROOMS);
}

#[test]
fn oversized_pairing_fields_are_refused_before_any_budget_or_session() {
    let (state, offer, _, _) = fixture();
    let ip: IpAddr = Ipv4Addr::LOCALHOST.into();
    let long = "x".repeat(MAX_PAIRING_FIELD_BYTES + 1);
    let mut request = create_request();
    request.device_name = long.clone();
    assert!(matches!(
        state.create_pairing(request, ip),
        Err(CreateError::Invalid(_))
    ));
    let mut request = create_request();
    request.pairing_session_id = PairingSessionId::from(long.as_str());
    assert!(matches!(
        state.create_pairing(request, ip),
        Err(CreateError::Invalid(_))
    ));
    let mut request = join(&offer.pairing_code, DeviceId::new());
    request.os = long;
    assert!(matches!(
        state.join_pairing(request, ip),
        Err(JoinError::Other(_))
    ));
    assert_eq!(state.pairing_by_code.len(), 1);
    assert!(state.join_budget.clients.is_empty());
    let mut request = create_request();
    request.device_name = "x".repeat(MAX_PAIRING_FIELD_BYTES);
    state.create_pairing(request, ip).unwrap();
}

/// Concurrent creations serialize on the capacity check: exactly the room cap
/// succeeds, however the threads interleave.
#[test]
fn concurrent_creations_never_overshoot_the_cap() {
    const MAX_ROOMS: usize = 7;
    const THREADS: usize = 16;
    let state = small_room_state(MAX_ROOMS);
    let created = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let (state, created) = (&state, &created);
            scope.spawn(move || {
                if state
                    .create_pairing(create_request(), client(thread))
                    .is_ok()
                {
                    created.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(created.into_inner(), MAX_ROOMS);
    assert_eq!(state.pairing_by_code.len(), MAX_ROOMS);
    assert_eq!(state.pairing_by_id.len(), MAX_ROOMS);
}

/// Regression for the room-cap pairing denial: anyone can fill the room cap
/// by pairing their own devices, and those rooms hold valid tokens. A fresh
/// client must still be able to pair; the least recently used room with no
/// device online makes way.
#[test]
fn regression_filled_room_cap_never_blocks_a_fresh_pairing() {
    let state = resource_limited_state(RelayResourceLimits::default());
    let attacker: IpAddr = "198.51.100.90".parse().unwrap();
    for _ in 0..MAX_ROOMS {
        reset_create_budget(&state);
        let request = create_request();
        let sid = request.pairing_session_id.clone();
        let offer = state.create_pairing(request, attacker).unwrap();
        complete(&state, &offer, sid.as_str());
    }
    state.gc();
    assert_eq!(state.rooms.len(), MAX_ROOMS);
    reset_create_budget(&state);
    let victim: IpAddr = "203.0.113.90".parse().unwrap();
    state
        .create_pairing(create_request(), victim)
        .expect("a full room cap must not refuse a fresh pairing");
    assert_eq!(state.rooms.len(), MAX_ROOMS - 1);
    assert_eq!(state.rooms_evicted.load(Ordering::Relaxed), 1);
}

/// Regression for one IPv6 site spending the relay-wide creation budget from
/// many /64s: one /48 gets a few clients' worth of creations, then is
/// refused, while another site and IPv4 clients are unaffected.
#[test]
fn ipv6_pairing_creation_is_also_limited_per_48_site() {
    use crate::rate_limit::IPV6_SITE_CLIENT_SHARE;
    let state = resource_limited_state(RelayResourceLimits::default());
    let site_limit = PAIRING_CREATE_CLIENT_LIMIT as usize * IPV6_SITE_CLIENT_SHARE;
    // One creation from each of many /64s inside 2001:db8:5::/48.
    let in_site = |subnet: u16| IpAddr::from([0x2001, 0xdb8, 0x5, subnet, 0, 0, 0, 1]);
    for subnet in 0..site_limit {
        state
            .create_pairing(create_request(), in_site(subnet as u16))
            .unwrap();
    }
    assert!(matches!(
        state.create_pairing(create_request(), in_site(u16::MAX)),
        Err(CreateError::Throttled)
    ));
    // The refusal is not charged to the fresh /64.
    assert!(state
        .create_budget
        .clients
        .get(&crate::rate_limit::client_key(in_site(u16::MAX)))
        .is_none());
    let other_site = IpAddr::from([0x2001, 0xdb8, 0x6, 0, 0, 0, 0, 1]);
    state.create_pairing(create_request(), other_site).unwrap();
    state.create_pairing(create_request(), client(1)).unwrap();
    // The site window resets and GC drops stale sites.
    let site = crate::rate_limit::site_key(in_site(0)).unwrap();
    state.create_budget.sites.get_mut(&site).unwrap().1 =
        Instant::now() - PAIRING_CREATE_CLIENT_WINDOW - Duration::from_secs(1);
    state.gc();
    assert!(state.create_budget.sites.get(&site).is_none());
}
