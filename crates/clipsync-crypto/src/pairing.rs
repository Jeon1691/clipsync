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
/// HKDF info label for the SAS of a pairing whose joiner committed to its
/// SPAKE2 message first. It differs from [`SAS_LABEL`], so the two modes
/// show unrelated codes: a relay that merely strips or withholds the
/// commitment from one device makes the codes differ. It does not stop a
/// relay that downgrades a device and then searches for messages toward
/// it; users must check that both devices show full protection.
pub const COMMITTED_SAS_LABEL: &[u8] = b"clipsync/v1/sas-committed";
/// Domain label of the joiner's commitment to its SPAKE2 message.
pub const SPAKE2_COMMIT_LABEL: &[u8] = b"clipsync/v1/spake2-commit";
/// Bytes in a commitment: one SHA-256 digest.
pub const SPAKE2_COMMITMENT_LEN: usize = 32;

/// Whether the SAS was derived after a commitment round.
///
/// With [`SasProtection::Committed`] the joiner (B) sent
/// `SHA-256(label, version, session, device IDs, role, its SPAKE2 message)`
/// before it saw the creator's message, and the creator (A) sent its own
/// message only after it held that commitment. A relay in the middle then
/// has to fix its message toward each device before it learns what that
/// device sends, so it gets one try per pairing at making two independent
/// codes collide. This is the commit-then-reveal order of ZRTP (RFC 6189,
/// section 4.4.1.1, `hvi`) and of the Bluetooth LE Secure Connections
/// numeric comparison (Core Specification v5.x, Vol 3, Part H, 2.3.5.6.2).
///
/// [`SasProtection::Uncommitted`] is the exchange without that round, used
/// with a peer or relay that does not support it. A relay can then search
/// offline for messages that make the codes match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SasProtection {
    Committed,
    Uncommitted,
}

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
    /// Input to [`short_authentication_string`] for
    /// [`SasProtection::Uncommitted`]; never sent on the wire.
    pub sas_key: [u8; KEY_LEN],
    /// Input to [`short_authentication_string`] for
    /// [`SasProtection::Committed`]; never sent on the wire.
    pub committed_sas_key: [u8; KEY_LEN],
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

/// Commitment input: the label and version, then every field with a 32-bit
/// big-endian length prefix, so no two field splits encode alike.
fn commitment_digest(
    version: u16,
    session_id: &PairingSessionId,
    device_a: &DeviceId,
    device_b: &DeviceId,
    role: PairingRole,
    message: &[u8],
) -> [u8; SPAKE2_COMMITMENT_LEN] {
    let role: &[u8] = match role {
        PairingRole::A => b"A",
        PairingRole::B => b"B",
    };
    let mut hash = Sha256::new();
    hash.update(SPAKE2_COMMIT_LABEL);
    hash.update(version.to_be_bytes());
    for field in [
        session_id.as_str().as_bytes(),
        device_a.as_str().as_bytes(),
        device_b.as_str().as_bytes(),
        role,
        message,
    ] {
        hash.update((field.len() as u32).to_be_bytes());
        hash.update(field);
    }
    hash.finalize().into()
}

/// Commitment to this side's SPAKE2 message, bound to the protocol version,
/// session, both device IDs and the role. The joiner sends it before its
/// message. The message itself is a fresh random group element, so it is
/// its own hiding randomness and no separate nonce is needed (as in ZRTP's
/// `hvi`); binding rests on SHA-256 collision resistance.
pub fn spake2_commitment(state: &PairingState) -> [u8; SPAKE2_COMMITMENT_LEN] {
    commitment_digest(
        PROTOCOL_VERSION,
        &state.session_id,
        &state.device_a,
        &state.device_b,
        state.role,
        &state.outbound,
    )
}

/// Check that the peer's SPAKE2 message opens the commitment it sent first.
pub fn verify_spake2_commitment(
    state: &PairingState,
    commitment: &[u8],
    peer_message: &[u8],
) -> Result<()> {
    let peer_role = match state.role {
        PairingRole::A => PairingRole::B,
        PairingRole::B => PairingRole::A,
    };
    let expected = commitment_digest(
        PROTOCOL_VERSION,
        &state.session_id,
        &state.device_a,
        &state.device_b,
        peer_role,
        peer_message,
    );
    use subtle::ConstantTimeEq;
    if commitment.len() != expected.len() || expected.ct_eq(commitment).unwrap_u8() != 1 {
        return Err(CryptoError::Commitment);
    }
    Ok(())
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
    let committed_sas_key = derive_labeled(&shared, &transcript_hash, COMMITTED_SAS_LABEL)?;

    Ok(PairingKeys {
        confirm_key_a,
        confirm_key_b,
        pairing_wrap_key,
        room_root_key,
        sas_key,
        committed_sas_key,
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
/// It is computed locally from the SPAKE2 secret, the transcript and the
/// [`SasProtection`] mode. Two devices show the same value only when they
/// ran SPAKE2 with each other in the same mode. A relay that ran one session
/// with each device, or a joiner that guessed the code, makes the values
/// differ. See `docs/security.md` for what the comparison does and does not
/// cover in each mode.
pub fn short_authentication_string(keys: &PairingKeys, protection: SasProtection) -> String {
    match protection {
        SasProtection::Committed => render_sas(&keys.committed_sas_key),
        SasProtection::Uncommitted => render_sas(&keys.sas_key),
    }
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
    /// The same seeded run in committed mode (`COMMITTED_SAS_LABEL`).
    const SEEDED_VECTOR_COMMITTED_SAS: &str = "153704";
    /// SHA-256 commitment for the fixed inputs in
    /// `commitment_matches_independent_sha256_vector`.
    const COMMITMENT_VECTOR: &str =
        "579d2556e8ff14a848720df6e46b37b4710252692c4602e66c3e6b283025bd30";

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
        let sas = short_authentication_string(&ka, SasProtection::Uncommitted);
        assert_eq!(
            sas,
            short_authentication_string(&kb, SasProtection::Uncommitted)
        );
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
            short_authentication_string(&keys_a, SasProtection::Uncommitted),
            short_authentication_string(&keys_b, SasProtection::Uncommitted),
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
            short_authentication_string(&creator_keys, SasProtection::Uncommitted),
            short_authentication_string(&guesser_keys, SasProtection::Uncommitted)
        );
        let other_a = seeded(PairingRole::A, 7, &code, &session, &a, &intended);
        let other_b = seeded(PairingRole::B, 8, &code, &session, &a, &intended);
        let (msg_a, msg_b) = (other_a.outbound.clone(), other_b.outbound.clone());
        let intended_keys = finish_pairing(other_b, &msg_a).unwrap();
        let _ = finish_pairing(other_a, &msg_b).unwrap();
        assert_ne!(
            short_authentication_string(&creator_keys, SasProtection::Uncommitted),
            short_authentication_string(&intended_keys, SasProtection::Uncommitted)
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
        // Same inputs with info=b"clipsync/v1/sas-committed".
        let committed = derive_labeled(&[7; 32], &[3; 32], COMMITTED_SAS_LABEL).unwrap();
        assert_eq!(
            hex::encode(committed),
            "381c2212a4cbc77b14f50c75a413b79e6be30faea4a91e604ab26ea18fc14898"
        );
        assert_eq!(render_sas(&committed), "436795");
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
        assert_eq!(
            short_authentication_string(&ka, SasProtection::Uncommitted),
            SEEDED_VECTOR_SAS
        );
        assert_eq!(
            short_authentication_string(&kb, SasProtection::Uncommitted),
            SEEDED_VECTOR_SAS
        );
    }

    /// Two honest devices in the order the protocol fixes: the joiner (B)
    /// commits, the creator (A) answers with its message, B reveals.
    fn committed_exchange(code: &str) -> (PairingKeys, PairingKeys) {
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let sa = start_pairing_a(code, session.clone(), a.clone(), b.clone()).unwrap();
        let sb = start_pairing_b(code, session, a, b).unwrap();
        let commitment = spake2_commitment(&sb);
        let (msg_a, msg_b) = (sa.outbound.clone(), sb.outbound.clone());
        verify_spake2_commitment(&sa, &commitment, &msg_b).unwrap();
        (
            finish_pairing(sa, &msg_b).unwrap(),
            finish_pairing(sb, &msg_a).unwrap(),
        )
    }

    #[test]
    fn honest_committed_pairing_opens_the_commitment_and_derives_the_same_sas() {
        let (ka, kb) = committed_exchange(&random_code());
        let sas = short_authentication_string(&ka, SasProtection::Committed);
        assert_eq!(
            sas,
            short_authentication_string(&kb, SasProtection::Committed)
        );
        assert_eq!(sas.len(), SAS_DIGITS as usize);
        assert_eq!(
            short_authentication_string(&ka, SasProtection::Uncommitted),
            short_authentication_string(&kb, SasProtection::Uncommitted)
        );
    }

    #[test]
    fn a_relay_must_fix_its_message_before_it_sees_the_creators() {
        // The relay plays B toward the creator. The creator sends its SPAKE2
        // message only after it holds a commitment, so the relay commits
        // first and learns the creator's message afterwards. Any message it
        // grinds from then on, to steer the creator's SAS toward the one the
        // joiner will show, fails to open the commitment; only the message it
        // committed to before seeing anything opens it.
        const GRINDING_CANDIDATES: u64 = 64;
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let code = random_code();
        let committed = seeded(PairingRole::B, 21, &code, &session, &a, &b);
        let commitment = spake2_commitment(&committed);
        // Only now does the creator send its message.
        let honest_a = start_pairing_a(&code, session.clone(), a.clone(), b.clone()).unwrap();
        let mut accepted = 0;
        for seed in 0..GRINDING_CANDIDATES {
            let candidate = seeded(PairingRole::B, 1_000 + seed, &code, &session, &a, &b);
            assert!(
                matches!(
                    verify_spake2_commitment(&honest_a, &commitment, &candidate.outbound),
                    Err(CryptoError::Commitment)
                ),
                "a ground message opened the relay's commitment"
            );
            if verify_spake2_commitment(&honest_a, &commitment, &committed.outbound).is_ok() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, GRINDING_CANDIDATES);
        // A commitment cannot be replayed into another session, toward
        // another device pair, or from the other role.
        let other_session =
            start_pairing_a(&code, PairingSessionId::new(), a.clone(), b.clone()).unwrap();
        let other_device =
            start_pairing_a(&code, session.clone(), a.clone(), "dev-c".into()).unwrap();
        let as_joiner = start_pairing_b(&code, session.clone(), a.clone(), b.clone()).unwrap();
        for verifier in [&other_session, &other_device, &as_joiner] {
            assert!(matches!(
                verify_spake2_commitment(verifier, &commitment, &committed.outbound),
                Err(CryptoError::Commitment)
            ));
        }
        for malformed in [Vec::new(), commitment[..SPAKE2_COMMITMENT_LEN - 1].to_vec()] {
            assert!(matches!(
                verify_spake2_commitment(&honest_a, &malformed, &committed.outbound),
                Err(CryptoError::Commitment)
            ));
        }
    }

    #[test]
    fn commitment_binds_version_session_devices_role_and_message() {
        let session = PairingSessionId::from("01JCLIPSYNCCOMMITVECTOR00");
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let base = commitment_digest(PROTOCOL_VERSION, &session, &a, &b, PairingRole::B, b"msg");
        let changed = [
            commitment_digest(
                PROTOCOL_VERSION + 1,
                &session,
                &a,
                &b,
                PairingRole::B,
                b"msg",
            ),
            commitment_digest(
                PROTOCOL_VERSION,
                &PairingSessionId::new(),
                &a,
                &b,
                PairingRole::B,
                b"msg",
            ),
            commitment_digest(PROTOCOL_VERSION, &session, &b, &a, PairingRole::B, b"msg"),
            commitment_digest(
                PROTOCOL_VERSION,
                &session,
                &"dev-ad".into(),
                &"ev-b".into(),
                PairingRole::B,
                b"msg",
            ),
            commitment_digest(PROTOCOL_VERSION, &session, &a, &b, PairingRole::A, b"msg"),
            commitment_digest(PROTOCOL_VERSION, &session, &a, &b, PairingRole::B, b"msh"),
        ];
        for other in changed {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn commitment_matches_independent_sha256_vector() {
        // Python: hashlib.sha256(b"clipsync/v1/spake2-commit" + (1).to_bytes(2, "big")
        //   + b"".join(len(f).to_bytes(4, "big") + f for f in
        //     [b"01JCLIPSYNCCOMMITVECTOR00", b"dev-a", b"dev-b", b"B", b"msg"])).hexdigest()
        let digest = commitment_digest(
            1,
            &PairingSessionId::from("01JCLIPSYNCCOMMITVECTOR00"),
            &"dev-a".into(),
            &"dev-b".into(),
            PairingRole::B,
            b"msg",
        );
        assert_eq!(hex::encode(digest), COMMITMENT_VECTOR);
        assert_eq!(SPAKE2_COMMIT_LABEL, b"clipsync/v1/spake2-commit");
        assert_eq!(COMMITTED_SAS_LABEL, b"clipsync/v1/sas-committed");
    }

    #[test]
    fn the_mode_is_an_input_to_the_sas() {
        // The same SPAKE2 run yields unrelated codes in the two modes, so
        // devices left in different modes without a further search show
        // different codes.
        let session = PairingSessionId::from("01JCLIPSYNCSASVECTOR00000");
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let sa = seeded(PairingRole::A, 11, "482913", &session, &a, &b);
        let sb = seeded(PairingRole::B, 12, "482913", &session, &a, &b);
        let (msg_a, msg_b) = (sa.outbound.clone(), sb.outbound.clone());
        let ka = finish_pairing(sa, &msg_b).unwrap();
        let kb = finish_pairing(sb, &msg_a).unwrap();
        // The uncommitted value is the one an older (pre-commitment) peer shows.
        assert_eq!(
            short_authentication_string(&ka, SasProtection::Uncommitted),
            SEEDED_VECTOR_SAS
        );
        assert_eq!(
            short_authentication_string(&kb, SasProtection::Committed),
            SEEDED_VECTOR_COMMITTED_SAS
        );
        assert_ne!(
            short_authentication_string(&ka, SasProtection::Committed),
            short_authentication_string(&kb, SasProtection::Uncommitted)
        );
        assert_ne!(
            short_authentication_string(&ka, SasProtection::Uncommitted),
            short_authentication_string(&kb, SasProtection::Committed)
        );
        assert_ne!(ka.sas_key, ka.committed_sas_key);
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
