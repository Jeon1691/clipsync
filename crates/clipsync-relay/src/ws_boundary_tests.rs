// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;

fn pending_pairing(state: &RelayState) -> (PairingSessionId, ConnCtx) {
    let sid = PairingSessionId::new();
    let device = DeviceId::new();
    let offer = state
        .create_pairing(
            CreatePairingRequest {
                protocol_version: clipsync_protocol::PROTOCOL_VERSION,
                pairing_session_id: sid.clone(),
                device_id: device.clone(),
                device_name: "owner".into(),
                os: "fixture".into(),
                ttl_secs: Some(60),
            },
            std::net::Ipv4Addr::LOCALHOST.into(),
        )
        .unwrap();
    (
        sid.clone(),
        ConnCtx {
            device_id: device.to_string(),
            room_id: offer.room_id.to_string(),
            pairing_sid: Some(sid.to_string()),
            device_role: DeviceRole::Owner,
            pairing_side: None,
            client: TEST_CLIENT,
            member_conn: None,
            presence: None,
            pairing_deadline: None,
            _live: state.live_connection(),
        },
    )
}

#[tokio::test]
async fn pairing_registration_rejects_wrong_room_completion_and_identity_without_announcing_presence(
) {
    for (case, expected) in [
        ("room", "pairing room mismatch"),
        ("completed", "pairing completed"),
        ("device", "pairing role or device mismatch"),
        ("role", "pairing role or device mismatch"),
        ("missing-session", "pairing session required"),
        ("unknown-session", "unknown pairing session"),
    ] {
        let state = state();
        let (sid, mut ctx) = pending_pairing(&state);
        let (tx, mut rx) = mpsc::channel(8);
        match case {
            "room" => ctx.room_id = RoomId::new().to_string(),
            "completed" => state.pairing_mut(sid.as_str()).unwrap().completed = true,
            "device" => ctx.device_id = DeviceId::new().to_string(),
            "role" => ctx.device_role = DeviceRole::Member,
            "missing-session" => ctx.pairing_sid = None,
            "unknown-session" => ctx.pairing_sid = Some(PairingSessionId::new().to_string()),
            _ => unreachable!(),
        }
        let error = register_pairing_connection(&state, &mut ctx, &tx, ClientRole::PairingA)
            .err()
            .expect("invalid admission was accepted");
        assert_eq!(error, expected, "{case}");
        assert!(ctx.pairing_side.is_none(), "{case}");
        assert!(rx.try_recv().is_err(), "{case}");
        assert!(state
            .pairing(sid.as_str())
            .unwrap()
            .pairing_senders
            .iter()
            .all(Option::is_none));
        assert!(state.member_routes.is_empty(), "{case}");
    }
}

#[tokio::test]
async fn pairing_forwarding_rechecks_lifecycle_and_socket_ownership_before_sending() {
    for (case, expected) in [
        ("room", "pairing room mismatch"),
        ("completed", "pairing completed"),
        ("frame", "unexpected pairing frame"),
        ("session", "pairing frame session mismatch"),
        ("unknown-session", "unknown pairing session"),
        ("unauthenticated", "authenticated pairing hello required"),
        ("replaced", "pairing connection replaced"),
    ] {
        let state = state();
        let (sid, mut ctx) = pending_pairing(&state);
        let (tx, mut rx) = mpsc::channel(8);
        register_pairing_connection(&state, &mut ctx, &tx, ClientRole::PairingA).unwrap();
        let mut message = ControlMessage::KeyConfirm {
            pairing_session_id: sid.clone(),
            mac: "fixture".into(),
        };
        let (replacement, _replacement_rx) = mpsc::channel(8);
        match case {
            "room" => ctx.room_id = RoomId::new().to_string(),
            "completed" => state.pairing_mut(sid.as_str()).unwrap().completed = true,
            "frame" => message = ControlMessage::Ping { ts: 1 },
            "session" => {
                message = ControlMessage::KeyConfirm {
                    pairing_session_id: PairingSessionId::new(),
                    mac: "fixture".into(),
                }
            }
            "unknown-session" => state.abort_pairing(sid.as_str()),
            "unauthenticated" => ctx.pairing_side = None,
            "replaced" => {
                state.pairing_mut(sid.as_str()).unwrap().pairing_senders[0] = Some(replacement)
            }
            _ => unreachable!(),
        }
        assert_eq!(
            forward_pairing_or_peer(&state, &ctx, &tx, message)
                .await
                .unwrap_err(),
            expected,
            "{case}"
        );
        assert!(rx.try_recv().is_err(), "{case}");
        assert!(state.room(&ctx.room_id).is_none(), "{case}");
        if let Some(session) = state.pairing(sid.as_str()) {
            assert!(session.pending_spake.iter().all(Option::is_none), "{case}");
            assert_eq!(session.bind_count, 0, "{case}");
        };
    }
}

#[tokio::test]
async fn repeated_hello_cannot_switch_authenticated_identity_or_replace_its_routing_entry() {
    let state = state();
    let (rid, owner, member, owner_token, member_token) = room(&state);
    let (tx, mut rx) = mpsc::channel(8);
    let mut ctx = None;
    handle_control(
        &state,
        &tx,
        &mut ctx,
        hello(&owner, &rid, &owner_token),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::HelloOk { .. }
    ));
    assert_eq!(
        handle_control(
            &state,
            &tx,
            &mut ctx,
            hello(&member, &rid, &member_token),
            None
        )
        .await
        .unwrap_err(),
        "hello already received"
    );
    assert_eq!(ctx.as_ref().unwrap().device_id, owner.as_str());
    assert_eq!(ctx.as_ref().unwrap().device_role, DeviceRole::Owner);
    let senders = state.member_senders(rid.as_str(), owner.as_str());
    assert_eq!(senders.len(), 1);
    assert!(senders[0].1.same_channel(&tx));
    assert!(!state.is_online(rid.as_str(), member.as_str()));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn revoking_an_offline_member_and_reporting_client_errors_preserve_the_owner_session() {
    let state = state();
    let (rid, owner, member, token, _) = room(&state);
    let (tx, mut rx) = mpsc::channel(8);
    let mut ctx = None;
    handle_control(&state, &tx, &mut ctx, hello(&owner, &rid, &token), None)
        .await
        .unwrap();
    receive(&mut rx).await;
    handle_control(
        &state,
        &tx,
        &mut ctx,
        ControlMessage::Revoke {
            device_id: member.clone(),
        },
        None,
    )
    .await
    .unwrap();
    assert!(state.room(rid.as_str()).unwrap().devices[member.as_str()].revoked);
    assert!(state.is_online(rid.as_str(), owner.as_str()));
    for authenticated in [false, true] {
        let error = ControlMessage::Error {
            code: "client-report".into(),
            message: "recoverable".into(),
        };
        if authenticated {
            handle_control(&state, &tx, &mut ctx, error, None)
                .await
                .unwrap();
        } else {
            handle_control(&state, &tx, &mut None, error, None)
                .await
                .unwrap();
        }
        assert!(state.is_online(rid.as_str(), owner.as_str()));
        assert!(state.room(rid.as_str()).is_some());
    }
    handle_control(&state, &tx, &mut ctx, ControlMessage::Ping { ts: 77 }, None)
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut rx).await,
        ControlMessage::Pong { ts: 77 }
    ));
}
