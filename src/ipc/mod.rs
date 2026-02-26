//! Inter-process communication for the watcher daemon.
//!
//! On Unix, uses Unix domain sockets for IPC.
//! On Windows, uses TCP on localhost with a port file for daemon discovery.

mod client;
mod protocol;
mod server;

pub use client::{print_response, IpcClient};
pub use protocol::{IpcCommand, IpcData, IpcResponse, WatcherState};
pub use server::IpcServer;

use std::path::PathBuf;

/// Returns a private runtime directory for IPC files, scoped to the current user.
///
/// On Linux, prefers `XDG_RUNTIME_DIR` (already user-private, typically mode 0700).
/// On Windows, uses `%LOCALAPPDATA%\claudear` or falls back to `%TEMP%\claudear`.
/// Otherwise, creates a subdirectory `claudear-{uid}` under the system temp dir
/// with mode 0700 to prevent other users from accessing the socket/PID files.
fn ipc_runtime_dir() -> PathBuf {
    #[cfg(windows)]
    {
        // Prefer %LOCALAPPDATA%\claudear (user-private on Windows)
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let dir = PathBuf::from(local_app_data).join("claudear");
            if !dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    tracing::error!(
                        "Failed to create IPC runtime dir {:?}: {} — falling back to temp dir",
                        dir,
                        e
                    );
                    return std::env::temp_dir().join("claudear");
                }
            }
            return dir;
        }
        // Fallback: %TEMP%\claudear
        let dir = std::env::temp_dir().join("claudear");
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
        }
        dir
    }

    #[cfg(not(windows))]
    {
        // On Linux, XDG_RUNTIME_DIR is already user-private
        if !cfg!(target_os = "macos") {
            if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
                return PathBuf::from(xdg);
            }
        }

        // Fallback: create a user-scoped subdirectory in the temp dir with restricted permissions
        let uid = unsafe { libc::getuid() };
        let dir = std::env::temp_dir().join(format!("claudear-{}", uid));
        if !dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                // SECURITY: Falling back to the system temp dir is unsafe because it is
                // world-readable, which could allow other users to access or tamper with
                // the IPC socket. This should be treated as a critical failure.
                tracing::error!("Failed to create IPC runtime dir {:?}: {} — falling back to world-readable temp dir", dir, e);
                return std::env::temp_dir();
            }
            // Set directory permissions to 0700 (owner-only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o700);
                if let Err(e) = std::fs::set_permissions(&dir, perms) {
                    tracing::warn!("Failed to set IPC runtime dir permissions: {}", e);
                }
            }
        }
        dir
    }
}

/// Default socket path for the IPC server (Unix) or port file path (Windows).
pub fn default_socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        ipc_runtime_dir().join("claudear.port")
    }
    #[cfg(not(windows))]
    {
        ipc_runtime_dir().join("claudear.sock")
    }
}

/// Default PID file path.
pub fn default_pid_path() -> PathBuf {
    ipc_runtime_dir().join("claudear.pid")
}

/// Check if a watcher daemon is running.
pub fn is_daemon_running() -> bool {
    #[cfg(windows)]
    {
        let port_path = default_socket_path();
        if !port_path.exists() {
            return false;
        }
        if let Ok(contents) = std::fs::read_to_string(&port_path) {
            if let Ok(port) = contents.trim().parse::<u16>() {
                return std::net::TcpStream::connect(("127.0.0.1", port)).is_ok();
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        let socket_path = default_socket_path();
        socket_path.exists() && std::os::unix::net::UnixStream::connect(&socket_path).is_ok()
    }
}

/// Get the PID of the running daemon, if any.
pub fn get_daemon_pid() -> Option<u32> {
    let pid_path = default_pid_path();
    if pid_path.exists() {
        std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    } else {
        None
    }
}

/// Write the current process PID to the pid file.
pub fn write_pid_file() -> std::io::Result<()> {
    let pid_path = default_pid_path();
    std::fs::write(&pid_path, std::process::id().to_string())
}

/// Remove the PID file.
pub fn remove_pid_file() {
    let pid_path = default_pid_path();
    let _ = std::fs::remove_file(&pid_path);
}

/// Remove the socket file (Unix) or port file (Windows).
pub fn remove_socket_file() {
    let socket_path = default_socket_path();
    let _ = std::fs::remove_file(&socket_path);
}

/// Clean up stale socket/pid files from a previous crash.
pub fn cleanup_stale_files() {
    if let Some(pid) = get_daemon_pid() {
        let is_running = is_process_running(pid);
        if !is_running {
            tracing::info!("Cleaning up stale files from previous run (PID {})", pid);
            remove_pid_file();
            remove_socket_file();
        }
    } else {
        // No PID file but socket/port file exists - check if stale
        let socket_path = default_socket_path();
        if socket_path.exists() {
            let is_stale = {
                #[cfg(windows)]
                {
                    if let Ok(contents) = std::fs::read_to_string(&socket_path) {
                        if let Ok(port) = contents.trim().parse::<u16>() {
                            std::net::TcpStream::connect(("127.0.0.1", port)).is_err()
                        } else {
                            true // Invalid port file content
                        }
                    } else {
                        true
                    }
                }
                #[cfg(not(windows))]
                {
                    std::os::unix::net::UnixStream::connect(&socket_path).is_err()
                }
            };
            if is_stale {
                tracing::info!("Cleaning up stale socket file");
                remove_socket_file();
            }
        }
    }
}

/// Check if a process with the given PID is running.
fn is_process_running(pid: u32) -> bool {
    // Try to check /proc on Linux
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }

    // On macOS/BSD, use kill(pid, 0) to check if process exists
    #[cfg(target_os = "macos")]
    {
        // SAFETY: kill with signal 0 doesn't actually send a signal,
        // it just checks if the process exists and we have permission to signal it.
        // Returns 0 if process exists, -1 if not (with errno set to ESRCH).
        match i32::try_from(pid) {
            Ok(pid_i32) => unsafe { libc::kill(pid_i32, 0) == 0 },
            Err(_) => false, // PID exceeds i32::MAX, cannot be valid
        }
    }

    // On Windows, use OpenProcess to check if a process handle can be obtained
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: OpenProcess with PROCESS_QUERY_LIMITED_INFORMATION is a read-only check.
        // If the process exists and we have permission, it returns a valid handle.
        // We immediately close the handle after the check.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle != 0 {
                CloseHandle(handle);
                true
            } else {
                false
            }
        }
    }

    // Fallback for other platforms
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = pid; // Suppress unused warning
                     // Assume running if we can't check
        true
    }
}

/// Read the TCP port from the port file (Windows only).
#[cfg(windows)]
pub(crate) fn read_port_from_file() -> Option<u16> {
    let port_path = default_socket_path();
    std::fs::read_to_string(&port_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Write the TCP port to the port file (Windows only).
#[cfg(windows)]
pub(crate) fn write_port_file(port: u16) -> std::io::Result<()> {
    let port_path = default_socket_path();
    std::fs::write(&port_path, port.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_socket_path_has_expected_extension() {
        let path = default_socket_path();
        #[cfg(windows)]
        assert!(
            path.ends_with("claudear.port"),
            "Expected socket path to end with 'claudear.port', got: {:?}",
            path
        );
        #[cfg(not(windows))]
        assert!(
            path.ends_with("claudear.sock"),
            "Expected socket path to end with 'claudear.sock', got: {:?}",
            path
        );
    }

    #[test]
    fn test_default_pid_path_ends_with_claudear_pid() {
        let path = default_pid_path();
        assert!(
            path.ends_with("claudear.pid"),
            "Expected pid path to end with 'claudear.pid', got: {:?}",
            path
        );
    }

    #[test]
    fn test_ipc_runtime_dir_is_absolute() {
        let dir = ipc_runtime_dir();
        assert!(
            dir.is_absolute(),
            "Expected ipc_runtime_dir to return an absolute path, got: {:?}",
            dir
        );
    }

    #[test]
    fn test_get_daemon_pid_returns_option() {
        // This tests that get_daemon_pid does not panic and returns an Option<u32>.
        // If no PID file exists, it returns None. If one does exist (from a running
        // daemon), it returns Some(pid). Either outcome is acceptable.
        let result = get_daemon_pid();
        // Just verify the function completes without panicking.
        // If a daemon happens to be running, the PID should be > 0.
        if let Some(pid) = result {
            assert!(pid > 0, "PID should be positive, got: {}", pid);
        }
    }

    #[test]
    fn test_write_and_remove_pid_file_does_not_panic() {
        // We avoid actually writing the PID file if a daemon is running,
        // as that could interfere with the running daemon. Instead, we test
        // the functions only when no daemon is active.
        if is_daemon_running() {
            // A daemon is running; skip this test to avoid interfering.
            return;
        }

        // Save any existing PID file content so we can restore it.
        let pid_path = default_pid_path();
        let existing_content = std::fs::read_to_string(&pid_path).ok();

        // Write our PID file.
        let write_result = write_pid_file();
        assert!(write_result.is_ok(), "write_pid_file should succeed");

        // Verify get_daemon_pid returns our PID.
        let read_pid = get_daemon_pid();
        assert_eq!(
            read_pid,
            Some(std::process::id()),
            "get_daemon_pid should return our process PID after write_pid_file"
        );

        // Remove the PID file.
        remove_pid_file();

        // Verify the PID file is gone (or restore the original if there was one).
        if let Some(content) = existing_content {
            // Restore original content for the running daemon.
            let _ = std::fs::write(&pid_path, content);
        } else {
            assert!(
                !pid_path.exists(),
                "PID file should be removed after remove_pid_file"
            );
        }
    }

    #[test]
    fn test_remove_socket_file_does_not_panic_when_no_socket() {
        // remove_socket_file should silently succeed even if no socket file exists.
        // We cannot unconditionally call it because a daemon might be using the socket.
        if !is_daemon_running() {
            // The socket file may or may not exist; either way this should not panic.
            remove_socket_file();
        }
    }

    #[test]
    fn test_is_daemon_running_returns_bool() {
        // When no daemon is running, this should return false.
        // If a daemon happens to be running (e.g., in a dev environment), true is also valid.
        let result = is_daemon_running();
        let _: bool = result;
    }

    #[test]
    fn test_is_process_running_with_own_pid() {
        // Our own process is guaranteed to be running and we have permission to signal it.
        let own_pid = std::process::id();
        assert!(
            is_process_running(own_pid),
            "Our own PID ({}) should be reported as running",
            own_pid
        );
    }

    #[test]
    fn test_is_process_running_with_invalid_pid() {
        // u32::MAX is extremely unlikely to be a valid PID on any system.
        assert!(
            !is_process_running(u32::MAX),
            "PID u32::MAX should not be reported as running"
        );
    }

    #[test]
    fn test_cleanup_stale_files_does_not_panic() {
        // cleanup_stale_files should handle all cases gracefully:
        // no PID file, stale socket, running daemon, etc.
        cleanup_stale_files();
    }
}
