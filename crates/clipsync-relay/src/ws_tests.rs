// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use crate::{auth::issue_token, RelayConfig};
use clipsync_protocol::{
    CreatePairingRequest, JoinPairingRequest, PairingSessionId, Recipient, RoomId, RoutingHeader,
    TransferId,
};

fn state() -> Arc<RelayState> {
    Arc::new(RelayState::new(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: vec![42; 32],
        webhook_url: None,
        webhook_secret: None,
    }))
}

fn room(state: &RelayState) -> (RoomId, DeviceId, DeviceId, String, String) {
    let owner = DeviceId::new();
    let member = DeviceId::new();
    let sid = PairingSessionId::new();
    let offer = state
        .create_pairing(CreatePairingRequest {
            protocol_version: clipsync_protocol::PROTOCOL_VERSION,
            pairing_session_id: sid.clone(),
            device_id: owner.clone(),
            device_name: "owner".into(),
            os: "fixture".into(),
            ttl_secs: Some(60),
        })
        .unwrap();
    state
        .join_pairing(
            JoinPairingRequest {
                protocol_version: clipsync_protocol::PROTOCOL_VERSION,
                pairing_code: offer.pairing_code,
                device_id: member.clone(),
                device_name: "member".into(),
                os: "fixture".into(),
            },
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
    assert!(state.complete_pairing(sid.as_str()).is_none());
    let (owner_token, member_token, id, _, _) = state.complete_pairing(sid.as_str()).unwrap();
    (id, owner, member, owner_token, member_token)
}

fn hello(device_id: &DeviceId, room_id: &RoomId, token: &str) -> ControlMessage {
    ControlMessage::Hello {
        protocol_version: clipsync_protocol::PROTOCOL_VERSION,
        device_id: device_id.clone(),
        device_name: "fixture".into(),
        os: "test".into(),
        role: ClientRole::Member,
        room_id: Some(room_id.clone()),
        token: token.into(),
        pairing_session_id: None,
    }
}

async fn receive(rx: &mut mpsc::Receiver<WsOut>) -> ControlMessage {
    match tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        WsOut::Text(text) => ControlMessage::decode(&text).unwrap(),
        other => panic!("expected control message, got {other:?}"),
    }
}

#[tokio::test]
async fn hello_checks_version_signature_device_room_and_revocation() {
    let state = state();
    let (rid, owner, member, token, _) = room(&state);
    let (tx, mut rx) = mpsc::channel(32);
    let mut ctx = None;
    let mut bad = hello(&owner, &rid, &token);
    if let ControlMessage::Hello {
        protocol_version, ..
    } = &mut bad
    {
        *protocol_version = 0;
    }
    assert_eq!(
        handle_control(&state, &tx, &mut ctx, bad, None)
            .await
            .unwrap_err(),
        "unsupported protocol version"
    );
    assert!(
        handle_control(&state, &tx, &mut ctx, hello(&owner, &rid, "invalid"), None)
            .await
            .is_err()
    );
    assert_eq!(
        handle_control(&state, &tx, &mut ctx, hello(&member, &rid, &token), None)
            .await
            .unwrap_err(),
        "token device mismatch"
    );
    assert_eq!(
        handle_control(
            &state,
            &tx,
            &mut ctx,
            hello(&owner, &RoomId::new(), &token),
            None
        )
        .await
        .unwrap_err(),
        "token room mismatch"
    );
    let unknown = DeviceId::new();
    let unknown_token = issue_token(
        &state.config.token_secret,
        "device",
        &rid,
        &unknown,
        DeviceRole::Member,
        None,
        60,
    );
    assert_eq!(
        handle_control(
            &state,
            &tx,
            &mut ctx,
            hello(&unknown, &rid, &unknown_token),
            None
        )
        .await
        .unwrap_err(),
        "unknown device"
    );
    let missing_room = RoomId::new();
    let missing_token = issue_token(
        &state.config.token_secret,
        "device",
        &missing_room,
        &owner,
        DeviceRole::Owner,
        None,
        60,
    );
    assert_eq!(
        handle_control(
            &state,
            &tx,
            &mut ctx,
            hello(&owner, &missing_room, &missing_token),
            None
        )
        .await
        .unwrap_err(),
        "unknown room"
    );
    handle_control(&state, &tx, &mut ctx, hello(&owner, &rid, ""), Some(&token))
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::HelloOk {
            peer_online: false,
            ..
        }
    ));
    assert_eq!(ctx.as_ref().unwrap().device_id, owner.as_str());
    state.revoke(rid.as_str(), owner.as_str());
    assert_eq!(
        handle_control(&state, &tx, &mut None, hello(&owner, &rid, &token), None)
            .await
            .unwrap_err(),
        "device revoked"
    );
}

#[tokio::test]
async fn control_messages_require_hello_and_revoke_requires_the_owner() {
    let state = state();
    let (rid, owner, member, owner_token, member_token) = room(&state);
    let (tx, mut rx) = mpsc::channel(32);
    let revoke = || ControlMessage::Revoke {
        device_id: owner.clone(),
    };
    assert_eq!(
        handle_control(&state, &tx, &mut None, revoke(), None)
            .await
            .unwrap_err(),
        "hello required"
    );
    let mut ctx = None;
    handle_control(
        &state,
        &tx,
        &mut ctx,
        hello(&member, &rid, &member_token),
        None,
    )
    .await
    .unwrap();
    receive(&mut rx).await;
    assert_eq!(
        handle_control(&state, &tx, &mut ctx, revoke(), None)
            .await
            .unwrap_err(),
        "manage permission required"
    );
    assert!(!state.room(rid.as_str()).unwrap().devices[owner.as_str()].revoked);
    let mut owner_ctx = None;
    handle_control(
        &state,
        &tx,
        &mut owner_ctx,
        hello(&owner, &rid, &owner_token),
        None,
    )
    .await
    .unwrap();
    receive(&mut rx).await;
    assert_eq!(
        handle_control(
            &state,
            &tx,
            &mut owner_ctx,
            ControlMessage::Revoke {
                device_id: DeviceId::new()
            },
            None
        )
        .await
        .unwrap_err(),
        "device not found"
    );
    handle_control(
        &state,
        &tx,
        &mut owner_ctx,
        ControlMessage::Revoke {
            device_id: member.clone(),
        },
        None,
    )
    .await
    .unwrap();
    assert!(state.room(rid.as_str()).unwrap().devices[member.as_str()].revoked);
    assert!(!state.is_online(member.as_str()));
}

#[tokio::test]
async fn text_queues_offline_but_binary_requires_an_online_peer() {
    let state = state();
    let (rid, owner, member, token, _) = room(&state);
    let (tx, mut rx) = mpsc::channel(32);
    let mut ctx = None;
    handle_control(&state, &tx, &mut ctx, hello(&owner, &rid, &token), None)
        .await
        .unwrap();
    receive(&mut rx).await;
    let routing = RoutingHeader {
        version: clipsync_protocol::PROTOCOL_VERSION,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: rid,
        sender_device_id: owner,
        recipient: Recipient::Broadcast,
        sent_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(60),
        item_kind: ItemKind::Text,
        seq: 1,
    };
    let envelope = ControlMessage::TextEnvelope {
        routing: routing.clone(),
        nonce: "opaque-nonce".into(),
        ciphertext: "opaque-ciphertext".into(),
    };
    handle_control(&state, &tx, &mut ctx, envelope.clone(), None)
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::Ack {
            status: AckStatus::Accepted,
            ..
        }
    ));
    assert_eq!(
        state.take_queue(member.as_str()).unwrap().ciphertext,
        "opaque-ciphertext"
    );
    let manifest = ControlMessage::Manifest {
        routing: routing.clone(),
        chunk_count: 1,
        total_bytes: 3,
        nonce: "opaque".into(),
        ciphertext: "opaque".into(),
    };
    handle_control(&state, &tx, &mut ctx, manifest.clone(), None)
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::Ack {
            status: AckStatus::PeerOffline,
            detail: Some(_),
            ..
        }
    ));
    forward_binary(&state, ctx.as_ref().unwrap(), vec![1, 2, 3], &tx).await;
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::Ack {
            status: AckStatus::PeerOffline,
            ..
        }
    ));
    let (peer_tx, mut peer_rx) = mpsc::channel(32);
    state.register_conn(member.as_str(), peer_tx);
    handle_control(&state, &tx, &mut ctx, envelope, None)
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut peer_rx).await,
        ControlMessage::TextEnvelope { .. }
    ));
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::Ack {
            status: AckStatus::Delivered,
            ..
        }
    ));
    handle_control(&state, &tx, &mut ctx, manifest, None)
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut peer_rx).await,
        ControlMessage::Manifest { .. }
    ));
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::Ack {
            status: AckStatus::Streaming,
            ..
        }
    ));
    forward_binary(&state, ctx.as_ref().unwrap(), vec![1, 2, 3], &tx).await;
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(3), peer_rx.recv()).await.unwrap(), Some(WsOut::Binary(bytes)) if bytes == [1,2,3])
    );
    notify_peers_left(&state, ctx.as_ref().unwrap()).await;
    assert!(matches!(
        receive(&mut peer_rx).await,
        ControlMessage::PeerLeft { .. }
    ));
    let mut wrong_kind = routing;
    wrong_kind.item_kind = ItemKind::Image;
    assert_eq!(
        handle_control(
            &state,
            &tx,
            &mut ctx,
            ControlMessage::TextEnvelope {
                routing: wrong_kind,
                nonce: "x".into(),
                ciphertext: "y".into()
            },
            None
        )
        .await
        .unwrap_err(),
        "text envelope item_kind mismatch"
    );
}

#[tokio::test]
async fn missing_peers_and_pairing_abort_report_explicit_control_outcomes() {
    let state = state();
    let (rid, owner, member, token, _) = room(&state);
    let (tx, mut rx) = mpsc::channel(16);
    let mut ctx = None;
    handle_control(&state, &tx, &mut ctx, hello(&owner, &rid, &token), None)
        .await
        .unwrap();
    receive(&mut rx).await;
    assert!(state.revoke(rid.as_str(), member.as_str()));
    let now = chrono::Utc::now();
    let routing = RoutingHeader {
        version: clipsync_protocol::PROTOCOL_VERSION,
        message_id: MessageId::new(),
        transfer_id: TransferId::new(),
        room_id: rid,
        sender_device_id: owner,
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + chrono::Duration::seconds(60),
        item_kind: ItemKind::Text,
        seq: 1,
    };
    let message_id = routing.message_id.clone();
    handle_control(
        &state,
        &tx,
        &mut ctx,
        ControlMessage::TextEnvelope {
            routing,
            nonce: "opaque".into(),
            ciphertext: "opaque".into(),
        },
        None,
    )
    .await
    .unwrap();
    assert!(
        matches!(receive(&mut rx).await, ControlMessage::Ack { message_id: id, status: AckStatus::PeerOffline, .. } if id == message_id)
    );
    let sid = PairingSessionId::new();
    let pending = state
        .create_pairing(CreatePairingRequest {
            protocol_version: clipsync_protocol::PROTOCOL_VERSION,
            pairing_session_id: sid.clone(),
            device_id: DeviceId::new(),
            device_name: "fixture".into(),
            os: "fixture".into(),
            ttl_secs: None,
        })
        .unwrap();
    ctx.as_mut().unwrap().pairing_sid = Some(sid.to_string());
    handle_control(
        &state,
        &tx,
        &mut ctx,
        ControlMessage::Error {
            code: "abort".into(),
            message: "cancelled by fixture".into(),
        },
        None,
    )
    .await
    .unwrap();
    assert!(state.pairing(&pending.pairing_code).is_none());
    send_err(&tx, "fixture_error", "details").await;
    assert!(
        matches!(receive(&mut rx).await, ControlMessage::Error { code, message } if code == "fixture_error" && message == "details")
    );
    handle_control(&state, &tx, &mut ctx, ControlMessage::Pong { ts: 5 }, None)
        .await
        .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "unsolicited pong must not generate a reply loop"
    );
}
