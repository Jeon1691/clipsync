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

#[test]
fn websocket_message_budget_rejects_the_eighty_first_fast_message_and_resets_after_twenty_ms() {
    let start = Instant::now();
    let mut last = start;
    let mut burst = 0;
    for _ in 0..80 {
        assert!(!message_rate_exceeded(
            &mut last,
            &mut burst,
            start + Duration::from_millis(19)
        ));
    }
    assert!(message_rate_exceeded(
        &mut last,
        &mut burst,
        start + Duration::from_millis(19)
    ));
    assert!(!message_rate_exceeded(
        &mut last,
        &mut burst,
        start + Duration::from_millis(20)
    ));
    assert_eq!(burst, 0);
    assert_eq!(last, start + Duration::from_millis(20));
    assert!(!message_rate_exceeded(
        &mut last,
        &mut burst,
        start + Duration::from_millis(21)
    ));
    assert_eq!(burst, 1);
}

#[tokio::test]
async fn websocket_recovers_after_malformed_control_and_unauthenticated_binary() {
    tokio::time::timeout(Duration::from_secs(10), async {
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    let state = state();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .route("/v1/ws", axum::routing::get(ws_handler))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/v1/ws"))
        .await
        .unwrap();
    socket
        .send(ClientMessage::Text("invalid JSON".into()))
        .await
        .unwrap();
    socket
        .send(ClientMessage::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    socket
        .send(ClientMessage::Pong(vec![].into()))
        .await
        .unwrap();
    let mut invalid = hello(&DeviceId::new(), &RoomId::new(), "invalid");
    if let ControlMessage::Hello {
        protocol_version, ..
    } = &mut invalid
    {
        *protocol_version = 0;
    }
    socket
        .send(ClientMessage::Text(invalid.encode().unwrap().into()))
        .await
        .unwrap();
    socket
        .send(ClientMessage::Text(
            ControlMessage::Ping { ts: 912 }.encode().unwrap().into(),
        ))
        .await
        .unwrap();
    let messages = tokio::time::timeout(Duration::from_secs(5), async {
        let mut messages = Vec::new();
        while messages.len() < 2 {
            if let ClientMessage::Text(text) = socket.next().await.unwrap().unwrap() {
                messages.push(ControlMessage::decode(&text).unwrap());
            }
        }
        messages
    })
    .await
    .unwrap();
    assert!(
        matches!(&messages[0], ControlMessage::Error { code, message } if code == "error" && message == "unsupported protocol version")
    );
    assert!(matches!(messages[1], ControlMessage::Pong { ts: 912 }));
    socket.close(None).await.unwrap();
    server.abort();

        }).await.expect("loopback protocol fixture exceeded its deadline");
}

#[tokio::test]
async fn unauthenticated_forwarding_and_absent_pairing_peers_fail_without_inventing_recipients() {
    let state = state();
    let (tx, mut rx) = mpsc::channel(32);
    let sid = PairingSessionId::new();
    let owner = DeviceId::new();
    let offer = state
        .create_pairing(CreatePairingRequest {
            pairing_session_id: sid.clone(),
            device_id: owner.clone(),
            device_name: "owner".into(),
            os: "fixture".into(),
            protocol_version: clipsync_protocol::PROTOCOL_VERSION,
            ttl_secs: Some(60),
        })
        .unwrap();
    for message in [
        ControlMessage::NoiseHandshake {
            message: "opaque".into(),
        },
        ControlMessage::Manifest {
            routing: RoutingHeader {
                version: clipsync_protocol::PROTOCOL_VERSION,
                message_id: MessageId::new(),
                transfer_id: TransferId::new(),
                room_id: offer.room_id.clone(),
                sender_device_id: owner.clone(),
                recipient: Recipient::Broadcast,
                sent_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now(),
                item_kind: ItemKind::Files,
                seq: 1,
            },
            nonce: "opaque".into(),
            ciphertext: "opaque".into(),
            chunk_count: 1,
            total_bytes: 1,
        },
        ControlMessage::Ack {
            message_id: MessageId::new(),
            transfer_id: None,
            status: AckStatus::Delivered,
            detail: None,
        },
    ] {
        assert_eq!(
            handle_control(&state, &tx, &mut None, message, None)
                .await
                .unwrap_err(),
            "hello required"
        );
    }
    let mut creator = None;
    let mut greeting = hello(&owner, &offer.room_id, &offer.pairing_token);
    if let ControlMessage::Hello { role, room_id, .. } = &mut greeting {
        *role = ClientRole::PairingA;
        *room_id = None;
    }
    handle_control(&state, &tx, &mut creator, greeting, None)
        .await
        .unwrap();
    receive(&mut rx).await;
    let message = ControlMessage::Spake2Msg {
        pairing_session_id: sid.clone(),
        message: "opaque".into(),
    };
    forward_pairing_or_peer(&state, creator.as_ref().unwrap(), message.clone())
        .await
        .unwrap();
    assert!(rx.try_recv().is_err());
    creator.as_mut().unwrap().pairing_sid = Some("absent session".into());
    forward_pairing_or_peer(&state, creator.as_ref().unwrap(), message)
        .await
        .unwrap();
    assert!(rx.try_recv().is_err());
    handle_control(
        &state,
        &tx,
        &mut None,
        ControlMessage::Error {
            code: "cancelled".into(),
            message: "no session".into(),
        },
        None,
    )
    .await
    .unwrap();
    let no_session_token = issue_token(
        &state.config.token_secret,
        "pair",
        &offer.room_id,
        &owner,
        DeviceRole::Owner,
        None,
        60,
    );
    let mut greeting = hello(&owner, &offer.room_id, &no_session_token);
    if let ControlMessage::Hello { role, .. } = &mut greeting {
        *role = ClientRole::PairingB;
    }
    handle_control(&state, &tx, &mut None, greeting, None)
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::HelloOk { .. }
    ));
}

#[tokio::test]
async fn regression_f88_revocation_closes_the_registered_socket_before_discarding_its_sender() {
    let state = state();
    let (rid, owner, member, owner_token, member_token) = room(&state);
    let (owner_tx, mut owner_rx) = mpsc::channel(16);
    let (member_tx, mut member_rx) = mpsc::channel(16);
    let mut owner_ctx = None;
    let mut member_ctx = None;
    handle_control(
        &state,
        &owner_tx,
        &mut owner_ctx,
        hello(&owner, &rid, &owner_token),
        None,
    )
    .await
    .unwrap();
    receive(&mut owner_rx).await;
    handle_control(
        &state,
        &member_tx,
        &mut member_ctx,
        hello(&member, &rid, &member_token),
        None,
    )
    .await
    .unwrap();
    receive(&mut member_rx).await;
    receive(&mut owner_rx).await;
    handle_control(
        &state,
        &owner_tx,
        &mut owner_ctx,
        ControlMessage::Revoke {
            device_id: member.clone(),
        },
        None,
    )
    .await
    .unwrap();
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_millis(500), member_rx.recv()).await,
            Ok(Some(WsOut::Close))
        ),
        "revoked socket was removed from routing without being told to close"
    );
    assert!(!state.is_online(member.as_str()));
    let room = state.room(rid.as_str()).unwrap();
    assert!(room.devices[member.as_str()].revoked);
    assert!(!room.devices[owner.as_str()].revoked);
    assert_eq!(room.epoch, 2);
    drop(room);
    assert_eq!(
        handle_control(
            &state,
            &member_tx,
            &mut None,
            hello(&member, &rid, &member_token),
            None
        )
        .await
        .unwrap_err(),
        "device revoked"
    );
    assert!(
        !state.is_online(member.as_str()),
        "rejected credentials were re-registered as an online device"
    );
    assert!(state.take_queue(owner.as_str()).is_none());
}

#[tokio::test]
async fn regression_f88_revoked_network_peer_is_closed_cannot_send_and_cannot_reauthenticate() {
    tokio::time::timeout(Duration::from_secs(10), async {
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    let state = state();
    let (rid, owner, member, owner_token, member_token) = room(&state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .route("/v1/ws", axum::routing::get(ws_handler))
                .with_state(serving),
        )
        .await
        .unwrap();
    });
    let url = format!("ws://{address}/v1/ws");
    let (mut owner_socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    owner_socket
        .send(ClientMessage::Text(
            hello(&owner, &rid, &owner_token).encode().unwrap().into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        owner_socket.next().await.unwrap().unwrap(),
        ClientMessage::Text(_)
    ));
    let (mut member_socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    member_socket
        .send(ClientMessage::Text(
            hello(&member, &rid, &member_token).encode().unwrap().into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        member_socket.next().await.unwrap().unwrap(),
        ClientMessage::Text(_)
    ));
    assert!(matches!(
        owner_socket.next().await.unwrap().unwrap(),
        ClientMessage::Text(_)
    ));
    let forwarded = state.frames_forwarded.load(Ordering::Relaxed);
    owner_socket
        .send(ClientMessage::Text(
            ControlMessage::Revoke {
                device_id: member.clone(),
            }
            .encode()
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(3), member_socket.next())
        .await
        .expect("revoked websocket remained connected")
        .unwrap()
        .unwrap();
    assert!(matches!(closed, ClientMessage::Close(_)));
    let now = chrono::Utc::now();
    let forbidden = ControlMessage::TextEnvelope {
        routing: RoutingHeader {
            version: clipsync_protocol::PROTOCOL_VERSION,
            message_id: MessageId::new(),
            transfer_id: TransferId::new(),
            room_id: rid.clone(),
            sender_device_id: member.clone(),
            recipient: Recipient::Broadcast,
            sent_at: now,
            expires_at: now + chrono::Duration::seconds(60),
            item_kind: ItemKind::Text,
            seq: 1,
        },
        nonce: "opaque".into(),
        ciphertext: "must not forward".into(),
    };
    assert!(member_socket
        .send(ClientMessage::Text(forbidden.encode().unwrap().into()))
        .await
        .is_err());
    assert_eq!(state.frames_forwarded.load(Ordering::Relaxed), forwarded);
    assert!(state.take_queue(owner.as_str()).is_none());
    let (mut rejected, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    rejected
        .send(ClientMessage::Text(
            hello(&member, &rid, &member_token).encode().unwrap().into(),
        ))
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(3), rejected.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ClientMessage::Text(text) = frame else {
        panic!("expected rejection")
    };
    assert!(
        matches!(ControlMessage::decode(&text).unwrap(), ControlMessage::Error { message, .. } if message == "device revoked")
    );
    assert!(!state.is_online(member.as_str()));
    assert_eq!(state.connections_total.load(Ordering::Relaxed), 2);
    owner_socket
        .send(ClientMessage::Text(
            ControlMessage::Ping { ts: 89 }.encode().unwrap().into(),
        ))
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(3), owner_socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ClientMessage::Text(text) = frame else {
        panic!("owner connection stopped")
    };
    assert!(matches!(
        ControlMessage::decode(&text).unwrap(),
        ControlMessage::Pong { ts: 89 }
    ));
    owner_socket.close(None).await.unwrap();
    rejected.close(None).await.unwrap();
    server.abort();

        }).await.expect("loopback protocol fixture exceeded its deadline");
}
