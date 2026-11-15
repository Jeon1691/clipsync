// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Controlled private-CA HTTPS/WSS endpoints. Each client runs in a child
//! process so the CA environment and cached TLS configuration cannot leak into
//! other tests or mutate the host's trust store.

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use clipsync_protocol::ControlMessage;
use clipsync_transport::{configure_reqwest, Incoming, RelayConnection, CA_FILE_ENV};
use futures_util::{SinkExt, StreamExt};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

const CHILD_MODE: &str = "CLIPSYNC_TEST_TLS_MODE";
const CHILD_URL: &str = "CLIPSYNC_TEST_TLS_URL";

fn private_ca() -> (rcgen::Certificate, KeyPair) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "ClipSync private-CA regression root");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let now = SystemTime::now();
    params.not_before = (now - Duration::from_secs(86_400)).into();
    params.not_after = (now + Duration::from_secs(7 * 86_400)).into();
    let key = KeyPair::generate().unwrap();
    (params.self_signed(&key).unwrap(), key)
}

async fn private_server(websocket: bool) -> (u16, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let (ca, ca_key) = private_ca();
    let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    params.use_authority_key_identifier_extension = true;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let now = SystemTime::now();
    params.not_before = (now - Duration::from_secs(86_400)).into();
    params.not_after = (now + Duration::from_secs(7 * 86_400)).into();
    let key = KeyPair::generate().unwrap();
    let leaf = params.signed_by(&key, &ca, &ca_key).unwrap();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap();
    let roots = tempfile::tempdir().unwrap();
    std::fs::write(roots.path().join("trusted.pem"), ca.pem()).unwrap();
    let (unrelated, _) = private_ca();
    std::fs::write(roots.path().join("unrelated.pem"), unrelated.pem()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // Rejected client certificates are expected in the negative cases.
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    return;
                };
                if websocket {
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(Ok(message)) = ws.next().await {
                        if let Message::Text(text) = message {
                            if let Ok(ControlMessage::Ping { ts: 42 }) =
                                ControlMessage::decode(&text)
                            {
                                ws.send(Message::Text(
                                    ControlMessage::Pong { ts: 42 }.encode().unwrap().into(),
                                ))
                                .await
                                .unwrap();
                                // Keep the positive TLS endpoint alive until
                                // the client has observed Pong and sends Close.
                                // Dropping TLS here races buffered delivery on
                                // Windows and tests truncation rather than trust.
                            }
                        }
                    }
                } else {
                    let mut request = Vec::new();
                    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                        let mut buffer = [0; 1024];
                        let count = stream.read(&mut buffer).await.unwrap();
                        assert_ne!(count, 0);
                        request.extend_from_slice(&buffer[..count]);
                        assert!(request.len() < 16_384);
                    }
                    assert!(request.starts_with(b"GET /healthz HTTP/1.1\r\n"));
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\ntrusted")
                        .await
                        .unwrap();
                    stream.shutdown().await.unwrap();
                }
            });
        }
    });
    (port, roots, server)
}

async fn exercise_private_trust(websocket: bool) {
    let (port, roots, server) = private_server(websocket).await;
    let scheme = if websocket { "wss" } else { "https" };
    for (mode, host, root) in [
        ("accept", "localhost", "trusted.pem"),
        ("reject", "127.0.0.1", "trusted.pem"),
        ("reject", "localhost", "unrelated.pem"),
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tls_client_child", "--nocapture"])
            .env(CHILD_MODE, mode)
            .env(CHILD_URL, format!("{scheme}://{host}:{port}/healthz"))
            .env(CA_FILE_ENV, roots.path().join(root))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{scheme} mode={mode} host={host} root={root}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_ca_https_trust_preserves_hostname_and_issuer_checks() {
    exercise_private_trust(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_ca_wss_trust_preserves_hostname_and_issuer_checks() {
    exercise_private_trust(true).await;
}

#[tokio::test]
async fn tls_client_child() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let url = std::env::var(CHILD_URL).unwrap();
    let accept = mode == "accept";
    if url.starts_with("https:") {
        let builder = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5));
        let client = configure_reqwest(builder).unwrap().build().unwrap();
        let response = client.get(url).send().await;
        if accept {
            assert_eq!(response.unwrap().text().await.unwrap(), "trusted");
        } else {
            assert!(
                response.unwrap_err().is_connect(),
                "TLS policy must reject the connection"
            );
        }
    } else {
        let connection =
            tokio::time::timeout(Duration::from_secs(5), RelayConnection::connect(&url))
                .await
                .expect("TLS connection must finish within the test deadline");
        if accept {
            let mut connection = connection.unwrap();
            connection
                .send_control(ControlMessage::Ping { ts: 42 })
                .await
                .unwrap();
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(5), connection.incoming.recv())
                    .await
                    .unwrap(),
                Some(Incoming::Pong)
            ));
            connection.close().await;
        } else {
            assert!(connection.is_err(), "TLS policy must reject the connection");
        }
    }
}
