// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! OS trust store (macOS Keychain / system CAs) plus optional extra PEM roots.

use std::sync::{Arc, OnceLock};

use rustls::pki_types::{pem::PemObject, CertificateDer};
use rustls::{ClientConfig, RootCertStore};
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

/// Load the extra CA certificates specified by `CLIPSYNC_CA_FILE`.
///
/// Returns no extra certificates when unset; rejects unreadable, malformed,
/// or certificate-free PEM bundles.
fn load_extra_cas() -> Result<Vec<CertificateDer<'static>>, TransportError> {
    let Some(path) = ca_file_path() else {
        return Ok(Vec::new());
    };
    let data = std::fs::read(&path)
        .map_err(|e| TransportError::Protocol(format!("{CA_FILE_ENV} ({path}): {e}")))?;
    let certs = CertificateDer::pem_slice_iter(&data)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TransportError::Protocol(format!("{CA_FILE_ENV} parse: {e}")))?;
    if certs.is_empty() {
        return Err(TransportError::Protocol(format!(
            "{CA_FILE_ENV} ({path}) contained no certificates"
        )));
    }
    // PEM decoding checks base64, not certificate DER. Some platform
    // verifiers silently discard invalid roots, so validate every entry here.
    let mut validation = RootCertStore::empty();
    for (index, certificate) in certs.iter().enumerate() {
        validation.add(certificate.clone()).map_err(|error| {
            TransportError::Protocol(format!(
                "{CA_FILE_ENV} ({path}) certificate {} invalid DER: {error}",
                index + 1
            ))
        })?;
    }
    Ok(certs)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::process::Command;

    const CHILD: &str = "CLIPSYNC_CA_CONFIG_CHILD";

    fn child(case: &str, path: Option<&std::path::Path>, valid: &std::path::Path) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tls::tests::configured_ca_child", "--nocapture"])
            .env(CHILD, case)
            .env("CLIPSYNC_TEST_VALID_CA", valid);
        if let Some(path) = path {
            command.env(CA_FILE_ENV, path);
        } else {
            command.env_remove(CA_FILE_ENV);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "isolated CA case {case}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn isolated_ca_files_reject_missing_empty_malformed_and_invalid_certificates() {
        let directory = tempfile::tempdir().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let certificate = params.self_signed(&key).unwrap();
        let valid = directory.path().join("valid.pem");
        std::fs::write(&valid, certificate.pem()).unwrap();
        child("default", None, &valid);
        child("default", Some(std::path::Path::new("")), &valid);
        child("io", Some(&directory.path().join("missing.pem")), &valid);
        child("io", Some(directory.path()), &valid);
        let empty = directory.path().join("empty.pem");
        std::fs::write(&empty, "no certificates here").unwrap();
        child("empty", Some(&empty), &valid);
        let malformed = directory.path().join("malformed.pem");
        std::fs::write(
            &malformed,
            "-----BEGIN CERTIFICATE-----\n%%%\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        child("pem", Some(&malformed), &valid);
        let invalid_der = directory.path().join("invalid-der.pem");
        std::fs::write(
            &invalid_der,
            "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        child("der", Some(&invalid_der), &valid);
        for (case, contents) in [
            (
                "mixed-der-first",
                format!(
                    "{}{}",
                    std::fs::read_to_string(&invalid_der).unwrap(),
                    certificate.pem()
                ),
            ),
            (
                "mixed-der-last",
                format!(
                    "{}{}",
                    certificate.pem(),
                    std::fs::read_to_string(&invalid_der).unwrap()
                ),
            ),
        ] {
            let mixed = directory.path().join(format!("{case}.pem"));
            std::fs::write(&mixed, contents).unwrap();
            child(case, Some(&mixed), &valid);
        }
        let bundle = directory.path().join("bundle.pem");
        std::fs::write(&bundle, certificate.pem().repeat(2)).unwrap();
        child("bundle", Some(&bundle), &valid);
        child("recover", Some(&invalid_der), &valid);
    }

    #[test]
    fn configured_ca_child() {
        let Ok(case) = std::env::var(CHILD) else {
            return;
        };
        match case.as_str() {
            "default" => {
                assert!(ca_file_path().is_none());
                assert!(load_extra_cas().unwrap().is_empty());
                let first = client_config().unwrap();
                assert!(Arc::ptr_eq(&first, &client_config().unwrap()));
                configure_reqwest(reqwest::Client::builder().no_proxy())
                    .unwrap()
                    .build()
                    .unwrap();
            }
            "bundle" => {
                assert_eq!(load_extra_cas().unwrap().len(), 2);
                assert!(client_config().is_ok());
            }
            "recover" => {
                assert!(client_config().is_err());
                let path = ca_file_path().unwrap();
                std::fs::copy(std::env::var("CLIPSYNC_TEST_VALID_CA").unwrap(), &path).unwrap();
                let first = client_config().unwrap();
                std::fs::write(path, "invalid after successful initialization").unwrap();
                assert!(
                    Arc::ptr_eq(&first, &client_config().unwrap()),
                    "successful configuration is immutable for this process"
                );
            }
            "io" | "empty" | "pem" | "der" | "mixed-der-first" | "mixed-der-last" => {
                let expected = match case.as_str() {
                    "empty" => "contained no certificates",
                    "pem" => "parse:",
                    "der" | "mixed-der-first" => "certificate 1 invalid DER:",
                    "mixed-der-last" => "certificate 2 invalid DER:",
                    _ => CA_FILE_ENV,
                };
                let error = match load_extra_cas() {
                    Err(TransportError::Protocol(error)) => error,
                    other => panic!("invalid CA configuration was accepted: {other:?}"),
                };
                assert!(
                    error.contains(expected) && error.contains(CA_FILE_ENV),
                    "{error}"
                );
                if matches!(case.as_str(), "der" | "mixed-der-first" | "mixed-der-last") {
                    assert!(error.contains(&ca_file_path().unwrap()), "{error}");
                }
                assert!(client_config().is_err());
                assert!(configure_reqwest(reqwest::Client::builder().no_proxy()).is_err());
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                assert!(
                    matches!(
                        runtime.block_on(crate::RelayConnection::connect("ws://127.0.0.1:0/v1/ws")),
                        Err(TransportError::Protocol(_))
                    ),
                    "a bad CA configuration must fail before any connection attempt"
                );
            }
            _ => panic!("unknown isolated CA fixture"),
        }
    }
}
