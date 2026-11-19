// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_protocol::{ControlMessage, PairingSessionId};

#[test]
fn spake2_frames_stay_compatible_with_peers_and_relays_without_commitments() {
    let sid = PairingSessionId::from("01JCLIPSYNCWIREFIXTURE0000");
    // A frame without the flag is byte-for-byte what older peers send.
    let plain = ControlMessage::Spake2Msg {
        pairing_session_id: sid.clone(),
        message: "AAAA".into(),
        after_commitment: false,
    };
    assert_eq!(
        plain.encode().unwrap(),
        r#"{"type":"spake2_msg","pairing_session_id":"01JCLIPSYNCWIREFIXTURE0000","message":"AAAA"}"#
    );
    assert_eq!(
        ControlMessage::decode(&plain.encode().unwrap()).unwrap(),
        plain
    );
    let flagged = ControlMessage::Spake2Msg {
        pairing_session_id: sid.clone(),
        message: "AAAA".into(),
        after_commitment: true,
    };
    let wire = flagged.encode().unwrap();
    assert!(wire.contains(r#""after_commitment":true"#), "{wire}");
    assert_eq!(ControlMessage::decode(&wire).unwrap(), flagged);

    let commit = ControlMessage::Spake2Commit {
        pairing_session_id: sid,
        commitment: "BBBB".into(),
    };
    assert_eq!(
        commit.encode().unwrap(),
        r#"{"type":"spake2_commit","pairing_session_id":"01JCLIPSYNCWIREFIXTURE0000","commitment":"BBBB"}"#
    );
    assert_eq!(
        ControlMessage::decode(&commit.encode().unwrap()).unwrap(),
        commit
    );
}
