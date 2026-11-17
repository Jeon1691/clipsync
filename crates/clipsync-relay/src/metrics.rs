// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Opt-in, bearer-protected operational metrics.
//!
//! `/metrics` exposes live connection counts and pairing outcome counters, so
//! it is only routed when an operator configures a scrape token.

use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::state::RelayState;

const METRICS_TOKEN_ENV: &str = "CLIPSYNC_RELAY_METRICS_TOKEN";
const METRICS_TOKEN_FILE_ENV: &str = "CLIPSYNC_RELAY_METRICS_TOKEN_FILE";
/// Same 256-bit floor as the relay token key; `openssl rand -hex 32` yields 64.
pub const MIN_METRICS_TOKEN_BYTES: usize = 32;
/// RFC 6750 bearer challenge returned with every rejected scrape.
const BEARER_CHALLENGE: &str = "Bearer realm=\"clipsync-relay-metrics\"";

/// Scrape credential for `GET /metrics`.
///
/// Only a SHA-256 digest is retained, so the comparison runs over fixed-size
/// values and does not reveal the configured token's length or bytes.
#[derive(Clone)]
pub struct MetricsToken {
    digest: sha2::digest::Output<Sha256>,
}

impl MetricsToken {
    /// Accept a token of at least [`MIN_METRICS_TOKEN_BYTES`] visible ASCII
    /// characters, the bytes an HTTP `Authorization` header can carry verbatim.
    pub fn new(token: impl AsRef<[u8]>) -> anyhow::Result<Self> {
        let token = token.as_ref();
        if token.len() < MIN_METRICS_TOKEN_BYTES {
            anyhow::bail!("metrics token must contain at least {MIN_METRICS_TOKEN_BYTES} bytes");
        }
        if !token.iter().all(u8::is_ascii_graphic) {
            anyhow::bail!("metrics token must contain only visible ASCII characters");
        }
        Ok(Self {
            digest: Sha256::digest(token),
        })
    }

    /// Compare a presented credential in constant time.
    pub fn matches(&self, presented: &[u8]) -> bool {
        let presented = Sha256::digest(presented);
        self.digest.as_slice().ct_eq(presented.as_slice()).into()
    }
}

impl std::fmt::Debug for MetricsToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MetricsToken(<redacted>)")
    }
}

/// Read the optional scrape token. Unset, empty, or whitespace-only
/// `CLIPSYNC_RELAY_METRICS_TOKEN` leaves `/metrics` disabled; a selected file
/// must be private, and surrounding whitespace such as its trailing newline is
/// trimmed before validation.
pub(crate) fn metrics_token_from_env() -> anyhow::Result<Option<MetricsToken>> {
    let value = std::env::var_os(METRICS_TOKEN_ENV);
    let file = std::env::var_os(METRICS_TOKEN_FILE_ENV);
    if value.is_some() && file.is_some() {
        anyhow::bail!("configure only one of {METRICS_TOKEN_ENV} and {METRICS_TOKEN_FILE_ENV}");
    }
    if let Some(file) = file {
        let bytes = crate::transport::read_config_file(Path::new(&file), true)
            .map_err(|error| anyhow::anyhow!("{METRICS_TOKEN_FILE_ENV}: {error:#}"))?;
        return MetricsToken::new(bytes.trim_ascii())
            .map(Some)
            .map_err(|error| anyhow::anyhow!("{METRICS_TOKEN_FILE_ENV}: {error:#}"));
    }
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow::anyhow!("{METRICS_TOKEN_ENV} must be UTF-8"))?;
    if value.trim().is_empty() {
        return Ok(None);
    }
    MetricsToken::new(value)
        .map(Some)
        .map_err(|error| anyhow::anyhow!("{METRICS_TOKEN_ENV}: {error:#}"))
}

/// Extract the credential from `Authorization: Bearer <token>`; the scheme is
/// case-insensitive per RFC 7235.
fn bearer_credential(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(credential)
}

pub(crate) async fn metrics(State(state): State<Arc<RelayState>>, headers: HeaderMap) -> Response {
    let authorized = match (&state.config.metrics_token, bearer_credential(&headers)) {
        (Some(token), Some(credential)) => token.matches(credential.as_bytes()),
        _ => false,
    };
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static(BEARER_CHALLENGE),
            )],
        )
            .into_response();
    }
    state.metrics_text().into_response()
}
