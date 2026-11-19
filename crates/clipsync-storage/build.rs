// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Tells `src/secrets.rs` whether Cargo is building a release-profile target,
//! so that its guard against the test-only `test-isolated-keychain` feature
//! does not rest on `debug_assertions` alone: a release profile can turn debug
//! assertions back on (`CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true`).

/// `cfg` set on release-profile builds of this crate.
const RELEASE_PROFILE_CFG: &str = "clipsync_release_profile";

/// Cargo's `PROFILE` value for `release` and every profile that inherits from
/// it, whatever its debug-assertions or opt-level settings.
const RELEASE_PROFILE: &str = "release";

fn main() {
    println!("cargo::rustc-check-cfg=cfg({RELEASE_PROFILE_CFG})");
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("PROFILE").as_deref() == Ok(RELEASE_PROFILE) {
        println!("cargo::rustc-cfg={RELEASE_PROFILE_CFG}");
    }
}
