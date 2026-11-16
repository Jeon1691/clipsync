// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Room routing belongs to authenticated members, is scoped to one room, and
//! is owned by exactly one socket at a time.

use chrono::Utc;
use clipsync_protocol::{
    AckStatus, ClientRole, ControlMessage, CreatePairingRequest, CreatePairingResponse, DeviceId,
    ItemKind, JoinPairingRequest, JoinPairingResponse, MessageId, PairingSessionId, Recipient,
    RoomId, RoutingHeader, TransferId, PROTOCOL_VERSION,
};
use clipsync_relay::{router, RelayConfig};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// Generous bounds for loopback fixtures; every wait is condition-driven, so
// these only turn a regression into a failure instead of a hang.
const FIXTURE_DEADLINE: Duration = Duration::from_secs(20);
const FRAME_DEADLINE: Duration = Duration::from_secs(3);
const METRICS_POLL: Duration = Duration::from_millis(5);
// Well inside the relay's queue retention, so queued text cannot expire mid-test.
const TEXT_LIFETIME_SECS: i64 = 60;
// The relay forwards nonces opaquely; this is a 12-byte AEAD nonce in hex.
const OPAQUE_NONCE: &str = "000000000000000000000000";

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

struct Room {
    id: RoomId,
    owner: DeviceId,
    owner_token: String,
    member_token: String,
}

impl Room {
    fn token(&self, device: &DeviceId) -> &str {
        if *device == self.owner {
            &self.owner_token
        } else {
            &self.member_token
        }
    }
}

impl Relay {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");
        let app = router(RelayConfig {
            bind: address,
            public_url: url.clone(),
            token_secret: vec![73; 32],
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
            .timeout(FRAME_DEADLINE)
            .build()
            .unwrap();
        Self { url, client, task }
    }

    /// `/v1/pairing/create` is unauthenticated: any caller may name any device.
    async fn create(&self, owner: &DeviceId) -> (PairingSessionId, CreatePairingResponse) {
        let sid = PairingSessionId::new();
        let created = self
            .client
            .post(format!("{}/v1/pairing/create", self.url))
            .json(&CreatePairingRequest {
                pairing_session_id: sid.clone(),
                device_id: owner.clone(),
                device_name: "registry owner".into(),
                os: "fixture".into(),
                protocol_version: PROTOCOL_VERSION,
                ttl_secs: None,
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        (sid, created)
    }

    async fn join(&self, code: &str, member: &DeviceId) -> JoinPairingResponse {
        self.client
            .post(format!("{}/v1/pairing/join", self.url))
            .json(&JoinPairingRequest {
                pairing_code: code.into(),
                device_id: member.clone(),
                device_name: "registry member".into(),
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
            .unwrap()
    }

    async fn socket(&self, hello: ControlMessage) -> Socket {
        let (mut socket, _) = tokio::time::timeout(
            FRAME_DEADLINE,
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

    /// Completes a real relay pairing, then closes both pairing sockets and
    /// waits until `settled` authenticated sockets remain.
    async fn pair(&self, owner: &DeviceId, member: &DeviceId, settled: Option<usize>) -> Room {
        let (room, mut creator, mut joiner) = self.pair_open(owner, member).await;
        creator.close(None).await.unwrap();
        joiner.close(None).await.unwrap();
        if let Some(settled) = settled {
            self.connections(settled).await;
        }
        room
    }

    /// Completes a real relay pairing and keeps both pairing sockets open.
    async fn pair_open(&self, owner: &DeviceId, member: &DeviceId) -> (Room, Socket, Socket) {
        let (sid, created) = self.create(owner).await;
        let joined = self.join(&created.pairing_code, member).await;
        let mut creator = self
            .socket(pairing_hello(
                owner,
                ClientRole::PairingA,
                &created.room_id,
                &created.pairing_token,
                &sid,
            ))
            .await;
        until(&mut creator, "creator HelloOk", |m| {
            matches!(m, ControlMessage::HelloOk { .. })
        })
        .await;
        let mut joiner = self
            .socket(pairing_hello(
                member,
                ClientRole::PairingB,
                &joined.room_id,
                &joined.pairing_token,
                &sid,
            ))
            .await;
        until(&mut joiner, "joiner HelloOk", |m| {
            matches!(m, ControlMessage::HelloOk { .. })
        })
        .await;
        until(&mut creator, "joiner presence", |m| {
            matches!(m, ControlMessage::PeerJoined { .. })
        })
        .await;
        let bind = ControlMessage::DeviceBind {
            pairing_session_id: sid.clone(),
            nonce: OPAQUE_NONCE.into(),
            ciphertext: "opaque device binding".into(),
        };
        send(&mut creator, bind.clone()).await;
        send(&mut joiner, bind).await;
        let owner_token = pairing_token(&mut creator).await;
        let member_token = pairing_token(&mut joiner).await;
        let room = Room {
            id: created.room_id,
            owner: owner.clone(),
            owner_token,
            member_token,
        };
        (room, creator, joiner)
    }

    /// Connects an established-room device and returns its `HelloOk`.
    async fn member(&self, room: &Room, device: &DeviceId) -> (Socket, ControlMessage) {
        let mut socket = self
            .socket(ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: device.clone(),
                device_name: "registry device".into(),
                os: "fixture".into(),
                role: ClientRole::Member,
                room_id: Some(room.id.clone()),
                token: room.token(device).into(),
                pairing_session_id: None,
            })
            .await;
        let hello_ok = until(&mut socket, "member HelloOk", |m| {
            matches!(m, ControlMessage::HelloOk { .. })
        })
        .await;
        (socket, hello_ok)
    }

    async fn connections(&self, expected: usize) {
        let line = format!("clipsync_ws_connections {expected}");
        tokio::time::timeout(FRAME_DEADLINE, async {
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
                if metrics.lines().any(|candidate| candidate == line) {
                    break;
                }
                tokio::time::sleep(METRICS_POLL).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("relay did not reach `{line}`"));
    }
}

fn pairing_hello(
    device: &DeviceId,
    role: ClientRole,
    room: &RoomId,
    token: &str,
    sid: &PairingSessionId,
) -> ControlMessage {
    ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: device.clone(),
        device_name: "registry pairing".into(),
        os: "fixture".into(),
        role,
        room_id: Some(room.clone()),
        token: token.into(),
        pairing_session_id: Some(sid.clone()),
    }
}

fn text(room: &RoomId, sender: &DeviceId, ciphertext: &str) -> ControlMessage {
    let sent_at = Utc::now();
    ControlMessage::TextEnvelope {
        routing: RoutingHeader {
            version: PROTOCOL_VERSION,
            message_id: MessageId::new(),
            transfer_id: TransferId::new(),
            room_id: room.clone(),
            sender_device_id: sender.clone(),
            recipient: Recipient::Broadcast,
            sent_at,
            expires_at: sent_at + chrono::Duration::seconds(TEXT_LIFETIME_SECS),
            item_kind: ItemKind::Text,
            seq: 1,
        },
        nonce: OPAQUE_NONCE.into(),
        ciphertext: ciphertext.into(),
    }
}

async fn send(socket: &mut Socket, message: ControlMessage) {
    socket
        .send(Message::Text(message.encode().unwrap().into()))
        .await
        .unwrap();
}

async fn next_control(socket: &mut Socket) -> Option<ControlMessage> {
    loop {
        match socket.next().await? {
            Ok(Message::Text(text)) => return Some(ControlMessage::decode(&text).unwrap()),
            Ok(Message::Ping(bytes)) => socket.send(Message::Pong(bytes)).await.unwrap(),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// Reads until `wanted` matches, skipping unrelated control frames.
async fn until(
    socket: &mut Socket,
    what: &str,
    wanted: impl Fn(&ControlMessage) -> bool,
) -> ControlMessage {
    tokio::time::timeout(FRAME_DEADLINE, async {
        loop {
            match next_control(socket).await {
                Some(message) if wanted(&message) => return message,
                Some(_) => {}
                None => panic!("socket closed before {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} was not delivered"))
}

/// Everything the relay queued for this socket before it answers `ts`.
async fn drain_to_pong(socket: &mut Socket, ts: i64) -> Vec<ControlMessage> {
    send(socket, ControlMessage::Ping { ts }).await;
    let mut before = Vec::new();
    tokio::time::timeout(FRAME_DEADLINE, async {
        loop {
            match next_control(socket).await {
                Some(ControlMessage::Pong { ts: received }) if received == ts => return,
                Some(message) => before.push(message),
                None => panic!("socket closed before pong {ts}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("pong {ts} was not delivered"));
    before
}

async fn pairing_token(socket: &mut Socket) -> String {
    match until(socket, "PairingComplete", |m| {
        matches!(m, ControlMessage::PairingComplete { .. })
    })
    .await
    {
        ControlMessage::PairingComplete { device_token, .. } => device_token,
        _ => unreachable!("filtered by until"),
    }
}

fn ack_status(message: &ControlMessage) -> Option<AckStatus> {
    match message {
        ControlMessage::Ack { status, .. } => Some(*status),
        _ => None,
    }
}

async fn text_ack(socket: &mut Socket) -> AckStatus {
    let ack = until(socket, "sender Ack", |m| ack_status(m).is_some()).await;
    ack_status(&ack).unwrap()
}

fn ciphertext(message: &ControlMessage) -> Option<&str> {
    match message {
        ControlMessage::TextEnvelope { ciphertext, .. } => Some(ciphertext),
        _ => None,
    }
}

async fn received_text(socket: &mut Socket, what: &str) -> String {
    let envelope = until(socket, what, |m| ciphertext(m).is_some()).await;
    ciphertext(&envelope).unwrap().to_owned()
}

fn assert_no_text(frames: &[ControlMessage], who: &str) {
    assert!(
        frames.iter().all(|frame| ciphertext(frame).is_none()),
        "{who} must not receive room text: {frames:?}"
    );
}

fn assert_not_left(frames: &[ControlMessage], device: &DeviceId) {
    assert!(
        !frames.iter().any(
            |frame| matches!(frame, ControlMessage::PeerLeft { device_id } if device_id == device)
        ),
        "a live member must not be announced as left: {frames:?}"
    );
}

/// Sends room text from `sender` and requires live delivery to `recipient`.
async fn assert_live_delivery(
    room: &Room,
    sender_id: &DeviceId,
    sender: &mut Socket,
    recipient: &mut Socket,
    marker: &str,
) {
    send(sender, text(&room.id, sender_id, marker)).await;
    assert_eq!(text_ack(sender).await, AckStatus::Delivered);
    assert_eq!(received_text(recipient, marker).await, marker);
}

#[tokio::test]
async fn pairing_token_for_a_member_device_id_cannot_take_over_its_room_route() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member) = (DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        let (mut member_socket, _) = relay.member(&room, &member).await;
        until(
            &mut owner_socket,
            "member presence",
            |m| matches!(m, ControlMessage::PeerJoined { device } if device.device_id == member),
        )
        .await;

        // Device IDs are public (PeerJoined, webhooks). Anyone can open an
        // unauthenticated pairing that names the member's device ID.
        let (sid, attack) = relay.create(&member).await;
        let mut attacker = relay
            .socket(pairing_hello(
                &member,
                ClientRole::PairingA,
                &attack.room_id,
                &attack.pairing_token,
                &sid,
            ))
            .await;
        // HelloOk is sent only after the socket is registered.
        until(&mut attacker, "attacker HelloOk", |m| {
            matches!(m, ControlMessage::HelloOk { .. })
        })
        .await;

        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut member_socket,
            "owner-to-member while pairing socket is open",
        )
        .await;
        assert_no_text(&drain_to_pong(&mut attacker, 1).await, "pairing socket");

        attacker.close(None).await.unwrap();
        relay.connections(2).await;
        assert_not_left(&drain_to_pong(&mut owner_socket, 2).await, &member);
        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut member_socket,
            "owner-to-member after pairing socket closed",
        )
        .await;
    })
    .await
    .expect("pairing-token hijack fixture exceeded its deadline");
}

#[tokio::test]
async fn same_device_id_in_another_room_cannot_shadow_a_member_route() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member, accomplice) = (DeviceId::new(), DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        let (mut member_socket, _) = relay.member(&room, &member).await;
        until(
            &mut owner_socket,
            "member presence",
            |m| matches!(m, ControlMessage::PeerJoined { device } if device.device_id == member),
        )
        .await;

        // A fully authenticated device token, but for a room the attacker
        // created under the victim's device ID.
        let attack_room = relay.pair(&member, &accomplice, None).await;
        let (mut shadow, _) = relay.member(&attack_room, &member).await;

        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut member_socket,
            "owner-to-member while shadow is online",
        )
        .await;
        assert_no_text(&drain_to_pong(&mut shadow, 1).await, "other-room device");

        shadow.close(None).await.unwrap();
        relay.connections(2).await;
        assert_not_left(&drain_to_pong(&mut owner_socket, 2).await, &member);
        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut member_socket,
            "owner-to-member after shadow closed",
        )
        .await;
    })
    .await
    .expect("cross-room shadow fixture exceeded its deadline");
}

#[tokio::test]
async fn queued_text_for_a_device_id_is_scoped_to_its_room() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member, accomplice) = (DeviceId::new(), DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let attack_room = relay.pair(&member, &accomplice, Some(0)).await;

        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        send(&mut owner_socket, text(&room.id, &owner, "queued by owner")).await;
        assert_eq!(text_ack(&mut owner_socket).await, AckStatus::Accepted);

        let (mut accomplice_socket, _) = relay.member(&attack_room, &accomplice).await;
        send(
            &mut accomplice_socket,
            text(&attack_room.id, &accomplice, "queued by other room"),
        )
        .await;
        assert_eq!(text_ack(&mut accomplice_socket).await, AckStatus::Accepted);

        let (_member_socket, hello_ok) = relay.member(&room, &member).await;
        let ControlMessage::HelloOk { queued_text, .. } = hello_ok else {
            unreachable!("filtered by member()")
        };
        assert_eq!(
            queued_text.map(|queued| queued.ciphertext).as_deref(),
            Some("queued by owner"),
            "another room must not replace or receive this room's queued text"
        );
    })
    .await
    .expect("room-scoped queue fixture exceeded its deadline");
}

#[tokio::test]
async fn a_superseded_member_socket_is_closed_and_keeps_the_newer_route() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member) = (DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        let (mut superseded, _) = relay.member(&room, &member).await;
        let (mut current, _) = relay.member(&room, &member).await;

        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut current,
            "owner-to-newest member socket",
        )
        .await;
        // The replacement evicts the older socket: the relay closes it, and
        // it never receives the newer socket's room traffic.
        let (superseded_frames, superseded_closed) =
            drain_to_pong_or_close(&mut superseded, 1).await;
        assert_no_text(&superseded_frames, "superseded socket");
        assert!(superseded_closed, "the relay kept a superseded socket open");

        relay.connections(2).await;
        assert_not_left(&drain_to_pong(&mut owner_socket, 2).await, &member);
        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut current,
            "owner-to-member after superseded socket closed",
        )
        .await;
    })
    .await
    .expect("superseded member fixture exceeded its deadline");
}

#[tokio::test]
async fn pairing_socket_kept_open_after_completion_cannot_send_room_traffic() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member) = (DeviceId::new(), DeviceId::new());
        let (room, mut creator, mut joiner) = relay.pair_open(&owner, &member).await;
        joiner.close(None).await.unwrap();
        let (mut member_socket, _) = relay.member(&room, &member).await;

        // The creator's pairing token names the now-established room, but it
        // never authenticated as a room member and revocation cannot close it.
        let refused = |m: &ControlMessage| {
            matches!(m, ControlMessage::Error { message, .. } if message == "room member connection required")
        };
        send(&mut creator, text(&room.id, &owner, "from pairing socket")).await;
        until(&mut creator, "text refusal", refused).await;
        send(&mut creator, ControlMessage::Revoke { device_id: member.clone() }).await;
        until(&mut creator, "revoke refusal", refused).await;
        creator
            .send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .unwrap();
        let after_binary = drain_to_pong(&mut creator, 1).await;
        assert!(
            !after_binary
                .iter()
                .any(|m| matches!(m, ControlMessage::Ack { .. })),
            "pairing-socket binary must be dropped, not routed: {after_binary:?}"
        );

        let member_frames = drain_to_pong(&mut member_socket, 2).await;
        assert_no_text(&member_frames, "member");
        assert!(
            !member_frames
                .iter()
                .any(|m| matches!(m, ControlMessage::Revoke { .. })),
            "pairing socket must not revoke room members: {member_frames:?}"
        );
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut member_socket,
            "member route intact after pairing-socket attempts",
        )
        .await;
        creator.close(None).await.unwrap();
    })
    .await
    .expect("open pairing socket fixture exceeded its deadline");
}

/// Sends `ts` and collects frames until its pong, tolerating a relay close.
async fn drain_to_pong_or_close(socket: &mut Socket, ts: i64) -> (Vec<ControlMessage>, bool) {
    // The relay may already have closed this socket, so a failed write is an
    // expected outcome here rather than a fixture error.
    let _ = socket
        .send(Message::Text(
            ControlMessage::Ping { ts }.encode().unwrap().into(),
        ))
        .await;
    let mut before = Vec::new();
    let closed = tokio::time::timeout(FRAME_DEADLINE, async {
        loop {
            match next_control(socket).await {
                Some(ControlMessage::Pong { ts: received }) if received == ts => return false,
                Some(message) => before.push(message),
                None => return true,
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("neither pong {ts} nor a close was delivered"));
    (before, closed)
}

#[tokio::test]
async fn revoked_member_cannot_send_room_traffic_through_a_superseded_socket() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member) = (DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        // A normal reconnect overlap: the second socket replaces the first
        // socket's route while the first is still open.
        let (mut superseded, _) = relay.member(&room, &member).await;
        let (mut current, _) = relay.member(&room, &member).await;

        send(
            &mut owner_socket,
            ControlMessage::Revoke {
                device_id: member.clone(),
            },
        )
        .await;
        drain_to_pong(&mut owner_socket, 1).await;
        let (_, current_closed) = drain_to_pong_or_close(&mut current, 2).await;
        assert!(
            current_closed,
            "the revoked member's route socket stays open"
        );

        // The older socket's frames are processed before its pong, so any text
        // it got through is already queued for the owner before the owner's pong.
        let _ = superseded
            .send(Message::Text(
                text(&room.id, &member, "from revoked superseded socket")
                    .encode()
                    .unwrap()
                    .into(),
            ))
            .await;
        let _ = superseded.send(Message::Binary(vec![1, 2, 3].into())).await;
        let (superseded_frames, superseded_closed) =
            drain_to_pong_or_close(&mut superseded, 3).await;
        assert!(
            !superseded_frames
                .iter()
                .any(|m| ack_status(m) == Some(AckStatus::Delivered)),
            "revoked member text was delivered: {superseded_frames:?}"
        );
        assert!(
            superseded_closed,
            "the revoked member's superseded socket stays open"
        );
        let owner_frames = drain_to_pong(&mut owner_socket, 4).await;
        assert_no_text(&owner_frames, "owner from a revoked member");
        relay.connections(1).await;
    })
    .await
    .expect("revoked superseded socket fixture exceeded its deadline");
}
