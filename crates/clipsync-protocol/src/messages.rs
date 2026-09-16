use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, MessageId, PairingSessionId, RoomId, TransferId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Text,
    Image,
    Files,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckStatus {
    Accepted,
    Streaming,
    Delivered,
    Expired,
    Rejected,
    TooLarge,
    PeerOffline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    Owner,
    Member,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    #[default]
    Both,
    SendOnly,
    ReceiveOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    PairingA,
    PairingB,
    Member,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingHeader {
    pub version: u16,
    pub message_id: MessageId,
    pub transfer_id: TransferId,
    pub room_id: RoomId,
    pub sender_device_id: DeviceId,
    pub recipient: Recipient,
    pub sent_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub item_kind: ItemKind,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recipient {
    Broadcast,
    Device(DeviceId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPlain {
    pub item_kind: ItemKind,
    pub total_bytes: u64,
    pub chunk_count: u32,
    pub content_hash: String,
    pub files: Vec<FileManifest>,
    pub image: Option<ImageManifest>,
    pub text_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifest {
    pub name: String,
    pub size: u64,
    pub mime: Option<String>,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageManifest {
    pub mime: String,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceBindPlain {
    pub device_id: DeviceId,
    pub static_public: String,
    pub device_name: String,
    pub os: String,
    pub fingerprint: String,
    pub epoch: u64,
    pub epoch_key: String,
    pub role: DeviceRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochWrap {
    pub device_id: DeviceId,
    pub eph_public: String,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedEnvelope {
    pub routing: RoutingHeader,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    Hello {
        protocol_version: u16,
        device_id: DeviceId,
        device_name: String,
        os: String,
        role: ClientRole,
        room_id: Option<RoomId>,
        token: String,
        pairing_session_id: Option<PairingSessionId>,
    },
    HelloOk {
        room_id: RoomId,
        peer_online: bool,
        peers: Vec<PeerInfo>,
        queued_text: Option<QueuedEnvelope>,
        device_token: Option<String>,
    },
    PeerJoined {
        device: PeerInfo,
    },
    PeerLeft {
        device_id: DeviceId,
    },
    Spake2Msg {
        pairing_session_id: PairingSessionId,
        message: String,
    },
    KeyConfirm {
        pairing_session_id: PairingSessionId,
        mac: String,
    },
    DeviceBind {
        pairing_session_id: PairingSessionId,
        nonce: String,
        ciphertext: String,
    },
    PairingComplete {
        room_id: RoomId,
        epoch: u64,
        device_token: String,
        role: DeviceRole,
    },
    NoiseHandshake {
        message: String,
    },
    TextEnvelope {
        routing: RoutingHeader,
        nonce: String,
        ciphertext: String,
    },
    Manifest {
        routing: RoutingHeader,
        nonce: String,
        ciphertext: String,
        chunk_count: u32,
        total_bytes: u64,
    },
    Ack {
        message_id: MessageId,
        transfer_id: Option<TransferId>,
        status: AckStatus,
        detail: Option<String>,
    },
    EpochWrapMsg {
        epoch: u64,
        wraps: Vec<EpochWrap>,
    },
    Revoke {
        device_id: DeviceId,
    },
    Ping {
        ts: i64,
    },
    Pong {
        ts: i64,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub device_id: DeviceId,
    pub device_name: String,
    pub os: String,
    pub role: DeviceRole,
    pub online: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreatePairingRequest {
    pub pairing_session_id: PairingSessionId,
    pub device_id: DeviceId,
    pub device_name: String,
    pub os: String,
    pub protocol_version: u16,
    pub ttl_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreatePairingResponse {
    pub room_id: RoomId,
    pub pairing_code: String,
    pub pairing_token: String,
    pub expires_at: DateTime<Utc>,
    pub ws_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinPairingRequest {
    pub pairing_code: String,
    pub device_id: DeviceId,
    pub device_name: String,
    pub os: String,
    pub protocol_version: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinPairingResponse {
    pub pairing_session_id: PairingSessionId,
    pub room_id: RoomId,
    pub pairing_token: String,
    pub peer: PeerInfo,
    pub expires_at: DateTime<Utc>,
    pub ws_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ControlMessage {
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn decode(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}
