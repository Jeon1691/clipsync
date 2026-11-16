// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#[path = "ws_boundary_tests.rs"]
mod boundary_tests;

use super::*;
use crate::{auth::issue_token, RelayConfig, MAX_MEMBER_SOCKETS};
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
        let old_sender = state.member_senders(rid.as_str(), owner.as_str())[0].1.clone();
        let mut current = connect(address, &owner, &rid, &owner_token).await;
        assert!(matches!(control(&mut peer).await, ControlMessage::PeerJoined { device } if device.device_id == owner));
        // Both sockets of the device stay routed, oldest first.
        let senders = state.member_senders(rid.as_str(), owner.as_str());
        assert_eq!(senders.len(), 2);
        assert!(senders[0].1.same_channel(&old_sender));
        let current_sender = senders[1].1.clone();

        old.close(None).await.unwrap();
        // The old writer's receiver closes only after its socket cleanup has
        // queued any leave notification and sent its Close. No blind delay.
        old_sender.closed().await;
        let senders = state.member_senders(rid.as_str(), owner.as_str());
        assert_eq!(senders.len(), 1);
        assert!(senders[0].1.same_channel(&current_sender));
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
        assert!(!state.is_online(rid.as_str(), owner.as_str()));
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
    assert!(!state.is_online(rid.as_str(), member.as_str()));
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
        room_id: rid.clone(),
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
        state
            .take_queue(rid.as_str(), member.as_str())
            .unwrap()
            .ciphertext,
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
    state
        .register_conn(rid.as_str(), member.as_str(), peer_tx)
        .unwrap();
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
    let registration = member_ctx.as_ref().unwrap().member_conn.as_ref().unwrap();
    // The socket loop closes the connection once it observes the eviction.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), registration.evicted.notified())
            .await
            .is_ok(),
        "revoked socket was removed from routing without being told to close"
    );
    assert!(!state.owns_route(rid.as_str(), member.as_str(), registration.id));
    assert!(member_rx.try_recv().is_err());
    assert_eq!(
        handle_control(
            &state,
            &member_tx,
            &mut member_ctx,
            ControlMessage::Ack {
                message_id: MessageId::from("after-revoke"),
                transfer_id: None,
                status: AckStatus::Delivered,
                detail: None,
            },
            None
        )
        .await
        .unwrap_err(),
        "room member connection required"
    );
    assert!(owner_rx.try_recv().is_err());
    assert!(!state.is_online(rid.as_str(), member.as_str()));
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
        !state.is_online(rid.as_str(), member.as_str()),
        "rejected credentials were re-registered as an online device"
    );
    assert!(state.take_queue(rid.as_str(), owner.as_str()).is_none());
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
    // Only the owner's PeerLeft was forwarded: the evicted socket's cleanup
    // fans it out before writing its Close, and the forbidden text adds none.
    assert_eq!(state.frames_forwarded.load(Ordering::Relaxed), forwarded + 1);
    assert!(state.take_queue(rid.as_str(), owner.as_str()).is_none());
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
    assert!(!state.is_online(rid.as_str(), member.as_str()));
    assert_eq!(state.connections_total.load(Ordering::Relaxed), 2);
    owner_socket
        .send(ClientMessage::Text(
            ControlMessage::Ping { ts: 89 }.encode().unwrap().into(),
        ))
        .await
        .unwrap();
    let mut owner_frames = Vec::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(3), owner_socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ClientMessage::Text(text) = frame else {
            panic!("owner connection stopped")
        };
        owner_frames.push(ControlMessage::decode(&text).unwrap());
    }
    assert!(
        matches!(
            owner_frames.as_slice(),
            [ControlMessage::PeerLeft { device_id }, ControlMessage::Pong { ts: 89 }]
                if *device_id == member
        ),
        "the owner must see the revoked member leave, then its pong: {owner_frames:?}"
    );
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
                let owner_task = {
                    let store = owner_store.clone();
                    tokio::spawn(async move {
                        clipsync_core::create_room(
                            &store,
                            &owner,
                            &store.load_config().unwrap(),
                            60,
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
                        )
                        .await
                    })
                };
                // Observe a genuine processed B SPAKE; do not fabricate pairing state
                // or rely on socket-file presence, scheduler order or a blind delay.
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if state.pairing(&offered.pairing_code).unwrap().pending_spake[1].is_some()
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
                assert!(state
                    .pairing(&offered.pairing_code)
                    .unwrap()
                    .pending_spake
                    .iter()
                    .all(Option::is_none));
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
    };
    handle_control(&state, &member_tx, &mut member_ctx, early.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        state.pairing(sid.as_str()).unwrap().pending_spake[1].as_deref(),
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
    assert!(state.pairing(sid.as_str()).unwrap().pairing_senders[0].is_none());
    assert!(owner_rx.try_recv().is_err());
    assert!(state
        .complete_pairing_bound(sid.as_str(), Some(offer.room_id.as_str()))
        .is_none());
    assert!(state.room(offer.room_id.as_str()).is_none());
    state.gc();
    assert!(state.pairing(sid.as_str()).is_none());
}

#[tokio::test]
async fn a_socket_evicted_past_the_member_cap_cannot_send_room_traffic_before_it_closes() {
    let state = state();
    let (rid, owner, member, owner_token, member_token) = room(&state);
    let (owner_tx, mut owner_rx) = mpsc::channel(16);
    let mut owner_ctx = None;
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
    let (old_tx, _old_rx) = mpsc::channel(16);
    let mut old_ctx = None;
    handle_control(
        &state,
        &old_tx,
        &mut old_ctx,
        hello(&member, &rid, &member_token),
        None,
    )
    .await
    .unwrap();
    let mut newer_rxs = Vec::new();
    for _ in 0..MAX_MEMBER_SOCKETS {
        let (new_tx, new_rx) = mpsc::channel(16);
        newer_rxs.push(new_rx);
        handle_control(
            &state,
            &new_tx,
            &mut None,
            hello(&member, &rid, &member_token),
            None,
        )
        .await
        .unwrap();
    }
    // Every registration announced the member; nothing else reached the owner.
    for _ in 0..=MAX_MEMBER_SOCKETS {
        assert!(matches!(
            receive(&mut owner_rx).await,
            ControlMessage::PeerJoined { .. }
        ));
    }
    let registration = old_ctx.as_ref().unwrap().member_conn.as_ref().unwrap();
    tokio::time::timeout(Duration::from_millis(500), registration.evicted.notified())
        .await
        .expect("the oldest socket past the cap was not evicted");

    // Until the old socket's loop observes its eviction, its frames must be
    // refused rather than routed under the member's identity.
    for frame in [
        ControlMessage::NoiseHandshake {
            message: "opaque".into(),
        },
        ControlMessage::EpochWrapMsg {
            epoch: 2,
            wraps: Vec::new(),
        },
    ] {
        assert_eq!(
            handle_control(&state, &old_tx, &mut old_ctx, frame, None)
                .await
                .unwrap_err(),
            "room member connection required"
        );
    }
    // The binary path uses the same gate, so the old socket's chunks are dropped.
    assert!(member_ctx(&state, &old_ctx).is_err());
    assert!(owner_rx.try_recv().is_err());
    assert!(state.is_online(rid.as_str(), member.as_str()));
}

// Stands in for `STALLED_MEMBER_SOCKET_TIMEOUT` so the stall is observed fast.
const TEST_STALL_TIMEOUT: Duration = Duration::from_millis(50);

#[tokio::test]
async fn a_stalled_older_member_socket_is_evicted_instead_of_blocking_delivery() {
    let state = state();
    let (rid, _, member, _, _) = room(&state);
    // A half-open socket the client already replaced: its queue never drains.
    let (stalled_tx, _stalled_rx) = mpsc::channel(1);
    stalled_tx
        .try_send(WsOut::Text("unsent".into()))
        .expect("fill the stalled queue");
    let stalled = state
        .register_conn(rid.as_str(), member.as_str(), stalled_tx)
        .unwrap();
    let (live_tx, mut live_rx) = mpsc::channel(1);
    let live = state
        .register_conn(rid.as_str(), member.as_str(), live_tx)
        .unwrap();

    let frame = WsOut::Binary(vec![4, 5, 6]);
    assert!(tokio::time::timeout(
        Duration::from_secs(3),
        deliver_within(
            &state,
            rid.as_str(),
            member.as_str(),
            frame,
            TEST_STALL_TIMEOUT
        )
    )
    .await
    .expect("a stalled socket blocked delivery to the member's live socket"));
    assert!(matches!(live_rx.recv().await, Some(WsOut::Binary(bytes)) if bytes == [4, 5, 6]));
    tokio::time::timeout(Duration::from_millis(500), stalled.evicted.notified())
        .await
        .expect("the stalled socket was not evicted");
    assert!(!state.owns_route(rid.as_str(), member.as_str(), stalled.id));
    assert!(state.owns_route(rid.as_str(), member.as_str(), live.id));
}

mod liveness {
    //! A member's sockets are routed only while they are alive and closed for
    //! real once they are not: a silent socket times out, stalled sockets cost
    //! one stall timeout per delivery, and an evicted socket frees its queue.
    use super::*;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    // Stands in for `READ_IDLE_TIMEOUT` so a silent socket is dropped quickly.
    const TEST_READ_IDLE_TIMEOUT: Duration = Duration::from_millis(800);
    // Stands in for the client's `HEARTBEAT_SECS`, well inside the idle timeout.
    const TEST_HEARTBEAT: Duration = Duration::from_millis(100);
    // Bounds every wait, so a regression fails instead of hanging.
    const FIXTURE_DEADLINE: Duration = Duration::from_secs(10);
    // Large enough that a few hundred frames fill the loopback socket buffers.
    const TEST_FILL_FRAME_BYTES: usize = 64 * 1024;
    // A send blocked this long means the writer stopped and the queue is full.
    const TEST_QUEUE_FULL_PROBE: Duration = Duration::from_millis(200);
    const METRICS_POLL: Duration = Duration::from_millis(5);

    fn state_with_read_idle_timeout(timeout: Duration) -> Arc<RelayState> {
        let _ = product_logs();
        let mut state = RelayState::new(RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "http://127.0.0.1".into(),
            token_secret: vec![42; 32],
            webhook_url: None,
            webhook_secret: None,
        });
        state.read_idle_timeout = timeout;
        Arc::new(state)
    }

    async fn serve(state: Arc<RelayState>) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
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
        (address, server)
    }

    /// The next control frame, while heartbeating like a real client so the
    /// relay keeps this socket alive. Pongs answer those heartbeats.
    async fn control(socket: &mut Client) -> ControlMessage {
        loop {
            match tokio::time::timeout(TEST_HEARTBEAT, socket.next()).await {
                Err(_) => socket
                    .send(ClientMessage::Text(
                        ControlMessage::Ping { ts: 0 }.encode().unwrap().into(),
                    ))
                    .await
                    .unwrap(),
                Ok(frame) => match frame.unwrap().unwrap() {
                    ClientMessage::Text(text) => match ControlMessage::decode(&text).unwrap() {
                        ControlMessage::Pong { .. } => {}
                        other => return other,
                    },
                    ClientMessage::Ping(bytes) => {
                        socket.send(ClientMessage::Pong(bytes)).await.unwrap()
                    }
                    // The relay answers a WebSocket ping with an empty binary frame.
                    ClientMessage::Binary(bytes) if bytes.is_empty() => {}
                    other => panic!("expected a control frame, got {other:?}"),
                },
            }
        }
    }

    async fn connect(
        address: std::net::SocketAddr,
        device: &DeviceId,
        room: &RoomId,
        token: &str,
    ) -> (Client, ControlMessage) {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/v1/ws"))
            .await
            .unwrap();
        socket
            .send(ClientMessage::Text(
                hello(device, room, token).encode().unwrap().into(),
            ))
            .await
            .unwrap();
        let hello_ok = control(&mut socket).await;
        assert!(matches!(hello_ok, ControlMessage::HelloOk { .. }));
        (socket, hello_ok)
    }

    fn text_envelope(room: &RoomId, sender: &DeviceId, ciphertext: &str) -> ControlMessage {
        ControlMessage::TextEnvelope {
            routing: RoutingHeader {
                version: clipsync_protocol::PROTOCOL_VERSION,
                message_id: MessageId::new(),
                transfer_id: TransferId::new(),
                room_id: room.clone(),
                sender_device_id: sender.clone(),
                recipient: Recipient::Broadcast,
                sent_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::seconds(60),
                item_kind: ItemKind::Text,
                seq: 1,
            },
            nonce: "opaque-nonce".into(),
            ciphertext: ciphertext.into(),
        }
    }

    async fn wait_for_live_connections(state: &RelayState, expected: u64) {
        let line = format!("clipsync_ws_connections {expected}\n");
        while !state.metrics_text().starts_with(&line) {
            tokio::time::sleep(METRICS_POLL).await;
        }
    }

    #[tokio::test]
    async fn a_silent_socket_is_dropped_so_text_is_queued_once_the_live_socket_quits() {
        tokio::time::timeout(FIXTURE_DEADLINE, async {
            let state = state_with_read_idle_timeout(TEST_READ_IDLE_TIMEOUT);
            let (rid, owner, member, owner_token, member_token) = room(&state);
            let (address, server) = serve(state.clone()).await;
            let (mut peer, _) = connect(address, &member, &rid, &member_token).await;
            // The daemon's first socket goes half-open (laptop sleep, network
            // change): the relay sees no FIN, and the client sends nothing more.
            let (stale, _) = connect(address, &owner, &rid, &owner_token).await;
            assert!(matches!(control(&mut peer).await,
                ControlMessage::PeerJoined { device } if device.device_id == owner));
            // The client watchdog reconnects, and later the daemon quits cleanly.
            let (mut live, _) = connect(address, &owner, &rid, &owner_token).await;
            assert!(matches!(control(&mut peer).await,
                ControlMessage::PeerJoined { device } if device.device_id == owner));
            let live_sender = state.member_senders(rid.as_str(), owner.as_str())[1]
                .1
                .clone();
            live.close(None).await.unwrap();
            live_sender.closed().await;

            // The silent socket must not keep the device online: the relay
            // drops it and announces the device left.
            assert!(
                matches!(control(&mut peer).await,
                ControlMessage::PeerLeft { device_id } if device_id == owner),
                "the silent socket kept the device online"
            );
            assert!(!state.is_online(rid.as_str(), owner.as_str()));

            // Text for the device is now queued, not acked into a dead socket.
            peer.send(ClientMessage::Text(
                text_envelope(&rid, &member, "for-the-returning-daemon")
                    .encode()
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
            assert!(matches!(
                control(&mut peer).await,
                ControlMessage::Ack {
                    status: AckStatus::Accepted,
                    ..
                }
            ));
            let (back, hello_ok) = connect(address, &owner, &rid, &owner_token).await;
            assert!(matches!(hello_ok,
                ControlMessage::HelloOk { queued_text: Some(queued), .. }
                    if queued.ciphertext == "for-the-returning-daemon"));
            drop((stale, back, peer));
            server.abort();
        })
        .await
        .expect("silent-socket liveness fixture exceeded its deadline");
    }

    // Stands in for `STALLED_MEMBER_SOCKET_TIMEOUT`, long enough that
    // scheduling jitter is small beside it.
    const TEST_STALL_TIMEOUT: Duration = Duration::from_millis(300);

    #[tokio::test]
    async fn stalled_older_sockets_delay_one_delivery_by_one_stall_timeout_in_total() {
        let state = state();
        let (rid, _, member, _, _) = room(&state);
        let mut stalled = Vec::new();
        for _ in 1..MAX_MEMBER_SOCKETS {
            let (tx, rx) = mpsc::channel(1);
            tx.try_send(WsOut::Text("unsent".into()))
                .expect("fill the stalled queue");
            let registration = state
                .register_conn(rid.as_str(), member.as_str(), tx)
                .unwrap();
            stalled.push((registration, rx));
        }
        let (live_tx, mut live_rx) = mpsc::channel(1);
        let live = state
            .register_conn(rid.as_str(), member.as_str(), live_tx)
            .unwrap();

        let started = tokio::time::Instant::now();
        assert!(
            deliver_within(
                &state,
                rid.as_str(),
                member.as_str(),
                WsOut::Binary(vec![7, 8, 9]),
                TEST_STALL_TIMEOUT,
            )
            .await
        );
        let waited = started.elapsed();
        // Waiting on the stalled sockets one after another takes at least one
        // stall timeout each.
        let one_after_another = TEST_STALL_TIMEOUT * u32::try_from(stalled.len()).unwrap();
        assert!(
            waited < one_after_another,
            "{} stalled sockets delayed delivery by {waited:?}, not one stall timeout",
            stalled.len()
        );
        assert!(matches!(live_rx.recv().await, Some(WsOut::Binary(bytes)) if bytes == [7, 8, 9]));
        for (registration, _) in &stalled {
            assert!(!state.owns_route(rid.as_str(), member.as_str(), registration.id));
        }
        assert!(state.owns_route(rid.as_str(), member.as_str(), live.id));
    }

    #[tokio::test]
    async fn an_evicted_socket_with_a_full_queue_closes_and_frees_its_resources() {
        let state = state();
        let (rid, _, member, _, member_token) = room(&state);
        let (address, server) = serve(state.clone()).await;
        // A client that stopped reading: its writer blocks once the socket
        // buffers fill, and then its outbound queue fills too.
        let (mut stuck, _) = connect(address, &member, &rid, &member_token).await;
        let (id, sender) = state.member_senders(rid.as_str(), member.as_str())[0].clone();
        loop {
            match sender
                .send_timeout(
                    WsOut::Binary(vec![0; TEST_FILL_FRAME_BYTES]),
                    TEST_QUEUE_FULL_PROBE,
                )
                .await
            {
                Ok(()) => {}
                Err(SendTimeoutError::Timeout(_)) => break,
                Err(SendTimeoutError::Closed(_)) => panic!("the socket closed before it stalled"),
            }
        }
        wait_for_live_connections(&state, 1).await;

        assert!(state.evict_conn(rid.as_str(), member.as_str(), id));
        tokio::time::timeout(FIXTURE_DEADLINE, async {
            // The writer and its queued frames are released...
            sender.closed().await;
            // ...the metric stops counting the socket only once that happened...
            wait_for_live_connections(&state, 0).await;
            // ...and the TCP connection is closed: the client reads to the end.
            while let Some(Ok(_)) = stuck.next().await {}
        })
        .await
        .expect("an evicted socket with a full queue was never closed");
        server.abort();
    }
}
