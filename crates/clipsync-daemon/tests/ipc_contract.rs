// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use clipsync_daemon::{
    connect_ipc, pid_is_running, read_request, remove_pid, write_pid, write_response, IpcRequest,
    IpcResponse, IpcServer, PullPayload,
};
use clipsync_storage::AppPaths;
use tokio::io::AsyncWriteExt;

fn paths(root: &tempfile::TempDir) -> AppPaths {
    let paths = AppPaths::from_root(root.path().to_path_buf());
    paths.ensure().unwrap();
    paths
}

#[test]
fn pid_records_distinguish_missing_malformed_live_and_dead_processes() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(&root);
    assert!(!pid_is_running(&paths));
    std::fs::write(paths.pid_file(), "not a pid").unwrap();
    assert!(!pid_is_running(&paths));
    write_pid(&paths).unwrap();
    assert!(pid_is_running(&paths));
    std::fs::write(paths.pid_file(), i32::MAX.to_string()).unwrap();
    assert!(!pid_is_running(&paths));
    remove_pid(&paths);
    remove_pid(&paths);
    assert!(!paths.pid_file().exists());
}

#[tokio::test]
async fn ipc_framing_preserves_unicode_and_reports_malformed_requests() {
    let (mut writer, mut reader) = tokio::io::duplex(4096);
    writer
        .write_all("{\"cmd\":\"push_text\",\"text\":\"line\\n한글\"}\n".as_bytes())
        .await
        .unwrap();
    assert!(
        matches!(read_request(&mut reader).await.unwrap(), Some(IpcRequest::PushText { text }) if text == "line\n한글")
    );
    writer.write_all(b"not-json\n").await.unwrap();
    assert!(read_request(&mut reader).await.is_err());
    writer.shutdown().await.unwrap();
    assert!(read_request(&mut reader).await.unwrap().is_none());
}

#[tokio::test]
async fn private_ipc_roundtrips_multiple_requests_and_removes_its_socket() {
    const CHILD: &str = "CLIPSYNC_TEST_ISOLATED_IPC";
    if std::env::var_os(CHILD).is_none() {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "private_ipc_roundtrips_multiple_requests_and_removes_its_socket",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove(clipsync_daemon::LISTEN_ENV)
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(20), command.output())
            .await
            .expect("isolated IPC exchange exceeded its deadline")
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let paths = paths(&root);
    assert!(connect_ipc(&paths).await.is_err());
    let server = IpcServer::bind(&paths).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(paths.socket_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut client = connect_ipc(&paths).await.unwrap();
    let responder = tokio::spawn(async move {
        let mut stream = server.accept().await.unwrap();
        assert!(matches!(
            read_request(&mut stream).await.unwrap(),
            Some(IpcRequest::Pause)
        ));
        write_response(
            &mut stream,
            &IpcResponse {
                ok: true,
                error: None,
                status: None,
                pull: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_request(&mut stream).await.unwrap(),
            Some(IpcRequest::Pull)
        ));
        write_response(
            &mut stream,
            &IpcResponse {
                ok: true,
                error: None,
                status: None,
                pull: Some(PullPayload {
                    kind: "text".into(),
                    text: Some("private\n한글".into()),
                    summary: "received".into(),
                }),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_request(&mut stream).await.unwrap(),
            Some(IpcRequest::Status)
        ));
        stream.write_all(b"invalid response\n").await.unwrap();
    });
    let paused = client.request(&IpcRequest::Pause).await.unwrap();
    assert!(paused.ok);
    assert!(paused.pull.is_none());
    let pulled = client.request(&IpcRequest::Pull).await.unwrap();
    assert_eq!(pulled.pull.unwrap().text.as_deref(), Some("private\n한글"));
    assert!(client.request(&IpcRequest::Status).await.is_err());
    responder.await.unwrap();
    #[cfg(unix)]
    assert!(!paths.socket_file().exists());
}
