//! Inter-process communication via Unix socket.
//!
//! Enables the CLI to communicate with a running watcher daemon.

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
/// Otherwise, creates a subdirectory `claudear-{uid}` under the system temp dir
/// with mode 0700 to prevent other users from accessing the socket/PID files.
fn ipc_runtime_dir() -> PathBuf {
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
            tracing::warn!("Failed to create IPC runtime dir {:?}: {}", dir, e);
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

/// Default socket path for the IPC server.
pub fn default_socket_path() -> PathBuf {
    ipc_runtime_dir().join("claudear.sock")
}

/// Default PID file path.
pub fn default_pid_path() -> PathBuf {
    ipc_runtime_dir().join("claudear.pid")
}

/// Check if a watcher daemon is running.
pub fn is_daemon_running() -> bool {
    let socket_path = default_socket_path();
    socket_path.exists() && std::os::unix::net::UnixStream::connect(&socket_path).is_ok()
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

/// Remove the socket file.
pub fn remove_socket_file() {
    let socket_path = default_socket_path();
    let _ = std::fs::remove_file(&socket_path);
}

/// Clean up stale socket/pid files from a previous crash.
pub fn cleanup_stale_files() {
    if let Some(pid) = get_daemon_pid() {
        // Check if process is still running by trying to read /proc/{pid} or using kill -0
        let is_running = is_process_running(pid);
        if !is_running {
            tracing::info!("Cleaning up stale files from previous run (PID {})", pid);
            remove_pid_file();
            remove_socket_file();
        }
    } else {
        // No PID file but socket exists - stale
        let socket_path = default_socket_path();
        if socket_path.exists() && std::os::unix::net::UnixStream::connect(&socket_path).is_err() {
            tracing::info!("Cleaning up stale socket file");
            remove_socket_file();
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

    // Fallback for other platforms
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid; // Suppress unused warning
                     // Assume running if we can't check
        true
    }
}
