//! Agent runner abstraction layer.
//!
//! Defines the `AgentRunner` trait for pluggable AI coding agent providers,
//! along with shared types, free functions, and concrete implementations.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod orchestrator;

use crate::error::Result;
use crate::types::{AgentResult, Issue};
use async_trait::async_trait;
use std::path::Path;

// Re-export concrete implementations and key types.
pub use claude::{ClaudeAgentRunner, ClaudeRunnerConfig};
pub use codex::CodexAgentRunner;
pub use orchestrator::{AgentOrchestrator, SelectionStrategy, WeightedProvider};

/// Re-export resolve_log_root from the claude module (backward compat).
pub use claude::resolve_log_root;

/// What a provider supports.
#[derive(Debug, Clone, Default)]
pub struct ProviderCapabilities {
    pub structured_output: bool,
    pub tool_permissions: bool,
    pub custom_instructions: bool,
    pub streaming_events: bool,
    pub cost_reporting: bool,
}

/// Uniform interface for AI coding agent providers.
///
/// Follows the same pattern as `Notifier`, `IssueSource`, and `ScmProvider`.
#[async_trait]
pub trait AgentRunner: Send + Sync {
    /// Provider identifier (e.g. "claude", "codex", "gemini").
    fn name(&self) -> &str;

    /// What this provider supports.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Build a prompt for an issue.
    fn build_prompt_for_issue(
        &self,
        issue: &Issue,
        context: &str,
        project_dir: &Path,
    ) -> String;

    /// Run the agent and return a uniform result.
    async fn execute_with_attempt(
        &self,
        prompt: &str,
        issue: Option<&Issue>,
        attempt_id: Option<i64>,
        project_dir: &Path,
    ) -> Result<AgentResult>;
}

/// Best-effort detection for rate limit failures (generic, not provider-specific).
pub fn is_rate_limit_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    is_rate_limit_error_lower(&lower)
}

/// Rate-limit detection on an already-lowercased string (avoids double allocation).
fn is_rate_limit_error_lower(lower: &str) -> bool {
    [
        "rate limit",
        "ratelimit",
        "too many requests",
        "429",
        "quota exceeded",
        "resource exhausted",
        "retry-after",
        "try again later",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Detect "hard" runtime failures that should be escalated immediately.
pub fn is_hard_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    is_rate_limit_error_lower(&lower)
        || [
            "failed to spawn",
            "failed to wait for",
            "failed to capture stdout",
            "failed to capture stderr",
            "process timed out",
            "timed out after",
            "connection reset",
            "service unavailable",
            "internal server error",
            "network error",
            "broken pipe",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ProviderCapabilities tests ---

    #[test]
    fn test_provider_capabilities_default_all_false() {
        let caps = ProviderCapabilities::default();
        assert!(!caps.structured_output);
        assert!(!caps.tool_permissions);
        assert!(!caps.custom_instructions);
        assert!(!caps.streaming_events);
        assert!(!caps.cost_reporting);
    }

    #[test]
    fn test_provider_capabilities_custom() {
        let caps = ProviderCapabilities {
            structured_output: true,
            tool_permissions: true,
            custom_instructions: false,
            streaming_events: true,
            cost_reporting: false,
        };
        assert!(caps.structured_output);
        assert!(caps.tool_permissions);
        assert!(!caps.custom_instructions);
        assert!(caps.streaming_events);
        assert!(!caps.cost_reporting);
    }

    // --- is_rate_limit_error tests ---

    #[test]
    fn test_is_rate_limit_error_rate_limit() {
        assert!(is_rate_limit_error("Error: rate limit exceeded"));
    }

    #[test]
    fn test_is_rate_limit_error_429() {
        assert!(is_rate_limit_error("HTTP 429 Too Many Requests"));
    }

    #[test]
    fn test_is_rate_limit_error_quota_exceeded() {
        assert!(is_rate_limit_error("API quota exceeded for project"));
    }

    #[test]
    fn test_is_rate_limit_error_resource_exhausted() {
        assert!(is_rate_limit_error("Resource exhausted: try again later"));
    }

    #[test]
    fn test_is_rate_limit_error_retry_after() {
        assert!(is_rate_limit_error("retry-after: 30"));
    }

    #[test]
    fn test_is_rate_limit_error_ratelimit_one_word() {
        assert!(is_rate_limit_error("ratelimit hit"));
    }

    #[test]
    fn test_is_rate_limit_error_too_many_requests() {
        assert!(is_rate_limit_error("too many requests"));
    }

    #[test]
    fn test_is_rate_limit_error_try_again_later() {
        assert!(is_rate_limit_error("Please try again later"));
    }

    #[test]
    fn test_is_rate_limit_error_case_insensitive() {
        assert!(is_rate_limit_error("RATE LIMIT EXCEEDED"));
        assert!(is_rate_limit_error("Rate Limit"));
    }

    #[test]
    fn test_is_rate_limit_error_negative() {
        assert!(!is_rate_limit_error("connection refused"));
        assert!(!is_rate_limit_error("file not found"));
        assert!(!is_rate_limit_error("success"));
        assert!(!is_rate_limit_error(""));
    }

    // --- is_hard_error tests ---

    #[test]
    fn test_is_hard_error_spawn_failure() {
        assert!(is_hard_error("Failed to spawn process"));
    }

    #[test]
    fn test_is_hard_error_wait_failure() {
        assert!(is_hard_error("Failed to wait for child process"));
    }

    #[test]
    fn test_is_hard_error_stdout_capture() {
        assert!(is_hard_error("Failed to capture stdout"));
    }

    #[test]
    fn test_is_hard_error_stderr_capture() {
        assert!(is_hard_error("Failed to capture stderr"));
    }

    #[test]
    fn test_is_hard_error_timeout() {
        assert!(is_hard_error("Process timed out"));
        assert!(is_hard_error("Timed out after 3600 seconds"));
    }

    #[test]
    fn test_is_hard_error_network_errors() {
        assert!(is_hard_error("Connection reset by peer"));
        assert!(is_hard_error("Service unavailable"));
        assert!(is_hard_error("Internal server error"));
        assert!(is_hard_error("Network error: DNS resolution failed"));
        assert!(is_hard_error("Broken pipe"));
    }

    #[test]
    fn test_is_hard_error_includes_rate_limits() {
        // is_hard_error is a superset of is_rate_limit_error
        assert!(is_hard_error("rate limit exceeded"));
        assert!(is_hard_error("429"));
    }

    #[test]
    fn test_is_hard_error_case_insensitive() {
        assert!(is_hard_error("FAILED TO SPAWN"));
        assert!(is_hard_error("Service Unavailable"));
    }

    #[test]
    fn test_is_hard_error_negative() {
        assert!(!is_hard_error("syntax error in code"));
        assert!(!is_hard_error("test failed"));
        assert!(!is_hard_error("compilation error"));
        assert!(!is_hard_error(""));
    }

    // --- Additional edge case tests ---

    #[test]
    fn test_is_rate_limit_error_partial_match_boundary() {
        // Should NOT match "rated" or "limiting"
        assert!(!is_rate_limit_error("highly rated code"));
        // But should match "rate limit" as a substring
        assert!(is_rate_limit_error("some rate limit error occurred"));
    }

    #[test]
    fn test_is_hard_error_combined_messages() {
        assert!(is_hard_error(
            "Failed to spawn /usr/bin/claude: No such file or directory"
        ));
        assert!(is_hard_error(
            "Connection reset by peer while streaming"
        ));
    }

    #[test]
    fn test_provider_capabilities_debug() {
        let caps = ProviderCapabilities {
            structured_output: true,
            tool_permissions: false,
            custom_instructions: true,
            streaming_events: false,
            cost_reporting: true,
        };
        let debug = format!("{:?}", caps);
        assert!(debug.contains("structured_output: true"));
        assert!(debug.contains("cost_reporting: true"));
    }

    #[test]
    fn test_provider_capabilities_clone() {
        let original = ProviderCapabilities {
            structured_output: true,
            tool_permissions: true,
            custom_instructions: false,
            streaming_events: true,
            cost_reporting: false,
        };
        let cloned = original.clone();
        assert_eq!(cloned.structured_output, original.structured_output);
        assert_eq!(cloned.tool_permissions, original.tool_permissions);
        assert_eq!(cloned.custom_instructions, original.custom_instructions);
        assert_eq!(cloned.streaming_events, original.streaming_events);
        assert_eq!(cloned.cost_reporting, original.cost_reporting);
    }

    #[test]
    fn test_is_rate_limit_error_unicode_safe() {
        // Should handle non-ASCII without panic
        assert!(!is_rate_limit_error("错误：无法连接"));
        assert!(is_rate_limit_error("Error: rate limit exceeded. 请稍后再试"));
    }

    #[test]
    fn test_is_hard_error_long_message() {
        let long_msg = "a".repeat(100_000) + " failed to spawn process";
        assert!(is_hard_error(&long_msg));
    }
}
