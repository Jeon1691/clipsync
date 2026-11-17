// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#[path = "ws_boundary_tests.rs"]
mod boundary_tests;

use super::*;
use crate::{auth::issue_token, RelayConfig};
use clipsync_protocol::{
    CreatePairingRequest, JoinPairingRequest, PairingSessionId, Recipient, RoomId, RoutingHeader,
    TransferId,
};

#[tokio::test]
async fn regression_f98_stale_room_socket_close_does_not_announce_live_replacement_left() {
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn control(socket: &mut Client) -> ControlMessage {
        loop {
            match socket.next().await.unwrap().unwrap() {
                ClientMessage::Text(text) => return ControlMessage::decode(&text).unwrap(),
                ClientMessage::Ping(bytes) => {
                    socket.send(ClientMessage::Pong(bytes)).await.unwrap()
                }
                other => panic!("expected established-room control frame, got {other:?}"),
            }
        }
    }
    async fn connect(
        address: std::net::SocketAddr,
        device: &DeviceId,
        room: &RoomId,
        token: &str,
    ) -> Client {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/v1/ws"))
            .await
            .unwrap();
        socket
            .send(ClientMessage::Text(
                hello(device, room, token).encode().unwrap().into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            control(&mut socket).await,
            ControlMessage::HelloOk { .. }
        ));
        socket
    }

    tokio::time::timeout(Duration::from_secs(10), async {
        let state = state();
        let (rid, owner, member, owner_token, member_token) = room(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new()
                .route("/v1/ws", axum::routing::get(ws_handler))
                .with_state(server_state)).await.unwrap();
        });
        let mut peer = connect(address, &member, &rid, &member_token).await;
        let mut old = connect(address, &owner, &rid, &owner_token).await;
        assert!(matches!(control(&mut peer).await, ControlMessage::PeerJoined { device } if device.device_id == owner));
        let old_sender = state.connections.get(owner.as_str()).unwrap().value().clone();
        let mut current = connect(address, &owner, &rid, &owner_token).await;
        assert!(matches!(control(&mut peer).await, ControlMessage::PeerJoined { device } if device.device_id == owner));
        let current_sender = state.connections.get(owner.as_str()).unwrap().value().clone();
        assert!(!old_sender.same_channel(&current_sender));

        old.close(None).await.unwrap();
        // The old writer's receiver closes only after its socket cleanup has
        // queued any leave notification and sent its Close. No blind delay.
        old_sender.closed().await;
        assert!(state.connections.get(owner.as_str()).unwrap().same_channel(&current_sender));
        peer.send(ClientMessage::Text(ControlMessage::Ping { ts: 9801 }.encode().unwrap().into())).await.unwrap();
        assert!(matches!(control(&mut peer).await, ControlMessage::Pong { ts: 9801 }),
            "a stale socket must not announce a still-live device as offline");

        peer.send(ClientMessage::Text(ControlMessage::Ack {
            message_id: MessageId::from("replacement-live"),
            transfer_id: None,
            status: AckStatus::Delivered,
            detail: None,
        }.encode().unwrap().into())).await.unwrap();
        assert!(matches!(control(&mut current).await, ControlMessage::Ack { message_id, .. } if message_id.as_str() == "replacement-live"));

        current.close(None).await.unwrap();
        current_sender.closed().await;
        assert!(matches!(control(&mut peer).await, ControlMessage::PeerLeft { device_id } if device_id == owner));
        assert!(!state.is_online(owner.as_str()));
        peer.send(ClientMessage::Text(ControlMessage::Ping { ts: 9802 }.encode().unwrap().into())).await.unwrap();
        assert!(matches!(control(&mut peer).await, ControlMessage::Pong { ts: 9802 }),
            "retiring the current socket must announce exactly one leave");
        peer.close(None).await.unwrap();
        server.abort();
    }).await.expect("established-room replacement fixture exceeded its deadline");
}

struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn product_logs() -> &'static Arc<std::sync::Mutex<Vec<u8>>> {
    static LOGS: std::sync::OnceLock<Arc<std::sync::Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(move || LogWriter(writer.clone()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        logs
    })
}

fn state() -> Arc<RelayState> {
    let _ = product_logs();
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
async fn websocket_frame_limits_reject_oversized_binary_before_dispatch() {
    use tokio_tungstenite::tungstenite::protocol::frame::{
        coding::{Data, OpCode},
        Frame,
    };
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    tokio::time::timeout(Duration::from_secs(10), async {
        for fragmented in [false, true] {
            let state = state();
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
            let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/v1/ws"))
                .await
                .unwrap();
            if fragmented {
                for (opcode, final_frame) in [(Data::Binary, false), (Data::Continue, true)] {
                    socket
                        .send(ClientMessage::Frame(Frame::message(
                            vec![7; FRAME_HARD_CAP / 2],
                            OpCode::Data(opcode),
                            final_frame,
                        )))
                        .await
                        .unwrap();
                }
            } else {
                socket
                    .send(ClientMessage::Binary(vec![7; FRAME_HARD_CAP].into()))
                    .await
                    .unwrap();
            }
            socket
                .send(ClientMessage::Text(
                    ControlMessage::Ping { ts: 384 }.encode().unwrap().into(),
                ))
                .await
                .unwrap();
            let ClientMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("valid boundary message closed the websocket")
            };
            assert!(matches!(
                ControlMessage::decode(&text).unwrap(),
                ControlMessage::Pong { ts: 384 }
            ));
            let sent = if fragmented {
                socket
                    .send(ClientMessage::Frame(Frame::message(
                        vec![7; FRAME_HARD_CAP / 2 + 1],
                        OpCode::Data(Data::Binary),
                        false,
                    )))
                    .await
                    .unwrap();
                socket
                    .send(ClientMessage::Frame(Frame::message(
                        vec![7; FRAME_HARD_CAP / 2 + 1],
                        OpCode::Data(Data::Continue),
                        true,
                    )))
                    .await
            } else {
                socket
                    .send(ClientMessage::Binary(vec![7; FRAME_HARD_CAP + 1].into()))
                    .await
            };
            if sent.is_ok() {
                assert!(!matches!(
                    socket.next().await,
                    Some(Ok(ClientMessage::Binary(_) | ClientMessage::Text(_)))
                ));
            }
            assert_eq!(state.frames_forwarded.load(Ordering::Relaxed), 0);
            assert_eq!(state.connections_total.load(Ordering::Relaxed), 0);
            server.abort();
        }
    })
    .await
    .expect("oversized websocket fixture exceeded its deadline");
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
        let logs = String::from_utf8(product_logs().lock().unwrap().clone()).unwrap();
        assert!(logs.contains("bad control"), "malformed control lost its diagnostic context");
        assert!(!logs.contains("invalid JSON"), "malformed payload bytes entered relay logs");
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
        after_commitment: false,
    };
    forward_pairing_or_peer(&state, creator.as_ref().unwrap(), &tx, message.clone())
        .await
        .unwrap();
    assert!(rx.try_recv().is_err());
    state.abort_pairing(sid.as_str());
    assert_eq!(
        forward_pairing_or_peer(&state, creator.as_ref().unwrap(), &tx, message)
            .await
            .unwrap_err(),
        "unknown pairing session"
    );
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
    assert_eq!(
        handle_control(&state, &tx, &mut None, greeting, None)
            .await
            .unwrap_err(),
        "pairing session required"
    );
    assert!(rx.try_recv().is_err());
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

#[test]
fn regression_f98_late_creator_completes_real_core_pairing_and_persists_matching_keys() {
    const NAME: &str = "ws::tests::regression_f98_late_creator_completes_real_core_pairing_and_persists_matching_keys";
    const WORKER: &str = "CLIPSYNC_RELAY_PAIRING_WORKER";
    if std::env::var(WORKER).as_deref() != Ok(NAME) {
        let home = tempfile::TempDir::new().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(WORKER, NAME)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("CLIPSYNC_HOME", home.path().join("app"))
            .env("CLIPSYNC_NO_DAEMON", "1")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("CLIPSYNC_LISTEN")
            .env_remove("CLIPSYNC_CA_FILE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        let completed = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = child.wait_with_output().unwrap();
        assert!(
            completed && output.status.success(),
            "private pairing worker failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // Select the library's memory-only test backend before constructing any
    // credential entries, and only in this exact-test child process.
    keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(15), async {
                let state = state();
                let (offered, offer) = tokio::sync::oneshot::channel();
                let offered = Arc::new(std::sync::Mutex::new(Some(offered)));
                let (release, released) = tokio::sync::oneshot::channel();
                let released = Arc::new(std::sync::Mutex::new(Some(released)));
                let app = axum::Router::new()
                    .route(
                        "/v1/pairing/create",
                        axum::routing::post(crate::create_pairing),
                    )
                    .route("/v1/pairing/join", axum::routing::post(crate::join_pairing))
                    .route("/v1/ws", axum::routing::get(ws_handler))
                    .with_state(state.clone())
                    .layer(axum::middleware::from_fn(
                        move |request: axum::extract::Request, next: axum::middleware::Next| {
                            let offered = offered.clone();
                            let released = released.clone();
                            async move {
                                let create = request.uri().path() == "/v1/pairing/create";
                                let mut response = next.run(request).await;
                                if create {
                                    let body = std::mem::replace(
                                        response.body_mut(),
                                        axum::body::Body::empty(),
                                    );
                                    let bytes =
                                        axum::body::to_bytes(body, 16 * 1024).await.unwrap();
                                    let created: clipsync_protocol::CreatePairingResponse =
                                        serde_json::from_slice(&bytes).unwrap();
                                    offered
                                        .lock()
                                        .unwrap()
                                        .take()
                                        .unwrap()
                                        .send(created)
                                        .unwrap();
                                    // A cannot open its WebSocket until this real HTTP
                                    // response is released. B proceeds using the real join.
                                    let released = released.lock().unwrap().take().unwrap();
                                    released.await.unwrap();
                                    *response.body_mut() = axum::body::Body::from(bytes);
                                }
                                response
                            }
                        },
                    ));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let server = tokio::spawn(async move {
                    axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                    )
                    .await
                    .unwrap();
                });
                let owner_dir = tempfile::TempDir::new().unwrap();
                let member_dir = tempfile::TempDir::new().unwrap();
                let make_store = |path: &std::path::Path| {
                    let store = clipsync_core::App::open_from(
                        clipsync_storage::AppPaths::from_root(path.into()),
                    )
                    .unwrap()
                    .store;
                    let mut config = store.load_config().unwrap();
                    config.relay_url = url.clone();
                    config.notify.enabled = false;
                    config.sync.auto_start = false;
                    store.save_config(&config).unwrap();
                    store
                };
                let owner_store = make_store(owner_dir.path());
                let member_store = make_store(member_dir.path());
                let owner =
                    clipsync_crypto::DeviceIdentity::from_bytes("late-creator".into(), [81; 32]);
                let member =
                    clipsync_crypto::DeviceIdentity::from_bytes("early-joiner".into(), [82; 32]);
                // Each side shows its verification code to the other and
                // confirms only on a match, as two users comparing screens would.
                let (owner_sas_tx, owner_sas_rx) = tokio::sync::oneshot::channel::<String>();
                let (member_sas_tx, member_sas_rx) = tokio::sync::oneshot::channel::<String>();
                let owner_task = {
                    let store = owner_store.clone();
                    tokio::spawn(async move {
                        clipsync_core::create_room(
                            &store,
                            &owner,
                            &store.load_config().unwrap(),
                            60,
                            move |check: clipsync_core::SasCheck| async move {
                                let _ = owner_sas_tx.send(check.sas.clone());
                                match member_sas_rx.await {
                                    Ok(other) if check.matches(&other) => {
                                        clipsync_core::SasDecision::Confirmed
                                    }
                                    _ => clipsync_core::SasDecision::Rejected,
                                }
                            },
                        )
                        .await
                    })
                };
                let offered = offer.await.unwrap();
                let member_task = {
                    let store = member_store.clone();
                    let code = offered.pairing_code.clone();
                    tokio::spawn(async move {
                        clipsync_core::join_room(
                            &store,
                            &member,
                            &store.load_config().unwrap(),
                            &code,
                            move |check: clipsync_core::SasCheck| async move {
                                let _ = member_sas_tx.send(check.sas.clone());
                                match owner_sas_rx.await {
                                    Ok(other) if check.matches(&other) => {
                                        clipsync_core::SasDecision::Confirmed
                                    }
                                    _ => clipsync_core::SasDecision::Rejected,
                                }
                            },
                        )
                        .await
                    })
                };
                // Observe a genuine processed B commitment (its first pairing
                // frame; B keeps its SPAKE message back until A's arrives); do
                // not fabricate pairing state or rely on socket-file presence,
                // scheduler order or a blind delay.
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if state.pairing(&offered.pairing_code).unwrap().pending_commit[1].is_some()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                })
                .await
                .unwrap();
                release.send(()).unwrap();
                let created = owner_task.await.unwrap().unwrap();
                let joined = member_task.await.unwrap().unwrap();
                assert_eq!(created.room_id, joined.room_id);
                assert_eq!(created.pairing_code, offered.pairing_code);
                assert_eq!(owner_store.current_room().unwrap().role, DeviceRole::Owner);
                assert_eq!(
                    member_store.current_room().unwrap().role,
                    DeviceRole::Member
                );
                assert_eq!(
                    owner_store.load_room_root(&created.room_id).unwrap(),
                    member_store.load_room_root(&created.room_id).unwrap()
                );
                assert_eq!(
                    owner_store.load_epoch_key(&created.room_id).unwrap(),
                    member_store.load_epoch_key(&created.room_id).unwrap()
                );
                let session = state.pairing(&offered.pairing_code).unwrap();
                assert!(session.pending_spake.iter().all(Option::is_none));
                assert!(session.pending_commit.iter().all(Option::is_none));
                drop(session);
                server.abort();
            })
            .await
            .expect("late creator Core pairing exceeded its deadline");
        });
}

#[tokio::test]
async fn regression_f98_expiry_discards_pending_spake_and_prevents_admission_or_completion() {
    let state = state();
    let sid = PairingSessionId::new();
    let owner = DeviceId::new();
    let member = DeviceId::new();
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
    let joined = state
        .join_pairing(
            JoinPairingRequest {
                protocol_version: clipsync_protocol::PROTOCOL_VERSION,
                pairing_code: offer.pairing_code.clone(),
                device_id: member.clone(),
                device_name: "member".into(),
                os: "fixture".into(),
            },
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
    let (member_tx, mut member_rx) = mpsc::channel(16);
    let mut member_ctx = None;
    let mut member_hello = hello(&member, &offer.room_id, &joined.pairing_token);
    if let ControlMessage::Hello { role, .. } = &mut member_hello {
        *role = ClientRole::PairingB;
    }
    handle_control(&state, &member_tx, &mut member_ctx, member_hello, None)
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut member_rx).await,
        ControlMessage::HelloOk { .. }
    ));
    let early = ControlMessage::Spake2Msg {
        pairing_session_id: sid.clone(),
        message: "early".into(),
        after_commitment: false,
    };
    handle_control(&state, &member_tx, &mut member_ctx, early.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        state.pairing(sid.as_str()).unwrap().pending_spake[1]
            .as_ref()
            .map(|pending| pending.message.as_str()),
        Some("early")
    );
    // Advance the existing session's expiry boundary without a two-minute test.
    state.pairing_mut(sid.as_str()).unwrap().expires_at =
        chrono::Utc::now() - chrono::Duration::seconds(1);
    assert_eq!(
        handle_control(&state, &member_tx, &mut member_ctx, early, None)
            .await
            .unwrap_err(),
        "pairing expired"
    );
    assert!(state
        .pairing(sid.as_str())
        .unwrap()
        .pending_spake
        .iter()
        .all(Option::is_none));
    let (owner_tx, mut owner_rx) = mpsc::channel(16);
    let mut owner_hello = hello(&owner, &offer.room_id, &offer.pairing_token);
    if let ControlMessage::Hello { role, .. } = &mut owner_hello {
        *role = ClientRole::PairingA;
    }
    assert_eq!(
        handle_control(&state, &owner_tx, &mut None, owner_hello, None)
            .await
            .unwrap_err(),
        "pairing expired"
    );
    assert!(!state.is_online(owner.as_str()));
    assert!(owner_rx.try_recv().is_err());
    assert!(state
        .complete_pairing_bound(sid.as_str(), Some(offer.room_id.as_str()))
        .is_none());
    assert!(state.room(offer.room_id.as_str()).is_none());
    state.gc();
    assert!(state.pairing(sid.as_str()).is_none());
}
