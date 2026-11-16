// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;

#[test]
fn invalid_urls_directories_and_oversized_configuration_are_rejected() {
    let http = RelayTransportConfig {
        allow_http: true,
        ..Default::default()
    };
    for url in [
        "not-a-url",
        "ftp://localhost",
        "http:///",
        "file:///tmp/relay",
    ] {
        assert!(http.tls_config(url).is_err());
    }
    let directory = tempfile::tempdir().unwrap();
    assert!(read_config_file(directory.path(), false).is_err());
    let path = directory.path().join("oversized.pem");
    std::fs::write(&path, vec![0; MAX_PEM_BYTES as usize + 1]).unwrap();
    assert!(read_config_file(&path, false).is_err());
}

#[tokio::test]
async fn handshakes_reject_capacity_overflow_and_release_cancelled_permits() {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
    let acceptor = LimitedTlsAcceptor::new(RustlsConfig::from_config(Arc::new(config)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut clients = Vec::new();
    let mut pending = Vec::new();
    for _ in 0..MAX_HANDSHAKES {
        clients.push(
            TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap(),
        );
        let (server, _) = listener.accept().await.unwrap();
        pending.push(acceptor.accept(server, ()));
    }
    assert_eq!(acceptor.permits.available_permits(), 0);
    let _overflow_client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    let result = acceptor.accept(server, ()).await;
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    drop(pending);
    assert_eq!(acceptor.permits.available_permits(), MAX_HANDSHAKES);
}

#[test]
fn trusted_proxies_default_to_none_and_parse_comma_separated_ranges() {
    #[derive(Parser)]
    struct Args {
        #[command(flatten)]
        transport: RelayTransportConfig,
    }
    let parse = |args: &[&str]| {
        Args::try_parse_from(std::iter::once("clipsync-relay").chain(args.iter().copied()))
            .map(|args| args.transport.trusted_proxies)
    };
    assert!(RelayTransportConfig::default().trusted_proxies.is_empty());
    assert_eq!(
        parse(&["--trusted-proxies", "10.0.0.0/8, 127.0.0.1,::1"]).unwrap(),
        ["10.0.0.0/8", "127.0.0.1/32", "::1/128"].map(|net| net.parse::<ipnet::IpNet>().unwrap())
    );
    assert!(parse(&["--trusted-proxies", "10.0.0.0/8,proxy.example"]).is_err());
}
