// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! Zero-knowledge ClipSync relay: pairing rendezvous and encrypted frame routing.

mod auth;
mod state;
mod transport;
mod webhook;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use clipsync_protocol::{
    CreatePairingRequest, CreatePairingResponse, ErrorBody, JoinPairingRequest,
    JoinPairingResponse, FRAME_HARD_CAP,
};

pub use state::{RelayState, MAX_MEMBER_SOCKETS};
pub use transport::RelayTransportConfig;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub bind: SocketAddr,
    pub public_url: String,
    pub token_secret: Vec<u8>,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
}

impl RelayConfig {
    pub fn from_env() -> Self {
        let bind: SocketAddr = std::env::var("CLIPSYNC_RELAY_BIND")
            .unwrap_or_else(|_| "0.0.0.0:7600".into())
            .parse()
            .expect("CLIPSYNC_RELAY_BIND");
        let public_url = std::env::var("CLIPSYNC_RELAY_PUBLIC_URL")
            .unwrap_or_else(|_| format!("https://127.0.0.1:{}", bind.port()));
        let token_secret = token_secret_from_env().expect("relay token key configuration");
        Self {
            bind,
            public_url,
            token_secret,
            webhook_url: std::env::var("CLIPSYNC_RELAY_WEBHOOK_URL").ok(),
            webhook_secret: std::env::var("CLIPSYNC_RELAY_WEBHOOK_SECRET").ok(),
        }
    }
}

fn token_secret_from_env() -> anyhow::Result<Vec<u8>> {
    let value = std::env::var_os("CLIPSYNC_RELAY_TOKEN_SECRET");
    let file = std::env::var_os("CLIPSYNC_RELAY_TOKEN_SECRET_FILE");
    if value.is_some() && file.is_some() {
        anyhow::bail!("configure only one of CLIPSYNC_RELAY_TOKEN_SECRET and CLIPSYNC_RELAY_TOKEN_SECRET_FILE");
    }
    let secret = if let Some(file) = file {
        Some(
            transport::read_config_file(std::path::Path::new(&file), true)
                .map_err(|error| anyhow::anyhow!("CLIPSYNC_RELAY_TOKEN_SECRET_FILE: {error:#}"))?,
        )
    } else if let Some(value) = value {
        let value = value
            .into_string()
            .map_err(|_| anyhow::anyhow!("CLIPSYNC_RELAY_TOKEN_SECRET must be UTF-8"))?;
        if value.trim().is_empty() {
            None
        } else {
            Some(value.into_bytes())
        }
    } else {
        None
    };
    if let Some(secret) = secret {
        if secret.len() < 32 {
            anyhow::bail!("CLIPSYNC_RELAY_TOKEN_SECRET or CLIPSYNC_RELAY_TOKEN_SECRET_FILE must contain at least 32 bytes; leave them unset to generate a random key");
        }
        Ok(secret)
    } else {
        let mut secret = vec![0u8; 32];
        rand::TryRngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut secret)
            .map_err(|error| anyhow::anyhow!("operating system randomness unavailable: {error}"))?;
        Ok(secret)
    }
}

/// Serve with environment-selected TLS, failing before binding unless TLS is
/// configured or plaintext HTTP is explicitly enabled.
pub async fn serve(config: RelayConfig) -> anyhow::Result<()> {
    serve_with_transport(config, RelayTransportConfig::from_env()?).await
}

pub async fn serve_with_transport(
    config: RelayConfig,
    transport: RelayTransportConfig,
) -> anyhow::Result<()> {
    let tls = transport.tls_config(&config.public_url)?;
    let listener = std::net::TcpListener::bind(config.bind)?;
    let addr = listener.local_addr()?;
    let app = router(config).into_make_service_with_connect_info::<SocketAddr>();
    let server = axum_server::from_tcp(listener)?.first_request_timeout(Duration::from_secs(10));
    if let Some(tls) = tls {
        info!(%addr, "clipsync-relay listening with TLS 1.2/1.3");
        server
            .acceptor(transport::LimitedTlsAcceptor::new(tls))
            .serve(app)
            .await?;
    } else {
        info!(%addr, "clipsync-relay listening with explicitly enabled plaintext HTTP");
        server.serve(app).await?;
    }
    Ok(())
}

pub fn router(config: RelayConfig) -> Router {
    let state = Arc::new(RelayState::new(config));
    let janitor_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            janitor_state.gc();
        }
    });
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route("/v1/pairing/create", post(create_pairing))
        .route("/v1/pairing/join", post(join_pairing))
        .route("/v1/ws", get(ws::ws_handler))
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(FRAME_HARD_CAP))
        .with_state(state)
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "service": "clipsync-relay"}))
}

async fn metrics(State(state): State<Arc<RelayState>>) -> String {
    state.metrics_text()
}

async fn create_pairing(
    State(state): State<Arc<RelayState>>,
    Json(req): Json<CreatePairingRequest>,
) -> Result<Json<CreatePairingResponse>, (StatusCode, Json<ErrorBody>)> {
    if req.protocol_version != clipsync_protocol::PROTOCOL_VERSION {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "protocol_version",
            "unsupported protocol version",
        ));
    }
    let created = state
        .create_pairing(req)
        .map_err(|e| err(StatusCode::BAD_REQUEST, "pairing_create", &e))?;
    Ok(Json(created))
}

async fn join_pairing(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    State(state): State<Arc<RelayState>>,
    Json(req): Json<JoinPairingRequest>,
) -> Result<Json<JoinPairingResponse>, (StatusCode, Json<ErrorBody>)> {
    if req.protocol_version != clipsync_protocol::PROTOCOL_VERSION {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "protocol_version",
            "unsupported protocol version",
        ));
    }
    match state.join_pairing(req, addr.ip()) {
        Ok(resp) => Ok(Json(resp)),
        Err(JoinError::NotFound) => Err(err(
            StatusCode::NOT_FOUND,
            "code_invalid",
            "unknown pairing code",
        )),
        Err(JoinError::Expired) => Err(err(
            StatusCode::GONE,
            "code_expired",
            "pairing session expired",
        )),
        Err(JoinError::Attempts) => Err(err(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "pairing attempt limit reached",
        )),
        Err(JoinError::Busy) => Err(err(
            StatusCode::CONFLICT,
            "busy",
            "pairing session already has a joiner",
        )),
        Err(JoinError::Other(m)) => Err(err(StatusCode::BAD_REQUEST, "join_failed", &m)),
    }
}

fn err(status: StatusCode, code: &str, message: &str) -> (StatusCode, Json<ErrorBody>) {
    (
        status,
        Json(ErrorBody {
            code: code.into(),
            message: message.into(),
        }),
    )
}

#[derive(Debug)]
pub enum JoinError {
    NotFound,
    Expired,
    Attempts,
    Busy,
    Other(String),
}

pub use auth::{issue_token, verify_token, TokenClaims};
pub use ws::WsOut;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "http_tests.rs"]
mod http_tests;
