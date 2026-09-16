//! Zero-knowledge ClipSync relay: pairing rendezvous and encrypted frame routing.

mod auth;
mod state;
mod webhook;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use clipsync_protocol::{
    CreatePairingRequest, CreatePairingResponse, ErrorBody, JoinPairingRequest,
    JoinPairingResponse, FRAME_HARD_CAP,
};

pub use state::RelayState;

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
            .unwrap_or_else(|_| format!("http://127.0.0.1:{}", bind.port()));
        let token_secret = std::env::var("CLIPSYNC_RELAY_TOKEN_SECRET")
            .map(|s| s.into_bytes())
            .unwrap_or_else(|_| {
                let mut s = [0u8; 32];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut s);
                s.to_vec()
            });
        Self {
            bind,
            public_url,
            token_secret,
            webhook_url: std::env::var("CLIPSYNC_RELAY_WEBHOOK_URL").ok(),
            webhook_secret: std::env::var("CLIPSYNC_RELAY_WEBHOOK_SECRET").ok(),
        }
    }
}

pub async fn serve(config: RelayConfig) -> anyhow::Result<()> {
    let addr = config.bind;
    let app = router(config);
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "clipsync-relay listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
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
