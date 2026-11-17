// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::process::Stdio;
use std::time::Duration;

/// Scrape credential for the binary-level metrics contract.
const METRICS_TEST_TOKEN: &str = "binary-metrics-scrape-token-0123456789";

struct RelayProcess {
    child: tokio::process::Child,
    url: String,
    client: reqwest::Client,
}

impl RelayProcess {
    async fn start(metrics_token: Option<&str>) -> Self {
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_clipsync-relay"));
        command
            .args([
                "--allow-http",
                "--bind",
                &address.to_string(),
                "--public-url",
                &format!("http://{address}"),
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
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(token) = metrics_token {
            command.env("CLIPSYNC_RELAY_METRICS_TOKEN", token);
        } else {
            command.env_remove("CLIPSYNC_RELAY_METRICS_TOKEN");
        }
        let mut child = command.spawn().unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{address}");
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "relay exited before health check"
                );
                if let Ok(response) = client.get(format!("{url}/healthz")).send().await {
                    assert!(response.status().is_success());
                    assert!(response.text().await.unwrap().contains("ok"));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        Self { child, url, client }
    }

    async fn metrics_status(&self, bearer: Option<&str>) -> reqwest::StatusCode {
        let mut request = self.client.get(format!("{}/metrics", self.url));
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap().status()
    }

    async fn stop(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
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
