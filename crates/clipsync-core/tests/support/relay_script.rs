// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! A bounded loopback relay for hostile and interrupted protocol exchanges.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::{routing::get, Router};
use clipsync_protocol::ControlMessage;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Server {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
    finished: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub async fn http(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            url,
            task,
            finished: None,
        }
    }

    pub async fn websocket<F, Fut>(script: F) -> Self
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::websocket_routes(Router::new(), script).await
    }

    pub async fn websocket_routes<F, Fut>(router: Router, script: F) -> Self
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::websocket_with_deadline(router, Duration::from_secs(8), script).await
    }

    pub async fn websocket_with_deadline<F, Fut>(
        router: Router,
        deadline: Duration,
        script: F,
    ) -> Self
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (finished, completion) = tokio::sync::oneshot::channel();
        let script = Arc::new(Mutex::new(Some((script, finished))));
        let mut server = Self::http(router.route(
            "/v1/ws",
            get(move |upgrade: WebSocketUpgrade| {
                let (script, finished) = script.lock().unwrap().take().unwrap();
                async move {
                    upgrade.on_upgrade(move |socket| async move {
                        tokio::time::timeout(deadline, script(socket))
                            .await
                            .expect("scripted relay timed out");
                        let _ = finished.send(());
                    })
                }
            }),
        ))
        .await;
        server.finished = Some(completion);
        server
    }

    pub async fn finish(&mut self) {
        if let Some(finished) = self.finished.take() {
            tokio::time::timeout(Duration::from_secs(5), finished)
                .await
                .expect("relay script failed to finish")
                .expect("relay script panicked");
        }
    }
}

pub async fn receive(socket: &mut WebSocket) -> ControlMessage {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.recv())
            .await
            .expect("client did not send expected control frame")
            .expect("client closed before expected control frame")
            .unwrap();
        if let Message::Text(text) = frame {
            let message = ControlMessage::decode(&text).unwrap();
            if let ControlMessage::Ping { ts } = message {
                send(socket, ControlMessage::Pong { ts }).await;
                continue;
            }
            return message;
        }
    }
}

/// End a script with a complete WebSocket closing handshake.
///
/// Dropping the socket while client frames are still unread (the client's
/// connect-time heartbeat, for example) makes the kernel abort the connection
/// with a TCP reset, and a reset can discard frames the client has not read
/// yet. Draining until the client disconnects (or answers the close frame)
/// guarantees that every frame sent before this call reaches the client
/// before it observes the disconnect. The surrounding script deadline bounds
/// the wait.
pub async fn close(mut socket: WebSocket) {
    if socket.send(Message::Close(None)).await.is_err() {
        // The client already disconnected, so no frame is left to drain.
        return;
    }
    while let Some(Ok(frame)) = socket.recv().await {
        if matches!(frame, Message::Close(_)) {
            break;
        }
    }
}

pub async fn send(socket: &mut WebSocket, message: ControlMessage) {
    socket
        .send(Message::Text(message.encode().unwrap().into()))
        .await
        .unwrap();
}
