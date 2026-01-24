//! IPC client for communicating with the watcher daemon.

use super::default_socket_path;
use super::protocol::{IpcCommand, IpcData, IpcResponse};
use crate::error::{Error, Result};

use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

/// Default timeout for IPC operations.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Client for communicating with the watcher daemon via Unix socket.
pub struct IpcClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl IpcClient {
    /// Create a new IPC client with the default socket path.
    pub fn new() -> Self {
        Self {
            socket_path: default_socket_path(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Create a client with a custom socket path.
    pub fn with_socket_path(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Set the timeout for operations.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Check if the daemon is running.
    pub fn is_daemon_running(&self) -> bool {
        self.socket_path.exists()
            && std::os::unix::net::UnixStream::connect(&self.socket_path).is_ok()
    }

    /// Send a command and receive a response.
    pub async fn send(&self, command: IpcCommand) -> Result<IpcResponse> {
        let result = timeout(self.timeout, self.send_internal(command)).await;

        match result {
            Ok(inner) => inner,
            Err(_) => Err(Error::Other("IPC request timed out".to_string())),
        }
    }

    async fn send_internal(&self, command: IpcCommand) -> Result<IpcResponse> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| Error::Other(format!("Failed to connect to daemon: {}", e)))?;

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        // Send command
        let json = serde_json::to_string(&command)? + "\n";
        writer.write_all(json.as_bytes()).await?;

        // Read response
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let response: IpcResponse = serde_json::from_str(line.trim())?;
        Ok(response)
    }

    /// Ping the daemon.
    pub async fn ping(&self) -> Result<bool> {
        match self.send(IpcCommand::Ping).await {
            Ok(IpcResponse::Ok(IpcData::Pong)) => Ok(true),
            Ok(_) => Ok(false),
            Err(_) => Ok(false),
        }
    }

    /// Get the daemon status.
    pub async fn status(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::Status).await
    }

    /// Pause the watcher.
    pub async fn pause(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::Pause).await
    }

    /// Resume the watcher.
    pub async fn resume(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::Resume).await
    }

    /// Trigger a fix for an issue.
    pub async fn trigger(&self, source: &str, issue_id: &str) -> Result<IpcResponse> {
        self.send(IpcCommand::Trigger {
            source: source.to_string(),
            issue_id: issue_id.to_string(),
        })
        .await
    }

    /// Reset a failed attempt.
    pub async fn reset(&self, source: &str, issue_id: &str) -> Result<IpcResponse> {
        self.send(IpcCommand::Reset {
            source: source.to_string(),
            issue_id: issue_id.to_string(),
        })
        .await
    }

    /// Get statistics.
    pub async fn stats(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::Stats).await
    }

    /// List pending PRs.
    pub async fn list_prs(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::ListPrs).await
    }

    /// List retryable issues.
    pub async fn list_retries(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::ListRetries).await
    }

    /// Process ready retries.
    pub async fn process_retries(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::ProcessRetries).await
    }

    /// Get recent activity.
    pub async fn activity(&self, limit: usize) -> Result<IpcResponse> {
        self.send(IpcCommand::Activity { limit }).await
    }

    /// Request graceful shutdown.
    pub async fn shutdown(&self) -> Result<IpcResponse> {
        self.send(IpcCommand::Shutdown).await
    }
}

impl Default for IpcClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Print an IPC response in a human-readable format.
pub fn print_response(response: &IpcResponse) {
    match response {
        IpcResponse::Ok(data) => match data {
            IpcData::None => println!("OK"),
            IpcData::Pong => println!("Pong!"),
            IpcData::Message(msg) => println!("{}", msg),
            IpcData::State(state) => {
                println!("\nWatcher Status:");
                println!("  Running: {}", state.running);
                println!("  Paused:  {}", state.paused);
                println!("  Mode:    {}", state.mode);
                println!("  Uptime:  {}s", state.uptime_secs);
                println!("  Issues processed: {}", state.issues_processed);
                println!("  PRs created:      {}", state.prs_created);
                println!("  Sources: {}", state.sources.join(", "));
                if let Some(interval) = state.poll_interval_ms {
                    println!("  Poll interval: {}ms", interval);
                }
                if !state.processing.is_empty() {
                    println!("  Currently processing: {}", state.processing.join(", "));
                }
            }
            IpcData::Stats(stats) => {
                println!("\nFix Attempt Statistics:");
                println!("  Total:      {}", stats.total);
                println!("  Pending:    {}", stats.pending);
                println!("  Success:    {}", stats.success);
                println!("  Merged:     {}", stats.merged);
                println!("  Closed:     {}", stats.closed);
                println!("  Failed:     {}", stats.failed);
                println!("  Cannot Fix: {}", stats.cannot_fix);

                if !stats.by_source.is_empty() {
                    println!("\nBy Source:");
                    for (source, source_stats) in &stats.by_source {
                        println!(
                            "  {}: {} total, {} merged, {} failed",
                            source, source_stats.total, source_stats.merged, source_stats.failed
                        );
                    }
                }
            }
            IpcData::Attempts(attempts) => {
                if attempts.is_empty() {
                    println!("No attempts found.");
                } else {
                    for attempt in attempts {
                        println!(
                            "  [{}] {} - {:?} - {}",
                            attempt.source,
                            attempt.short_id,
                            attempt.status,
                            attempt.pr_url.as_deref().unwrap_or("N/A")
                        );
                    }
                }
            }
            IpcData::Triggered {
                source,
                issue_id,
                pr_url,
            } => {
                println!("Triggered fix for {}:{}", source, issue_id);
                if let Some(url) = pr_url {
                    println!("  PR: {}", url);
                }
            }
            IpcData::Reset { source, issue_id } => {
                println!("Reset {}:{}", source, issue_id);
            }
            IpcData::RetriesProcessed { count } => {
                println!("Processed {} retries", count);
            }
            IpcData::Activity(entries) => {
                if entries.is_empty() {
                    println!("No recent activity.");
                } else {
                    println!("\nRecent Activity:");
                    for entry in entries {
                        let source_info = entry
                            .source
                            .as_ref()
                            .map(|s| format!("[{}] ", s))
                            .unwrap_or_default();
                        let issue_info = entry
                            .issue_id
                            .as_ref()
                            .map(|id| format!("{}: ", id))
                            .unwrap_or_default();
                        println!(
                            "  {} {}{}{:?}: {}",
                            entry.timestamp,
                            source_info,
                            issue_info,
                            entry.activity_type,
                            entry.message
                        );
                    }
                }
            }
        },
        IpcResponse::Error { message } => {
            eprintln!("Error: {}", message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_default() {
        let client = IpcClient::default();
        assert!(!client.is_daemon_running()); // Assuming no daemon in tests
    }

    #[test]
    fn test_client_with_timeout() {
        let client = IpcClient::new().with_timeout(Duration::from_secs(5));
        assert_eq!(client.timeout, Duration::from_secs(5));
    }

    #[test]
    fn test_client_with_socket_path() {
        let path = PathBuf::from("/tmp/test.sock");
        let client = IpcClient::with_socket_path(path.clone());
        assert_eq!(client.socket_path, path);
    }
}
