use clipsync_clipboard::ClipboardError;
use clipsync_crypto::{AeadError, CryptoError};
use clipsync_daemon::DaemonError;
use clipsync_storage::StorageError;
use clipsync_transfer::TransferError;
use clipsync_transport::TransportError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Message(String),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("crypto: {0}")]
    Crypto(#[from] CryptoError),
    #[error("clipboard: {0}")]
    Clipboard(String),
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("transfer: {0}")]
    Transfer(#[from] TransferError),
    #[error("daemon: {0}")]
    Daemon(#[from] DaemonError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("not paired")]
    NotPaired,
    #[error("timeout")]
    Timeout,
}

impl From<ClipboardError> for CoreError {
    fn from(value: ClipboardError) -> Self {
        Self::Clipboard(value.to_string())
    }
}

impl From<AeadError> for CoreError {
    fn from(value: AeadError) -> Self {
        Self::Crypto(CryptoError::Aead(value))
    }
}

pub fn format_error(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut cur = err.source();
    while let Some(e) = cur {
        let s = e.to_string();
        if !parts.iter().any(|p| p.contains(&s)) {
            parts.push(s);
        }
        cur = e.source();
    }
    parts.join(": ")
}

impl CoreError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Message(_) => ExitCode::Error as i32,
            Self::Storage(StorageError::NotInitialized) => ExitCode::NotPaired as i32,
            Self::Storage(StorageError::NotPaired) | Self::NotPaired => ExitCode::NotPaired as i32,
            Self::Pairing(_) => ExitCode::PairingFailed as i32,
            Self::Crypto(_) => ExitCode::Crypto as i32,
            Self::Clipboard(_) => ExitCode::Clipboard as i32,
            Self::Transport(_) | Self::Http(_) => ExitCode::Network as i32,
            Self::Timeout => ExitCode::Timeout as i32,
            _ => ExitCode::Error as i32,
        }
    }
}

#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    Error = 1,
    Usage = 2,
    PairingFailed = 3,
    NotPaired = 4,
    Network = 5,
    Crypto = 6,
    Clipboard = 7,
    PeerOffline = 8,
    TooLarge = 9,
    Timeout = 10,
}

#[cfg(test)]
mod tests {
    use super::format_error;

    #[test]
    fn format_error_includes_source_chain() {
        let inner = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "tls eof");
        let outer = std::io::Error::new(std::io::ErrorKind::Other, inner);
        let msg = format_error(&outer);
        assert!(msg.contains("tls eof"), "{msg}");
    }
}
