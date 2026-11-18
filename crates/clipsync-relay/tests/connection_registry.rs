// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Room routing belongs to authenticated members, is scoped to one room, and
//! reaches every live socket of a member (daemon plus CLI one-shot commands),
//! up to a per-member cap.

use clipsync_protocol::Timestamp;
use clipsync_protocol::{
    AckStatus, ClientRole, ControlMessage, CreatePairingRequest, CreatePairingResponse, DeviceId,
    ItemKind, JoinPairingRequest, JoinPairingResponse, MessageId, PairingSessionId, Recipient,
    RoomId, RoutingHeader, TransferId, PROTOCOL_VERSION,
};
use clipsync_relay::{router, MetricsToken, RelayConfig, MAX_MEMBER_SOCKETS};
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
/// Scrape credential for this fixture's relay; readiness polls read `/metrics`.
const METRICS_TEST_TOKEN: &str = "connection-registry-metrics-token-0123456789";
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
            metrics_token: Some(MetricsToken::new(METRICS_TEST_TOKEN).unwrap()),
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
                    .bearer_auth(METRICS_TEST_TOKEN)
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
    let sent_at = Timestamp::now();
    ControlMessage::TextEnvelope {
        routing: RoutingHeader {
            version: PROTOCOL_VERSION,
            message_id: MessageId::new(),
            transfer_id: TransferId::new(),
            room_id: room.clone(),
            sender_device_id: sender.clone(),
            recipient: Recipient::Broadcast,
            sent_at,
            expires_at: sent_at + time::Duration::seconds(TEXT_LIFETIME_SECS),
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
async fn every_socket_of_a_member_gets_room_traffic_and_closes_independently() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member) = (DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        // A device's daemon and a CLI command each hold a socket at once.
        let (mut daemon, _) = relay.member(&room, &member).await;
        let (mut command, _) = relay.member(&room, &member).await;
        relay.connections(3).await;

        let marker = "owner-to-every-member-socket";
        send(&mut owner_socket, text(&room.id, &owner, marker)).await;
        assert_eq!(text_ack(&mut owner_socket).await, AckStatus::Delivered);
        assert_eq!(received_text(&mut daemon, "daemon text").await, marker);
        assert_eq!(received_text(&mut command, "command text").await, marker);
        owner_socket
            .send(Message::Binary(vec![7, 8, 9].into()))
            .await
            .unwrap();
        assert_eq!(next_binary(&mut daemon, "daemon chunk").await, [7, 8, 9]);
        assert_eq!(next_binary(&mut command, "command chunk").await, [7, 8, 9]);

        // Either socket may send room traffic for the member.
        for (socket, marker) in [
            (&mut daemon, "member daemon to owner"),
            (&mut command, "member command to owner"),
        ] {
            assert_live_delivery(&room, &member, socket, &mut owner_socket, marker).await;
        }

        // The command finishing leaves the daemon routed and the member online.
        command.close(None).await.unwrap();
        relay.connections(2).await;
        assert_not_left(&drain_to_pong(&mut owner_socket, 1).await, &member);
        assert_live_delivery(
            &room,
            &owner,
            &mut owner_socket,
            &mut daemon,
            "owner-to-daemon after the command closed",
        )
        .await;
        let (daemon_frames, daemon_closed) = drain_to_pong_or_close(&mut daemon, 2).await;
        assert!(
            !daemon_closed,
            "a concurrent socket closed the daemon: {daemon_frames:?}"
        );
    })
    .await
    .expect("concurrent member sockets fixture exceeded its deadline");
}

#[tokio::test]
async fn a_member_past_the_socket_cap_loses_only_its_oldest_socket() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member) = (DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        let mut sockets = Vec::new();
        for _ in 0..MAX_MEMBER_SOCKETS {
            sockets.push(relay.member(&room, &member).await.0);
        }
        relay.connections(1 + MAX_MEMBER_SOCKETS).await;

        // One more socket than the cap evicts the member's oldest one.
        let (newest, _) = relay.member(&room, &member).await;
        let mut oldest = sockets.remove(0);
        sockets.push(newest);
        let (oldest_frames, oldest_closed) = drain_to_pong_or_close(&mut oldest, 1).await;
        assert!(
            oldest_closed,
            "the relay kept more member sockets than its cap"
        );
        assert_no_text(&oldest_frames, "evicted socket");
        relay.connections(1 + MAX_MEMBER_SOCKETS).await;

        let marker = "owner-to-capped-member";
        send(&mut owner_socket, text(&room.id, &owner, marker)).await;
        assert_eq!(text_ack(&mut owner_socket).await, AckStatus::Delivered);
        for socket in &mut sockets {
            assert_eq!(received_text(socket, "routed member socket").await, marker);
        }
        assert_not_left(&drain_to_pong(&mut owner_socket, 2).await, &member);
    })
    .await
    .expect("per-member socket cap fixture exceeded its deadline");
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

/// Reads until the next binary frame, skipping control frames.
async fn next_binary(socket: &mut Socket, what: &str) -> Vec<u8> {
    tokio::time::timeout(FRAME_DEADLINE, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Binary(bytes))) => return bytes.to_vec(),
                Some(Ok(Message::Ping(bytes))) => socket.send(Message::Pong(bytes)).await.unwrap(),
                Some(Ok(Message::Close(_)) | Err(_)) | None => {
                    panic!("socket closed before {what}")
                }
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} was not delivered"))
}

#[tokio::test]
async fn revocation_closes_every_socket_of_the_member_and_none_can_send() {
    tokio::time::timeout(FIXTURE_DEADLINE, async {
        let relay = Relay::start().await;
        let (owner, member) = (DeviceId::new(), DeviceId::new());
        let room = relay.pair(&owner, &member, Some(0)).await;
        let (mut owner_socket, _) = relay.member(&room, &owner).await;
        // The member's daemon and a concurrent CLI command are both routed.
        let (first, _) = relay.member(&room, &member).await;
        let (second, _) = relay.member(&room, &member).await;
        relay.connections(3).await;

        send(
            &mut owner_socket,
            ControlMessage::Revoke {
                device_id: member.clone(),
            },
        )
        .await;
        drain_to_pong(&mut owner_socket, 1).await;

        for (index, mut socket) in [first, second].into_iter().enumerate() {
            // Each socket's frames are processed before its pong, so any text
            // it got through would already be queued for the owner.
            let _ = socket
                .send(Message::Text(
                    text(&room.id, &member, "from a revoked member socket")
                        .encode()
                        .unwrap()
                        .into(),
                ))
                .await;
            let _ = socket.send(Message::Binary(vec![1, 2, 3].into())).await;
            let ts = 2 + index as i64;
            let (frames, closed) = drain_to_pong_or_close(&mut socket, ts).await;
            assert!(
                !frames
                    .iter()
                    .any(|m| ack_status(m) == Some(AckStatus::Delivered)),
                "revoked member text was delivered: {frames:?}"
            );
            assert!(closed, "socket {index} of the revoked member stays open");
        }
        let owner_frames = drain_to_pong(&mut owner_socket, 4).await;
        assert_no_text(&owner_frames, "owner from a revoked member");
        relay.connections(1).await;
    })
    .await
    .expect("revoked member sockets fixture exceeded its deadline");
}
