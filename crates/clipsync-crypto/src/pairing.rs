// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::{Zeroize, ZeroizeOnDrop};

use clipsync_protocol::{DeviceId, PairingSessionId, PAIRING_TRANSCRIPT_LABEL, PROTOCOL_VERSION};

use crate::kdf::{derive_labeled, KEY_LEN};
use crate::{CryptoError, Result};

type HmacSha256 = Hmac<Sha256>;

/// HKDF info label that separates the short authentication string from every
/// other key derived from the SPAKE2 secret.
pub const SAS_LABEL: &[u8] = b"clipsync/v1/sas";
/// Decimal digits in the SAS. Six digits match the length users already type
/// for the pairing code; two unrelated sessions collide with probability 10^-6.
pub const SAS_DIGITS: u32 = 6;
/// Leading key bytes reduced to the SAS. A 64-bit sample keeps the modulo
/// bias toward low values below 10^6 / 2^64 (about 2^-44).
const SAS_SAMPLE_BYTES: usize = 8;
const SAS_MODULUS: u64 = 10u64.pow(SAS_DIGITS);

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
    /// Input to [`short_authentication_string`]; never sent on the wire.
    pub sas_key: [u8; KEY_LEN],
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

/// Seeded variant of [`start`] so tests can pin SPAKE2-derived vectors.
#[cfg(test)]
fn start_with_rng(
    role: PairingRole,
    password: &str,
    session_id: PairingSessionId,
    device_a: DeviceId,
    device_b: DeviceId,
    rng: impl spake2_rand_core::RngCore + spake2_rand_core::CryptoRng,
) -> PairingState {
    let pw = Password::new(password.as_bytes());
    let id_a = Identity::new(device_a.as_str().as_bytes());
    let id_b = Identity::new(device_b.as_str().as_bytes());
    let (spake, outbound) = match role {
        PairingRole::A => Spake2::<Ed25519Group>::start_a_with_rng(&pw, &id_a, &id_b, rng),
        PairingRole::B => Spake2::<Ed25519Group>::start_b_with_rng(&pw, &id_a, &id_b, rng),
    };
    PairingState {
        role,
        session_id,
        device_a,
        device_b,
        outbound,
        spake: Some(spake),
    }
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
    let sas_key = derive_sas_key(&shared, &transcript_hash)?;

    Ok(PairingKeys {
        confirm_key_a,
        confirm_key_b,
        pairing_wrap_key,
        room_root_key,
        sas_key,
        transcript_hash,
    })
}

fn derive_sas_key(shared: &[u8], transcript_hash: &[u8; 32]) -> Result<[u8; KEY_LEN]> {
    derive_labeled(shared, transcript_hash, SAS_LABEL)
}

fn render_sas(sas_key: &[u8; KEY_LEN]) -> String {
    let mut sample = [0u8; SAS_SAMPLE_BYTES];
    sample.copy_from_slice(&sas_key[..SAS_SAMPLE_BYTES]);
    let value = u64::from_be_bytes(sample) % SAS_MODULUS;
    format!("{value:0width$}", width = SAS_DIGITS as usize)
}

/// Short authentication string both users compare out of band before a
/// pairing is committed.
///
/// It is computed locally from the SPAKE2 secret and transcript, so it adds
/// nothing to the wire. Two devices show the same value only when they ran
/// SPAKE2 with each other. A relay that ran one session with each device, or
/// a joiner that guessed the code, makes the values differ. See
/// `docs/security.md` for what the comparison does and does not cover.
pub fn short_authentication_string(keys: &PairingKeys) -> String {
    render_sas(&keys.sas_key)
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
#[cfg_attr(coverage_nightly, coverage(off))]
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

    #[test]
    fn malformed_spake_messages_are_rejected_before_key_confirmation() {
        for inbound in [Vec::new(), vec![0; 32], vec![b'A'; 33], vec![b'B'; 34]] {
            let state = start_pairing_a(
                "482913",
                PairingSessionId::new(),
                "dev-a".into(),
                "dev-b".into(),
            )
            .unwrap();
            assert!(matches!(
                finish_pairing(state, &inbound),
                Err(CryptoError::Pairing("spake2 finish failed"))
            ));
        }
    }

    #[test]
    fn confirmation_checks_length_role_and_transcript_authentication() {
        let session = PairingSessionId::new();
        let a = start_pairing_a("482913", session.clone(), "dev-a".into(), "dev-b".into()).unwrap();
        let b = start_pairing_b("482913", session, "dev-a".into(), "dev-b".into()).unwrap();
        let keys = finish_pairing(a, &b.outbound).unwrap();
        let valid = confirm_mac(&keys, PairingRole::B);
        verify_confirm_mac(&keys, PairingRole::B, &valid).unwrap();
        for invalid in [Vec::new(), valid[..31].to_vec(), vec![0; 33]] {
            assert!(matches!(
                verify_confirm_mac(&keys, PairingRole::B, &invalid),
                Err(CryptoError::Confirm)
            ));
        }
        assert!(matches!(
            verify_confirm_mac(&keys, PairingRole::A, &valid),
            Err(CryptoError::Confirm)
        ));
        let mut damaged = valid.clone();
        damaged[0] ^= 1;
        assert!(matches!(
            verify_confirm_mac(&keys, PairingRole::B, &damaged),
            Err(CryptoError::Confirm)
        ));
        let mut changed = keys;
        changed.transcript_hash[0] ^= 1;
        assert!(matches!(
            verify_confirm_mac(&changed, PairingRole::B, &valid),
            Err(CryptoError::Confirm)
        ));
    }

    /// Recorded from the seeded run below. It pins SPAKE2 group constants,
    /// the transcript layout, the HKDF label, and the decimal rendering
    /// together, so a change to any of them is visible as a compatibility break.
    const SEEDED_VECTOR_SAS: &str = "401515";

    /// Deterministic `rand_core` 0.6 source for SPAKE2 scalars in tests only.
    struct SeededRng(u64);

    impl spake2_rand_core::RngCore for SeededRng {
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }
        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_be_bytes(bytes)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(32) {
                let block = Sha256::digest(self.0.to_be_bytes());
                self.0 += 1;
                chunk.copy_from_slice(&block[..chunk.len()]);
            }
        }
        fn try_fill_bytes(
            &mut self,
            dest: &mut [u8],
        ) -> std::result::Result<(), spake2_rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl spake2_rand_core::CryptoRng for SeededRng {}

    /// A fresh pairing code for tests whose outcome must not depend on a
    /// particular secret; only the pinned vectors use fixed inputs.
    fn random_code() -> String {
        // Six decimal digits, the relay's pairing-code format.
        const PAIRING_CODE_SPACE: u32 = 1_000_000;
        format!("{:06}", rand::random_range(0..PAIRING_CODE_SPACE))
    }

    fn seeded(
        role: PairingRole,
        seed: u64,
        password: &str,
        session_id: &PairingSessionId,
        device_a: &DeviceId,
        device_b: &DeviceId,
    ) -> PairingState {
        start_with_rng(
            role,
            password,
            session_id.clone(),
            device_a.clone(),
            device_b.clone(),
            SeededRng(seed),
        )
    }

    #[test]
    fn honest_peers_derive_the_same_sas() {
        let code = random_code();
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let sa = start_pairing_a(&code, session.clone(), a.clone(), b.clone()).unwrap();
        let sb = start_pairing_b(&code, session, a, b).unwrap();
        let (msg_a, msg_b) = (sa.outbound.clone(), sb.outbound.clone());
        let ka = finish_pairing(sa, &msg_b).unwrap();
        let kb = finish_pairing(sb, &msg_a).unwrap();
        let sas = short_authentication_string(&ka);
        assert_eq!(sas, short_authentication_string(&kb));
        assert_eq!(sas.len(), SAS_DIGITS as usize);
        assert!(sas.bytes().all(|byte| byte.is_ascii_digit()));
    }

    #[test]
    fn relay_mitm_with_the_code_yields_different_sas_on_each_device() {
        // The relay issues the code, so it can complete one SPAKE2 session
        // with each device. Key confirmation succeeds on both, so only the
        // out-of-band SAS comparison can reveal the split.
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let code = &random_code();
        let honest_a = seeded(PairingRole::A, 1, code, &session, &a, &b);
        let relay_as_b = seeded(PairingRole::B, 2, code, &session, &a, &b);
        let relay_as_a = seeded(PairingRole::A, 3, code, &session, &a, &b);
        let honest_b = seeded(PairingRole::B, 4, code, &session, &a, &b);
        let (to_relay_from_a, to_a) = (honest_a.outbound.clone(), relay_as_b.outbound.clone());
        let (to_b, to_relay_from_b) = (relay_as_a.outbound.clone(), honest_b.outbound.clone());
        let keys_a = finish_pairing(honest_a, &to_a).unwrap();
        let relay_keys_a = finish_pairing(relay_as_b, &to_relay_from_a).unwrap();
        let relay_keys_b = finish_pairing(relay_as_a, &to_relay_from_b).unwrap();
        let keys_b = finish_pairing(honest_b, &to_b).unwrap();
        // Both devices accept the relay's key confirmation.
        verify_confirm_mac(
            &keys_a,
            PairingRole::B,
            &confirm_mac(&relay_keys_a, PairingRole::B),
        )
        .unwrap();
        verify_confirm_mac(
            &keys_b,
            PairingRole::A,
            &confirm_mac(&relay_keys_b, PairingRole::A),
        )
        .unwrap();
        assert_ne!(
            short_authentication_string(&keys_a),
            short_authentication_string(&keys_b),
            "a split pairing must not show matching SAS values"
        );
        assert_ne!(keys_a.sas_key, keys_b.sas_key);
    }

    #[test]
    fn wrong_peer_with_a_guessed_code_yields_a_different_sas() {
        // A guesser completes SPAKE2 with the creator; the device the user
        // intended to pair ran its own session and shows a different value.
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let guesser = DeviceId::from("dev-guesser");
        let intended = DeviceId::from("dev-intended");
        let code = random_code();
        let creator = seeded(PairingRole::A, 5, &code, &session, &a, &guesser);
        let joiner = seeded(PairingRole::B, 6, &code, &session, &a, &guesser);
        let (msg_a, msg_b) = (creator.outbound.clone(), joiner.outbound.clone());
        let creator_keys = finish_pairing(creator, &msg_b).unwrap();
        let guesser_keys = finish_pairing(joiner, &msg_a).unwrap();
        assert_eq!(
            short_authentication_string(&creator_keys),
            short_authentication_string(&guesser_keys)
        );
        let other_a = seeded(PairingRole::A, 7, &code, &session, &a, &intended);
        let other_b = seeded(PairingRole::B, 8, &code, &session, &a, &intended);
        let (msg_a, msg_b) = (other_a.outbound.clone(), other_b.outbound.clone());
        let intended_keys = finish_pairing(other_b, &msg_a).unwrap();
        let _ = finish_pairing(other_a, &msg_b).unwrap();
        assert_ne!(
            short_authentication_string(&creator_keys),
            short_authentication_string(&intended_keys)
        );
    }

    #[test]
    fn sas_derivation_matches_independent_hkdf_vector() {
        // Python: HKDF-SHA256(ikm=[7]*32, salt=[3]*32, info=b"clipsync/v1/sas"),
        // then the first 8 bytes big-endian modulo 10**6, zero-padded.
        let key = derive_sas_key(&[7; 32], &[3; 32]).unwrap();
        assert_eq!(
            hex::encode(key),
            "19acaf08b7cfd91531f560b32cc1cb6ec712b3941cb0fb5113662171e092c59d"
        );
        assert_eq!(render_sas(&key), "882453");
        assert_eq!(SAS_LABEL, b"clipsync/v1/sas");
    }

    #[test]
    fn sas_rendering_zero_pads_and_reduces_the_leading_bytes() {
        let mut key = [0u8; KEY_LEN];
        assert_eq!(render_sas(&key), "000000");
        key[7] = 42;
        assert_eq!(render_sas(&key), "000042");
        key = [0xff; KEY_LEN];
        // u64::MAX % 10^6 = 551615; bytes past the sample do not matter.
        assert_eq!(render_sas(&key), "551615");
        key[SAS_SAMPLE_BYTES] = 0;
        assert_eq!(render_sas(&key), "551615");
    }

    #[test]
    fn seeded_pairing_pins_the_end_to_end_sas_vector() {
        let session = PairingSessionId::from("01JCLIPSYNCSASVECTOR00000");
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let sa = seeded(PairingRole::A, 11, "482913", &session, &a, &b);
        let sb = seeded(PairingRole::B, 12, "482913", &session, &a, &b);
        let (msg_a, msg_b) = (sa.outbound.clone(), sb.outbound.clone());
        let ka = finish_pairing(sa, &msg_b).unwrap();
        let kb = finish_pairing(sb, &msg_a).unwrap();
        assert_eq!(short_authentication_string(&ka), SEEDED_VECTOR_SAS);
        assert_eq!(short_authentication_string(&kb), SEEDED_VECTOR_SAS);
    }

    #[test]
    fn transcript_message_lengths_and_device_order_are_bound() {
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let transcript = pairing_transcript(&session, &a, &b, b"ab", b"c");
        assert_ne!(
            transcript,
            pairing_transcript(&session, &a, &b, b"a", b"bc")
        );
        assert_ne!(
            transcript,
            pairing_transcript(&session, &b, &a, b"ab", b"c")
        );
        assert_ne!(
            transcript,
            pairing_transcript(&PairingSessionId::new(), &a, &b, b"ab", b"c")
        );
    }
}
