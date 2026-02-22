//! Copilot agent runner (stub).
//!
//! Skeleton implementation showing the pattern for future contributors.

use super::{AgentRunner, ProviderCapabilities};
use crate::error::{Error, Result};
use crate::types::{AgentResult, Issue};
use async_trait::async_trait;
use std::path::Path;

/// Copilot agent runner (not yet implemented).
pub struct CopilotAgentRunner;

#[async_trait]
impl AgentRunner for CopilotAgentRunner {
    fn name(&self) -> &str {
        "copilot"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn build_prompt_for_issue(
        &self,
        _issue: &Issue,
        _context: &str,
        _project_dir: &Path,
    ) -> String {
        String::new()
    }

    async fn execute_with_attempt(
        &self,
        _prompt: &str,
        _issue: Option<&Issue>,
        _attempt_id: Option<i64>,
        _project_dir: &Path,
    ) -> Result<AgentResult> {
        Err(Error::runner("Copilot provider not yet implemented"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copilot_name() {
        let runner = CopilotAgentRunner;
        assert_eq!(runner.name(), "copilot");
    }

    #[test]
    fn test_copilot_capabilities_all_false() {
        let runner = CopilotAgentRunner;
        let caps = runner.capabilities();
        assert!(!caps.structured_output);
        assert!(!caps.tool_permissions);
        assert!(!caps.custom_instructions);
        assert!(!caps.streaming_events);
        assert!(!caps.cost_reporting);
    }

    #[test]
    fn test_copilot_build_prompt_returns_empty() {
        let runner = CopilotAgentRunner;
        let issue = Issue::new("1", "COP-1", "Bug", "url", "test");
        let prompt = runner.build_prompt_for_issue(&issue, "ctx", Path::new("/tmp"));
        assert!(prompt.is_empty());
    }

    #[tokio::test]
    async fn test_copilot_execute_returns_not_implemented() {
        let runner = CopilotAgentRunner;
        let result = runner
            .execute_with_attempt("test", None, None, Path::new("/tmp"))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }
}
