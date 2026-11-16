// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use chrono::{Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use clipsync_protocol::{DeviceId, DeviceRole, RoomId};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenClaims {
    pub typ: String,
    pub rid: RoomId,
    pub did: DeviceId,
    pub role: DeviceRole,
    pub sid: Option<String>,
    pub exp: i64,
}

pub fn issue_token(
    secret: &[u8],
    typ: &str,
    room_id: &RoomId,
    device_id: &DeviceId,
    role: DeviceRole,
    session_id: Option<&str>,
    ttl_secs: i64,
) -> String {
    let claims = TokenClaims {
        typ: typ.into(),
        rid: room_id.clone(),
        did: device_id.clone(),
        role,
        sid: session_id.map(|s| s.to_string()),
        exp: (Utc::now() + Duration::seconds(ttl_secs)).timestamp(),
    };
    let payload = serde_json::to_vec(&claims).expect("token json");
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac key");
    mac.update(&payload);
    let tag = mac.finalize().into_bytes();
    format!(
        "{}.{}",
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &payload),
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, tag)
    )
}

pub fn verify_token(secret: &[u8], token: &str) -> Result<TokenClaims, &'static str> {
    let (p, t) = token.split_once('.').ok_or("malformed token")?;
    let payload = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, p)
        .map_err(|_| "bad payload b64")?;
    let tag = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, t)
        .map_err(|_| "bad tag b64")?;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| "hmac")?;
    mac.update(&payload);
    mac.verify_slice(&tag).map_err(|_| "bad signature")?;
    let claims: TokenClaims = serde_json::from_slice(&payload).map_err(|_| "bad json")?;
    if claims.exp < Utc::now().timestamp() {
        return Err("expired");
    }
    Ok(claims)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn token_signature_accepts_python_hmac_fixture() {
        // Python hmac/sha256 over the literal JSON, encoded using urlsafe_b64encode without padding.
        let token = concat!(
            "eyJ0eXAiOiJkZXZpY2UiLCJyaWQiOiJyb29tLWZpeHR1cmUiLCJkaWQiOiJkZXZpY2UtZml4dHVyZSIs",
            "InJvbGUiOiJvd25lciIsInNpZCI6bnVsbCwiZXhwIjoyMzAwMDAwMDAwfQ.",
            "M3vMtUpqpB12qfOrlO1gx2F8FLT_YSuCtmUvjDv--cs"
        );
        let claims = verify_token(&[17; 32], token).unwrap();
        assert_eq!(claims.typ, "device");
        assert_eq!(claims.rid.as_str(), "room-fixture");
        assert_eq!(claims.did.as_str(), "device-fixture");
        assert_eq!(claims.role, DeviceRole::Owner);
        assert_eq!(claims.sid, None);
        assert_eq!(claims.exp, 2_300_000_000);
        assert_eq!(verify_token(&[18; 32], token).unwrap_err(), "bad signature");
    }

    #[test]
    fn malformed_or_expired_tokens_fail_at_the_correct_validation_boundary() {
        let secret = [17; 32];
        for (token, expected) in [
            ("missing-dot", "malformed token"),
            ("!.AA", "bad payload b64"),
            ("e30.!", "bad tag b64"),
            ("e30.AA", "bad signature"),
        ] {
            assert_eq!(verify_token(&secret, token).unwrap_err(), expected);
        }
        let mut mac = HmacSha256::new_from_slice(&secret).unwrap();
        mac.update(b"not json");
        let codec = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let signed_invalid_json = format!(
            "{}.{}",
            codec.encode(b"not json"),
            codec.encode(mac.finalize().into_bytes())
        );
        assert_eq!(
            verify_token(&secret, &signed_invalid_json).unwrap_err(),
            "bad json"
        );
        let room = RoomId::new();
        let device = DeviceId::new();
        let expired = issue_token(
            &secret,
            "device",
            &room,
            &device,
            DeviceRole::Owner,
            None,
            -60,
        );
        assert_eq!(verify_token(&secret, &expired).unwrap_err(), "expired");
        let valid = issue_token(
            &secret,
            "pair",
            &room,
            &device,
            DeviceRole::Member,
            Some("session"),
            60,
        );
        let claims = verify_token(&secret, &valid).unwrap();
        assert_eq!(claims.rid, room);
        assert_eq!(claims.did, device);
        assert_eq!(claims.role, DeviceRole::Member);
        assert_eq!(claims.sid.as_deref(), Some("session"));
    }
}
