// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use crate::test_relay::{close, receive, send, Server};
use clipsync_protocol::PeerInfo;

fn online_peer(identity: &DeviceIdentity) -> PeerInfo {
    PeerInfo {
        device_id: identity.device_id.clone(),
        device_name: "authenticated peer".into(),
        os: "fixture".into(),
        role: DeviceRole::Member,
        online: true,
    }
}

#[tokio::test]
async fn sessions_complete_real_noise_as_initiator_and_responder_before_continuing_sync() {
    for (initiator, initial_presence) in [(true, true), (true, false), (false, true)] {
        let local = DeviceIdentity::from_bytes(
            if initiator { "device-a" } else { "device-z" }.into(),
            [81; 32],
        );
        let remote = DeviceIdentity::from_bytes("device-m".into(), [82; 32]);
        let local_public = local.public_bytes();
        let remote_public = remote.public_bytes();
        let remote_info = online_peer(&remote);
        let room_id = RoomId::new();
        let room = room_id.clone();
        let mut server = Server::websocket(move |mut socket| async move {
            assert!(matches!(
                receive(&mut socket).await,
                ControlMessage::Hello { .. }
            ));
            let presence = || ControlMessage::HelloOk {
                room_id: room.clone(),
                peer_online: true,
                peers: vec![remote_info.clone()],
                queued_text: None,
                device_token: None,
            };
            if !initial_presence {
                send(&mut socket, hello(&room)).await;
            }
            send(&mut socket, presence()).await;
            let public = parse_public(&hex::encode(local_public)).unwrap();
            let mut handshake = if initiator {
                NoiseHandshake::responder(&remote.static_secret(), &public).unwrap()
            } else {
                NoiseHandshake::initiator(&remote.static_secret(), &public).unwrap()
            };
            if !initiator {
                send(
                    &mut socket,
                    ControlMessage::NoiseHandshake {
                        message: hex::encode(handshake.write_message().unwrap()),
                    },
                )
                .await;
            }
            let ControlMessage::NoiseHandshake { message } = receive(&mut socket).await else {
                panic!("session did not authenticate the peer");
            };
            handshake
                .read_message(&hex::decode(message).unwrap())
                .unwrap();
            if initiator {
                send(
                    &mut socket,
                    ControlMessage::NoiseHandshake {
                        message: hex::encode(handshake.write_message().unwrap()),
                    },
                )
                .await;
            }
            assert!(handshake.is_finished());
            // A new presence announcement must not restart a completed handshake.
            send(&mut socket, presence()).await;
            send(
                &mut socket,
                encrypted_text(
                    routing_from(&room, "device-m", ItemKind::Text),
                    "after authentication",
                ),
            )
            .await;
            assert!(matches!(
                receive(&mut socket).await,
                ControlMessage::Ack {
                    status: AckStatus::Delivered,
                    ..
                }
            ));
        })
        .await;
        let peer = Peer::new(&server.url, local.device_id.as_str(), 81);
        let other = Peer::new(&server.url, "device-m", 82);
        assert_eq!(other.identity.public_bytes(), remote_public);
        peer.save_room(room_id, "fixture-token".into(), &other, DeviceRole::Owner);
        let clipboard = MockClipboard::new();
        let handle = sync_handle();
        let (_shutdown, mut stopped) = watch::channel(false);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            run_session(
                &peer.app.store,
                &SystemClipboard::mock(clipboard.clone()),
                &handle,
                &mut stopped,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        server.finish().await;
        assert!(error.to_string().contains("relay disconnected"));
        assert!(
            matches!(clipboard.get(), Some(ClipboardItem::Text { text }) if text == "after authentication")
        );
        assert_eq!(clipboard.signature().await.unwrap(), "1");
    }
}

#[tokio::test]
async fn live_presence_and_untrusted_noise_do_not_replace_the_last_authenticated_clipboard_item() {
    for case in ["offline", "unknown", "bad-key", "no-peers", "responder"] {
        let room_id = RoomId::new();
        let room = room_id.clone();
        let mut server = Server::websocket(move |mut socket| async move {
            receive(&mut socket).await;
            send(&mut socket, hello(&room)).await;
            let ControlMessage::TextEnvelope {
                routing,
                nonce,
                mut ciphertext,
            } = encrypted_text(text_routing(&room), "forged queued text")
            else {
                unreachable!()
            };
            let changed = u8::from_str_radix(&ciphertext[..2], 16).unwrap() ^ 1;
            ciphertext.replace_range(..2, &format!("{changed:02x}"));
            let mut peer = online_peer(&DeviceIdentity::from_bytes(
                "script-device-z".into(),
                [52; 32],
            ));
            if case == "offline" {
                peer.online = false;
            }
            if case == "unknown" {
                peer.device_id = "not-pinned".into();
            }
            if case == "responder" {
                peer.device_id = "000-first".into();
            }
            send(
                &mut socket,
                ControlMessage::HelloOk {
                    room_id: room.clone(),
                    peer_online: true,
                    peers: if case == "no-peers" {
                        vec![]
                    } else {
                        vec![peer]
                    },
                    queued_text: Some(clipsync_protocol::QueuedEnvelope {
                        routing,
                        nonce,
                        ciphertext,
                    }),
                    device_token: None,
                },
            )
            .await;
            send(
                &mut socket,
                ControlMessage::NoiseHandshake {
                    message: "invalid hex".into(),
                },
            )
            .await;
            send(
                &mut socket,
                ControlMessage::Revoke {
                    device_id: "unrelated".into(),
                },
            )
            .await;
            send(
                &mut socket,
                encrypted_text(
                    routing_from(
                        &room,
                        if case == "responder" {
                            "000-first"
                        } else {
                            SCRIPT_SENDER
                        },
                        ItemKind::Text,
                    ),
                    "valid after hostile presence",
                ),
            )
            .await;
            // A room without pinned peers accepts no sender, so nothing is acknowledged.
            if case != "no-peers" {
                assert!(matches!(
                    receive(&mut socket).await,
                    ControlMessage::Ack {
                        status: AckStatus::Delivered,
                        ..
                    }
                ));
            }
            // The client's connect-time heartbeat is still unread here, notably when no
            // Ack was awaited; dropping the socket would reset the connection and could
            // discard the frames above before the session reads them. A graceful close
            // delivers them all, so the session handles the final envelope before it
            // reports the disconnect.
            close(socket).await;
        })
        .await;
        let peer = scripted_peer(&server.url, room_id);
        let mut room = peer.app.store.current_room().unwrap();
        match case {
            "bad-key" => room.peers[0].public_key_hex = "not a public key".into(),
            "no-peers" => room.peers.clear(),
            "responder" => room.peers[0].device_id = "000-first".into(),
            _ => {}
        }
        peer.app.store.upsert_room(room).unwrap();
        let clipboard = MockClipboard::new();
        let handle = sync_handle();
        let (_shutdown, mut stopped) = watch::channel(false);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            run_session(
                &peer.app.store,
                &SystemClipboard::mock(clipboard.clone()),
                &handle,
                &mut stopped,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        server.finish().await;
        assert!(error.to_string().contains("relay disconnected"), "{case}");
        if case == "no-peers" {
            assert!(
                clipboard.get().is_none(),
                "unpinned sender reached the clipboard"
            );
            continue;
        }
        assert!(
            matches!(clipboard.get(), Some(ClipboardItem::Text { text }) if text == "valid after hostile presence")
        );
        assert_eq!(clipboard.signature().await.unwrap(), "1", "{case}");
    }
}

#[tokio::test]
async fn already_cancelled_sync_does_not_open_a_session_or_require_pairing() {
    let peer = Peer::new("http://127.0.0.1:1", "cancelled", 83);
    let (shutdown, stopped) = watch::channel(true);
    let handle = sync_handle();
    let connected = handle.connected.clone();
    tokio::time::timeout(
        Duration::from_secs(1),
        sync_loop(
            peer.app.store.clone(),
            SystemClipboard::mock(MockClipboard::new()),
            handle,
            stopped,
        ),
    )
    .await
    .expect("cancelled sync did not stop");
    assert!(!connected.load(Ordering::Relaxed));
    assert!(peer.app.store.current_room().is_err());
    drop(shutdown);
}
