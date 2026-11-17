// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::path::Path;
use std::process::{Command, Output};

use clipsync_relay::RelayConfig;

const SECRET_ENV: &str = "CLIPSYNC_RELAY_TOKEN_SECRET";
const CASE_ENV: &str = "CLIPSYNC_RELAY_CONFIG_TEST_CASE";
const VALID_TEST_KEY: &str = "0123456789abcdef0123456789abcdef";
const METRICS_ENV: &str = "CLIPSYNC_RELAY_METRICS_TOKEN";
const METRICS_FILE_ENV: &str = "CLIPSYNC_RELAY_METRICS_TOKEN_FILE";
const METRICS_EXPECT_ENV: &str = "CLIPSYNC_RELAY_CONFIG_TEST_METRICS";
const METRICS_TEST_TOKEN: &str = "config-metrics-scrape-token-0123456789";

// Each child configures its own environment before running real production code.
// The test process never changes global environment variables or prints key bytes.
fn config_child(case: &str, secret: Option<&str>) -> Output {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "relay_config_from_env_child", "--nocapture"])
        .env(CASE_ENV, case)
        .env_remove("CLIPSYNC_RELAY_TOKEN_SECRET_FILE")
        .env_remove("CLIPSYNC_RELAY_BIND")
        .env_remove("CLIPSYNC_RELAY_PUBLIC_URL")
        .env_remove("CLIPSYNC_RELAY_WEBHOOK_URL")
        .env_remove("CLIPSYNC_RELAY_WEBHOOK_SECRET")
        .env_remove(METRICS_ENV)
        .env_remove(METRICS_FILE_ENV)
        .env("RUST_BACKTRACE", "0");
    if let Some(secret) = secret {
        command.env(SECRET_ENV, secret);
    } else {
        command.env_remove(SECRET_ENV);
    }
    command.output().expect("run isolated config check")
}

fn assert_child_success(output: Output) {
    assert!(
        output.status.success(),
        "isolated config check failed: {}",
        output.status
    );
}

#[test]
fn absent_token_secret_generates_fresh_keys() {
    assert_child_success(config_child("random", None));
}

#[test]
fn empty_token_secret_generates_fresh_keys() {
    assert_child_success(config_child("random", Some("")));
}

#[test]
fn whitespace_only_token_secret_generates_fresh_keys() {
    for value in [" ", "\t \r\n", "\u{2003}"] {
        assert_child_success(config_child("random", Some(value)));
    }
}

#[test]
fn minimum_length_token_secret_is_preserved() {
    assert_child_success(config_child("preserve", Some(VALID_TEST_KEY)));
}

#[test]
fn valid_token_secret_preserves_surrounding_whitespace() {
    let key = format!(" {VALID_TEST_KEY} ");
    assert_child_success(config_child("preserve", Some(&key)));
}

#[test]
fn token_secret_minimum_counts_bytes_instead_of_characters() {
    let key = "\u{1f980}".repeat(8);
    assert_child_success(config_child("preserve", Some(&key)));
}

#[test]
fn short_explicit_token_secrets_are_rejected_without_disclosing_them() {
    for key in ["change-me-in-production", "0123456789abcdef0123456789abcde"] {
        let output = config_child("load", Some(key));
        assert!(
            !output.status.success(),
            "short explicit token secret was accepted"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(SECRET_ENV),
            "error must identify the setting"
        );
        assert!(
            stderr.contains("32"),
            "error must explain the minimum length"
        );
        assert!(
            !stderr.contains(key) && !String::from_utf8_lossy(&output.stdout).contains(key),
            "configuration failure disclosed key material"
        );
    }
}

#[test]
fn relay_config_from_env_child() {
    let Ok(case) = std::env::var(CASE_ENV) else {
        return;
    };
    let first = RelayConfig::from_env();
    match case.as_str() {
        "random" => {
            let second = RelayConfig::from_env();
            assert_eq!(first.token_secret.len(), 32);
            assert_eq!(second.token_secret.len(), 32);
            assert!(first.token_secret != second.token_secret, "key was reused");
        }
        "file" => {
            assert!(first.token_secret == [0u8; 32], "file key bytes changed");
        }
        "preserve" => {
            let supplied = std::env::var(SECRET_ENV).expect("explicit test key");
            assert!(
                first.token_secret == supplied.as_bytes(),
                "explicit key bytes changed"
            );
        }
        "load" => {}
        "metrics" => {
            let token = first.metrics_token.expect("metrics enabled");
            let expected = std::env::var(METRICS_EXPECT_ENV).expect("expected metrics token");
            assert!(token.matches(expected.as_bytes()), "metrics token changed");
            assert!(!token.matches(b"x"), "metrics token accepted a wrong value");
        }
        "no-metrics" => assert!(first.metrics_token.is_none(), "metrics were enabled"),
        _ => panic!("unknown isolated config test case"),
    }
}

#[test]
fn token_secret_file_preserves_binary_key_and_rejects_ambiguous_or_bad_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("token.secret");
    std::fs::write(&path, [0u8; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let run = |secret: Option<&str>| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "relay_config_from_env_child", "--nocapture"])
            .env(CASE_ENV, "file")
            .env("CLIPSYNC_RELAY_BIND", "127.0.0.1:0")
            .env("CLIPSYNC_RELAY_TOKEN_SECRET_FILE", &path)
            .env_remove(METRICS_ENV)
            .env_remove(METRICS_FILE_ENV);
        if let Some(value) = secret {
            command.env(SECRET_ENV, value);
        } else {
            command.env_remove(SECRET_ENV);
        }
        command.output().unwrap()
    };
    assert_child_success(run(None));
    for value in ["", VALID_TEST_KEY] {
        let output = run(Some(value));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("configure only one"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(VALID_TEST_KEY));
    }
    std::fs::write(&path, [0u8; 31]).unwrap();
    let output = run(None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("at least 32 bytes"));
    std::fs::remove_file(&path).unwrap();
    let output = run(None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("CLIPSYNC_RELAY_TOKEN_SECRET_FILE"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, [0u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let output = run(None);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("private credential permissions"));
    }
}

#[cfg(unix)]
#[test]
fn token_secret_symlink_preserves_private_regular_file_bytes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("token.secret");
    let link = directory.path().join("token-link.secret");
    std::fs::write(&path, [0u8; 32]).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    symlink("token.secret", &link).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "relay_config_from_env_child", "--nocapture"])
        .env(CASE_ENV, "file")
        .env("CLIPSYNC_RELAY_BIND", "127.0.0.1:0")
        .env("CLIPSYNC_RELAY_TOKEN_SECRET_FILE", link)
        .env_remove(SECRET_ENV)
        .env_remove(METRICS_ENV)
        .env_remove(METRICS_FILE_ENV)
        .output()
        .unwrap();
    assert_child_success(output);
}

/// Run the isolated child with a valid token key and the given metrics settings.
fn metrics_child(case: &str, value: Option<&str>, file: Option<&Path>, expected: &str) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "relay_config_from_env_child", "--nocapture"])
        .env(CASE_ENV, case)
        .env(SECRET_ENV, VALID_TEST_KEY)
        .env(METRICS_EXPECT_ENV, expected)
        .env("CLIPSYNC_RELAY_BIND", "127.0.0.1:0")
        .env_remove("CLIPSYNC_RELAY_TOKEN_SECRET_FILE")
        .env("RUST_BACKTRACE", "0");
    match value {
        Some(value) => command.env(METRICS_ENV, value),
        None => command.env_remove(METRICS_ENV),
    };
    match file {
        Some(file) => command.env(METRICS_FILE_ENV, file),
        None => command.env_remove(METRICS_FILE_ENV),
    };
    command.output().unwrap()
}

fn assert_child_rejects(output: Output, setting: &str, reason: &str) {
    assert_child_rejects_without(output, setting, reason, METRICS_TEST_TOKEN);
}

fn assert_child_rejects_without(output: Output, setting: &str, reason: &str, secret: &str) {
    assert!(
        !output.status.success(),
        "invalid metrics setting was accepted"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(setting),
        "error must identify {setting}: {stderr}"
    );
    assert!(
        stderr.contains(reason),
        "error must explain {reason}: {stderr}"
    );
    assert!(
        !stderr.contains(secret) && !String::from_utf8_lossy(&output.stdout).contains(secret),
        "configuration failure disclosed the metrics token"
    );
}

#[test]
fn metrics_stay_disabled_when_the_token_is_unset_empty_or_blank() {
    for value in [None, Some(""), Some(" \t")] {
        assert_child_success(metrics_child("no-metrics", value, None, ""));
    }
}

#[test]
fn metrics_token_from_environment_enables_the_route_with_that_exact_token() {
    assert_child_success(metrics_child(
        "metrics",
        Some(METRICS_TEST_TOKEN),
        None,
        METRICS_TEST_TOKEN,
    ));
}

#[test]
fn short_or_non_header_metrics_tokens_are_rejected_without_disclosing_them() {
    let short = &METRICS_TEST_TOKEN[..clipsync_relay::MIN_METRICS_TOKEN_BYTES - 1];
    let output = metrics_child("load", Some(short), None, "");
    assert_child_rejects_without(output, METRICS_ENV, "at least 32 bytes", short);
    let spaced = format!(" {METRICS_TEST_TOKEN}");
    let output = metrics_child("load", Some(&spaced), None, "");
    assert_child_rejects(output, METRICS_ENV, "visible ASCII");
}

#[test]
fn metrics_token_file_trims_its_newline_and_rejects_ambiguous_or_bad_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metrics.token");
    std::fs::write(&path, format!("{METRICS_TEST_TOKEN}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_child_success(metrics_child(
        "metrics",
        None,
        Some(&path),
        METRICS_TEST_TOKEN,
    ));
    for value in ["", METRICS_TEST_TOKEN] {
        let output = metrics_child("load", Some(value), Some(&path), "");
        assert_child_rejects(output, METRICS_FILE_ENV, "configure only one");
    }
    std::fs::write(&path, "\n").unwrap();
    let output = metrics_child("load", None, Some(&path), "");
    assert_child_rejects(output, METRICS_FILE_ENV, "at least 32 bytes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, METRICS_TEST_TOKEN).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let output = metrics_child("load", None, Some(&path), "");
        assert_child_rejects(output, METRICS_FILE_ENV, "private credential permissions");
    }
    std::fs::remove_file(&path).unwrap();
    let output = metrics_child("load", None, Some(&path), "");
    assert_child_rejects(output, METRICS_FILE_ENV, "cannot open");
}
