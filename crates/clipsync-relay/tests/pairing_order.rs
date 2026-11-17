// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Pairing presence is scoped to the authenticated session and connection order.

use base64::{engine::general_purpose::STANDARD, Engine};
use clipsync_crypto::{finish_pairing, start_pairing_a, start_pairing_b};
use clipsync_protocol::{
    ClientRole, ControlMessage, CreatePairingRequest, CreatePairingResponse, DeviceId, DeviceRole,
    JoinPairingRequest, JoinPairingResponse, PairingSessionId, RoomId, PROTOCOL_VERSION,
};
use clipsync_relay::{router, RelayConfig};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Relay {
    url: String,
    client: reqwest::Client,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Pairing {
    sid: PairingSessionId,
    room: RoomId,
    owner: DeviceId,
    member: DeviceId,
    owner_token: String,
    member_token: String,
}

impl Relay {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");
        let app = router(RelayConfig {
            bind: address,
            public_url: url.clone(),
            token_secret: vec![72; 32],
            webhook_url: None,
            webhook_secret: None,
        });
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        Self { url, client, task }
    }

    async fn pairing(&self, owner: DeviceId, member: DeviceId) -> Pairing {
        let sid = PairingSessionId::new();
        let created: CreatePairingResponse = self
            .client
            .post(format!("{}/v1/pairing/create", self.url))
            .json(&CreatePairingRequest {
                pairing_session_id: sid.clone(),
                device_id: owner.clone(),
                device_name: "order owner".into(),
                os: "fixture".into(),
                protocol_version: PROTOCOL_VERSION,
                ttl_secs: Some(60),
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let joined: JoinPairingResponse = self
            .client
            .post(format!("{}/v1/pairing/join", self.url))
            .json(&JoinPairingRequest {
                pairing_code: created.pairing_code,
                device_id: member.clone(),
                device_name: "order member".into(),
                os: "fixture".into(),
                protocol_version: PROTOCOL_VERSION,
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(joined.pairing_session_id, sid);
        assert_eq!(joined.room_id, created.room_id);
        Pairing {
            sid,
            room: created.room_id,
            owner,
            member,
            owner_token: created.pairing_token,
            member_token: joined.pairing_token,
        }
    }

    async fn connect(&self, pairing: &Pairing, owner: bool) -> Socket {
        self.connect_greeting(greeting(pairing, owner)).await
    }

    async fn connect_greeting(&self, hello: ControlMessage) -> Socket {
        let (mut socket, _) = tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async(format!(
                "{}/v1/ws",
                self.url.replacen("http://", "ws://", 1)
            )),
        )
        .await
        .unwrap()
        .unwrap();
        send(&mut socket, hello).await;
        socket
    }

    async fn connections(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let metrics = self
                    .client
                    .get(format!("{}/metrics", self.url))
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap();
                if metrics
                    .lines()
                    .any(|line| line == format!("clipsync_ws_connections {expected}"))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

fn greeting(pairing: &Pairing, owner: bool) -> ControlMessage {
    ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: if owner {
            pairing.owner.clone()
        } else {
            pairing.member.clone()
        },
        device_name: if owner { "order owner" } else { "order member" }.into(),
        os: "fixture".into(),
        role: if owner {
            ClientRole::PairingA
        } else {
            ClientRole::PairingB
        },
        room_id: Some(pairing.room.clone()),
        token: if owner {
            pairing.owner_token.clone()
        } else {
            pairing.member_token.clone()
        },
        pairing_session_id: Some(pairing.sid.clone()),
    }
}

async fn fence(socket: &mut Socket, ts: i64) {
    send(socket, ControlMessage::Ping { ts }).await;
    assert!(
        matches!(control(socket).await, ControlMessage::Pong { ts: received } if received == ts)
    );
}

async fn send(socket: &mut Socket, message: ControlMessage) {
    socket
        .send(Message::Text(message.encode().unwrap().into()))
        .await
        .unwrap();
}

async fn control(socket: &mut Socket) -> ControlMessage {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return ControlMessage::decode(&text).unwrap(),
                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await.unwrap(),
                Message::Close(_) => panic!("pairing socket closed unexpectedly"),
                _ => {}
            }
        }
    })
    .await
    .expect("expected pairing control frame was not delivered")
}

async fn hello_without_joined(socket: &mut Socket, pairing: &Pairing) {
    assert!(
        matches!(control(socket).await, ControlMessage::HelloOk { room_id, .. } if room_id == pairing.room)
    );
    // The pong fences all notifications caused by the preceding Hello. An HTTP
    // reservation alone must not have queued a spurious PeerJoined before it.
    send(socket, ControlMessage::Ping { ts: 98 }).await;
    assert!(matches!(
        control(socket).await,
        ControlMessage::Pong { ts: 98 }
    ));
}

fn joined(message: ControlMessage, pairing: &Pairing) {
    assert!(
        matches!(message, ControlMessage::PeerJoined { device } if device.device_id == pairing.member && device.role == DeviceRole::Member && device.online)
    );
}

#[tokio::test]
async fn regression_f98_joiner_websocket_before_creator_delivers_presence_and_typed_frames() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let pairing = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        let mut member = relay.connect(&pairing, false).await;
        assert!(matches!(
            control(&mut member).await,
            ControlMessage::HelloOk { .. }
        ));
        let member_state = start_pairing_b(
            "123456",
            pairing.sid.clone(),
            pairing.owner.clone(),
            pairing.member.clone(),
        )
        .unwrap();
        let early = STANDARD.encode(&member_state.outbound);
        send(
            &mut member,
            ControlMessage::Spake2Msg {
                pairing_session_id: pairing.sid.clone(),
                message: early.clone(),
                after_commitment: false,
            },
        )
        .await;
        // The relay has consumed B's first SPAKE while A has no socket at all.
        fence(&mut member, 101).await;
        let mut owner = relay.connect(&pairing, true).await;
        let mut hello = false;
        let mut peer = false;
        let mut replay = None;
        while !hello || !peer || replay.is_none() {
            match control(&mut owner).await {
                ControlMessage::HelloOk { room_id, .. } => {
                    assert_eq!(room_id, pairing.room);
                    hello = true;
                }
                message @ ControlMessage::PeerJoined { .. } => {
                    joined(message, &pairing);
                    peer = true;
                }
                ControlMessage::Spake2Msg {
                    pairing_session_id,
                    message,
                    ..
                } => {
                    assert_eq!(pairing_session_id, pairing.sid);
                    assert_eq!(message, early);
                    assert!(
                        replay.replace(message).is_none(),
                        "early SPAKE replayed twice"
                    );
                }
                message => panic!("unexpected pairing frame: {message:?}"),
            }
        }
        fence(&mut owner, 102).await;
        let owner_state = start_pairing_a(
            "123456",
            pairing.sid.clone(),
            pairing.owner.clone(),
            pairing.member.clone(),
        )
        .unwrap();
        let outbound = owner_state.outbound.clone();
        send(
            &mut owner,
            ControlMessage::Spake2Msg {
                pairing_session_id: pairing.sid.clone(),
                message: STANDARD.encode(&outbound),
                after_commitment: false,
            },
        )
        .await;
        let ControlMessage::Spake2Msg {
            pairing_session_id,
            message,
            ..
        } = control(&mut member).await
        else {
            panic!("missing owner SPAKE")
        };
        assert_eq!(pairing_session_id, pairing.sid);
        assert_eq!(STANDARD.decode(message).unwrap(), outbound);
        let owner_keys =
            finish_pairing(owner_state, &STANDARD.decode(replay.unwrap()).unwrap()).unwrap();
        let member_keys = finish_pairing(member_state, &outbound).unwrap();
        assert_eq!(owner_keys.room_root_key, member_keys.room_root_key);
        assert_eq!(owner_keys.pairing_wrap_key, member_keys.pairing_wrap_key);
        assert_eq!(owner_keys.transcript_hash, member_keys.transcript_hash);
        owner.close(None).await.unwrap();
        member.close(None).await.unwrap();
    })
    .await
    .expect("late-creator pairing fixture exceeded its deadline");
}

#[tokio::test]
async fn regression_f98_http_admission_waits_for_the_joiner_websocket() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let pairing = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        let mut owner = relay.connect(&pairing, true).await;
        hello_without_joined(&mut owner, &pairing).await;
        let mut member = relay.connect(&pairing, false).await;
        assert!(matches!(
            control(&mut member).await,
            ControlMessage::HelloOk { .. }
        ));
        joined(control(&mut owner).await, &pairing);
        owner.close(None).await.unwrap();
        member.close(None).await.unwrap();
    })
    .await
    .expect("HTTP admission pairing fixture exceeded its deadline");
}

#[tokio::test]
async fn regression_f98_other_session_or_disconnected_joiner_is_not_announced() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let device = DeviceId::new();
        let pending = relay.pairing(DeviceId::new(), device.clone()).await;
        let other = relay.pairing(DeviceId::new(), device).await;
        let mut member = relay.connect(&other, false).await;
        assert!(matches!(
            control(&mut member).await,
            ControlMessage::HelloOk { .. }
        ));
        let mut owner = relay.connect(&pending, true).await;
        hello_without_joined(&mut owner, &pending).await;
        owner.close(None).await.unwrap();
        member.close(None).await.unwrap();
        relay.connections(0).await;
        let disconnected = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        let mut member = relay.connect(&disconnected, false).await;
        assert!(matches!(
            control(&mut member).await,
            ControlMessage::HelloOk { .. }
        ));
        send(
            &mut member,
            ControlMessage::Spake2Msg {
                pairing_session_id: disconnected.sid.clone(),
                message: "must be discarded on disconnect".into(),
                after_commitment: false,
            },
        )
        .await;
        fence(&mut member, 301).await;
        member.close(None).await.unwrap();
        relay.connections(0).await;
        let mut owner = relay.connect(&disconnected, true).await;
        hello_without_joined(&mut owner, &disconnected).await;
        owner.close(None).await.unwrap();
    })
    .await
    .expect("session-scoped pairing fixture exceeded its deadline");
}

#[tokio::test]
async fn regression_f98_pending_spake_is_bounded_take_once_and_session_bound() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let pairing = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        let mut member = relay.connect(&pairing, false).await;
        assert!(matches!(control(&mut member).await, ControlMessage::HelloOk { .. }));
        // No other handshake phase is retained while the peer is offline.
        send(&mut member, ControlMessage::KeyConfirm { pairing_session_id: pairing.sid.clone(), mac: "unbuffered".into() }).await;
        send(&mut member, ControlMessage::Spake2Msg { pairing_session_id: PairingSessionId::new(), message: "wrong session".into(), after_commitment: false }).await;
        assert!(matches!(control(&mut member).await, ControlMessage::Error { message, .. } if message == "pairing frame session mismatch"));
        send(&mut member, ControlMessage::Spake2Msg { pairing_session_id: pairing.sid.clone(), message: "x".repeat(1025), after_commitment: false }).await;
        assert!(matches!(control(&mut member).await, ControlMessage::Error { message, .. } if message == "pairing SPAKE message too large"));
        let first = "x".repeat(1024);
        for _ in 0..2 {
            send(&mut member, ControlMessage::Spake2Msg { pairing_session_id: pairing.sid.clone(), message: first.clone(), after_commitment: false }).await;
        }
        send(&mut member, ControlMessage::Spake2Msg { pairing_session_id: pairing.sid.clone(), message: "replacement".into(), after_commitment: false }).await;
        assert!(matches!(control(&mut member).await, ControlMessage::Error { message, .. } if message == "pairing SPAKE already pending"));
        fence(&mut member, 201).await;
        let mut owner = relay.connect(&pairing, true).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::HelloOk { .. }));
        joined(control(&mut owner).await, &pairing);
        assert!(matches!(control(&mut owner).await, ControlMessage::Spake2Msg { pairing_session_id, message, .. } if pairing_session_id == pairing.sid && message == first));
        fence(&mut owner, 202).await;
        owner.close(None).await.unwrap();
        member.close(None).await.unwrap();
    }).await.expect("bounded pairing buffer fixture exceeded its deadline");
}

#[tokio::test]
async fn regression_f98_signed_session_and_pairing_roles_are_checked_before_presence() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let pairing = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        for field in ["sid", "role"] {
            let mut hello = greeting(&pairing, false);
            if let ControlMessage::Hello { role, pairing_session_id, .. } = &mut hello {
                if field == "sid" { *pairing_session_id = Some(PairingSessionId::new()); }
                else { *role = ClientRole::PairingA; }
            }
            let mut member = relay.connect_greeting(hello).await;
            assert!(matches!(control(&mut member).await, ControlMessage::Error { message, .. } if message == if field == "sid" { "token pairing session mismatch" } else { "pairing role or device mismatch" }));
            relay.connections(0).await;
            member.close(None).await.unwrap();
        }
        // Older protocol-v1 clients may omit the redundant session in Hello;
        // the signed token remains the authoritative session binding.
        let mut hello = greeting(&pairing, false);
        if let ControlMessage::Hello { pairing_session_id, .. } = &mut hello { *pairing_session_id = None; }
        let mut member = relay.connect_greeting(hello).await;
        assert!(matches!(control(&mut member).await, ControlMessage::HelloOk { .. }));
        let mut owner = relay.connect(&pairing, true).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::HelloOk { .. }));
        joined(control(&mut owner).await, &pairing);
        owner.close(None).await.unwrap();
        member.close(None).await.unwrap();
    }).await.expect("pairing authentication fixture exceeded its deadline");
}

#[tokio::test]
async fn regression_f98_replaced_socket_cannot_replay_or_unregister_its_replacement() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let pairing = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        let mut first = relay.connect(&pairing, false).await;
        assert!(matches!(control(&mut first).await, ControlMessage::HelloOk { .. }));
        send(&mut first, ControlMessage::Spake2Msg { pairing_session_id: pairing.sid.clone(), message: "old connection".into(), after_commitment: false }).await;
        fence(&mut first, 401).await;
        let mut member = relay.connect(&pairing, false).await;
        assert!(matches!(control(&mut member).await, ControlMessage::HelloOk { .. }));
        send(&mut first, ControlMessage::Spake2Msg { pairing_session_id: pairing.sid.clone(), message: "stale sender".into(), after_commitment: false }).await;
        assert!(matches!(control(&mut first).await, ControlMessage::Error { message, .. } if message == "pairing connection replaced"));
        first.close(None).await.unwrap();
        let mut owner = relay.connect(&pairing, true).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::HelloOk { .. }));
        joined(control(&mut owner).await, &pairing);
        fence(&mut owner, 402).await;
        relay.connections(2).await;
        send(&mut member, ControlMessage::Spake2Msg { pairing_session_id: pairing.sid.clone(), message: "new connection".into(), after_commitment: false }).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::Spake2Msg { message, .. } if message == "new connection"));
        owner.close(None).await.unwrap(); member.close(None).await.unwrap();
    }).await.expect("replacement socket fixture exceeded its deadline");
}

#[tokio::test]
async fn regression_pairing_commitment_is_bounded_buffered_and_only_from_the_joiner() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let pairing = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        let commit = |commitment: &str| ControlMessage::Spake2Commit {
            pairing_session_id: pairing.sid.clone(),
            commitment: commitment.into(),
        };
        let mut member = relay.connect(&pairing, false).await;
        assert!(matches!(control(&mut member).await, ControlMessage::HelloOk { .. }));
        send(
            &mut member,
            ControlMessage::Spake2Commit {
                pairing_session_id: PairingSessionId::new(),
                commitment: "wrong session".into(),
            },
        )
        .await;
        assert!(matches!(control(&mut member).await, ControlMessage::Error { message, .. } if message == "pairing frame session mismatch"));
        // A commitment is one SHA-256 digest; nothing larger is relayed.
        send(&mut member, commit(&"x".repeat(65))).await;
        assert!(matches!(control(&mut member).await, ControlMessage::Error { message, .. } if message == "pairing commitment too large"));
        // The creator is offline: the first commitment is held, a repeat is
        // harmless and a different one is refused.
        let first = STANDARD.encode([7u8; 32]);
        send(&mut member, commit(&first)).await;
        send(&mut member, commit(&first)).await;
        send(&mut member, commit("replacement")).await;
        assert!(matches!(control(&mut member).await, ControlMessage::Error { message, .. } if message == "pairing commitment already pending"));
        send(
            &mut member,
            ControlMessage::Spake2Msg {
                pairing_session_id: pairing.sid.clone(),
                message: "joiner message".into(),
                after_commitment: false,
            },
        )
        .await;
        fence(&mut member, 501).await;
        // The arriving creator gets the commitment before the message it
        // commits to.
        let mut owner = relay.connect(&pairing, true).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::HelloOk { .. }));
        joined(control(&mut owner).await, &pairing);
        assert!(matches!(control(&mut owner).await, ControlMessage::Spake2Commit { pairing_session_id, commitment } if pairing_session_id == pairing.sid && commitment == first));
        assert!(matches!(control(&mut owner).await, ControlMessage::Spake2Msg { message, after_commitment: false, .. } if message == "joiner message"));
        // Only the joiner commits.
        send(&mut owner, commit(&first)).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::Error { message, .. } if message == "only the joiner sends a pairing commitment"));
        // The creator's acknowledgement reaches the joiner unchanged.
        send(
            &mut owner,
            ControlMessage::Spake2Msg {
                pairing_session_id: pairing.sid.clone(),
                message: "creator message".into(),
                after_commitment: true,
            },
        )
        .await;
        assert!(matches!(control(&mut member).await, ControlMessage::Spake2Msg { message, after_commitment: true, .. } if message == "creator message"));
        // While both are connected a commitment is forwarded at once.
        send(&mut member, commit(&first)).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::Spake2Commit { commitment, .. } if commitment == first));
        fence(&mut owner, 502).await;
        owner.close(None).await.unwrap();
        member.close(None).await.unwrap();
    })
    .await
    .expect("pairing commitment fixture exceeded its deadline");
}

#[tokio::test]
async fn regression_buffered_creator_message_keeps_its_commitment_acknowledgement() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let relay = Relay::start().await;
        let pairing = relay.pairing(DeviceId::new(), DeviceId::new()).await;
        let mut owner = relay.connect(&pairing, true).await;
        assert!(matches!(control(&mut owner).await, ControlMessage::HelloOk { .. }));
        send(
            &mut owner,
            ControlMessage::Spake2Msg {
                pairing_session_id: pairing.sid.clone(),
                message: "creator message".into(),
                after_commitment: true,
            },
        )
        .await;
        fence(&mut owner, 601).await;
        let mut member = relay.connect(&pairing, false).await;
        assert!(matches!(control(&mut member).await, ControlMessage::HelloOk { .. }));
        assert!(matches!(control(&mut member).await, ControlMessage::Spake2Msg { message, after_commitment: true, .. } if message == "creator message"));
        owner.close(None).await.unwrap();
        member.close(None).await.unwrap();
    })
    .await
    .expect("buffered acknowledgement fixture exceeded its deadline");
}
