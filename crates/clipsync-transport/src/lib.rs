// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! WebSocket client with reconnect, heartbeat, and typed frames.

mod relay_policy;
mod tls;

use std::time::{Duration, SystemTime};

use tokio::time::MissedTickBehavior;

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

pub use relay_policy::{
    check_relay_url, insecure_relay_opt_in, normalize_relay_url, relay_url_security, RelaySecurity,
    RelayUrlError, ALLOW_INSECURE_RELAY_ENV,
};
pub use tls::{ca_file_path, configure_reqwest, CA_FILE_ENV};
use tracing::{debug, warn};

use clipsync_protocol::{ControlMessage, CHUNK_MAGIC, HEARTBEAT_SECS};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("{0}")]
    RelayUrl(#[from] RelayUrlError),
    #[error("websocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("closed")]
    Closed,
}

pub type Result<T> = std::result::Result<T, TransportError>;

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "The 256-entry queue bounds inline control messages; avoid per-message allocation and preserve the public Incoming layout."
)]
pub enum Incoming {
    Control(ControlMessage),
    Binary(Vec<u8>),
    Pong,
}

pub struct RelayConnection {
    pub incoming: mpsc::Receiver<Incoming>,
    outgoing: mpsc::Sender<Outgoing>,
}

/// How long a closing connection keeps reading for the peer's close reply.
/// A responsive peer answers within a round trip; the bound keeps a peer that
/// stopped reading from holding the task.
const CLOSE_HANDSHAKE_WAIT: Duration = Duration::from_secs(2);

#[expect(
    clippy::large_enum_variant,
    reason = "The 256-entry queue bounds inline control messages; avoid per-message allocation and preserve the public Incoming layout."
)]
enum Outgoing {
    Control(ControlMessage),
    Binary(Vec<u8>),
    Close,
}

impl RelayConnection {
    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public TransportError payload and conversions; see docs/compiler-warnings.md."
    )]
    pub async fn connect(ws_url: &str) -> Result<Self> {
        // Re-check at connect time: configs written before this policy, or
        // `CLIPSYNC_RELAY_URL`, never went through `config set`.
        check_relay_url(ws_url)?;
        let mut req = ws_url
            .into_client_request()
            .map_err(|e| TransportError::Protocol(e.to_string()))?;
        relay_policy::require_canonical_host(ws_url, req.uri().host())?;
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
            beat.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut watchdog = tokio::time::interval(Duration::from_secs(1));
            watchdog.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut last_rx = SystemTime::now();
            let mut last_watchdog = SystemTime::now();
            loop {
                tokio::select! {
                    _ = watchdog.tick() => {
                        let now = SystemTime::now();
                        if should_force_reconnect(last_rx, last_watchdog, now) {
                            warn!("websocket stale or machine woke from sleep; reconnecting");
                            break;
                        }
                        last_watchdog = now;
                    }
                    _ = beat.tick() => {
                        let ping = ControlMessage::Ping { ts: chrono_now() };
                        if let Ok(s) = ping.encode() {
                            if sink.send(Message::Text(s.into())).await.is_err() {
                                break;
                            }
                        }
                        if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                            break;
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
                                // Read until the peer answers the close. A socket
                                // closed with unread input is reset, and a reset can
                                // discard frames sent just before, such as the
                                // `error` that aborts a pairing on the relay.
                                let _ = tokio::time::timeout(CLOSE_HANDSHAKE_WAIT, async {
                                    while let Some(Ok(message)) = stream.next().await {
                                        if matches!(message, Message::Close(_)) {
                                            break;
                                        }
                                    }
                                })
                                .await;
                                break;
                            }
                        }
                    }
                    next = stream.next() => {
                        match next {
                            Some(Ok(Message::Text(t))) => {
                                last_rx = SystemTime::now();
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
                                last_rx = SystemTime::now();
                                if (b.starts_with(CHUNK_MAGIC) || !b.is_empty())
                                    && in_tx.send(Incoming::Binary(b.to_vec())).await.is_err()
                                {
                                    break;
                                }
                            }
                            Some(Ok(Message::Ping(p))) => {
                                last_rx = SystemTime::now();
                                let _ = sink.send(Message::Pong(p)).await;
                            }
                            Some(Ok(Message::Pong(_))) => {
                                last_rx = SystemTime::now();
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

    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public TransportError payload and conversions; see docs/compiler-warnings.md."
    )]
    pub async fn send_control(&self, msg: ControlMessage) -> Result<()> {
        self.outgoing
            .send(Outgoing::Control(msg))
            .await
            .map_err(|_| TransportError::Closed)
    }

    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public TransportError payload and conversions; see docs/compiler-warnings.md."
    )]
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

/// Map an HTTP(S) relay base to its WebSocket scheme. Input without a known
/// scheme is returned unchanged rather than guessed as plaintext `ws://`, so
/// [`check_relay_url`] rejects it at connect time.
pub fn http_to_ws(http_url: &str) -> String {
    if let Some(rest) = http_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        http_url.to_string()
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

/// After sleep/wake the wall clock jumps; treat that as a dead socket.
/// Also drop sockets that have not received anything for two heartbeats.
pub fn should_force_reconnect(
    last_rx: SystemTime,
    last_watchdog: SystemTime,
    now: SystemTime,
) -> bool {
    // A 2s gap also happens when the runtime is busy. Real sleep jumps are much larger.
    const WAKE_JUMP: Duration = Duration::from_secs(8);
    let stale = Duration::from_secs(HEARTBEAT_SECS.saturating_mul(2).saturating_add(5));
    if now.duration_since(last_watchdog).unwrap_or_default() > WAKE_JUMP {
        return true;
    }
    now.duration_since(last_rx).unwrap_or_default() > stale
}

/// A session that stayed up resets the failure streak so one blip does not
/// climb to the 15s cap.
pub fn next_attempt(attempt: u32, session_uptime: Duration) -> u32 {
    if session_uptime >= Duration::from_secs(30) {
        1
    } else {
        attempt.saturating_add(1).min(8)
    }
}

pub fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = match attempt {
        0 | 1 => 150,
        2 => 400,
        3 => 800,
        4 => 1_500,
        n => (1u64 << n.min(5)).min(15) * 1000,
    };
    let jitter = rand::random::<u64>() % 250;
    Duration::from_millis(base_ms + jitter)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn clock_jump_after_sleep_forces_reconnect() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let later = t0 + Duration::from_secs(30);
        assert!(should_force_reconnect(t0, t0, later));
    }

    #[test]
    fn fresh_traffic_does_not_reconnect() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let t1 = t0 + Duration::from_millis(900);
        assert!(!should_force_reconnect(t0, t0, t1));
    }

    #[test]
    fn stale_socket_forces_reconnect() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let last_wd = t0 + Duration::from_secs(50);
        let now = last_wd + Duration::from_millis(500);
        assert!(should_force_reconnect(t0, last_wd, now));
    }

    #[test]
    fn first_backoff_is_subsecond() {
        let d = backoff_delay(1);
        assert!(d < Duration::from_secs(1));
    }

    #[test]
    fn stability_short_stall_is_not_sleep() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let now = t0 + Duration::from_secs(3);
        assert!(!should_force_reconnect(t0, t0, now));
    }

    #[test]
    fn stability_long_session_resets_backoff() {
        assert_eq!(next_attempt(6, Duration::from_secs(120)), 1);
        assert_eq!(next_attempt(2, Duration::from_millis(200)), 3);
    }

    #[test]
    fn stability_backoff_stays_bounded() {
        let mut attempt = 0u32;
        let mut max = Duration::ZERO;
        for _ in 0..500 {
            attempt = next_attempt(attempt, Duration::from_millis(10));
            max = max.max(backoff_delay(attempt));
        }
        assert!(max <= Duration::from_secs(16), "{max:?}");
    }

    #[tokio::test]
    async fn malformed_relay_frames_do_not_hide_later_binary_or_control_messages() {
        tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (consumed_tx, consumed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            socket
                .send(Message::Text("malformed JSON".into()))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    ControlMessage::Ping { ts: 77 }.encode().unwrap().into(),
                ))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let frame = socket.next().await.unwrap().unwrap();
                    if let Message::Text(text) = frame {
                        if matches!(
                            ControlMessage::decode(&text),
                            Ok(ControlMessage::Pong { ts: 77 })
                        ) {
                            break;
                        }
                    }
                }
            })
            .await
            .unwrap();
            socket
                .send(Message::Binary(Vec::new().into()))
                .await
                .unwrap();
            socket
                .send(Message::Binary(vec![1, 2, 3].into()))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    ControlMessage::PeerLeft {
                        device_id: "fixture".into(),
                    }
                    .encode()
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            // Keep the peer alive until the consumer has observed both frames.
            // The connection emits an immediate heartbeat, so closing earlier
            // can legitimately reset TCP before its receive task drains them.
            consumed_rx.await.unwrap();
            socket.send(Message::Close(None)).await.unwrap();
        });
        let mut connection = RelayConnection::connect(&format!("ws://{address}/v1/ws"))
            .await
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), connection.incoming.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(first, Incoming::Binary(bytes) if bytes == [1,2,3]));
        let second = tokio::time::timeout(Duration::from_secs(5), connection.incoming.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(second, Incoming::Control(ControlMessage::PeerLeft { device_id }) if device_id.as_str() == "fixture")
        );
        consumed_tx.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), connection.incoming.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            connection
                .send_control(ControlMessage::Ping { ts: 88 })
                .await,
            Err(TransportError::Closed)
        ));
        assert!(matches!(
            connection.send_binary(vec![4]).await,
            Err(TransportError::Closed)
        ));
        connection.close().await;
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();

        }).await.expect("loopback protocol fixture exceeded its deadline");
    }

    #[tokio::test]
    async fn invalid_urls_and_invalid_websocket_protocol_close_with_useful_errors() {
        tokio::time::timeout(Duration::from_secs(10), async {
            // Scheme-less input gets the relay policy's actionable error.
            assert!(matches!(
                RelayConnection::connect("not a URL").await,
                Err(TransportError::RelayUrl(RelayUrlError::MissingScheme))
            ));
            assert!(RelayConnection::connect("https://127.0.0.1/")
                .await
                .is_err());
            // URL normalization accepts a trailing newline; HTTP request
            // construction must reject the original control character.
            assert!(matches!(
                RelayConnection::connect("ws://127.0.0.1:0/\n").await,
                Err(TransportError::Protocol(_))
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                // A reserved opcode is invalid on the wire and must terminate the
                // transport rather than delivering an invented control message.
                socket.get_mut().write_all(&[0x83, 0x00]).await.unwrap();
            });
            let mut connection = RelayConnection::connect(&format!("ws://{address}/v1/ws"))
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(5), connection.incoming.recv())
                    .await
                    .unwrap()
                    .is_none()
            );
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
        })
        .await
        .expect("loopback protocol fixture exceeded its deadline");
    }

    #[test]
    fn backward_clock_and_exact_deadlines_do_not_disconnect_healthy_sessions() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        // Windows SystemTime stores 100ns ticks, so a 1ns addition rounds away.
        // Keep the exact boundary and exercise a representable instant after it.
        let after_boundary = Duration::from_micros(1);
        assert!(!should_force_reconnect(
            now,
            now,
            now - Duration::from_secs(1)
        ));
        assert!(!should_force_reconnect(
            now,
            now,
            now + Duration::from_secs(8)
        ));
        assert!(should_force_reconnect(
            now,
            now,
            now + Duration::from_secs(8) + after_boundary
        ));
        let stale = Duration::from_secs(HEARTBEAT_SECS * 2 + 5);
        let watchdog = now + stale;
        assert!(!should_force_reconnect(now, watchdog, watchdog));
        assert!(should_force_reconnect(
            now,
            watchdog,
            watchdog + after_boundary
        ));
        assert_eq!(next_attempt(u32::MAX, Duration::from_secs(29)), 8);
        assert_eq!(next_attempt(u32::MAX, Duration::from_secs(30)), 1);
    }

    #[tokio::test]
    async fn receiver_shutdown_closes_the_session_for_binary_and_control_frames() {
        tokio::time::timeout(Duration::from_secs(10), async {
            for binary in [false, true] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let (ready, received) = tokio::sync::oneshot::channel();
                let server = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                    received.await.unwrap();
                    let message = if binary {
                        Message::Binary(vec![1, 2, 3].into())
                    } else {
                        Message::Text(
                            ControlMessage::PeerLeft {
                                device_id: "synthetic-peer".into(),
                            }
                            .encode()
                            .unwrap()
                            .into(),
                        )
                    };
                    socket.send(message).await.unwrap();
                    while let Some(Ok(message)) = socket.next().await {
                        if matches!(message, Message::Close(_)) {
                            break;
                        }
                    }
                });
                let mut connection = RelayConnection::connect(&format!("ws://{address}/v1/ws"))
                    .await
                    .unwrap();
                connection.incoming.close();
                ready.send(()).unwrap();
                connection.outgoing.closed().await;
                assert!(matches!(
                    connection
                        .send_control(ControlMessage::Ping { ts: 12 })
                        .await,
                    Err(TransportError::Closed)
                ));
                assert!(matches!(
                    connection.send_binary(vec![4]).await,
                    Err(TransportError::Closed)
                ));
                assert!(connection.incoming.recv().await.is_none());
                server.await.unwrap();
            }
        })
        .await
        .expect("receiver shutdown must stop the websocket task");
    }

    #[tokio::test]
    async fn ping_pong_outgoing_frames_and_client_close_preserve_payloads() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                socket.send(Message::Ping(vec![71].into())).await.unwrap();
                socket.send(Message::Pong(vec![72].into())).await.unwrap();
                socket.send(Message::Text(ControlMessage::Pong { ts: 73 }.encode().unwrap().into())).await.unwrap();
                let mut control = false;
                let mut binary = false;
                let mut pong = false;
                let mut acknowledged = false;
                while let Some(Ok(message)) = socket.next().await {
                    match message {
                        Message::Text(text) if matches!(ControlMessage::decode(&text), Ok(ControlMessage::Ping { ts: 74 })) => control = true,
                        Message::Binary(bytes) => { assert_eq!(&bytes[..], &[75, 76]); binary = true; },
                        Message::Pong(bytes) if bytes.as_ref() == [71] => pong = true,
                        Message::Close(_) => break,
                        _ => {}
                    }
                    if control && binary && pong && !acknowledged {
                        acknowledged = true;
                        socket.send(Message::Text(ControlMessage::PeerLeft { device_id: "all-outgoing-seen".into() }.encode().unwrap().into())).await.unwrap();
                    }
                }
                assert!(control && binary && pong);
            });
            let mut connection = RelayConnection::connect(&format!("ws://{address}/v1/ws")).await.unwrap();
            connection.send_control(ControlMessage::Ping { ts: 74 }).await.unwrap();
            connection.send_binary(vec![75, 76]).await.unwrap();
            assert!(matches!(connection.incoming.recv().await, Some(Incoming::Pong)));
            assert!(matches!(connection.incoming.recv().await, Some(Incoming::Control(ControlMessage::PeerLeft { device_id })) if device_id.as_str() == "all-outgoing-seen"));
            connection.close().await;
            assert!(connection.incoming.recv().await.is_none());
            server.await.unwrap();
        }).await.expect("bidirectional payload/close fixture exceeded its deadline");
    }

    #[tokio::test]
    async fn dropping_the_frontend_sends_close_and_releases_the_websocket() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                socket
                    .send(Message::Text(
                        ControlMessage::Pong { ts: 1 }.encode().unwrap().into(),
                    ))
                    .await
                    .unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    if matches!(message, Message::Close(_)) {
                        return;
                    }
                }
                panic!("dropping all frontend channels must send a Close frame");
            });
            let mut connection = RelayConnection::connect(&format!("ws://{address}/v1/ws"))
                .await
                .unwrap();
            assert!(matches!(
                connection.incoming.recv().await,
                Some(Incoming::Pong)
            ));
            drop(connection);
            server.await.unwrap();
        })
        .await
        .expect("dropped frontend must release its websocket");
    }

    #[tokio::test]
    async fn suspended_runtime_forces_a_live_session_to_reconnect() {
        tokio::time::timeout(Duration::from_secs(25), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                socket
                    .send(Message::Text(
                        ControlMessage::Pong { ts: 2 }.encode().unwrap().into(),
                    ))
                    .await
                    .unwrap();
                while let Some(Ok(_)) = socket.next().await {}
            });
            let mut connection = RelayConnection::connect(&format!("ws://{address}/v1/ws"))
                .await
                .unwrap();
            assert!(matches!(
                connection.incoming.recv().await,
                Some(Incoming::Pong)
            ));
            // Suspend this private current-thread Tokio runtime, not the host
            // clock. A real nine-second wall-clock gap exercises the watchdog's
            // wake recovery rather than merely testing its time predicate.
            std::thread::sleep(Duration::from_secs(9));
            assert!(connection.incoming.recv().await.is_none());
            assert!(matches!(
                connection.send_binary(vec![1]).await,
                Err(TransportError::Closed)
            ));
            server.await.unwrap();
        })
        .await
        .expect("a suspended session must reconnect after its watchdog resumes");
    }
}
