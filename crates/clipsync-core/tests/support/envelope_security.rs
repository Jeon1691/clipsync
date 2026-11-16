// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Envelope authentication, expiry, and replay behavior through a real relay.

use super::*;
use crate::envelope::sequence_floor;
use crate::EnvelopeRejection;
use clipsync_protocol::CLOCK_SKEW_SECS;

/// Routing header a current sender in `pair` would emit from `from` to the room.
fn room_routing(from: &Peer, seq: u64) -> RoutingHeader {
    let room = from.app.store.current_room().unwrap();
    let now = Utc::now();
    RoutingHeader {
        version: ENVELOPE_VERSION_AUTHENTICATED,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: room.room_id,
        sender_device_id: from.identity.device_id.clone(),
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + ChronoDuration::seconds(60),
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
    expired.sent_at -= ChronoDuration::minutes(30);
    expired.expires_at = expired.sent_at + ChronoDuration::seconds(60);
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
    header.expires_at += ChronoDuration::seconds(120);
    rewritten.push(header);
    let mut header = honest.clone();
    header.sent_at -= ChronoDuration::seconds(1);
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
    push("sent_at", &|r| r.sent_at -= ChronoDuration::seconds(1));
    push("expires_at", &|r| {
        r.expires_at += ChronoDuration::seconds(1)
    });
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

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn admit_text(
    peer: &Peer,
    routing: &RoutingHeader,
    text: &str,
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
    }
    .text(routing, &nonce, &ciphertext)
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn admit_manifest(peer: &Peer, routing: &RoutingHeader) -> Result<ManifestPlain, CoreError> {
    let (room, store) = inbound_for(peer);
    let (nonce, ciphertext) =
        seal_manifest(&EPOCH, routing, &manifest(routing.item_kind, b"payload")).unwrap();
    Inbound {
        store: &store,
        room: &room,
        local: &peer.identity.device_id,
        epoch_key: &EPOCH,
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
            authenticated: true
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
                r.sent_at += ChronoDuration::seconds(CLOCK_SKEW_SECS + 60);
                r.expires_at = r.sent_at + ChronoDuration::seconds(60);
            }),
        ),
        (
            EnvelopeRejection::Expired,
            Box::new(|r| {
                r.expires_at = Utc::now() - ChronoDuration::seconds(CLOCK_SKEW_SECS + 1);
                r.sent_at = r.expires_at - ChronoDuration::seconds(60);
            }),
        ),
        (
            EnvelopeRejection::LifetimeTooLong,
            Box::new(|r| {
                r.expires_at = r.sent_at
                    + ChronoDuration::seconds(clipsync_protocol::HARD_MAX_TTL_SECS as i64 + 1)
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
    skewed.sent_at += ChronoDuration::seconds(CLOCK_SKEW_SECS / 2);
    skewed.expires_at = Utc::now() - ChronoDuration::seconds(CLOCK_SKEW_SECS / 2);
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
            }
            .manifest(&image, &nonce, &ciphertext)
        ),
        EnvelopeRejection::WrongItemKind
    );
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
    // Header checks still apply to legacy envelopes.
    let mut expired = legacy(false);
    expired.expires_at = Utc::now() - ChronoDuration::minutes(10);
    expired.sent_at = expired.expires_at - ChronoDuration::seconds(60);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn senders_emit_v2_by_default_and_legacy_v1_only_when_configured() {
    let pair = Pair::new().await;
    let mut receiver = pair.member(&pair.b).await;
    for legacy in [false, true] {
        pair.a
            .configure(|config| config.sync.legacy_envelopes = legacy);
        let a = Session::start(&pair.a).await;
        let text = format!("legacy={legacy}");
        a.clipboard.set(ClipboardItem::Text { text: text.clone() });
        let (routing, nonce, ciphertext) = loop {
            if let ControlMessage::TextEnvelope {
                routing,
                nonce,
                ciphertext,
            } = next_control(&mut receiver).await
            {
                break (routing, nonce, ciphertext);
            }
        };
        // What a receiver from before this change does: v1 AAD, header ignored.
        let key = derive_content_key(&EPOCH, routing.transfer_id.as_str()).unwrap();
        let old_receiver = open(
            &key,
            &decode_nonce(&nonce).unwrap(),
            format!("{}:text", routing.message_id).as_bytes(),
            &hex::decode(&ciphertext).unwrap(),
        );
        if legacy {
            assert_eq!(routing.version, ENVELOPE_VERSION_LEGACY);
            assert_eq!(old_receiver.unwrap(), text.as_bytes());
        } else {
            assert_eq!(routing.version, ENVELOPE_VERSION_AUTHENTICATED);
            assert!(old_receiver.is_err(), "v2 must not open under the v1 AAD");
        }
        assert!(
            matches!(open_text(&EPOCH, &routing, &nonce, &ciphertext).unwrap(),
                ClipboardItem::Text { text: opened } if opened == text)
        );
        // Sequence numbers are persisted and keep increasing across sessions.
        let last = pair.a.app.store.load_replay_state().unwrap().rooms[routing.room_id.as_str()]
            .last_sent_seq;
        assert_eq!(routing.seq, last);
        assert!(routing.seq >= sequence_floor(routing.sent_at - ChronoDuration::seconds(1)));
        a.stop().await;
    }
    receiver.close().await;
}
