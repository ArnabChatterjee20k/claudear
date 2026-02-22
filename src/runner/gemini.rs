//! Gemini agent runner (stub).
//!
//! Skeleton implementation showing the pattern for future contributors.

use super::{AgentRunner, ProviderCapabilities};
use crate::error::{Error, Result};
use crate::types::{AgentResult, Issue};
use async_trait::async_trait;
use std::path::Path;

/// Gemini agent runner (not yet implemented).
pub struct GeminiAgentRunner;

#[async_trait]
impl AgentRunner for GeminiAgentRunner {
    fn name(&self) -> &str {
        "gemini"
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
        Err(Error::runner("Gemini provider not yet implemented"))
    }
}
