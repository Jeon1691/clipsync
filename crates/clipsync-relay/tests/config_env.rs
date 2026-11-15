// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::process::{Command, Output};

use clipsync_relay::RelayConfig;

const SECRET_ENV: &str = "CLIPSYNC_RELAY_TOKEN_SECRET";
const CASE_ENV: &str = "CLIPSYNC_RELAY_CONFIG_TEST_CASE";
const VALID_TEST_KEY: &str = "0123456789abcdef0123456789abcdef";

// Each child configures its own environment before running real production code.
// The test process never changes global environment variables or prints key bytes.
fn config_child(case: &str, secret: Option<&str>) -> Output {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "relay_config_from_env_child", "--nocapture"])
        .env(CASE_ENV, case)
        .env_remove("CLIPSYNC_RELAY_BIND")
        .env_remove("CLIPSYNC_RELAY_PUBLIC_URL")
        .env_remove("CLIPSYNC_RELAY_WEBHOOK_URL")
        .env_remove("CLIPSYNC_RELAY_WEBHOOK_SECRET")
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
        "preserve" => {
            let supplied = std::env::var(SECRET_ENV).expect("explicit test key");
            assert!(
                first.token_secret == supplied.as_bytes(),
                "explicit key bytes changed"
            );
        }
        "load" => {}
        _ => panic!("unknown isolated config test case"),
    }
}
