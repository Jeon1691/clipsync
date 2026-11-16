// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Fail-closed listener configuration and bounded rustls handshakes.

use std::future::Future;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use axum_server::accept::Accept;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use clap::Parser;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

const MAX_HANDSHAKES: usize = 128;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PEM_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, clap::Args)]
pub struct RelayTransportConfig {
    /// PEM certificate chain for direct HTTPS/WSS (leaf certificate first)
    #[arg(long, env = "CLIPSYNC_RELAY_TLS_CERT_FILE")]
    pub tls_cert_file: Option<PathBuf>,
    /// PEM private key matching the TLS certificate
    #[arg(long, env = "CLIPSYNC_RELAY_TLS_KEY_FILE")]
    pub tls_key_file: Option<PathBuf>,
    /// Explicitly allow plaintext HTTP behind a TLS proxy or for local development
    #[arg(long, env = "CLIPSYNC_RELAY_ALLOW_HTTP")]
    pub allow_http: bool,
    /// Comma-separated proxy addresses/CIDRs whose X-Forwarded-For identifies
    /// clients for pairing limits; unset uses only the TCP peer address
    #[arg(
        long,
        env = "CLIPSYNC_RELAY_TRUSTED_PROXIES",
        value_delimiter = ',',
        value_parser = crate::client_ip::parse_trusted_proxy
    )]
    pub trusted_proxies: Vec<ipnet::IpNet>,
    #[command(flatten)]
    pub join_limits: crate::PairingJoinLimits,
}

impl RelayTransportConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        #[derive(Parser)]
        struct EnvArgs {
            #[command(flatten)]
            transport: RelayTransportConfig,
        }
        Ok(EnvArgs::try_parse_from(["clipsync-relay"])?.transport)
    }

    pub(crate) fn tls_config(&self, public_url: &str) -> anyhow::Result<Option<RustlsConfig>> {
        let url = url::Url::parse(public_url).context("CLIPSYNC_RELAY_PUBLIC_URL must be a URL")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            bail!("CLIPSYNC_RELAY_PUBLIC_URL must use http:// or https:// with a hostname");
        }
        match (&self.tls_cert_file, &self.tls_key_file, self.allow_http) {
            (None, None, true) => Ok(None),
            (None, None, false) => bail!(
                "TLS is required: configure CLIPSYNC_RELAY_TLS_CERT_FILE and CLIPSYNC_RELAY_TLS_KEY_FILE; use CLIPSYNC_RELAY_ALLOW_HTTP=true only behind a TLS proxy or for local development"
            ),
            (Some(_), Some(_), true) => bail!(
                "CLIPSYNC_RELAY_ALLOW_HTTP cannot be combined with TLS certificate/key settings"
            ),
            (Some(cert_path), Some(key_path), false) => {
                if url.scheme() != "https" {
                    bail!("CLIPSYNC_RELAY_PUBLIC_URL must use https:// with direct TLS");
                }
                let cert_bytes = read_config_file(cert_path, false)
                    .context("CLIPSYNC_RELAY_TLS_CERT_FILE")?;
                let key_bytes = read_config_file(key_path, true)
                    .context("CLIPSYNC_RELAY_TLS_KEY_FILE")?;
                let certs = CertificateDer::pem_slice_iter(&cert_bytes)
                    .collect::<Result<Vec<_>, _>>()
                    .context("CLIPSYNC_RELAY_TLS_CERT_FILE is not valid PEM")?;
                if certs.is_empty() {
                    bail!("CLIPSYNC_RELAY_TLS_CERT_FILE contains no certificates");
                }
                let key = PrivateKeyDer::from_pem_slice(&key_bytes)
                    .context("CLIPSYNC_RELAY_TLS_KEY_FILE contains no valid PEM private key")?;
                let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .context("TLS certificate and private key must be valid and match")?;
                // WebSocket upgrades use HTTP/1.1. HTTP/2 remains available to
                // normal HTTP clients without changing the peer address source.
                config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
                Ok(Some(RustlsConfig::from_config(Arc::new(config))))
            }
            _ => bail!(
                "configure both CLIPSYNC_RELAY_TLS_CERT_FILE and CLIPSYNC_RELAY_TLS_KEY_FILE"
            ),
        }
    }
}

pub(crate) fn read_config_file(path: &Path, private: bool) -> anyhow::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A blocking FIFO open waits for a writer before we can inspect its
        // descriptor. Nonblocking mode lets the regular-file check reject it;
        // it has no effect on regular files and still follows managed symlinks.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .context("cannot open configured file; check the relay user's access")?;
    let metadata = file
        .metadata()
        .context("cannot read configured file metadata")?;
    if !metadata.is_file() || metadata.len() > MAX_PEM_BYTES {
        bail!("configured file must be a regular file no larger than 1 MiB");
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        // Allow an explicitly selected reader group (e.g. container GID 65532),
        // but reject group-writable or world-accessible private credentials.
        if metadata.permissions().mode() & 0o027 != 0 {
            bail!("private credential permissions must be 0600 or 0640 (or stricter)");
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Vec::new();
    file.take(MAX_PEM_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("cannot read configured file")?;
    if bytes.len() as u64 > MAX_PEM_BYTES {
        bail!("configured file must be no larger than 1 MiB");
    }
    Ok(bytes)
}

/// Reject excess handshakes immediately instead of queuing accepted sockets.
/// The upstream acceptor owns TLS parsing and the handshake deadline.
#[derive(Clone)]
pub(crate) struct LimitedTlsAcceptor {
    inner: RustlsAcceptor,
    permits: Arc<Semaphore>,
}

impl LimitedTlsAcceptor {
    pub(crate) fn new(config: RustlsConfig) -> Self {
        Self {
            inner: RustlsAcceptor::new(config).handshake_timeout(HANDSHAKE_TIMEOUT),
            permits: Arc::new(Semaphore::new(MAX_HANDSHAKES)),
        }
    }
}

impl<S: Send + 'static> Accept<TcpStream, S> for LimitedTlsAcceptor {
    type Stream = <RustlsAcceptor as Accept<TcpStream, S>>::Stream;
    type Service = <RustlsAcceptor as Accept<TcpStream, S>>::Service;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Self::Service)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        let permit = self.permits.clone().try_acquire_owned();
        let handshake = self.inner.accept(stream, service);
        Box::pin(async move {
            let _permit = permit.map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "TLS handshake capacity reached")
            })?;
            handshake.await
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "transport_tests.rs"]
mod tests;
