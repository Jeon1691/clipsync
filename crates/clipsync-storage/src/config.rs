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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncConfig {
    pub enabled: bool,
    pub direction: SyncDirection,
    pub auto_start: bool,
    pub types: Vec<String>,
    pub debounce_ms: u64,
    pub ttl_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotifyConfig {
    pub enabled: bool,
    pub preview: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    pub secret: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            relay_url: default_relay_url(),
            sync: SyncConfig::default(),
            notify: NotifyConfig::default(),
            webhook: None,
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
            "notify.enabled" => Some(self.notify.enabled.to_string()),
            "notify.preview" => Some(self.notify.preview.to_string()),
            _ => None,
        }
    }

    pub fn set(&mut self, key: &str, value: &str) -> bool {
        match key {
            "relay_url" => {
                self.relay_url = value.to_string();
                true
            }
            "sync.enabled" => {
                self.sync.enabled = parse_bool(value);
                true
            }
            "sync.auto_start" => {
                self.sync.auto_start = parse_bool(value);
                true
            }
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
            "notify.enabled" => {
                self.notify.enabled = parse_bool(value);
                true
            }
            "notify.preview" => {
                self.notify.preview = parse_bool(value);
                true
            }
            _ => false,
        }
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(v, "1" | "true" | "yes" | "on")
}
