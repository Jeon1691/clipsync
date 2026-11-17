// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Capabilities a device advertises in its Noise handshake payload.
//!
//! The payload travels inside the `Noise_KK` handshake, so a successfully read
//! payload is authenticated to the static key of the pinned peer the handshake
//! ran against; the relay can drop it but cannot forge or alter it. Released
//! clients send an empty payload and ignore the payload they receive, so an
//! empty or unparseable payload simply means "no capabilities advertised".

use serde::{Deserialize, Serialize};

use crate::{ProtocolError, Result, ENVELOPE_VERSION_AUTHENTICATED};

/// Largest handshake payload released clients (v0.1.0 through v0.2.0) can read:
/// they decrypt it into a fixed 1024-byte buffer and fail the handshake if it
/// does not fit. Advertised capabilities must stay within it.
pub const LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES: usize = 1024;

/// Capabilities carried in a Noise handshake payload (JSON object).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeCapabilities {
    /// Envelope versions this device accepts on receive.
    #[serde(default)]
    pub envelope_versions: Vec<u16>,
}

impl HandshakeCapabilities {
    /// Capabilities of a device accepting `envelope_versions`.
    pub fn accepting(envelope_versions: &[u16]) -> Self {
        Self {
            envelope_versions: envelope_versions.to_vec(),
        }
    }

    /// Encode for a handshake payload, refusing anything a released client
    /// could not read.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let payload = serde_json::to_vec(self)?;
        if payload.len() > LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES {
            return Err(ProtocolError::Capabilities(
                "handshake payload exceeds the legacy client limit",
            ));
        }
        Ok(payload)
    }

    /// Decode a peer's handshake payload. An empty payload (released clients)
    /// or one this version does not understand advertises nothing.
    pub fn decode(payload: &[u8]) -> Self {
        serde_json::from_slice(payload).unwrap_or_default()
    }

    /// The peer can open authenticated (v2) envelopes.
    pub fn accepts_authenticated(&self) -> bool {
        self.envelope_versions
            .contains(&ENVELOPE_VERSION_AUTHENTICATED)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::ENVELOPE_VERSION_LEGACY;

    #[test]
    fn capabilities_roundtrip_and_fit_released_clients() {
        let caps = HandshakeCapabilities::accepting(&[
            ENVELOPE_VERSION_LEGACY,
            ENVELOPE_VERSION_AUTHENTICATED,
        ]);
        let payload = caps.encode().unwrap();
        assert!(payload.len() <= LEGACY_HANDSHAKE_PAYLOAD_MAX_BYTES);
        assert_eq!(payload, br#"{"envelope_versions":[1,2]}"#);
        let decoded = HandshakeCapabilities::decode(&payload);
        assert_eq!(decoded, caps);
        assert!(decoded.accepts_authenticated());
    }

    #[test]
    fn empty_unknown_or_oversized_payloads_advertise_nothing_or_fail_to_encode() {
        for payload in [
            &b""[..],
            b"not json",
            b"[2]",
            br#"{"envelope_versions":"2"}"#,
            br#"{"envelope_versions":[1]}"#,
        ] {
            assert!(
                !HandshakeCapabilities::decode(payload).accepts_authenticated(),
                "{payload:?}"
            );
        }
        // Unknown future fields are ignored, known ones still count.
        assert!(
            HandshakeCapabilities::decode(br#"{"envelope_versions":[2,3],"x":1}"#)
                .accepts_authenticated()
        );
        let oversized = HandshakeCapabilities::accepting(&[ENVELOPE_VERSION_AUTHENTICATED; 512]);
        assert!(matches!(
            oversized.encode(),
            Err(ProtocolError::Capabilities(_))
        ));
    }
}
