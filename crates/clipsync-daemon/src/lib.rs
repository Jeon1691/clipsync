//! User-level service lifecycle (launchd/systemd/Windows logon) and IPC.

mod ipc;
mod service;
mod winspawn;

pub use ipc::{
    connect_ipc, read_request, write_response, IpcClient, IpcRequest, IpcResponse, IpcServer,
    PullPayload, StatusPayload, LISTEN_ENV,
};
pub use service::{install_and_start, is_service_installed, restart_service, stop_and_uninstall};
pub use winspawn::spawn_isolated;

use clipsync_storage::AppPaths;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, DaemonError>;

pub fn pid_is_running(paths: &AppPaths) -> bool {
    let pid_path = paths.pid_file();
    let Ok(raw) = std::fs::read_to_string(pid_path) else {
        return false;
    };
    let Ok(pid) = raw.trim().parse::<i32>() else {
        return false;
    };
    #[cfg(unix)]
    {
        libc_kill(pid, 0) == 0
    }
    #[cfg(windows)]
    {
        windows_pid_alive(pid as u32)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(windows)]
fn windows_pid_alive(pid: u32) -> bool {
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            false
        } else {
            CloseHandle(handle);
            true
        }
    }
}

#[cfg(unix)]
fn libc_kill(pid: i32, sig: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, sig) }
}

pub fn write_pid(paths: &AppPaths) -> std::io::Result<()> {
    std::fs::write(paths.pid_file(), format!("{}", std::process::id()))
}

pub fn remove_pid(paths: &AppPaths) {
    let _ = std::fs::remove_file(paths.pid_file());
}
