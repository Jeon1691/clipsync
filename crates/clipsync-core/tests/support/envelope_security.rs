// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Envelope authentication, expiry, and replay behavior through a real relay.

use super::*;
use crate::envelope::{check_version, sending_version, sequence_floor, EnvelopePolicy};
use crate::EnvelopeRejection;
use clipsync_protocol::CLOCK_SKEW_SECS;

/// Routing header a current sender in `pair` would emit from `from` to the room.
fn room_routing(from: &Peer, seq: u64) -> RoutingHeader {
    let room = from.app.store.current_room().unwrap();
    let now = Timestamp::now();
    RoutingHeader {
        version: ENVELOPE_VERSION_AUTHENTICATED,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: room.room_id,
        sender_device_id: from.identity.device_id.clone(),
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + TimeDuration::seconds(60),
        item_kind: ItemKind::Text,
        seq,
    }
}

fn sealed_text(routing: RoutingHeader, text: &str) -> ControlMessage {
    encrypted_text(routing, text)
}

async fn deliver(sender: &mut RelayConnection, message: ControlMessage) {
    sender.send_control(message).await.unwrap();
    loop {
        if matches!(next_control(sender).await, ControlMessage::Ack { .. }) {
            return;
        }
    }
}

/// Send a fresh sentinel and wait for it, proving every earlier frame was processed.
async fn settle(sender: &mut RelayConnection, from: &Peer, session: &Session, seq: u64) {
    let sentinel = format!("sentinel {seq}");
    deliver(sender, sealed_text(room_routing(from, seq), &sentinel)).await;
    session.text(&sentinel).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replayed_text_is_rejected_within_the_session_and_after_a_restart() {
    let pair = Pair::new().await;
    let mut sender = pair.member(&pair.a).await;
    let original = sealed_text(room_routing(&pair.a, 10), "original clip");

    let b = Session::start(&pair.b).await;
    deliver(&mut sender, original.clone()).await;
    b.text("original clip").await;
    settle(&mut sender, &pair.a, &b, 11).await;
    deliver(&mut sender, original.clone()).await;
    settle(&mut sender, &pair.a, &b, 12).await;
    assert_eq!(
        b.clipboard.signature().await.unwrap(),
        "3",
        "an in-session replay overwrote the clipboard"
    );
    b.stop().await;

    // A restarted daemon has an empty in-memory cache, like one past its TTL.
    let restarted = Session::start(&pair.b).await;
    deliver(&mut sender, original).await;
    settle(&mut sender, &pair.a, &restarted, 13).await;
    assert_eq!(
        restarted.clipboard.signature().await.unwrap(),
        "1",
        "a replay after restart overwrote the clipboard with stale content"
    );
    sender.close().await;
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_and_misaddressed_text_envelopes_never_reach_the_clipboard() {
    let pair = Pair::new().await;
    let mut sender = pair.member(&pair.a).await;
    let b = Session::start(&pair.b).await;

    let mut expired = room_routing(&pair.a, 20);
    expired.sent_at -= TimeDuration::minutes(30);
    expired.expires_at = expired.sent_at + TimeDuration::seconds(60);
    deliver(&mut sender, sealed_text(expired, "expired clip")).await;

    let mut other_room = room_routing(&pair.a, 21);
    other_room.room_id = RoomId::new();
    deliver(&mut sender, sealed_text(other_room, "other room clip")).await;

    let mut other_recipient = room_routing(&pair.a, 22);
    other_recipient.recipient = Recipient::Device(clipsync_protocol::DeviceId::new());
    deliver(
        &mut sender,
        sealed_text(other_recipient, "misaddressed clip"),
    )
    .await;

    settle(&mut sender, &pair.a, &b, 23).await;
    assert_eq!(
        b.clipboard.signature().await.unwrap(),
        "1",
        "an expired or misaddressed envelope overwrote the clipboard"
    );
    sender.close().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_rewritten_routing_fields_are_rejected() {
    let pair = Pair::new().await;
    let mut sender = pair.member(&pair.a).await;
    let b = Session::start(&pair.b).await;

    // Seal under the honest header, then let the "relay" rewrite each field.
    let honest = room_routing(&pair.a, 30);
    let ControlMessage::TextEnvelope {
        nonce, ciphertext, ..
    } = sealed_text(honest.clone(), "rewritten clip")
    else {
        unreachable!()
    };
    let mut rewritten = Vec::new();
    let mut header = honest.clone();
    header.expires_at += TimeDuration::seconds(120);
    rewritten.push(header);
    let mut header = honest.clone();
    header.sent_at -= TimeDuration::seconds(1);
    rewritten.push(header);
    let mut header = honest.clone();
    header.seq += 1_000;
    rewritten.push(header);
    for header in rewritten {
        deliver(
            &mut sender,
            ControlMessage::TextEnvelope {
                routing: header,
                nonce: nonce.clone(),
                ciphertext: ciphertext.clone(),
            },
        )
        .await;
    }
    settle(&mut sender, &pair.a, &b, 31).await;
    assert_eq!(
        b.clipboard.signature().await.unwrap(),
        "1",
        "a relay-rewritten routing header was accepted"
    );
    sender.close().await;
    b.stop().await;
}

/// Every routing field a relay could rewrite, applied to an honest header.
fn rewritten_headers(honest: &RoutingHeader) -> Vec<(&'static str, RoutingHeader)> {
    let mut variants = Vec::new();
    let mut push = |field: &'static str, change: &dyn Fn(&mut RoutingHeader)| {
        let mut routing = honest.clone();
        change(&mut routing);
        variants.push((field, routing));
    };
    push("version", &|r| r.version = ENVELOPE_VERSION_LEGACY);
    push("message_id", &|r| r.message_id = MessageId::new());
    push("transfer_id", &|r| r.transfer_id = TransferId::new());
    push("room_id", &|r| r.room_id = RoomId::new());
    push("sender_device_id", &|r| r.sender_device_id = "other".into());
    push("recipient", &|r| {
        r.recipient = Recipient::Device(clipsync_protocol::DeviceId::new())
    });
    push("sent_at", &|r| r.sent_at -= TimeDuration::seconds(1));
    push("expires_at", &|r| r.expires_at += TimeDuration::seconds(1));
    push("item_kind", &|r| r.item_kind = ItemKind::Files);
    push("seq", &|r| r.seq += 1);
    variants
}

#[test]
fn tampering_any_authenticated_routing_field_fails_text_and_manifest_decryption() {
    let honest = routing_from(&RoomId::new(), SCRIPT_SENDER, ItemKind::Text);
    let (text_nonce, text_ct) = seal_text(&EPOCH, &honest, "authenticated").unwrap();
    let mut manifest_header = honest.clone();
    manifest_header.item_kind = ItemKind::Image;
    let plain = manifest(ItemKind::Image, b"image");
    let (manifest_nonce, manifest_ct) = seal_manifest(&EPOCH, &manifest_header, &plain).unwrap();
    assert!(open_text(&EPOCH, &honest, &text_nonce, &text_ct).is_ok());
    assert_eq!(
        open_manifest(&EPOCH, &manifest_header, &manifest_nonce, &manifest_ct).unwrap(),
        plain
    );
    for (field, routing) in rewritten_headers(&honest) {
        assert!(
            matches!(
                open_text(&EPOCH, &routing, &text_nonce, &text_ct),
                Err(CoreError::Crypto(_))
            ),
            "text accepted with rewritten {field}"
        );
    }
    for (field, mut routing) in rewritten_headers(&manifest_header) {
        if field == "item_kind" {
            routing.item_kind = ItemKind::Text;
        }
        assert!(
            matches!(
                open_manifest(&EPOCH, &routing, &manifest_nonce, &manifest_ct),
                Err(CoreError::Crypto(_))
            ),
            "manifest accepted with rewritten {field}"
        );
    }
    let mut unknown = honest;
    unknown.version = ENVELOPE_VERSION_AUTHENTICATED + 1;
    assert!(open_text(&EPOCH, &unknown, &text_nonce, &text_ct).is_err());
}

fn inbound_for(peer: &Peer) -> (RoomRecord, LocalStore) {
    let store = LocalStore::open_from(peer.app.store.paths.clone()).unwrap();
    (store.current_room().unwrap(), store)
}

fn admit_text(
    peer: &Peer,
    routing: &RoutingHeader,
    text: &str,
) -> Result<ClipboardItem, CoreError> {
    admit_text_with(peer, routing, text, EnvelopePolicy::Negotiate)
}

fn admit_text_with(
    peer: &Peer,
    routing: &RoutingHeader,
    text: &str,
    policy: EnvelopePolicy,
) -> Result<ClipboardItem, CoreError> {
    let (room, store) = inbound_for(peer);
    // An unknown version has no AAD; seal under v2 and present the header as is.
    let sealing = RoutingHeader {
        version: routing.version.min(ENVELOPE_VERSION_AUTHENTICATED),
        ..routing.clone()
    };
    let (nonce, ciphertext) = seal_text(&EPOCH, &sealing, text).unwrap();
    Inbound {
        store: &store,
        room: &room,
        local: &peer.identity.device_id,
        epoch_key: &EPOCH,
        policy,
    }
    .text(routing, &nonce, &ciphertext)
}

fn admit_manifest(peer: &Peer, routing: &RoutingHeader) -> Result<ManifestPlain, CoreError> {
    let (room, store) = inbound_for(peer);
    let (nonce, ciphertext) =
        seal_manifest(&EPOCH, routing, &manifest(routing.item_kind, b"payload")).unwrap();
    Inbound {
        store: &store,
        room: &room,
        local: &peer.identity.device_id,
        epoch_key: &EPOCH,
        policy: EnvelopePolicy::Negotiate,
    }
    .manifest(routing, &nonce, &ciphertext)
}

fn rejection(result: Result<impl std::fmt::Debug, CoreError>) -> EnvelopeRejection {
    match result {
        Err(CoreError::Envelope(rejection)) => rejection,
        other => panic!("expected an envelope rejection, got {other:?}"),
    }
}

#[test]
fn replays_are_rejected_without_the_memory_cache_and_after_reopening_the_store() {
    let room = RoomId::new();
    let peer = scripted_peer("http://127.0.0.1:1", room.clone());
    let text = text_routing(&room);
    let image = routing_from(&room, SCRIPT_SENDER, ItemKind::Image);
    // Each admission opens a fresh store and holds no LoopGuard, so the
    // in-memory message-ID cache (and its TTL) plays no part here.
    admit_text(&peer, &text, "first delivery").unwrap();
    admit_manifest(&peer, &image).unwrap();
    for _restart in 0..2 {
        assert_eq!(
            rejection(admit_text(&peer, &text, "first delivery")),
            EnvelopeRejection::Replayed {
                seq: text.seq,
                highest_seq: image.seq
            }
        );
        assert!(matches!(
            rejection(admit_manifest(&peer, &image)),
            EnvelopeRejection::Replayed { .. }
        ));
    }
    let state = peer.app.store.load_replay_state().unwrap();
    assert_eq!(
        state.rooms[room.as_str()].senders[SCRIPT_SENDER],
        clipsync_storage::SenderReplayState {
            highest_seq: image.seq,
            authenticated: true,
            advertised_authenticated: false,
        }
    );
    // A newer envelope is still accepted, and a rejected forgery cannot raise
    // the high-water mark because the sequence commits only after AEAD.
    let mut forged = text_routing(&room);
    forged.seq = u64::MAX;
    let (room_record, store) = inbound_for(&peer);
    let inbound = Inbound {
        store: &store,
        room: &room_record,
        local: &peer.identity.device_id,
        epoch_key: &EPOCH,
        policy: EnvelopePolicy::Negotiate,
    };
    let (nonce, ciphertext) = seal_text(&EPOCH, &text_routing(&room), "honest").unwrap();
    assert!(matches!(
        inbound.text(&forged, &nonce, &ciphertext),
        Err(CoreError::Crypto(_))
    ));
    admit_text(&peer, &text_routing(&room), "newer").unwrap();
}

type HeaderChange = Box<dyn Fn(&mut RoutingHeader)>;

#[test]
fn header_checks_cover_addressing_lifetime_and_frame_kind() {
    let room = RoomId::new();
    let peer = scripted_peer("http://127.0.0.1:1", room.clone());
    let mut direct = text_routing(&room);
    direct.recipient = Recipient::Device(peer.identity.device_id.clone());
    admit_text(&peer, &direct, "addressed to this device").unwrap();
    let cases: Vec<(EnvelopeRejection, HeaderChange)> = vec![
        (
            EnvelopeRejection::UnsupportedVersion(ENVELOPE_VERSION_AUTHENTICATED + 1),
            Box::new(|r| r.version = ENVELOPE_VERSION_AUTHENTICATED + 1),
        ),
        (
            EnvelopeRejection::WrongRoom,
            Box::new(|r| r.room_id = RoomId::new()),
        ),
        (
            EnvelopeRejection::WrongRecipient,
            Box::new(|r| r.recipient = Recipient::Device("someone-else".into())),
        ),
        (
            EnvelopeRejection::UnknownSender,
            Box::new(|r| r.sender_device_id = "unpinned-device".into()),
        ),
        (
            EnvelopeRejection::UnknownSender,
            Box::new(|r| r.sender_device_id = "script-device-a".into()),
        ),
        (
            EnvelopeRejection::WrongItemKind,
            Box::new(|r| r.item_kind = ItemKind::Image),
        ),
        (
            EnvelopeRejection::NotYetValid,
            Box::new(|r| {
                r.sent_at += TimeDuration::seconds(CLOCK_SKEW_SECS + 60);
                r.expires_at = r.sent_at + TimeDuration::seconds(60);
            }),
        ),
        (
            EnvelopeRejection::Expired,
            Box::new(|r| {
                r.expires_at = Timestamp::now() - TimeDuration::seconds(CLOCK_SKEW_SECS + 1);
                r.sent_at = r.expires_at - TimeDuration::seconds(60);
            }),
        ),
        (
            EnvelopeRejection::LifetimeTooLong,
            Box::new(|r| {
                r.expires_at = r.sent_at
                    + TimeDuration::seconds(clipsync_protocol::HARD_MAX_TTL_SECS as i64 + 1)
            }),
        ),
    ];
    for (expected, change) in cases {
        let mut routing = text_routing(&room);
        change(&mut routing);
        assert_eq!(rejection(admit_text(&peer, &routing, "rejected")), expected);
    }
    // Within the skew tolerance, a slightly early or late envelope still arrives.
    let mut skewed = text_routing(&room);
    skewed.sent_at += TimeDuration::seconds(CLOCK_SKEW_SECS / 2);
    skewed.expires_at = Timestamp::now() - TimeDuration::seconds(CLOCK_SKEW_SECS / 2);
    admit_text(&peer, &skewed, "skewed clocks").unwrap();
    // The decrypted manifest must describe the item kind its header announced.
    let image = routing_from(&room, SCRIPT_SENDER, ItemKind::Image);
    let (room_record, store) = inbound_for(&peer);
    let (nonce, ciphertext) =
        seal_manifest(&EPOCH, &image, &manifest(ItemKind::Files, b"payload")).unwrap();
    assert_eq!(
        rejection(
            Inbound {
                store: &store,
                room: &room_record,
                local: &peer.identity.device_id,
                epoch_key: &EPOCH,
                policy: EnvelopePolicy::Negotiate,
            }
            .manifest(&image, &nonce, &ciphertext)
        ),
        EnvelopeRejection::WrongItemKind
    );
}

/// Header timestamps arrive from the relay before authentication. Extremes of
/// the representable range must be rejected (or, for an unbound v1 `sent_at`, accepted
/// as before v2) without panicking: release builds abort on panic, so one
/// envelope would otherwise stop the receiving client.
#[test]
fn extreme_header_timestamps_are_rejected_without_panicking() {
    let max = Timestamp::MAX;
    let min = Timestamp::MIN;
    let fresh_expiry = || Timestamp::now() + TimeDuration::seconds(60);
    // (case, sent_at, expires_at, v1 outcome, v2 outcome); `None` = admitted.
    type Case = (
        &'static str,
        Timestamp,
        Timestamp,
        Option<EnvelopeRejection>,
        Option<EnvelopeRejection>,
    );
    let cases: Vec<Case> = vec![
        (
            "expires_at at the maximum",
            Timestamp::now(),
            max,
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            "expires_at just below the maximum",
            Timestamp::now(),
            max - TimeDuration::nanoseconds(1),
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            "expires_at at the minimum",
            min,
            min,
            Some(EnvelopeRejection::Expired),
            Some(EnvelopeRejection::Expired),
        ),
        (
            "sent_at at the minimum",
            min,
            fresh_expiry(),
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
        (
            // v1 does not bind `sent_at`, so it is not checked there.
            "sent_at at the maximum",
            max,
            fresh_expiry(),
            None,
            Some(EnvelopeRejection::NotYetValid),
        ),
        (
            "both at the maximum",
            max,
            max,
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::NotYetValid),
        ),
        (
            "sent_at at the minimum, expires_at at the maximum",
            min,
            max,
            Some(EnvelopeRejection::LifetimeTooLong),
            Some(EnvelopeRejection::LifetimeTooLong),
        ),
    ];
    for version in [ENVELOPE_VERSION_LEGACY, ENVELOPE_VERSION_AUTHENTICATED] {
        // A fresh receiver per version: an admitted v2 would refuse later v1.
        let room = RoomId::new();
        let peer = scripted_peer("http://127.0.0.1:1", room.clone());
        for (case, sent_at, expires_at, v1, v2) in &cases {
            let expected = if version == ENVELOPE_VERSION_LEGACY {
                v1
            } else {
                v2
            };
            for kind in [ItemKind::Text, ItemKind::Image] {
                let routing = RoutingHeader {
                    version,
                    sent_at: *sent_at,
                    expires_at: *expires_at,
                    ..routing_from(&room, SCRIPT_SENDER, kind)
                };
                let outcome = match kind {
                    ItemKind::Text => admit_text(&peer, &routing, "extreme").map(|_| ()),
                    _ => admit_manifest(&peer, &routing).map(|_| ()),
                };
                let context = format!("v{version} {kind:?}: {case}");
                match expected {
                    None => outcome.unwrap_or_else(|err| panic!("{context}: {err:?}")),
                    Some(rejected) => assert_eq!(rejection(outcome), *rejected, "{context}"),
                }
            }
        }
    }
}

#[test]
fn legacy_envelopes_are_accepted_until_the_sender_upgrades_then_refused_across_restarts() {
    let room = RoomId::new();
    let peer = scripted_peer("http://127.0.0.1:1", room.clone());
    let legacy = |seq_reset: bool| RoutingHeader {
        version: ENVELOPE_VERSION_LEGACY,
        // Legacy senders restart seq at 1 every session.
        seq: if seq_reset { 1 } else { 2 },
        ..text_routing(&room)
    };
    // An updated receiver keeps working with a not-yet-updated sender.
    admit_text(&peer, &legacy(true), "legacy one").unwrap();
    admit_text(&peer, &legacy(true), "legacy after peer restart").unwrap();
    // A released sender whose clock runs ahead keeps syncing, as before v2:
    // a relay can rewrite a v1 `sent_at` anyway, so "sent in the future" adds
    // nothing there. The same header under v2 is refused.
    let mut ahead = legacy(true);
    ahead.sent_at += TimeDuration::seconds(CLOCK_SKEW_SECS + 60);
    ahead.expires_at = ahead.sent_at + TimeDuration::seconds(60);
    admit_text(&peer, &ahead, "legacy sender clock ahead").unwrap();
    let ahead_v2 = RoutingHeader {
        version: ENVELOPE_VERSION_AUTHENTICATED,
        message_id: MessageId::new(),
        ..ahead
    };
    assert_eq!(
        rejection(admit_text(&peer, &ahead_v2, "v2 sender clock ahead")),
        EnvelopeRejection::NotYetValid
    );
    // Header checks still apply to legacy envelopes.
    let mut expired = legacy(false);
    expired.expires_at = Timestamp::now() - TimeDuration::minutes(10);
    expired.sent_at = expired.expires_at - TimeDuration::seconds(60);
    assert_eq!(
        rejection(admit_text(&peer, &expired, "expired legacy")),
        EnvelopeRejection::Expired
    );
    let mut relabeled = legacy(false);
    relabeled.sender_device_id = "fresh-unpinned-id".into();
    assert_eq!(
        rejection(admit_text(&peer, &relabeled, "relabeled legacy")),
        EnvelopeRejection::UnknownSender
    );
    assert!(!peer
        .app
        .store
        .sender_is_authenticated(&room, &SCRIPT_SENDER.into())
        .unwrap());

    // The first authenticated envelope from the sender pins it to v2 for good.
    admit_text(&peer, &text_routing(&room), "authenticated").unwrap();
    for _restart in 0..2 {
        assert_eq!(
            rejection(admit_text(&peer, &legacy(true), "downgraded")),
            EnvelopeRejection::Downgrade
        );
    }
    admit_text(&peer, &text_routing(&room), "authenticated again").unwrap();
}

/// What a receiver from before envelope v2 does: v1 AAD, header ignored.
fn released_client_opens(routing: &RoutingHeader, nonce: &str, ciphertext: &str) -> Option<String> {
    let key = derive_content_key(&EPOCH, routing.transfer_id.as_str()).unwrap();
    open(
        &key,
        &decode_nonce(nonce).unwrap(),
        format!("{}:text", routing.message_id).as_bytes(),
        &hex::decode(ciphertext).unwrap(),
    )
    .ok()
    .map(|plain| String::from_utf8(plain).unwrap())
}

async fn next_text_envelope(connection: &mut RelayConnection) -> (RoutingHeader, String, String) {
    loop {
        if let ControlMessage::TextEnvelope {
            routing,
            nonce,
            ciphertext,
        } = next_control(connection).await
        {
            return (routing, nonce, ciphertext);
        }
    }
}

fn accepts_v2(store: &LocalStore, room: &RoomId, peer: &DeviceId) -> bool {
    store
        .load_replay_state()
        .unwrap()
        .rooms
        .get(room.as_str())
        .is_some_and(|state| state.peer_accepts_authenticated(peer))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn senders_use_v1_until_the_peer_proves_v2_and_honor_both_overrides() {
    let pair = Pair::new().await;
    let room = pair.a.app.store.current_room().unwrap().room_id;
    // A raw connection never answers the handshake: no capability evidence.
    let mut receiver = pair.member(&pair.b).await;
    // Ordered so that no v2 has gone out before "peer proved v2": a sent v2
    // keeps the room on v2 by itself.
    let cases = [
        ("no evidence", false, false, false, ENVELOPE_VERSION_LEGACY),
        (
            "legacy override",
            true,
            false,
            false,
            ENVELOPE_VERSION_LEGACY,
        ),
        (
            "peer proved v2",
            false,
            false,
            true,
            ENVELOPE_VERSION_AUTHENTICATED,
        ),
        (
            "legacy beats evidence",
            true,
            false,
            true,
            ENVELOPE_VERSION_LEGACY,
        ),
        (
            "strict override",
            false,
            true,
            true,
            ENVELOPE_VERSION_AUTHENTICATED,
        ),
        (
            "strict beats legacy",
            true,
            true,
            true,
            ENVELOPE_VERSION_AUTHENTICATED,
        ),
    ];
    for (case, legacy, strict, evidence, expected) in cases {
        pair.a.configure(|config| {
            config.sync.legacy_envelopes = legacy;
            config.sync.strict_envelopes = strict;
        });
        if evidence {
            pair.a
                .app
                .store
                .mark_peer_advertised_authenticated(&room, &pair.b.identity.device_id)
                .unwrap();
        }
        let a = Session::start(&pair.a).await;
        let text = format!("case {case}");
        a.clipboard.set(ClipboardItem::Text { text: text.clone() });
        let (routing, nonce, ciphertext) = next_text_envelope(&mut receiver).await;
        assert_eq!(routing.version, expected, "{case}");
        assert_eq!(
            released_client_opens(&routing, &nonce, &ciphertext).is_some(),
            expected == ENVELOPE_VERSION_LEGACY,
            "{case}: only v1 may open under the released AAD"
        );
        assert!(
            matches!(open_text(&EPOCH, &routing, &nonce, &ciphertext).unwrap(),
                ClipboardItem::Text { text: opened } if opened == text),
            "{case}"
        );
        // Sequence numbers are persisted and keep increasing across sessions.
        let last = pair.a.app.store.load_replay_state().unwrap().rooms[routing.room_id.as_str()]
            .last_sent_seq;
        assert_eq!(routing.seq, last);
        assert!(routing.seq >= sequence_floor(routing.sent_at - TimeDuration::seconds(1)));
        a.stop().await;
    }
    receiver.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updated_clients_converge_on_v2_through_the_handshake_in_either_start_order() {
    for b_first in [false, true] {
        let pair = Pair::new().await;
        let room = pair.a.app.store.current_room().unwrap().room_id;
        let (a_id, b_id) = (&pair.a.identity.device_id, &pair.b.identity.device_id);
        assert!(!accepts_v2(&pair.a.app.store, &room, b_id));
        assert!(!accepts_v2(&pair.b.app.store, &room, a_id));
        // b has the smaller device ID and initiates; when b connects first,
        // only a's later arrival (peer_joined at b) can start the handshake.
        let (a, b) = if b_first {
            let b = Session::start(&pair.b).await;
            (Session::start(&pair.a).await, b)
        } else {
            let a = Session::start(&pair.a).await;
            (a, Session::start(&pair.b).await)
        };
        wait_until(|| async {
            accepts_v2(&pair.a.app.store, &room, b_id) && accepts_v2(&pair.b.app.store, &room, a_id)
        })
        .await;
        // Both directions now carry v2; each receiver locks the sender to v2.
        a.clipboard.set(ClipboardItem::Text {
            text: "from a".into(),
        });
        b.text("from a").await;
        b.clipboard.set(ClipboardItem::Text {
            text: "from b".into(),
        });
        a.text("from b").await;
        assert!(pair
            .b
            .app
            .store
            .sender_is_authenticated(&room, a_id)
            .unwrap());
        assert!(pair
            .a
            .app
            .store
            .sender_is_authenticated(&room, b_id)
            .unwrap());
        a.stop().await;

        // Downgrade after the switch: a v1 envelope claiming to be from a is
        // refused, while a fresh v2 sentinel still arrives.
        let mut sender = pair.member(&pair.a).await;
        let legacy = RoutingHeader {
            version: ENVELOPE_VERSION_LEGACY,
            ..room_routing(&pair.a, 1)
        };
        deliver(&mut sender, sealed_text(legacy, "downgraded clip")).await;
        let before = b.clipboard.signature().await.unwrap();
        settle(&mut sender, &pair.a, &b, sequence_floor(Timestamp::now())).await;
        let after: u64 = b.clipboard.signature().await.unwrap().parse().unwrap();
        assert_eq!(
            after,
            before.parse::<u64>().unwrap() + 1,
            "the v1 downgrade reached the clipboard"
        );
        sender.close().await;
        b.stop().await;
        // The switch is persisted: a new session still sends v2.
        assert_eq!(
            sending_version(
                &pair.a.app.store,
                &pair.a.app.store.current_room().unwrap(),
                a_id,
                EnvelopePolicy::Negotiate
            )
            .unwrap(),
            ENVELOPE_VERSION_AUTHENTICATED
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updated_and_released_clients_keep_syncing_both_ways_without_configuration() {
    let pair = Pair::new().await;
    let room = pair.a.app.store.current_room().unwrap().room_id;
    let a = Session::start(&pair.a).await;
    // b plays a released client: same handshake rule (smaller ID initiates),
    // an empty handshake payload, v1 envelopes only.
    let mut old = pair.member(&pair.b).await;
    let a_public = parse_public(&hex::encode(pair.a.identity.public_bytes())).unwrap();
    let mut handshake =
        NoiseHandshake::initiator(&pair.b.identity.static_secret(), &a_public).unwrap();
    old.send_control(ControlMessage::NoiseHandshake {
        message: hex::encode(handshake.write_message().unwrap()),
    })
    .await
    .unwrap();
    let reply = loop {
        if let ControlMessage::NoiseHandshake { message } = next_control(&mut old).await {
            break hex::decode(message).unwrap();
        }
    };
    let payload = handshake.read_message(&reply).unwrap();
    assert!(handshake.is_finished());
    // Released clients read the payload into a fixed buffer and drop it.
    assert!(payload.len() <= clipsync_protocol::LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES);
    assert!(clipsync_protocol::HandshakeCapabilities::decode(&payload).accepts_authenticated());

    // Updated -> released: v1, readable with the released AAD.
    a.clipboard.set(ClipboardItem::Text {
        text: "to the old client".into(),
    });
    let (routing, nonce, ciphertext) = next_text_envelope(&mut old).await;
    assert_eq!(routing.version, ENVELOPE_VERSION_LEGACY);
    assert_eq!(
        released_client_opens(&routing, &nonce, &ciphertext).as_deref(),
        Some("to the old client")
    );
    assert!(!accepts_v2(
        &pair.a.app.store,
        &room,
        &pair.b.identity.device_id
    ));

    // Released -> updated: v1 is applied, also after the old client restarts
    // its sequence numbers.
    for text in ["from the old client", "after the old client restarted"] {
        let legacy = RoutingHeader {
            version: ENVELOPE_VERSION_LEGACY,
            ..room_routing(&pair.b, 1)
        };
        old.send_control(sealed_text(legacy, text)).await.unwrap();
        a.text(text).await;
    }
    assert!(!pair
        .a
        .app
        .store
        .sender_is_authenticated(&room, &pair.b.identity.device_id)
        .unwrap());
    old.close().await;
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_updated_initiator_completes_the_handshake_with_a_released_responder() {
    let pair = Pair::new().await;
    let room = pair.b.app.store.current_room().unwrap().room_id;
    // a plays a released client; b (smaller ID) is updated and initiates.
    let mut old = pair.member(&pair.a).await;
    let b = Session::start(&pair.b).await;
    let first = loop {
        if let ControlMessage::NoiseHandshake { message } = next_control(&mut old).await {
            break hex::decode(message).unwrap();
        }
    };
    let b_public = parse_public(&hex::encode(pair.b.identity.public_bytes())).unwrap();
    let mut handshake =
        NoiseHandshake::responder(&pair.a.identity.static_secret(), &b_public).unwrap();
    let payload = handshake.read_message(&first).unwrap();
    assert!(payload.len() <= clipsync_protocol::LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES);
    old.send_control(ControlMessage::NoiseHandshake {
        message: hex::encode(handshake.write_message().unwrap()),
    })
    .await
    .unwrap();
    assert!(handshake.is_finished());

    // The empty reply advertises nothing: b keeps sending v1, and a's v1 arrives.
    let legacy = RoutingHeader {
        version: ENVELOPE_VERSION_LEGACY,
        ..room_routing(&pair.a, 1)
    };
    old.send_control(sealed_text(legacy, "from the released responder"))
        .await
        .unwrap();
    b.text("from the released responder").await;
    b.clipboard.set(ClipboardItem::Text {
        text: "to the released responder".into(),
    });
    let (routing, nonce, ciphertext) = next_text_envelope(&mut old).await;
    assert_eq!(routing.version, ENVELOPE_VERSION_LEGACY);
    assert_eq!(
        released_client_opens(&routing, &nonce, &ciphertext).as_deref(),
        Some("to the released responder")
    );
    assert!(!accepts_v2(
        &pair.b.app.store,
        &room,
        &pair.a.identity.device_id
    ));
    old.close().await;
    b.stop().await;
}

#[test]
fn strict_mode_refuses_every_legacy_envelope() {
    let room = RoomId::new();
    let peer = scripted_peer("http://127.0.0.1:1", room.clone());
    let legacy = RoutingHeader {
        version: ENVELOPE_VERSION_LEGACY,
        ..text_routing(&room)
    };
    // Default policy accepts a not-yet-upgraded sender; strict never does.
    admit_text(&peer, &legacy, "legacy").unwrap();
    let legacy = RoutingHeader {
        message_id: MessageId::new(),
        ..legacy
    };
    assert_eq!(
        rejection(admit_text_with(
            &peer,
            &legacy,
            "legacy",
            EnvelopePolicy::Strict
        )),
        EnvelopeRejection::LegacyRefused
    );
    admit_text_with(&peer, &text_routing(&room), "v2", EnvelopePolicy::Strict).unwrap();
    assert_eq!(
        EnvelopePolicy::Strict.accepted_versions(),
        [ENVELOPE_VERSION_AUTHENTICATED]
    );
}

#[test]
fn broadcasts_follow_the_least_capable_peer_and_advertising_alone_never_refuses_v1() {
    let room_id = RoomId::new();
    let peer = scripted_peer("http://127.0.0.1:1", room_id.clone());
    let store = &peer.app.store;
    let local = &peer.identity.device_id;
    let mut room = store.current_room().unwrap();
    let mut second = room.peers[0].clone();
    second.device_id = "script-device-y".into();
    room.peers.push(second);
    let version_for =
        |room: &RoomRecord, policy| sending_version(store, room, local, policy).unwrap();
    let version = |policy| version_for(&room, policy);

    assert_eq!(version(EnvelopePolicy::Negotiate), ENVELOPE_VERSION_LEGACY);
    assert_eq!(
        version(EnvelopePolicy::Strict),
        ENVELOPE_VERSION_AUTHENTICATED
    );
    // One peer advertised v2 in its handshake; the other has not: stay on v1.
    store
        .mark_peer_advertised_authenticated(&room_id, &"script-device-y".into())
        .unwrap();
    assert_eq!(version(EnvelopePolicy::Negotiate), ENVELOPE_VERSION_LEGACY);
    // The advertisement does not make that peer's legacy envelopes a downgrade.
    assert!(check_version(
        store,
        &RoutingHeader {
            version: ENVELOPE_VERSION_LEGACY,
            sender_device_id: "script-device-y".into(),
            ..text_routing(&room_id)
        },
        EnvelopePolicy::Negotiate
    )
    .is_ok());
    // The other peer sends an accepted v2 envelope: every peer is now capable.
    admit_text(&peer, &text_routing(&room_id), "v2 from z").unwrap();
    assert_eq!(
        version(EnvelopePolicy::Negotiate),
        ENVELOPE_VERSION_AUTHENTICATED
    );
    assert_eq!(version(EnvelopePolicy::Legacy), ENVELOPE_VERSION_LEGACY);
    // A peer pinned later without evidence keeps the room on v1 while this
    // device has not sent v2 into it yet.
    let mut grown = room.clone();
    let mut third = room.peers[0].clone();
    third.device_id = "script-device-x".into();
    grown.peers.push(third);
    assert_eq!(
        version_for(&grown, EnvelopePolicy::Negotiate),
        ENVELOPE_VERSION_LEGACY
    );
    assert_eq!(
        version_for(&grown, EnvelopePolicy::Strict),
        ENVELOPE_VERSION_AUTHENTICATED
    );
    // Once this device has sent v2 into the room, the earlier peers refuse its
    // v1: the room stays on v2 even with a peer that has given no evidence.
    store.reserve_send(&room_id, 0, true).unwrap();
    assert_eq!(
        version_for(&grown, EnvelopePolicy::Negotiate),
        ENVELOPE_VERSION_AUTHENTICATED
    );
    assert_eq!(
        version_for(&grown, EnvelopePolicy::Legacy),
        ENVELOPE_VERSION_LEGACY
    );
}

/// A daemon and a one-shot `push` stamp envelopes for one room from separate
/// stores on the same data directory. Once either has sent v2 into the room,
/// the peers refuse this device's v1, so no envelope with a later `seq` may be
/// stamped v1. That holds only if the version is decided under the same lock
/// that reserves the `seq`; a decision from an earlier unlocked read can miss
/// the other process's switch and send one v1 that peers drop as a downgrade.
#[test]
fn concurrent_stamps_never_send_v1_after_v2_went_into_the_room() {
    // Rooms switched from v1 to v2 while the daemon keeps stamping; each
    // switch is one chance for a stale decision. Enough that an unlocked
    // decision loses the race on every run.
    const SWITCHES: usize = 30;
    // Daemon stamps per room, spanning the one-shot's switch.
    const DAEMON_STAMPS: usize = 30;
    let peer = scripted_peer("http://127.0.0.1:1", RoomId::new());
    let daemon_store = &peer.app.store;
    let one_shot = LocalStore::open_from(daemon_store.paths.clone()).unwrap();
    let local = &peer.identity.device_id;
    let template = daemon_store.current_room().unwrap();
    let mut stale = 0;
    for _ in 0..SWITCHES {
        let mut room = template.clone();
        room.room_id = RoomId::new();
        let remote = room.peers[0].device_id.clone();
        let start = std::sync::Barrier::new(2);
        let stamp = |store: &LocalStore| {
            EnvelopeStamp::next(store, &room, local, EnvelopePolicy::Negotiate).unwrap()
        };
        let (daemon, switched) = std::thread::scope(|scope| {
            let daemon = scope.spawn(|| {
                start.wait();
                (0..DAEMON_STAMPS)
                    .map(|_| stamp(daemon_store))
                    .collect::<Vec<_>>()
            });
            start.wait();
            // The one-shot's handshake proves the peer opens v2; it sends v2.
            one_shot
                .mark_peer_advertised_authenticated(&room.room_id, &remote)
                .unwrap();
            let switched = stamp(&one_shot);
            (daemon.join().unwrap(), switched)
        });
        assert_eq!(switched.version, ENVELOPE_VERSION_AUTHENTICATED);
        stale += daemon
            .iter()
            .filter(|later| later.seq > switched.seq)
            .filter(|later| later.version != ENVELOPE_VERSION_AUTHENTICATED)
            .count();
    }
    assert_eq!(stale, 0, "v1 envelopes stamped after v2 went out");
}
