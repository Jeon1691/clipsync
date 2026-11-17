// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
mod support;

use std::net::Ipv4Addr;
use std::process::Stdio;

use support::{Relay, EPHEMERAL_LOOPBACK_BIND};

/// Scrape credential for the binary-level metrics contract.
const METRICS_TEST_TOKEN: &str = "binary-metrics-scrape-token-0123456789";

struct RelayProcess {
    relay: Relay,
    url: String,
    client: reqwest::Client,
}

impl RelayProcess {
    async fn start(metrics_token: Option<&str>) -> Self {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_clipsync-relay"));
        command
            .args([
                "--allow-http",
                "--bind",
                EPHEMERAL_LOOPBACK_BIND,
                "--public-url",
                "http://127.0.0.1",
            ])
            .env(
                "CLIPSYNC_RELAY_TOKEN_SECRET",
                "synthetic-relay-binary-test-secret-32bytes",
            )
            .env("CLIPSYNC_RELAY_BIND", "127.0.0.1:0")
            .env_remove("CLIPSYNC_RELAY_TLS_CERT_FILE")
            .env_remove("CLIPSYNC_RELAY_TLS_KEY_FILE")
            .env_remove("CLIPSYNC_RELAY_TOKEN_SECRET_FILE")
            .env_remove("CLIPSYNC_RELAY_WEBHOOK_URL")
            .env_remove("CLIPSYNC_RELAY_WEBHOOK_SECRET")
            .env_remove("CLIPSYNC_RELAY_METRICS_TOKEN_FILE")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(token) = metrics_token {
            command.env("CLIPSYNC_RELAY_METRICS_TOKEN", token);
        } else {
            command.env_remove("CLIPSYNC_RELAY_METRICS_TOKEN");
        }
        let relay = Relay::spawn(command).await;
        // The relay binds the requested interface; the OS chooses only the port.
        assert_eq!(relay.address.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(relay.address.port(), 0);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{}", relay.address);
        let response = client.get(format!("{url}/healthz")).send().await.unwrap();
        assert!(response.status().is_success());
        assert!(response.text().await.unwrap().contains("ok"));
        Self { relay, url, client }
    }

    async fn metrics_status(&self, bearer: Option<&str>) -> reqwest::StatusCode {
        let mut request = self.client.get(format!("{}/metrics", self.url));
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap().status()
    }

    async fn stop(self) {
        self.relay.stop().await;
    }
}

#[tokio::test]
async fn relay_binary_honors_bind_and_serves_health_before_shutdown() {
    let relay = RelayProcess::start(None).await;
    assert_eq!(
        relay.metrics_status(Some(METRICS_TEST_TOKEN)).await,
        reqwest::StatusCode::NOT_FOUND
    );
    relay.stop().await;
}

#[tokio::test]
async fn relay_binary_serves_metrics_only_to_the_configured_bearer() {
    let relay = RelayProcess::start(Some(METRICS_TEST_TOKEN)).await;
    assert_eq!(
        relay.metrics_status(None).await,
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        relay
            .metrics_status(Some("wrong-binary-metrics-token-0123456789"))
            .await,
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        relay.metrics_status(Some(METRICS_TEST_TOKEN)).await,
        reqwest::StatusCode::OK
    );
    relay.stop().await;
}

#[test]
fn malformed_bind_fails_before_listening() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_clipsync-relay"))
        .args(["--bind", "not-an-address"])
        .env("CLIPSYNC_RELAY_BIND", "127.0.0.1:0")
        .env(
            "CLIPSYNC_RELAY_TOKEN_SECRET",
            "synthetic-relay-binary-test-secret-32bytes",
        )
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid socket address"));
}
