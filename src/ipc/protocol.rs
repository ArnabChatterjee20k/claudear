//! IPC protocol definitions.

use crate::types::{FixAttempt, FixAttemptStats};
use serde::{Deserialize, Serialize};

/// Commands that can be sent to the watcher daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "args")]
pub enum IpcCommand {
    /// Get the current watcher state.
    Status,

    /// Pause the watcher (stop processing new issues).
    Pause,

    /// Resume the watcher.
    Resume,

    /// Trigger a fix for a specific issue.
    Trigger { source: String, issue_id: String },

    /// Reset a failed attempt.
    Reset { source: String, issue_id: String },

    /// Get statistics.
    Stats,

    /// Get pending PRs.
    ListPrs,

    /// Get retryable issues.
    ListRetries,

    /// Process ready retries now.
    ProcessRetries,

    /// Gracefully shutdown the daemon.
    Shutdown,

    /// Ping (health check).
    Ping,

    /// Get recent activity/logs.
    Activity { limit: usize },
}

/// Response from the watcher daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "data")]
pub enum IpcResponse {
    /// Success with optional data.
    Ok(IpcData),

    /// Error with message.
    Error { message: String },
}

/// Data returned in successful responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum IpcData {
    /// No data (acknowledgment).
    None,

    /// Pong response.
    Pong,

    /// Watcher state.
    State(WatcherState),

    /// Statistics.
    Stats(FixAttemptStats),

    /// List of fix attempts.
    Attempts(Vec<FixAttempt>),

    /// Triggered issue result.
    Triggered {
        source: String,
        issue_id: String,
        pr_url: Option<String>,
    },

    /// Reset result.
    Reset { source: String, issue_id: String },

    /// Retry processing result.
    RetriesProcessed { count: usize },

    /// Recent activity entries.
    Activity(Vec<ActivityEntry>),

    /// Text message.
    Message(String),
}

/// Current state of the watcher daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherState {
    /// Whether the watcher is running.
    pub running: bool,

    /// Whether the watcher is paused.
    pub paused: bool,

    /// Current mode (poll, webhook, dashboard).
    pub mode: String,

    /// Uptime in seconds.
    pub uptime_secs: u64,

    /// Number of issues processed since start.
    pub issues_processed: usize,

    /// Number of PRs created since start.
    pub prs_created: usize,

    /// Currently processing issues.
    pub processing: Vec<String>,

    /// Enabled sources.
    pub sources: Vec<String>,

    /// Poll interval (if in poll mode).
    pub poll_interval_ms: Option<u64>,
}

/// An activity log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Timestamp.
    pub timestamp: String,

    /// Activity type.
    pub activity_type: ActivityType,

    /// Short description.
    pub message: String,

    /// Related issue ID.
    pub issue_id: Option<String>,

    /// Related source.
    pub source: Option<String>,
}

/// Types of activity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActivityType {
    // === Issue Events ===
    /// Issue detected.
    IssueDetected,

    /// Issue status changed externally.
    IssueStatusChanged,

    /// Issue priority changed externally.
    IssuePriorityChanged,

    /// Issue received a comment.
    IssueCommented,

    /// Issue was resolved externally.
    IssueResolved,

    /// Issue was cancelled externally.
    IssueCancelled,

    /// Issue was escalated (e.g., Sentry event count spike).
    IssueEscalated,

    // === Processing Events ===
    /// Processing started.
    ProcessingStarted,

    /// Processing completed successfully.
    ProcessingCompleted,

    /// Processing failed.
    ProcessingFailed,

    /// Processing was skipped (already handled, not matching criteria, etc.).
    ProcessingSkipped,

    /// A retry has been scheduled.
    RetryScheduled,

    /// A retry is being executed.
    RetryExecuted,

    // === PR Events ===
    /// PR created.
    PrCreated,

    /// PR merged.
    PrMerged,

    /// PR closed without merge.
    PrClosed,

    /// PR review received.
    PrReviewReceived,

    /// PR review was requested.
    PrReviewRequested,

    /// PR received a comment.
    PrCommented,

    /// PR status check passed.
    PrStatusCheckPassed,

    /// PR status check failed.
    PrStatusCheckFailed,

    /// PR was auto-closed because the source issue was resolved/cancelled.
    PrAutoClosed,

    // === Claude Events ===
    /// Claude execution started.
    ClaudeStarted,

    /// Claude execution completed successfully.
    ClaudeCompleted,

    /// Claude execution timed out.
    ClaudeTimedOut,

    /// Claude execution failed.
    ClaudeFailed,

    // === Webhook Events ===
    /// Webhook received.
    WebhookReceived,

    /// Webhook processed successfully.
    WebhookProcessed,

    /// Webhook rejected (invalid signature, filtered out, etc.).
    WebhookRejected,

    // === System Events ===
    /// Watcher started.
    WatcherStarted,

    /// Watcher stopped.
    WatcherStopped,

    /// Watcher paused.
    WatcherPaused,

    /// Watcher resumed.
    WatcherResumed,

    /// Rate limit hit on an external API.
    RateLimitHit,

    /// Watcher state change (legacy, kept for compatibility).
    StateChange,

    /// Error occurred.
    Error,
}

impl IpcResponse {
    /// Create an OK response with no data.
    pub fn ok() -> Self {
        Self::Ok(IpcData::None)
    }

    /// Create an OK response with data.
    pub fn ok_with(data: IpcData) -> Self {
        Self::Ok(data)
    }

    /// Create an error response.
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Check if this is an OK response.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }

    /// Get the error message if this is an error.
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { message } => Some(message),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_serialization() {
        let cmd = IpcCommand::Trigger {
            source: "linear".to_string(),
            issue_id: "LIN-123".to_string(),
        };

        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("Trigger"));
        assert!(json.contains("linear"));

        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::Trigger { .. }));
    }

    #[test]
    fn test_response_serialization() {
        let resp = IpcResponse::ok_with(IpcData::Message("Hello".to_string()));

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Ok"));
        assert!(json.contains("Hello"));
    }

    #[test]
    fn test_response_error() {
        let resp = IpcResponse::error("Something went wrong");
        assert!(!resp.is_ok());
        assert_eq!(resp.error_message(), Some("Something went wrong"));
    }

    #[test]
    fn test_watcher_state() {
        let state = WatcherState {
            running: true,
            paused: false,
            mode: "poll".to_string(),
            uptime_secs: 3600,
            issues_processed: 10,
            prs_created: 5,
            processing: vec!["LIN-123".to_string()],
            sources: vec!["linear".to_string(), "sentry".to_string()],
            poll_interval_ms: Some(300000),
        };

        let json = serde_json::to_string(&state).unwrap();
        let parsed: WatcherState = serde_json::from_str(&json).unwrap();

        assert!(parsed.running);
        assert_eq!(parsed.uptime_secs, 3600);
    }
}
