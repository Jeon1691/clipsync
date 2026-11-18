// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! Zero-knowledge ClipSync relay: pairing rendezvous and encrypted frame routing.

mod auth;
mod client_ip;
mod connection_limits;
mod join_limits;
mod metrics;
mod rate_limit;
mod resource_limits;
mod state;
mod transport;
mod webhook;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use clipsync_protocol::{
    CreatePairingRequest, CreatePairingResponse, ErrorBody, JoinPairingRequest,
    JoinPairingResponse, FRAME_HARD_CAP,
};

pub use join_limits::PairingJoinLimits;
#[cfg(test)]
use join_limits::IP_FAILED_JOIN_LIMIT;
pub use metrics::{MetricsToken, MIN_METRICS_TOKEN_BYTES};
pub use resource_limits::RelayResourceLimits;
pub use state::{RelayState, MAX_MEMBER_SOCKETS};
pub use transport::RelayTransportConfig;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub bind: SocketAddr,
    pub public_url: String,
    pub token_secret: Vec<u8>,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
    /// Bearer token for `GET /metrics`; `None` leaves the route unregistered.
    pub metrics_token: Option<MetricsToken>,
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
            metrics_token: metrics::metrics_token_from_env()
                .expect("relay metrics token configuration"),
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
    transport.join_limits.validate()?;
    transport.resource_limits.validate()?;
    let listener = std::net::TcpListener::bind(config.bind)?;
    let addr = listener.local_addr()?;
    if !transport.trusted_proxies.is_empty() {
        let trusted = client_ip::canonical_trusted_proxies(&transport.trusted_proxies);
        info!(
            ranges = trusted.len(),
            "honoring X-Forwarded-For from trusted proxies for pairing limits"
        );
        for net in client_ip::trusts_every_address(&trusted) {
            warn!(
                %net,
                "trusted proxy range covers every address: any client can choose \
                 the address its pairing-join limits are charged to"
            );
        }
    }
    if transport.join_limits != PairingJoinLimits::default() {
        info!(limits = ?transport.join_limits, "using configured pairing-join limits");
    }
    if transport.resource_limits != RelayResourceLimits::default() {
        info!(limits = ?transport.resource_limits, "using configured resource limits");
    }
    let app = router_with_limits(
        config,
        transport.trusted_proxies.clone(),
        transport.join_limits,
        transport.resource_limits,
    )?
    .into_make_service_with_connect_info::<SocketAddr>();
    let server = axum_server::from_tcp(listener)?.first_request_timeout(Duration::from_secs(10));
    // Both listening diagnostics are emitted only after the bind succeeded and
    // carry the bound `addr`, which reports the OS-chosen port for `--bind
    // HOST:0`. The integration-test harness (tests/support) reads that message
    // and field to find the relay it spawned; keep them stable.
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
    router_with_state(Arc::new(RelayState::new(config)))
}

/// Like [`router`], also honoring `X-Forwarded-For` from `trusted_proxies`
/// when identifying clients for pairing limits, and applying `join_limits`.
/// Fails if `join_limits` does not pass [`PairingJoinLimits::validate`].
pub fn router_with_join_policy(
    config: RelayConfig,
    trusted_proxies: Vec<ipnet::IpNet>,
    join_limits: PairingJoinLimits,
) -> anyhow::Result<Router> {
    router_with_limits(
        config,
        trusted_proxies,
        join_limits,
        RelayResourceLimits::default(),
    )
}

/// Like [`router_with_join_policy`], also applying `resource_limits` to
/// pairing creation, held state and WebSocket connections. Fails if either set
/// of limits does not validate.
pub fn router_with_limits(
    config: RelayConfig,
    trusted_proxies: Vec<ipnet::IpNet>,
    join_limits: PairingJoinLimits,
    resource_limits: RelayResourceLimits,
) -> anyhow::Result<Router> {
    Ok(router_with_state(Arc::new(RelayState::with_limits(
        config,
        trusted_proxies,
        join_limits,
        resource_limits,
    )?)))
}

fn router_with_state(state: Arc<RelayState>) -> Router {
    let janitor_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            janitor_state.gc();
        }
    });
    let mut app = Router::new().route("/healthz", get(healthz));
    if state.config.metrics_token.is_some() {
        app = app.route("/metrics", get(metrics::metrics));
    }
    app.route("/v1/pairing/create", post(create_pairing))
        .route("/v1/pairing/join", post(join_pairing))
        .route("/v1/ws", get(ws::ws_handler))
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(FRAME_HARD_CAP))
        .with_state(state)
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "service": "clipsync-relay"}))
}

/// Creations that fail to decode ([`RequestJson`]), name another protocol
/// version or are otherwise invalid create nothing and are not charged to the
/// creation budget; [`RelayState::create_pairing`] charges accepted ones.
async fn create_pairing(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    State(state): State<Arc<RelayState>>,
    headers: HeaderMap,
    RequestJson(req): RequestJson<CreatePairingRequest>,
) -> Result<Json<CreatePairingResponse>, (StatusCode, Json<ErrorBody>)> {
    if req.protocol_version != clipsync_protocol::PROTOCOL_VERSION {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "protocol_version",
            "unsupported protocol version",
        ));
    }
    let client = state.client_ip(addr.ip(), &headers);
    match state.create_pairing(req, client) {
        Ok(created) => Ok(Json(created)),
        Err(CreateError::Throttled) => Err(err(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_pairings",
            "pairing creation limit reached; wait a few minutes and try again",
        )),
        Err(CreateError::Full) => Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_full",
            "the relay cannot hold more pairing sessions or rooms right now; try again later",
        )),
        Err(CreateError::Invalid(m)) => Err(err(StatusCode::BAD_REQUEST, "pairing_create", &m)),
    }
}

/// Joins that fail to decode ([`RequestJson`]) or name another protocol
/// version are refused before any code is looked up, with a response that
/// does not depend on the code, so they are not charged to the failed-join
/// budget: they test no code. Every request that reaches the lookup is charged
/// by [`RelayState::join_pairing`].
async fn join_pairing(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    State(state): State<Arc<RelayState>>,
    headers: HeaderMap,
    RequestJson(req): RequestJson<JoinPairingRequest>,
) -> Result<Json<JoinPairingResponse>, (StatusCode, Json<ErrorBody>)> {
    if req.protocol_version != clipsync_protocol::PROTOCOL_VERSION {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "protocol_version",
            "unsupported protocol version",
        ));
    }
    let client = state.client_ip(addr.ip(), &headers);
    match state.join_pairing(req, client) {
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

/// A JSON request body. A body that is not valid JSON or does not match the
/// request type (for example a malformed wire ID) is answered with
/// `400 Bad Request` and an [`ErrorBody`], where axum's `Json` would answer
/// `422` with its own text. Other rejections (wrong content type, body too
/// large) keep axum's response.
#[derive(FromRequest)]
#[from_request(via(Json), rejection(RequestBodyRejection))]
struct RequestJson<T>(T);

/// Rejection for [`RequestJson`]. The decode-failure message is fixed: the
/// decoder's own message can quote the rejected input, which may hold
/// control characters.
struct RequestBodyRejection(Response);

impl From<JsonRejection> for RequestBodyRejection {
    fn from(rejection: JsonRejection) -> Self {
        Self(match rejection {
            JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => err(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "malformed request body",
            )
            .into_response(),
            other => other.into_response(),
        })
    }
}

impl IntoResponse for RequestBodyRejection {
    fn into_response(self) -> Response {
        self.0
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
pub enum CreateError {
    /// A pairing-creation budget is spent.
    Throttled,
    /// The open-session or room cap is reached.
    Full,
    Invalid(String),
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "timestamp_shape_tests.rs"]
mod timestamp_shape_tests;
