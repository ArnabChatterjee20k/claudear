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

/// Default socket path for the IPC server.
pub fn default_socket_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        // macOS: use /tmp or user's temp dir
        std::env::temp_dir().join("claudear.sock")
    } else {
        // Linux: use XDG_RUNTIME_DIR if available, otherwise /tmp
        std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("claudear.sock")
    }
}

/// Default PID file path.
pub fn default_pid_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        std::env::temp_dir().join("claudear.pid")
    } else {
        std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("claudear.pid")
    }
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
        if socket_path.exists()
            && std::os::unix::net::UnixStream::connect(&socket_path).is_err()
        {
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
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    // Fallback for other platforms
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid; // Suppress unused warning
        // Assume running if we can't check
        true
    }
}
