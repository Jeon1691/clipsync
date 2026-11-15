// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::process::Stdio;
use std::time::Duration;

#[tokio::test]
async fn relay_binary_honors_bind_and_serves_health_before_shutdown() {
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_clipsync-relay"))
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "relay exited before health check"
            );
            if let Ok(response) = client.get(format!("http://{address}/healthz")).send().await {
                assert!(response.status().is_success());
                assert!(response.text().await.unwrap().contains("ok"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
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
