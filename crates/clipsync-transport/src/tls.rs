//! OS trust store (macOS Keychain / system CAs) plus optional extra PEM roots.

use std::sync::{Arc, OnceLock};

use rustls::pki_types::CertificateDer;
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use tracing::info;

use crate::TransportError;

/// `CLIPSYNC_CA_FILE` — PEM bundle of extra private/self-signed CAs.
pub const CA_FILE_ENV: &str = "CLIPSYNC_CA_FILE";

pub fn client_config() -> Result<Arc<ClientConfig>, TransportError> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(existing) = CONFIG.get() {
        return Ok(Arc::clone(existing));
    }
    let built = Arc::new(build()?);
    let _ = CONFIG.set(Arc::clone(&built));
    Ok(CONFIG.get().cloned().unwrap_or(built))
}

pub fn configure_reqwest(
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::ClientBuilder, TransportError> {
    Ok(builder.use_preconfigured_tls(client_config()?.as_ref().clone()))
}

pub fn ca_file_path() -> Option<String> {
    std::env::var(CA_FILE_ENV).ok().filter(|s| !s.is_empty())
}

fn build() -> Result<ClientConfig, TransportError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let extra = load_extra_cas()?;
    if extra.is_empty() {
        return Ok(ClientConfig::builder()
            .with_platform_verifier()
            .with_no_client_auth());
    }
    info!(
        count = extra.len(),
        "loaded extra TLS roots from {CA_FILE_ENV}"
    );
    let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(extra)
        .map_err(|e| TransportError::Protocol(format!("tls extra roots: {e}")))?;
    Ok(ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

fn load_extra_cas() -> Result<Vec<CertificateDer<'static>>, TransportError> {
    let Some(path) = ca_file_path() else {
        return Ok(Vec::new());
    };
    let data = std::fs::read(&path)
        .map_err(|e| TransportError::Protocol(format!("{CA_FILE_ENV} ({path}): {e}")))?;
    let mut cursor = std::io::Cursor::new(data);
    let certs = rustls_pemfile::certs(&mut cursor)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TransportError::Protocol(format!("{CA_FILE_ENV} parse: {e}")))?;
    if certs.is_empty() {
        return Err(TransportError::Protocol(format!(
            "{CA_FILE_ENV} ({path}) contained no certificates"
        )));
    }
    Ok(certs)
}
