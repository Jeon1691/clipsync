//! WebSocket client with reconnect, heartbeat, and typed frames.

mod tls;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::USER_AGENT, HeaderValue},
        Message,
    },
    Connector,
};

pub use tls::{ca_file_path, configure_reqwest, CA_FILE_ENV};
use tracing::{debug, warn};
use url::Url;

use clipsync_protocol::{ControlMessage, CHUNK_MAGIC, HEARTBEAT_SECS};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("websocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("closed")]
    Closed,
}

pub type Result<T> = std::result::Result<T, TransportError>;

#[derive(Debug)]
pub enum Incoming {
    Control(ControlMessage),
    Binary(Vec<u8>),
    Pong,
}

pub struct RelayConnection {
    pub incoming: mpsc::Receiver<Incoming>,
    outgoing: mpsc::Sender<Outgoing>,
}

enum Outgoing {
    Control(ControlMessage),
    Binary(Vec<u8>),
    Close,
}

impl RelayConnection {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let _ = Url::parse(ws_url)?;
        let mut req = ws_url
            .into_client_request()
            .map_err(|e| TransportError::Protocol(e.to_string()))?;
        req.headers_mut().insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("clipsync-cli/", env!("CARGO_PKG_VERSION"))),
        );
        let connector = Connector::Rustls(tls::client_config()?);
        let (stream, _) = connect_async_tls_with_config(req, None, false, Some(connector)).await?;
        let (mut sink, mut stream) = stream.split();
        let (in_tx, incoming) = mpsc::channel(256);
        let (out_tx, mut out_rx) = mpsc::channel::<Outgoing>(256);

        tokio::spawn(async move {
            let mut beat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
            loop {
                tokio::select! {
                    _ = beat.tick() => {
                        let ping = ControlMessage::Ping { ts: chrono_now() };
                        if let Ok(s) = ping.encode() {
                            if sink.send(Message::Text(s.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    msg = out_rx.recv() => {
                        match msg {
                            Some(Outgoing::Control(c)) => {
                                match c.encode() {
                                    Ok(s) => {
                                        if sink.send(Message::Text(s.into())).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => warn!(error = %e, "encode control"),
                                }
                            }
                            Some(Outgoing::Binary(b)) => {
                                if sink.send(Message::Binary(b.into())).await.is_err() {
                                    break;
                                }
                            }
                            Some(Outgoing::Close) | None => {
                                let _ = sink.send(Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                    next = stream.next() => {
                        match next {
                            Some(Ok(Message::Text(t))) => {
                                match ControlMessage::decode(&t) {
                                    Ok(ControlMessage::Pong { .. }) => {
                                        let _ = in_tx.send(Incoming::Pong).await;
                                    }
                                    Ok(ControlMessage::Ping { ts }) => {
                                        let pong = ControlMessage::Pong { ts };
                                        if let Ok(s) = pong.encode() {
                                            let _ = sink.send(Message::Text(s.into())).await;
                                        }
                                    }
                                    Ok(c) => {
                                        if in_tx.send(Incoming::Control(c)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => warn!(error = %e, "bad control json"),
                                }
                            }
                            Some(Ok(Message::Binary(b))) => {
                                if b.starts_with(CHUNK_MAGIC) || !b.is_empty() {
                                    if in_tx.send(Incoming::Binary(b.to_vec())).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(p))) => {
                                let _ = sink.send(Message::Pong(p)).await;
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(e)) => {
                                debug!(error = %e, "ws stream error");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            incoming,
            outgoing: out_tx,
        })
    }

    pub async fn send_control(&self, msg: ControlMessage) -> Result<()> {
        self.outgoing
            .send(Outgoing::Control(msg))
            .await
            .map_err(|_| TransportError::Closed)
    }

    pub async fn send_binary(&self, bytes: Vec<u8>) -> Result<()> {
        self.outgoing
            .send(Outgoing::Binary(bytes))
            .await
            .map_err(|_| TransportError::Closed)
    }

    pub async fn close(&self) {
        let _ = self.outgoing.send(Outgoing::Close).await;
    }
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn http_to_ws(http_url: &str) -> String {
    if let Some(rest) = http_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if http_url.starts_with("ws://") || http_url.starts_with("wss://") {
        http_url.to_string()
    } else {
        format!("ws://{http_url}")
    }
}

pub fn join_ws(base: &str, path: &str) -> String {
    let base = http_to_ws(base).trim_end_matches('/').to_string();
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

pub fn backoff_delay(attempt: u32) -> Duration {
    let exp = (1u64 << attempt.min(6)).min(30);
    let jitter = rand::random::<u64>() % 400;
    Duration::from_millis(exp * 1000 + jitter)
}
