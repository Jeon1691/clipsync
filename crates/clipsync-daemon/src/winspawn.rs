//! Windows child-process isolation so `Command::output()` does not wait on grandchildren.

use std::process::{Child, Command};

/// Spawn `cmd` without inheriting the current stdout/stderr pipes.
///
/// On Windows, `CreateProcess` copies every inheritable handle when any stdio
/// handle is inherited. The parent's stdout pipe then stays open in the child,
/// so a caller using `Command::output()` blocks until that grandchild exits.
pub fn spawn_isolated(cmd: &mut Command) -> std::io::Result<Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        let _guard = StdioInheritGuard::clear();
        match cmd.spawn() {
            Ok(child) => Ok(child),
            Err(_) => {
                cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
                cmd.spawn()
            }
        }
    }
    #[cfg(not(windows))]
    {
        cmd.spawn()
    }
}

#[cfg(windows)]
struct StdioInheritGuard {
    handles: [*mut core::ffi::c_void; 3],
}

#[cfg(windows)]
impl StdioInheritGuard {
    fn clear() -> Self {
        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
        const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
        const STD_ERROR_HANDLE: u32 = (-12i32) as u32;
        extern "system" {
            fn GetStdHandle(n: u32) -> *mut core::ffi::c_void;
            fn SetHandleInformation(h: *mut core::ffi::c_void, mask: u32, flags: u32) -> i32;
        }
        unsafe {
            let handles = [
                GetStdHandle(STD_INPUT_HANDLE),
                GetStdHandle(STD_OUTPUT_HANDLE),
                GetStdHandle(STD_ERROR_HANDLE),
            ];
            let invalid = (-1isize) as *mut core::ffi::c_void;
            for h in handles {
                if !h.is_null() && h != invalid {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
            Self { handles }
        }
    }
}

#[cfg(windows)]
impl Drop for StdioInheritGuard {
    fn drop(&mut self) {
        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        extern "system" {
            fn SetHandleInformation(h: *mut core::ffi::c_void, mask: u32, flags: u32) -> i32;
        }
        let invalid = (-1isize) as *mut core::ffi::c_void;
        unsafe {
            for h in self.handles {
                if !h.is_null() && h != invalid {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
                }
            }
        }
    }
}
