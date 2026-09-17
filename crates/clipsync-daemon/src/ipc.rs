#[cfg(unix)]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use clipsync_protocol::{DeviceId, ItemKind, RoomId};
use clipsync_storage::AppPaths;

use crate::{DaemonError, Result};

pub const LISTEN_ENV: &str = "CLIPSYNC_LISTEN";

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

#[cfg(windows)]
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

#[cfg(unix)]
pub type IpcConn = UnixStream;
#[cfg(windows)]
pub type IpcConn = NamedPipeServer;

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

pub fn endpoint(paths: &AppPaths) -> String {
    if let Ok(custom) = std::env::var(LISTEN_ENV) {
        return custom;
    }
    #[cfg(windows)]
    {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        paths.data_dir.hash(&mut h);
        format!(r"\\.\pipe\clipsync-{:x}", h.finish())
    }
    #[cfg(not(windows))]
    {
        paths.socket_file().display().to_string()
    }
}

#[cfg(unix)]
pub struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
}

#[cfg(windows)]
pub struct IpcServer {
    name: String,
    next: tokio::sync::Mutex<NamedPipeServer>,
}

#[cfg(unix)]
impl IpcServer {
    pub async fn bind(paths: &AppPaths) -> Result<Self> {
        let path = PathBuf::from(endpoint(paths));
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&path)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self { listener, path })
    }

    pub async fn accept(&self) -> Result<IpcConn> {
        Ok(self.listener.accept().await?.0)
    }
}

#[cfg(unix)]
impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
impl IpcServer {
    pub async fn bind(paths: &AppPaths) -> Result<Self> {
        let name = endpoint(paths);
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        Ok(Self {
            name,
            next: tokio::sync::Mutex::new(server),
        })
    }

    pub async fn accept(&self) -> Result<IpcConn> {
        let mut guard = self.next.lock().await;
        guard.connect().await?;
        let connected = std::mem::replace(
            &mut *guard,
            ServerOptions::new()
                .reject_remote_clients(true)
                .create(&self.name)?,
        );
        Ok(connected)
    }
}

#[cfg(unix)]
pub struct IpcClient {
    stream: UnixStream,
}

#[cfg(windows)]
pub struct IpcClient {
    stream: NamedPipeClient,
}

#[cfg(unix)]
pub async fn connect_ipc(paths: &AppPaths) -> Result<IpcClient> {
    let stream = UnixStream::connect(endpoint(paths))
        .await
        .map_err(|e| DaemonError::Message(format!("daemon not running: {e}")))?;
    Ok(IpcClient { stream })
}

#[cfg(windows)]
pub async fn connect_ipc(paths: &AppPaths) -> Result<IpcClient> {
    let name = endpoint(paths);
    let stream = ClientOptions::new()
        .open(&name)
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

pub async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Option<IpcRequest>> {
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let req = serde_json::from_str(line.trim()).map_err(|e| DaemonError::Message(e.to_string()))?;
    Ok(Some(req))
}

pub async fn write_response<S: AsyncWrite + Unpin>(
    stream: &mut S,
    resp: &IpcResponse,
) -> Result<()> {
    let mut line = serde_json::to_string(resp).map_err(|e| DaemonError::Message(e.to_string()))?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    Ok(())
}
