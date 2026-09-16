use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::{Zeroize, ZeroizeOnDrop};

use clipsync_protocol::{DeviceId, PairingSessionId, PAIRING_TRANSCRIPT_LABEL, PROTOCOL_VERSION};

use crate::kdf::{derive_labeled, KEY_LEN};
use crate::{CryptoError, Result};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingRole {
    A,
    B,
}

pub struct PairingState {
    pub role: PairingRole,
    pub session_id: PairingSessionId,
    pub device_a: DeviceId,
    pub device_b: DeviceId,
    pub outbound: Vec<u8>,
    spake: Option<Spake2<Ed25519Group>>,
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct PairingKeys {
    pub confirm_key_a: [u8; KEY_LEN],
    pub confirm_key_b: [u8; KEY_LEN],
    pub pairing_wrap_key: [u8; KEY_LEN],
    pub room_root_key: [u8; KEY_LEN],
    #[zeroize(skip)]
    pub transcript_hash: [u8; 32],
}

pub fn pairing_transcript(
    session_id: &PairingSessionId,
    device_a: &DeviceId,
    device_b: &DeviceId,
    msg_a: &[u8],
    msg_b: &[u8],
) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(PAIRING_TRANSCRIPT_LABEL.as_bytes());
    t.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    t.extend_from_slice(session_id.as_str().as_bytes());
    t.extend_from_slice(device_a.as_str().as_bytes());
    t.extend_from_slice(device_b.as_str().as_bytes());
    t.extend_from_slice(&(msg_a.len() as u32).to_be_bytes());
    t.extend_from_slice(msg_a);
    t.extend_from_slice(&(msg_b.len() as u32).to_be_bytes());
    t.extend_from_slice(msg_b);
    t
}

pub fn start_pairing_a(
    password: &str,
    session_id: PairingSessionId,
    device_a: DeviceId,
    device_b: DeviceId,
) -> Result<PairingState> {
    start(PairingRole::A, password, session_id, device_a, device_b)
}

pub fn start_pairing_b(
    password: &str,
    session_id: PairingSessionId,
    device_a: DeviceId,
    device_b: DeviceId,
) -> Result<PairingState> {
    start(PairingRole::B, password, session_id, device_a, device_b)
}

fn start(
    role: PairingRole,
    password: &str,
    session_id: PairingSessionId,
    device_a: DeviceId,
    device_b: DeviceId,
) -> Result<PairingState> {
    let pw = Password::new(password.as_bytes());
    let id_a = Identity::new(device_a.as_str().as_bytes());
    let id_b = Identity::new(device_b.as_str().as_bytes());
    let (spake, outbound) = match role {
        PairingRole::A => Spake2::<Ed25519Group>::start_a(&pw, &id_a, &id_b),
        PairingRole::B => Spake2::<Ed25519Group>::start_b(&pw, &id_a, &id_b),
    };
    Ok(PairingState {
        role,
        session_id,
        device_a,
        device_b,
        outbound,
        spake: Some(spake),
    })
}

pub fn finish_pairing(state: PairingState, inbound: &[u8]) -> Result<PairingKeys> {
    let spake = state.spake.ok_or(CryptoError::Pairing("missing state"))?;
    let shared = spake
        .finish(inbound)
        .map_err(|_| CryptoError::Pairing("spake2 finish failed"))?;

    let (msg_a, msg_b) = match state.role {
        PairingRole::A => (state.outbound.as_slice(), inbound),
        PairingRole::B => (inbound, state.outbound.as_slice()),
    };
    let transcript = pairing_transcript(
        &state.session_id,
        &state.device_a,
        &state.device_b,
        msg_a,
        msg_b,
    );
    let transcript_hash: [u8; 32] = Sha256::digest(&transcript).into();

    let confirm_key_a = derive_labeled(&shared, &transcript_hash, b"clipsync/v1/confirm-a")?;
    let confirm_key_b = derive_labeled(&shared, &transcript_hash, b"clipsync/v1/confirm-b")?;
    let pairing_wrap_key = derive_labeled(&shared, &transcript_hash, b"clipsync/v1/pairing-wrap")?;
    let room_root_key = derive_labeled(&shared, &transcript_hash, b"clipsync/v1/room-root")?;

    Ok(PairingKeys {
        confirm_key_a,
        confirm_key_b,
        pairing_wrap_key,
        room_root_key,
        transcript_hash,
    })
}

pub fn confirm_mac(keys: &PairingKeys, role: PairingRole) -> Vec<u8> {
    let (key, label) = match role {
        PairingRole::A => (&keys.confirm_key_a, b"clipsync/v1/confirm-a".as_slice()),
        PairingRole::B => (&keys.confirm_key_b, b"clipsync/v1/confirm-b".as_slice()),
    };
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts 32-byte keys");
    mac.update(label);
    mac.update(&keys.transcript_hash);
    mac.finalize().into_bytes().to_vec()
}

pub fn verify_confirm_mac(keys: &PairingKeys, role: PairingRole, mac: &[u8]) -> Result<()> {
    let expected = confirm_mac(keys, role);
    if expected.len() != mac.len() {
        return Err(CryptoError::Confirm);
    }
    use subtle::ConstantTimeEq;
    if expected.ct_eq(mac).unwrap_u8() != 1 {
        return Err(CryptoError::Confirm);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_roundtrip_same_code() {
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let code = "482913";
        let sa = start_pairing_a(code, session.clone(), a.clone(), b.clone()).unwrap();
        let sb = start_pairing_b(code, session, a, b).unwrap();
        let msg_a = sa.outbound.clone();
        let msg_b = sb.outbound.clone();
        let ka = finish_pairing(sa, &msg_b).unwrap();
        let kb = finish_pairing(sb, &msg_a).unwrap();
        assert_eq!(ka.room_root_key, kb.room_root_key);
        assert_eq!(ka.transcript_hash, kb.transcript_hash);
        let mac_a = confirm_mac(&ka, PairingRole::A);
        let mac_b = confirm_mac(&kb, PairingRole::B);
        verify_confirm_mac(&kb, PairingRole::A, &mac_a).unwrap();
        verify_confirm_mac(&ka, PairingRole::B, &mac_b).unwrap();
    }

    #[test]
    fn wrong_code_fails_confirmation_or_keys() {
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let sa = start_pairing_a("111111", session.clone(), a.clone(), b.clone()).unwrap();
        let sb = start_pairing_b("222222", session, a, b).unwrap();
        let msg_a = sa.outbound.clone();
        let msg_b = sb.outbound.clone();
        let ka = finish_pairing(sa, &msg_b);
        let kb = finish_pairing(sb, &msg_a);
        match (ka, kb) {
            (Ok(ka), Ok(kb)) => {
                assert_ne!(ka.room_root_key, kb.room_root_key);
                let mac_a = confirm_mac(&ka, PairingRole::A);
                assert!(verify_confirm_mac(&kb, PairingRole::A, &mac_a).is_err());
            }
            _ => {
                // SPAKE2 finish itself may fail for mismatched passwords.
            }
        }
    }
}
