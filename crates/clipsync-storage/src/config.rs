// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};

use clipsync_protocol::{SyncDirection, DEFAULT_TTL_SECS};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub relay_url: String,
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub webhook: Option<WebhookConfig>,
    #[serde(default)]
    pub pairing: PairingConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PairingConfig {
    /// Refuse to pair when the other device or the relay does not support
    /// the commitment round that keeps a relay from searching for matching
    /// verification codes (see `docs/security.md`). Off by default, so
    /// devices and relays without it still pair, with reduced protection.
    #[serde(default)]
    pub require_commitment: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncConfig {
    pub enabled: bool,
    pub direction: SyncDirection,
    pub auto_start: bool,
    pub types: Vec<String>,
    pub debounce_ms: u64,
    pub ttl_secs: u64,
    /// Override: always send legacy v1 envelopes, even to peers known to accept
    /// v2. Off by default, in which case the send version is negotiated per
    /// peer; see docs/protocol.md.
    #[serde(default)]
    pub legacy_envelopes: bool,
    /// Hardening override: always send authenticated v2 envelopes and refuse
    /// legacy v1 from every peer. Takes precedence over `legacy_envelopes`.
    #[serde(default)]
    pub strict_envelopes: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotifyConfig {
    pub enabled: bool,
    pub preview: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    // Accept legacy config files, but never serialize credentials into metadata.
    #[serde(default, skip_serializing)]
    pub secret: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            relay_url: default_relay_url(),
            sync: SyncConfig::default(),
            notify: NotifyConfig::default(),
            webhook: None,
            pairing: PairingConfig::default(),
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            direction: SyncDirection::Both,
            auto_start: true,
            types: vec!["text".into(), "image".into(), "files".into()],
            debounce_ms: clipsync_protocol::DEBOUNCE_MS,
            ttl_secs: DEFAULT_TTL_SECS,
            legacy_envelopes: false,
            strict_envelopes: false,
        }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            preview: false,
        }
    }
}

fn default_relay_url() -> String {
    std::env::var("CLIPSYNC_RELAY_URL").unwrap_or_else(|_| "https://clipsync.develicit.dev".into())
}

impl AppConfig {
    pub fn get(&self, key: &str) -> Option<String> {
        match key {
            "relay_url" => Some(self.relay_url.clone()),
            "sync.enabled" => Some(self.sync.enabled.to_string()),
            "sync.direction" => Some(format!("{:?}", self.sync.direction).to_lowercase()),
            "sync.auto_start" => Some(self.sync.auto_start.to_string()),
            "sync.ttl_secs" => Some(self.sync.ttl_secs.to_string()),
            "sync.legacy_envelopes" => Some(self.sync.legacy_envelopes.to_string()),
            "sync.strict_envelopes" => Some(self.sync.strict_envelopes.to_string()),
            "notify.enabled" => Some(self.notify.enabled.to_string()),
            "notify.preview" => Some(self.notify.preview.to_string()),
            "pairing.require_commitment" => Some(self.pairing.require_commitment.to_string()),
            _ => None,
        }
    }

    /// Set `key` from its `config set` string. Returns false, leaving the
    /// config unchanged, for an unknown key or a value the key does not take.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        match key {
            "relay_url" => {
                self.relay_url = value.to_string();
                true
            }
            "sync.enabled" => set_bool(&mut self.sync.enabled, value),
            "sync.auto_start" => set_bool(&mut self.sync.auto_start, value),
            "sync.legacy_envelopes" => set_bool(&mut self.sync.legacy_envelopes, value),
            "sync.strict_envelopes" => set_bool(&mut self.sync.strict_envelopes, value),
            "sync.direction" => {
                self.sync.direction = match value {
                    "both" => SyncDirection::Both,
                    "send-only" | "send_only" => SyncDirection::SendOnly,
                    "receive-only" | "receive_only" => SyncDirection::ReceiveOnly,
                    _ => return false,
                };
                true
            }
            "sync.ttl_secs" => {
                if let Ok(v) = value.parse() {
                    self.sync.ttl_secs = clipsync_protocol::clamp_ttl(v);
                    true
                } else {
                    false
                }
            }
            "notify.enabled" => set_bool(&mut self.notify.enabled, value),
            "notify.preview" => set_bool(&mut self.notify.preview, value),
            "pairing.require_commitment" => set_bool(&mut self.pairing.require_commitment, value),
            _ => false,
        }
    }
}

/// Parse an explicit boolean. Anything else is rejected rather than read as
/// false, so a typo cannot silently leave an opt-in such as
/// `pairing.require_commitment` or `sync.strict_envelopes` turned off.
fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Store `value` in `field` if it is an explicit boolean; otherwise leave
/// `field` unchanged and report the value as invalid.
fn set_bool(field: &mut bool, value: &str) -> bool {
    match parse_bool(value) {
        Some(parsed) => {
            *field = parsed;
            true
        }
        None => false,
    }
}
