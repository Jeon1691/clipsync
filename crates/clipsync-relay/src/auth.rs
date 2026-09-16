use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
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
