use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use clipsync_protocol::{DeviceId, ItemKind, RoomId};
use clipsync_storage::AppPaths;

use crate::{DaemonError, Result};

pub const LISTEN_ENV: &str = "CLIPSYNC_LISTEN";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Status,
    Pause,
    Resume,
    Shutdown,
    Pull,
    PushText { text: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull: Option<PullPayload>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusPayload {
    pub daemon: bool,
    pub paused: bool,
    pub connected: bool,
    pub peer_online: bool,
    pub room_id: Option<RoomId>,
    pub device_id: Option<DeviceId>,
    pub device_name: Option<String>,
    pub relay_url: Option<String>,
    pub last_item_kind: Option<ItemKind>,
    pub last_sync_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullPayload {
    pub kind: String,
    pub text: Option<String>,
    pub summary: String,
}

pub struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
}

impl IpcServer {
    pub async fn bind(paths: &AppPaths) -> Result<Self> {
        let path = paths.socket_file();
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self { listener, path })
    }

    pub async fn accept(&self) -> Result<UnixStream> {
        Ok(self.listener.accept().await?.0)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct IpcClient {
    stream: UnixStream,
}

pub async fn connect_ipc(paths: &AppPaths) -> Result<IpcClient> {
    let stream = UnixStream::connect(paths.socket_file())
        .await
        .map_err(|e| DaemonError::Message(format!("daemon not running: {e}")))?;
    Ok(IpcClient { stream })
}

impl IpcClient {
    pub async fn request(&mut self, req: &IpcRequest) -> Result<IpcResponse> {
        let mut line =
            serde_json::to_string(req).map_err(|e| DaemonError::Message(e.to_string()))?;
        line.push('\n');
        self.stream.write_all(line.as_bytes()).await?;
        let mut reader = BufReader::new(&mut self.stream);
        let mut resp = String::new();
        reader.read_line(&mut resp).await?;
        serde_json::from_str(resp.trim()).map_err(|e| DaemonError::Message(e.to_string()))
    }
}

pub async fn read_request(stream: &mut UnixStream) -> Result<Option<IpcRequest>> {
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let req = serde_json::from_str(line.trim()).map_err(|e| DaemonError::Message(e.to_string()))?;
    Ok(Some(req))
}

pub async fn write_response(stream: &mut UnixStream, resp: &IpcResponse) -> Result<()> {
    let mut line = serde_json::to_string(resp).map_err(|e| DaemonError::Message(e.to_string()))?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    Ok(())
}
