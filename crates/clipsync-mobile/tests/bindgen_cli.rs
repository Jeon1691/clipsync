// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! The shipped binding generator must expose its CLI without loading a user home.

use std::time::Duration;

#[tokio::test]
async fn binding_generator_help_and_missing_input_are_actionable_without_writing_files() {
    let root = tempfile::tempdir().unwrap();
    for (arguments, success) in [(vec!["--help"], true), (vec!["generate"], false)] {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_uniffi-bindgen"));
        command
            .args(arguments)
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("CLIPSYNC_HOME", root.path())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(5), command.output())
            .await
            .expect("binding generator did not terminate")
            .unwrap();
        assert_eq!(output.status.success(), success);
        let text = if success {
            &output.stdout
        } else {
            &output.stderr
        };
        assert!(String::from_utf8_lossy(text).contains("generate"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
