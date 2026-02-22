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
