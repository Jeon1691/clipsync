use clap::Parser;
use clipsync_relay::{serve, RelayConfig};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "clipsync-relay",
    version,
    about = "Zero-knowledge ClipSync WebSocket relay"
)]
struct Args {
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
    }
    serve(config).await
}
