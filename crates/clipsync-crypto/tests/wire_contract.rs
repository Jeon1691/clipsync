// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_crypto::{open_prefixed, parse_public, seal_prefixed, AeadError, DeviceIdentity};

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
