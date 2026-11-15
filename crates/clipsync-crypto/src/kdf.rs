// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use hkdf::Hkdf;
use sha2::Sha256;

use crate::{CryptoError, Result};

pub const KEY_LEN: usize = 32;
pub const CONTENT_HASH_LEN: usize = 32;

pub fn hkdf_sha256(ikm: &[u8], salt: Option<&[u8]>, info: &[u8], out: &mut [u8]) -> Result<()> {
    let hk = Hkdf::<Sha256>::new(salt, ikm);
    hk.expand(info, out).map_err(|_| CryptoError::Hkdf)
}

pub fn derive_labeled(ikm: &[u8], salt: &[u8], label: &[u8]) -> Result<[u8; KEY_LEN]> {
    let mut out = [0u8; KEY_LEN];
    hkdf_sha256(ikm, Some(salt), label, &mut out)?;
    Ok(out)
}

pub fn derive_content_key(epoch_key: &[u8; KEY_LEN], transfer_id: &str) -> Result<[u8; KEY_LEN]> {
    let info = format!("clipsync/v1/content/{transfer_id}");
    derive_labeled(epoch_key, b"clipsync-content", info.as_bytes())
}

pub fn derive_nonce(content_key: &[u8; KEY_LEN], chunk_index: u32) -> Result<[u8; 12]> {
    let mut okm = [0u8; 12];
    let info = format!("clipsync/v1/nonce/{chunk_index}");
    hkdf_sha256(
        content_key,
        Some(b"clipsync-nonce"),
        info.as_bytes(),
        &mut okm,
    )?;
    Ok(okm)
}

pub fn blake3_hash(data: &[u8]) -> [u8; CONTENT_HASH_LEN] {
    *blake3::hash(data).as_bytes()
}

pub fn blake3_hex(data: &[u8]) -> String {
    hex::encode(blake3_hash(data))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn hkdf_sha256_matches_rfc5869_with_and_without_salt() {
        // RFC 5869 Appendix A.1/A.3: independent known-answer vectors.
        let input = [0x0b; 22];
        let mut output = [0u8; 42];
        hkdf_sha256(
            &input,
            Some(&(0u8..=12).collect::<Vec<_>>()),
            &(0xf0u8..=0xf9).collect::<Vec<_>>(),
            &mut output,
        )
        .unwrap();
        assert_eq!(
            hex::encode(output),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
        hkdf_sha256(&input, None, &[], &mut output).unwrap();
        assert_eq!(
            hex::encode(output),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
        );
    }

    #[test]
    fn hkdf_output_limit_accepts_the_maximum_and_rejects_oversized_requests() {
        let mut longest = vec![0; 255 * 32];
        hkdf_sha256(
            b"synthetic key material",
            None,
            b"limit fixture",
            &mut longest,
        )
        .unwrap();
        let mut oversized = vec![0x55; 255 * 32 + 1];
        assert!(matches!(
            hkdf_sha256(
                b"synthetic key material",
                None,
                b"limit fixture",
                &mut oversized
            ),
            Err(CryptoError::Hkdf)
        ));
    }

    #[test]
    fn content_keys_and_chunk_nonces_separate_transfer_contexts() {
        let epoch = [13u8; KEY_LEN];
        let a = derive_content_key(&epoch, "transfer-a").unwrap();
        let b = derive_content_key(&epoch, "transfer-b").unwrap();
        assert_eq!(a, derive_content_key(&epoch, "transfer-a").unwrap());
        assert_ne!(a, b);
        assert_ne!(a, crate::initial_epoch_key(&epoch).unwrap());
        assert_ne!(
            derive_nonce(&a, 0).unwrap(),
            derive_nonce(&a, u32::MAX).unwrap()
        );
        assert_ne!(derive_nonce(&a, 0).unwrap(), derive_nonce(&b, 0).unwrap());
    }
}
