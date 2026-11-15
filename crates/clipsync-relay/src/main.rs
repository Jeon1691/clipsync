// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clap::Parser;
use clipsync_relay::{serve_with_transport, RelayConfig, RelayTransportConfig};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "clipsync-relay",
    version,
    about = "Zero-knowledge ClipSync WebSocket relay"
)]
struct Args {
    #[command(flatten)]
    transport: RelayTransportConfig,
    /// Listen address
    #[arg(long, env = "CLIPSYNC_RELAY_BIND")]
    bind: Option<String>,
    /// Public HTTP/WS base URL advertised to clients
    #[arg(long, env = "CLIPSYNC_RELAY_PUBLIC_URL")]
    public_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();
    let args = Args::parse();
    let mut config = RelayConfig::from_env();
    if let Some(bind) = args.bind {
        config.bind = bind.parse()?;
    }
    if let Some(url) = args.public_url {
        config.public_url = url;
    } else {
        let scheme = if args.transport.allow_http {
            "http"
        } else {
            "https"
        };
        config.public_url = format!("{scheme}://127.0.0.1:{}", config.bind.port());
    }
    serve_with_transport(config, args.transport).await
}
