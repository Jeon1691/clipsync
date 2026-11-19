// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Real relay binary with ephemeral CA/identity fixtures. No host trust-store,
//! clipboard, keychain or persistent user configuration is used.

mod support;

use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use clipsync_protocol::{
    ClientRole, ControlMessage, CreatePairingRequest, CreatePairingResponse, DeviceId,
    JoinPairingRequest, PairingSessionId, PROTOCOL_VERSION,
};
use futures_util::{SinkExt, StreamExt};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::ServerName;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{
    client_async_tls_with_config, tungstenite::Message, Connector, MaybeTlsStream, WebSocketStream,
};

use support::{Relay, EPHEMERAL_LOOPBACK_BIND};

/// Backdating `notBefore` by a day keeps the fixtures valid on hosts whose
/// clock is slightly behind the one that minted them.
const CERTIFICATE_BACKDATE: Duration = Duration::from_secs(24 * 60 * 60);
/// A week comfortably outlives any test run while staying short-lived.
const CERTIFICATE_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);

struct Identity {
    directory: tempfile::TempDir,
    root: rustls::pki_types::CertificateDer<'static>,
}

impl Identity {
    fn new() -> Self {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let now = SystemTime::now();
        ca_params.not_before = (now - CERTIFICATE_BACKDATE).into();
        ca_params.not_after = (now + CERTIFICATE_LIFETIME).into();
        let ca_key = KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap();
        // The issuer takes its subject name, key identifier method and key
        // usages from the same parameters that produced the root above.
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let mut leaf = CertificateParams::new(vec!["localhost".into()]).unwrap();
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        leaf.not_before = (now - CERTIFICATE_BACKDATE).into();
        leaf.not_after = (now + CERTIFICATE_LIFETIME).into();
        let key = KeyPair::generate().unwrap();
        let leaf = leaf.signed_by(&key, &issuer).unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("cert.pem"), leaf.pem()).unwrap();
        private_file(
            &directory.path().join("key.pem"),
            key.serialize_pem().as_bytes(),
        );
        Self {
            directory,
            root: ca.der().clone(),
        }
    }

    fn client_config(&self, tls12_only: bool) -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.root.clone()).unwrap();
        let versions: &[&rustls::SupportedProtocolVersion] = if tls12_only {
            &[&rustls::version::TLS12]
        } else {
            &[&rustls::version::TLS13, &rustls::version::TLS12]
        };
        Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(versions)
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        )
    }

    fn command(&self) -> Command {
        let mut command = isolated_command(self.directory.path());
        command.args(["--bind", EPHEMERAL_LOOPBACK_BIND]);
        command
    }

    fn tls_command(&self) -> Command {
        let mut command = self.command();
        command
            .arg("--tls-cert-file")
            .arg(self.directory.path().join("cert.pem"))
            .arg("--tls-key-file")
            .arg(self.directory.path().join("key.pem"));
        command
    }
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn isolated_command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_clipsync-relay"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CLIPSYNC_") {
            command.env_remove(name);
        }
    }
    command
        .env("HOME", home)
        .env("CLIPSYNC_LISTEN", "127.0.0.1:0")
        .env(
            "CLIPSYNC_RELAY_TOKEN_SECRET",
            "synthetic-private-relay-test-key-32bytes",
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// Per-request bound for loopback clients, so a misbehaving relay fails the
/// test instead of hanging it; loopback requests normally finish in
/// milliseconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// HTTP client whose `localhost` resolves only to the spawned relay's
/// address. System resolution may prefer `::1`, where an unrelated process
/// can hold the same port number as the relay's IPv4 listener.
fn pinned_client(tls: Option<&rustls::ClientConfig>, relay: SocketAddr) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .no_proxy()
        .resolve("localhost", relay)
        .timeout(REQUEST_TIMEOUT);
    match tls {
        Some(tls) => builder.use_preconfigured_tls(tls.clone()),
        None => builder,
    }
    .build()
    .unwrap()
}

/// WSS handshake over a TCP connection to the spawned relay, verifying the
/// certificate against `host` without resolving it. The handshake error is
/// rendered to text: callers only report it or assert that it occurred.
async fn pinned_wss(
    host: &str,
    relay: SocketAddr,
    tls: Arc<rustls::ClientConfig>,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, String> {
    let socket = TcpStream::connect(relay).await.unwrap();
    client_async_tls_with_config(
        format!("wss://{host}:{}/v1/ws", relay.port()),
        socket,
        None,
        Some(Connector::Rustls(tls)),
    )
    .await
    .map(|(ws, _)| ws)
    .map_err(|error| error.to_string())
}

async fn rejection(mut command: Command, expected: &str) {
    let child = command.spawn().unwrap();
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .expect("bad configuration must fail before listening")
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(expected), "expected {expected}: {error}");
    assert!(!error.contains("synthetic-private-relay-test-key-32bytes"));
}

#[cfg(unix)]
fn make_fifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(path)
        .status()
        .expect("create isolated FIFO fixture");
    assert!(status.success());
}

#[cfg(unix)]
#[tokio::test]
async fn token_secret_fifo_without_a_writer_is_rejected_before_listening() {
    let identity = Identity::new();
    let path = identity.directory.path().join("token.fifo");
    make_fifo(&path);
    let mut command = identity.command();
    command
        .env_remove("CLIPSYNC_RELAY_TOKEN_SECRET")
        .env("CLIPSYNC_RELAY_TOKEN_SECRET_FILE", path)
        .arg("--allow-http");
    rejection(command, "regular file").await;
}

#[cfg(unix)]
#[tokio::test]
async fn tls_key_fifo_without_a_writer_is_rejected_before_listening() {
    let identity = Identity::new();
    let path = identity.directory.path().join("key.pem");
    std::fs::remove_file(&path).unwrap();
    make_fifo(&path);
    rejection(identity.tls_command(), "regular file").await;
}

#[cfg(unix)]
#[tokio::test]
async fn tls_certificate_fifo_without_a_writer_is_rejected_before_listening() {
    let identity = Identity::new();
    let path = identity.directory.path().join("cert.pem");
    std::fs::remove_file(&path).unwrap();
    make_fifo(&path);
    rejection(identity.tls_command(), "regular file").await;
}

#[cfg(unix)]
#[tokio::test]
async fn verified_https_keeps_certificate_and_private_key_symlink_support() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let identity = Identity::new();
    let directory = identity.directory.path();
    for name in ["cert", "key"] {
        std::fs::rename(
            directory.join(format!("{name}.pem")),
            directory.join(format!("{name}.target")),
        )
        .unwrap();
        symlink(
            format!("{name}.target"),
            directory.join(format!("{name}.pem")),
        )
        .unwrap();
    }
    std::fs::set_permissions(
        directory.join("key.target"),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    let relay = Relay::spawn(identity.tls_command()).await;
    let client = pinned_client(Some(&identity.client_config(false)), relay.address);
    assert!(client
        .get(format!(
            "https://localhost:{}/healthz",
            relay.address.port()
        ))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    relay.stop().await;
}

#[tokio::test]
async fn default_and_incomplete_tls_configuration_fail_closed() {
    let identity = Identity::new();
    rejection(identity.command(), "TLS is required").await;
    let mut cert_only = identity.command();
    cert_only
        .arg("--tls-cert-file")
        .arg(identity.directory.path().join("cert.pem"));
    rejection(cert_only, "configure both").await;
    let mut key_only = identity.command();
    key_only
        .arg("--tls-key-file")
        .arg(identity.directory.path().join("key.pem"));
    rejection(key_only, "configure both").await;
    let mut ambiguous = identity.tls_command();
    ambiguous.arg("--allow-http");
    rejection(ambiguous, "cannot be combined").await;
    let mut invalid_bool = identity.command();
    invalid_bool.env("CLIPSYNC_RELAY_ALLOW_HTTP", "invalid");
    rejection(invalid_bool, "--allow-http").await;
}

#[tokio::test]
async fn malformed_missing_mismatched_and_public_private_keys_fail_closed() {
    let identity = Identity::new();
    std::fs::remove_file(identity.directory.path().join("cert.pem")).unwrap();
    rejection(identity.tls_command(), "CLIPSYNC_RELAY_TLS_CERT_FILE").await;
    std::fs::write(identity.directory.path().join("cert.pem"), "malformed").unwrap();
    rejection(identity.tls_command(), "contains no certificates").await;
    let identity = Identity::new();
    private_file(&identity.directory.path().join("key.pem"), b"malformed");
    rejection(identity.tls_command(), "no valid PEM private key").await;
    let identity = Identity::new();
    private_file(
        &identity.directory.path().join("key.pem"),
        KeyPair::generate().unwrap().serialize_pem().as_bytes(),
    );
    rejection(identity.tls_command(), "must be valid and match").await;
    let identity = Identity::new();
    let mut insecure_url = identity.tls_command();
    insecure_url.args(["--public-url", "http://localhost"]);
    rejection(insecure_url, "must use https://").await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let identity = Identity::new();
        std::fs::set_permissions(
            identity.directory.path().join("key.pem"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        rejection(identity.tls_command(), "private credential permissions").await;
    }
}

#[tokio::test]
async fn verified_https_wss_and_pairing_keep_peer_info_and_token_authentication() {
    let identity = Identity::new();
    let relay = Relay::spawn(identity.tls_command()).await;
    let tls = identity.client_config(false);
    let client = pinned_client(Some(&tls), relay.address);
    let base = format!("https://localhost:{}", relay.address.port());
    assert!(client
        .get(format!("{base}/healthz"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let device = DeviceId::new();
    let session = PairingSessionId::new();
    let offer: CreatePairingResponse = client
        .post(format!("{base}/v1/pairing/create"))
        .json(&CreatePairingRequest {
            protocol_version: PROTOCOL_VERSION,
            pairing_session_id: session.clone(),
            device_id: device.clone(),
            device_name: "synthetic owner".into(),
            os: "test".into(),
            ttl_secs: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    // join_pairing requires ConnectInfo<SocketAddr>. A successful join proves
    // the TLS service preserved the actual TCP peer information.
    let joined = client
        .post(format!("{base}/v1/pairing/join"))
        .json(&JoinPairingRequest {
            protocol_version: PROTOCOL_VERSION,
            pairing_code: offer.pairing_code,
            device_id: DeviceId::new(),
            device_name: "synthetic joiner".into(),
            os: "test".into(),
        })
        .send()
        .await
        .unwrap();
    assert!(
        joined.status().is_success(),
        "TLS pairing must retain ConnectInfo"
    );
    let mut ws = pinned_wss("localhost", relay.address, tls).await.unwrap();
    let hello = |token: String| ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: device.clone(),
        device_name: "synthetic owner".into(),
        os: "test".into(),
        role: ClientRole::PairingA,
        room_id: Some(offer.room_id.clone()),
        token,
        pairing_session_id: Some(session.clone()),
    };
    ws.send(Message::Text(
        hello("invalid-token".into()).encode().unwrap().into(),
    ))
    .await
    .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        ControlMessage::decode(message.to_text().unwrap()).unwrap(),
        ControlMessage::Error { .. }
    ));
    ws.send(Message::Text(
        hello(offer.pairing_token).encode().unwrap().into(),
    ))
    .await
    .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        ControlMessage::decode(message.to_text().unwrap()).unwrap(),
        ControlMessage::HelloOk { .. }
    ));
    ws.close(None).await.unwrap();
    relay.stop().await;
}

#[tokio::test]
async fn tls12_and_tls13_verify_issuer_hostname_and_reject_plaintext() {
    let identity = Identity::new();
    let mut command = identity.command();
    command
        .env(
            "CLIPSYNC_RELAY_TLS_CERT_FILE",
            identity.directory.path().join("cert.pem"),
        )
        .env(
            "CLIPSYNC_RELAY_TLS_KEY_FILE",
            identity.directory.path().join("key.pem"),
        );
    let relay = Relay::spawn(command).await;
    let port = relay.address.port();
    for tls12_only in [true, false] {
        let connector = TlsConnector::from(identity.client_config(tls12_only));
        let socket = TcpStream::connect(relay.address).await.unwrap();
        let stream = connector
            .connect(ServerName::try_from("localhost").unwrap(), socket)
            .await
            .unwrap();
        assert_eq!(
            stream.get_ref().1.protocol_version(),
            Some(if tls12_only {
                rustls::ProtocolVersion::TLSv1_2
            } else {
                rustls::ProtocolVersion::TLSv1_3
            })
        );
    }
    let unrelated = Identity::new();
    for (host, config) in [
        ("127.0.0.1", identity.client_config(false)),
        ("localhost", unrelated.client_config(false)),
    ] {
        let client = pinned_client(Some(&config), relay.address);
        assert!(client
            .get(format!("https://{host}:{port}/healthz"))
            .send()
            .await
            .unwrap_err()
            .is_connect());
        assert!(pinned_wss(host, relay.address, config).await.is_err());
    }
    assert!(pinned_client(None, relay.address)
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .is_err());
    relay.stop().await;
}

#[tokio::test]
async fn plaintext_requires_explicit_opt_in_and_stalled_tls_handshakes_expire() {
    let identity = Identity::new();
    let mut command = identity.command();
    command.env("CLIPSYNC_RELAY_ALLOW_HTTP", "true");
    let relay = Relay::spawn(command).await;
    assert!(pinned_client(None, relay.address)
        .get(format!("http://{}/healthz", relay.address))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/v1/ws", relay.address))
        .await
        .unwrap();
    ws.send(Message::Text(
        ControlMessage::Ping { ts: 73 }.encode().unwrap().into(),
    ))
    .await
    .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        ControlMessage::decode(reply.to_text().unwrap()).unwrap(),
        ControlMessage::Pong { ts: 73 }
    ));
    ws.close(None).await.unwrap();
    relay.stop().await;
    let relay = Relay::spawn(identity.tls_command()).await;
    let mut stalled = TcpStream::connect(relay.address).await.unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(7), stalled.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    relay.stop().await;
}

#[tokio::test]
async fn library_serve_uses_the_same_fail_closed_environment_policy() {
    const CHILD: &str = "CLIPSYNC_LIBRARY_SERVE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let result = clipsync_relay::serve(clipsync_relay::RelayConfig::from_env()).await;
        assert!(result.unwrap_err().to_string().contains("TLS is required"));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CLIPSYNC_") {
            command.env_remove(name);
        }
    }
    command
        .args([
            "--exact",
            "library_serve_uses_the_same_fail_closed_environment_policy",
            "--nocapture",
        ])
        .env(CHILD, "true")
        .env("HOME", directory.path())
        .env("CLIPSYNC_RELAY_BIND", "127.0.0.1:0")
        .env(
            "CLIPSYNC_RELAY_TOKEN_SECRET",
            "synthetic-private-relay-test-key-32bytes",
        )
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
