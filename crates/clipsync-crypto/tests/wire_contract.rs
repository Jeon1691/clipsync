// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_crypto::{
    confirm_mac, open_prefixed, parse_public, seal_prefixed, verify_confirm_mac, AeadError,
    DeviceIdentity, PairingKeys, PairingRole, StoredIdentity,
};

#[test]
fn aes256_gcm_wire_matches_openssl_fixture() {
    // Independently generated with OpenSSL EVP AES-256-GCM. All fixture keys are synthetic.
    let expected = hex::decode(concat!(
        "171717171717171717171717",
        "e68cc3bdfa86f141c1b1b5069213bc",
        "6f9984213eda758e3971b892b97a00ec"
    ))
    .unwrap();
    assert_eq!(
        seal_prefixed(
            &[19; 32],
            &[23; 12],
            b"room:message:seq",
            b"private\0payload"
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        open_prefixed(&[19; 32], b"room:message:seq", &expected).unwrap(),
        b"private\0payload"
    );
}

#[test]
fn persisted_x25519_identity_matches_openssl_fixture() {
    // OpenSSL X25519 raw public key, with its fingerprint computed by Python hashlib.sha256.
    let stored = StoredIdentity {
        device_id: "openssl-fixture".into(),
        secret: "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f".into(),
        public: "8f40c5adb68f25624ae5b214ea767a6ec94d829d3d7b5e1ad1ba6f3e2138285f".into(),
        fingerprint: "eedd883da0a9451593dc0a38e583fb770fa27dbbed209c20404e075e68c4f0c9".into(),
    };
    let restored = DeviceIdentity::from_stored(&stored).unwrap();
    assert_eq!(restored.device_id, stored.device_id);
    assert_eq!(hex::encode(restored.secret_bytes()), stored.secret);
    assert_eq!(hex::encode(restored.public_bytes()), stored.public);
    assert_eq!(restored.fingerprint(), stored.fingerprint);
}

#[test]
fn pairing_confirmation_matches_python_hmac_fixture() {
    // Python hmac.new(key, role_label + transcript_hash, hashlib.sha256).
    let keys = PairingKeys {
        confirm_key_a: [1; 32],
        confirm_key_b: [2; 32],
        transcript_hash: [3; 32],
        pairing_wrap_key: [4; 32],
        room_root_key: [5; 32],
    };
    for (role, expected) in [
        (
            PairingRole::A,
            "8f56ef9af9cfd3955ee0928b074bcfef3a08b5c7044d810a105d0c2509ef336c",
        ),
        (
            PairingRole::B,
            "e0677ba534053cb2fdb00d884c0588bea75d311e7069d78ab5b9d1d5090f6cec",
        ),
    ] {
        let expected = hex::decode(expected).unwrap();
        assert_eq!(confirm_mac(&keys, role), expected);
        verify_confirm_mac(&keys, role, &expected).unwrap();
    }
}

#[test]
fn nonce_prefixed_ciphertexts_authenticate_payload_context_and_nonce() {
    let key = [19; 32];
    let nonce = [23; 12];
    let ciphertext = seal_prefixed(&key, &nonce, b"room:message:seq", b"private\0payload").unwrap();
    assert_eq!(&ciphertext[..12], nonce);
    assert_eq!(
        open_prefixed(&key, b"room:message:seq", &ciphertext).unwrap(),
        b"private\0payload"
    );
    assert!(open_prefixed(&key, b"another message", &ciphertext).is_err());
    let mut damaged = ciphertext;
    damaged[0] ^= 1;
    assert!(open_prefixed(&key, b"room:message:seq", &damaged).is_err());
    assert!(matches!(
        open_prefixed(&key, b"", &[0; 11]),
        Err(AeadError::Nonce)
    ));
    assert!(matches!(
        open_prefixed(&key, b"", &[0; 12]),
        Err(AeadError::Decrypt)
    ));
}

#[test]
fn persisted_identity_rejects_invalid_key_lengths_and_reconstructs_public_material() {
    let identity = DeviceIdentity::generate();
    let stored = identity.to_stored();
    let restored = DeviceIdentity::from_stored(&stored).unwrap();
    assert_eq!(restored.device_id, identity.device_id);
    assert_eq!(restored.secret_bytes(), identity.secret_bytes());
    assert_eq!(restored.public_bytes(), identity.public_bytes());
    assert_eq!(restored.fingerprint(), identity.fingerprint());
    assert_eq!(
        parse_public(&stored.public).unwrap().as_bytes(),
        &identity.public_bytes()
    );
    for invalid in ["not-hex".to_owned(), "00".repeat(31), "00".repeat(33)] {
        let mut bad = stored.clone();
        bad.secret = invalid.clone();
        assert!(DeviceIdentity::from_stored(&bad).is_none());
        assert!(parse_public(&invalid).is_none());
    }
}
