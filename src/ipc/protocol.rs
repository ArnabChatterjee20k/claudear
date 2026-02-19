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

    /// Claude execution started.
    ClaudeStarted,

    /// Claude execution completed successfully.
    ClaudeCompleted,

    /// Claude execution timed out.
    ClaudeTimedOut,

    /// Claude execution failed.
    ClaudeFailed,

    /// Webhook received.
    WebhookReceived,

    /// Webhook processed successfully.
    WebhookProcessed,

    /// Webhook rejected (invalid signature, filtered out, etc.).
    WebhookRejected,

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

    // === IpcCommand serialization/deserialization round-trip tests ===

    #[test]
    fn test_command_ping_roundtrip() {
        let cmd = IpcCommand::Ping;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::Ping));
    }

    #[test]
    fn test_command_status_roundtrip() {
        let cmd = IpcCommand::Status;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::Status));
    }

    #[test]
    fn test_command_pause_roundtrip() {
        let cmd = IpcCommand::Pause;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::Pause));
    }

    #[test]
    fn test_command_resume_roundtrip() {
        let cmd = IpcCommand::Resume;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::Resume));
    }

    #[test]
    fn test_command_stats_roundtrip() {
        let cmd = IpcCommand::Stats;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::Stats));
    }

    #[test]
    fn test_command_list_prs_roundtrip() {
        let cmd = IpcCommand::ListPrs;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::ListPrs));
    }

    #[test]
    fn test_command_list_retries_roundtrip() {
        let cmd = IpcCommand::ListRetries;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::ListRetries));
    }

    #[test]
    fn test_command_trigger_roundtrip() {
        let cmd = IpcCommand::Trigger {
            source: "sentry".to_string(),
            issue_id: "SENTRY-456".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Trigger { source, issue_id } => {
                assert_eq!(source, "sentry");
                assert_eq!(issue_id, "SENTRY-456");
            }
            _ => panic!("Expected Trigger variant"),
        }
    }

    #[test]
    fn test_command_reset_roundtrip() {
        let cmd = IpcCommand::Reset {
            source: "linear".to_string(),
            issue_id: "LIN-789".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Reset { source, issue_id } => {
                assert_eq!(source, "linear");
                assert_eq!(issue_id, "LIN-789");
            }
            _ => panic!("Expected Reset variant"),
        }
    }

    #[test]
    fn test_command_process_retries_roundtrip() {
        let cmd = IpcCommand::ProcessRetries;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::ProcessRetries));
    }

    #[test]
    fn test_command_activity_roundtrip() {
        let cmd = IpcCommand::Activity { limit: 50 };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Activity { limit } => assert_eq!(limit, 50),
            _ => panic!("Expected Activity variant"),
        }
    }

    #[test]
    fn test_command_shutdown_roundtrip() {
        let cmd = IpcCommand::Shutdown;
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcCommand::Shutdown));
    }

    // === IpcResponse serialization tests ===

    #[test]
    fn test_response_ok_serialization() {
        let resp = IpcResponse::ok();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"Ok\""));
        let parsed: IpcResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_ok());
        assert!(parsed.error_message().is_none());
    }

    #[test]
    fn test_response_error_serialization() {
        let resp = IpcResponse::error("disk full");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: IpcResponse = serde_json::from_str(&json).unwrap();
        assert!(!parsed.is_ok());
        assert_eq!(parsed.error_message(), Some("disk full"));
    }

    #[test]
    fn test_response_ok_with_message_serialization() {
        let resp = IpcResponse::ok_with(IpcData::Message("all good".to_string()));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: IpcResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_ok());
        match parsed {
            IpcResponse::Ok(IpcData::Message(msg)) => assert_eq!(msg, "all good"),
            _ => panic!("Expected Ok(Message)"),
        }
    }

    #[test]
    fn test_response_ok_with_pong_serialization() {
        let resp = IpcResponse::ok_with(IpcData::Pong);
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: IpcResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcResponse::Ok(IpcData::Pong) => {}
            _ => panic!("Expected Ok(Pong)"),
        }
    }

    // === IpcData serialization tests ===

    #[test]
    fn test_ipc_data_none_serialization() {
        let data = IpcData::None;
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcData::None));
    }

    #[test]
    fn test_ipc_data_pong_serialization() {
        let data = IpcData::Pong;
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, IpcData::Pong));
    }

    #[test]
    fn test_ipc_data_state_serialization() {
        let state = WatcherState {
            running: true,
            paused: true,
            mode: "webhook".to_string(),
            uptime_secs: 120,
            issues_processed: 0,
            prs_created: 0,
            processing: vec![],
            sources: vec!["jira".to_string()],
            poll_interval_ms: None,
        };
        let data = IpcData::State(state);
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::State(s) => {
                assert!(s.running);
                assert!(s.paused);
                assert_eq!(s.mode, "webhook");
                assert_eq!(s.sources, vec!["jira"]);
                assert!(s.poll_interval_ms.is_none());
            }
            _ => panic!("Expected State variant"),
        }
    }

    #[test]
    fn test_ipc_data_triggered_serialization() {
        let data = IpcData::Triggered {
            source: "linear".to_string(),
            issue_id: "LIN-1".to_string(),
            pr_url: Some("https://github.com/org/repo/pull/42".to_string()),
        };
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::Triggered {
                source,
                issue_id,
                pr_url,
            } => {
                assert_eq!(source, "linear");
                assert_eq!(issue_id, "LIN-1");
                assert_eq!(
                    pr_url,
                    Some("https://github.com/org/repo/pull/42".to_string())
                );
            }
            _ => panic!("Expected Triggered variant"),
        }
    }

    #[test]
    fn test_ipc_data_triggered_no_pr_url_serialization() {
        let data = IpcData::Triggered {
            source: "sentry".to_string(),
            issue_id: "SENTRY-99".to_string(),
            pr_url: None,
        };
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::Triggered { pr_url, .. } => assert!(pr_url.is_none()),
            _ => panic!("Expected Triggered variant"),
        }
    }

    #[test]
    fn test_ipc_data_reset_serialization() {
        let data = IpcData::Reset {
            source: "jira".to_string(),
            issue_id: "JIRA-100".to_string(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::Reset { source, issue_id } => {
                assert_eq!(source, "jira");
                assert_eq!(issue_id, "JIRA-100");
            }
            _ => panic!("Expected Reset variant"),
        }
    }

    #[test]
    fn test_ipc_data_retries_processed_serialization() {
        let data = IpcData::RetriesProcessed { count: 7 };
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::RetriesProcessed { count } => assert_eq!(count, 7),
            _ => panic!("Expected RetriesProcessed variant"),
        }
    }

    #[test]
    fn test_ipc_data_activity_serialization() {
        let entries = vec![ActivityEntry {
            timestamp: "2026-01-15T10:30:00Z".to_string(),
            activity_type: ActivityType::PrCreated,
            message: "Created PR #42".to_string(),
            issue_id: Some("LIN-5".to_string()),
            source: Some("linear".to_string()),
        }];
        let data = IpcData::Activity(entries);
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::Activity(e) => {
                assert_eq!(e.len(), 1);
                assert_eq!(e[0].message, "Created PR #42");
            }
            _ => panic!("Expected Activity variant"),
        }
    }

    #[test]
    fn test_ipc_data_message_serialization() {
        let data = IpcData::Message("Watcher is healthy".to_string());
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::Message(msg) => assert_eq!(msg, "Watcher is healthy"),
            _ => panic!("Expected Message variant"),
        }
    }

    // === ActivityType serialization/deserialization tests ===

    #[test]
    fn test_activity_type_issue_variants_roundtrip() {
        let variants = vec![
            ActivityType::IssueDetected,
            ActivityType::IssueStatusChanged,
            ActivityType::IssuePriorityChanged,
            ActivityType::IssueCommented,
            ActivityType::IssueResolved,
            ActivityType::IssueCancelled,
            ActivityType::IssueEscalated,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ActivityType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_activity_type_processing_variants_roundtrip() {
        let variants = vec![
            ActivityType::ProcessingStarted,
            ActivityType::ProcessingCompleted,
            ActivityType::ProcessingFailed,
            ActivityType::ProcessingSkipped,
            ActivityType::RetryScheduled,
            ActivityType::RetryExecuted,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ActivityType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_activity_type_pr_variants_roundtrip() {
        let variants = vec![
            ActivityType::PrCreated,
            ActivityType::PrMerged,
            ActivityType::PrClosed,
            ActivityType::PrReviewReceived,
            ActivityType::PrReviewRequested,
            ActivityType::PrCommented,
            ActivityType::PrStatusCheckPassed,
            ActivityType::PrStatusCheckFailed,
            ActivityType::PrAutoClosed,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ActivityType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_activity_type_claude_variants_roundtrip() {
        let variants = vec![
            ActivityType::ClaudeStarted,
            ActivityType::ClaudeCompleted,
            ActivityType::ClaudeTimedOut,
            ActivityType::ClaudeFailed,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ActivityType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_activity_type_webhook_variants_roundtrip() {
        let variants = vec![
            ActivityType::WebhookReceived,
            ActivityType::WebhookProcessed,
            ActivityType::WebhookRejected,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ActivityType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_activity_type_watcher_variants_roundtrip() {
        let variants = vec![
            ActivityType::WatcherStarted,
            ActivityType::WatcherStopped,
            ActivityType::WatcherPaused,
            ActivityType::WatcherResumed,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ActivityType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_activity_type_misc_variants_roundtrip() {
        let variants = vec![
            ActivityType::RateLimitHit,
            ActivityType::StateChange,
            ActivityType::Error,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: ActivityType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    // === WatcherState serialization tests ===

    #[test]
    fn test_watcher_state_with_all_optional_fields() {
        let state = WatcherState {
            running: false,
            paused: true,
            mode: "dashboard".to_string(),
            uptime_secs: 0,
            issues_processed: 999,
            prs_created: 42,
            processing: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            sources: vec![
                "linear".to_string(),
                "sentry".to_string(),
                "jira".to_string(),
            ],
            poll_interval_ms: Some(60000),
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: WatcherState = serde_json::from_str(&json).unwrap();
        assert!(!parsed.running);
        assert!(parsed.paused);
        assert_eq!(parsed.mode, "dashboard");
        assert_eq!(parsed.uptime_secs, 0);
        assert_eq!(parsed.issues_processed, 999);
        assert_eq!(parsed.prs_created, 42);
        assert_eq!(parsed.processing.len(), 3);
        assert_eq!(parsed.sources.len(), 3);
        assert_eq!(parsed.poll_interval_ms, Some(60000));
    }

    #[test]
    fn test_watcher_state_without_optional_fields() {
        let state = WatcherState {
            running: true,
            paused: false,
            mode: "webhook".to_string(),
            uptime_secs: 86400,
            issues_processed: 0,
            prs_created: 0,
            processing: vec![],
            sources: vec![],
            poll_interval_ms: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: WatcherState = serde_json::from_str(&json).unwrap();
        assert!(parsed.running);
        assert!(!parsed.paused);
        assert_eq!(parsed.mode, "webhook");
        assert_eq!(parsed.uptime_secs, 86400);
        assert!(parsed.processing.is_empty());
        assert!(parsed.sources.is_empty());
        assert!(parsed.poll_interval_ms.is_none());
    }

    // === ActivityEntry serialization tests ===

    #[test]
    fn test_activity_entry_all_fields() {
        let entry = ActivityEntry {
            timestamp: "2026-02-19T08:00:00Z".to_string(),
            activity_type: ActivityType::ProcessingCompleted,
            message: "Successfully processed issue".to_string(),
            issue_id: Some("LIN-42".to_string()),
            source: Some("linear".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ActivityEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.timestamp, "2026-02-19T08:00:00Z");
        assert_eq!(parsed.activity_type, ActivityType::ProcessingCompleted);
        assert_eq!(parsed.message, "Successfully processed issue");
        assert_eq!(parsed.issue_id, Some("LIN-42".to_string()));
        assert_eq!(parsed.source, Some("linear".to_string()));
    }

    #[test]
    fn test_activity_entry_optional_fields_none() {
        let entry = ActivityEntry {
            timestamp: "2026-02-19T09:00:00Z".to_string(),
            activity_type: ActivityType::WatcherStarted,
            message: "Watcher started".to_string(),
            issue_id: None,
            source: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ActivityEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.activity_type, ActivityType::WatcherStarted);
        assert!(parsed.issue_id.is_none());
        assert!(parsed.source.is_none());
    }

    // === Edge case tests ===

    #[test]
    fn test_trigger_with_special_characters_in_source() {
        let cmd = IpcCommand::Trigger {
            source: "my-source/with:special_chars".to_string(),
            issue_id: "ID-with spaces & symbols!@#$%".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Trigger { source, issue_id } => {
                assert_eq!(source, "my-source/with:special_chars");
                assert_eq!(issue_id, "ID-with spaces & symbols!@#$%");
            }
            _ => panic!("Expected Trigger variant"),
        }
    }

    #[test]
    fn test_reset_with_special_characters() {
        let cmd = IpcCommand::Reset {
            source: "source\"with\"quotes".to_string(),
            issue_id: "id\nwith\nnewlines".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Reset { source, issue_id } => {
                assert_eq!(source, "source\"with\"quotes");
                assert_eq!(issue_id, "id\nwith\nnewlines");
            }
            _ => panic!("Expected Reset variant"),
        }
    }

    #[test]
    fn test_trigger_with_empty_strings() {
        let cmd = IpcCommand::Trigger {
            source: "".to_string(),
            issue_id: "".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Trigger { source, issue_id } => {
                assert_eq!(source, "");
                assert_eq!(issue_id, "");
            }
            _ => panic!("Expected Trigger variant"),
        }
    }

    #[test]
    fn test_trigger_with_unicode() {
        let cmd = IpcCommand::Trigger {
            source: "\u{1f600}\u{1f680}".to_string(),
            issue_id: "\u{4f60}\u{597d}\u{4e16}\u{754c}".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Trigger { source, issue_id } => {
                assert_eq!(source, "\u{1f600}\u{1f680}");
                assert_eq!(issue_id, "\u{4f60}\u{597d}\u{4e16}\u{754c}");
            }
            _ => panic!("Expected Trigger variant"),
        }
    }

    #[test]
    fn test_activity_with_limit_zero() {
        let cmd = IpcCommand::Activity { limit: 0 };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Activity { limit } => assert_eq!(limit, 0),
            _ => panic!("Expected Activity variant"),
        }
    }

    #[test]
    fn test_activity_with_large_limit() {
        let cmd = IpcCommand::Activity { limit: usize::MAX };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Activity { limit } => assert_eq!(limit, usize::MAX),
            _ => panic!("Expected Activity variant"),
        }
    }

    #[test]
    fn test_response_error_with_empty_message() {
        let resp = IpcResponse::error("");
        assert_eq!(resp.error_message(), Some(""));
    }

    #[test]
    fn test_response_ok_is_ok() {
        let resp = IpcResponse::ok();
        assert!(resp.is_ok());
        assert!(resp.error_message().is_none());
    }

    #[test]
    fn test_ipc_data_empty_activity_list() {
        let data = IpcData::Activity(vec![]);
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::Activity(entries) => assert!(entries.is_empty()),
            _ => panic!("Expected Activity variant"),
        }
    }

    #[test]
    fn test_ipc_data_empty_attempts_list() {
        let data = IpcData::Attempts(vec![]);
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::Attempts(a) => assert!(a.is_empty()),
            _ => panic!("Expected Attempts variant"),
        }
    }

    #[test]
    fn test_ipc_data_retries_processed_zero() {
        let data = IpcData::RetriesProcessed { count: 0 };
        let json = serde_json::to_string(&data).unwrap();
        let parsed: IpcData = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcData::RetriesProcessed { count } => assert_eq!(count, 0),
            _ => panic!("Expected RetriesProcessed variant"),
        }
    }
}
