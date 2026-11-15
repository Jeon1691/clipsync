// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![no_main]

use clipsync_crypto::{open, open_prefixed, seal, seal_prefixed, AeadError};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Even undersized inputs exercise the untrusted nonce-prefixed decoder.
    let _ = open_prefixed(&[0; 32], &[], data);
    if data.len() < 45 {
        if data.len() < 12 {
            assert!(matches!(
                open_prefixed(&[0; 32], &[], data),
                Err(AeadError::Nonce)
            ));
        }
        return;
    }

    let mut key = [0; 32];
    key.copy_from_slice(&data[..32]);
    let mut nonce = [0; 12];
    nonce.copy_from_slice(&data[32..44]);
    let payload = &data[45..];
    let aad_len = usize::from(data[44]).min(payload.len());
    let (aad, plaintext) = payload.split_at(aad_len);

    let ciphertext = seal(&key, &nonce, aad, plaintext).expect("valid AEAD inputs must encrypt");
    assert_eq!(
        open(&key, &nonce, aad, &ciphertext).expect("sealed bytes must decrypt"),
        plaintext
    );
    let prefixed = seal_prefixed(&key, &nonce, aad, plaintext).expect("valid inputs must encrypt");
    assert_eq!(prefixed.len(), plaintext.len() + 28);
    assert_eq!(
        open_prefixed(&key, aad, &prefixed).expect("prefixed bytes must decrypt"),
        plaintext
    );

    // The ciphertext includes a 16-byte tag even for an empty plaintext.
    let mut altered = ciphertext;
    altered[0] ^= 1;
    assert!(open(&key, &nonce, aad, &altered).is_err());
    let mut altered_key = key;
    altered_key[0] ^= 1;
    assert!(open_prefixed(&altered_key, aad, &prefixed).is_err());
    let mut altered_aad = aad.to_vec();
    altered_aad.push(1);
    assert!(open_prefixed(&key, &altered_aad, &prefixed).is_err());
    let mut altered_nonce = prefixed;
    altered_nonce[0] ^= 1;
    assert!(open_prefixed(&key, aad, &altered_nonce).is_err());
});
