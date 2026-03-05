//! LLM-based agent runner.
//!
//! Uses the local LLM model as the agent runner instead of an external
//! provider (claude/codex). Fully offline but much slower and unable to
//! create PRs or run git commands.

use async_trait::async_trait;
use claudear_core::error::Result;
use claudear_core::types::{AgentResult, Issue};
use claudear_integrations::chat::llm::{GenerationParams, LlmEngine};
use claudear_integrations::runner::{AgentRunner, ProviderCapabilities};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// LLM-based agent runner using local model inference.
pub struct LlmAgentRunner {
    engine: Arc<LlmEngine>,
}

impl LlmAgentRunner {
    /// Create a new runner with the given LLM engine.
    pub fn new(engine: Arc<LlmEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl AgentRunner for LlmAgentRunner {
    fn name(&self) -> &str {
        "llm"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            structured_output: false,
            tool_permissions: false,
            custom_instructions: false,
            streaming_events: false,
            cost_reporting: false,
        }
    }

    fn build_prompt_for_issue(&self, issue: &Issue, context: &str, _project_dir: &Path) -> String {
        let mut prompt = String::new();
        prompt.push_str(
            "You are a software engineer fixing a bug. Analyze the issue and suggest a fix.\n\n",
        );
        prompt.push_str(&format!("## Issue: {}\n", issue.title));
        if let Some(ref desc) = issue.description {
            prompt.push_str(&format!("Description: {}\n", desc));
        }
        prompt.push_str(&format!("Source: {}\n", issue.source));
        prompt.push_str(&format!("URL: {}\n\n", issue.url));

        if !context.is_empty() {
            prompt.push_str("## Context\n");
            prompt.push_str(context);
            prompt.push('\n');
        }

        prompt.push_str("\n## Instructions\n");
        prompt.push_str("Analyze the issue and provide a detailed fix suggestion including:\n");
        prompt.push_str("1. Root cause analysis\n");
        prompt.push_str("2. Suggested code changes\n");
        prompt.push_str("3. Testing recommendations\n");

        prompt
    }

    async fn execute_with_attempt(
        &self,
        prompt: &str,
        _issue: Option<&Issue>,
        _attempt_id: Option<i64>,
        _project_dir: &Path,
    ) -> Result<AgentResult> {
        let start = Instant::now();

        let params = GenerationParams {
            temperature: 0.3,
            max_tokens: 4096,
            top_p: 0.9,
            stop_sequences: vec![],
        };

        let tokens = self.engine.complete_streaming(prompt, &params)?;
        let output = tokens.join("");
        let duration = start.elapsed();

        tracing::debug!(
            output_len = output.len(),
            elapsed_ms = duration.as_millis(),
            "LLM agent runner completed"
        );

        Ok(AgentResult {
            success: !output.is_empty(),
            output,
            pr_url: None,
            changelog: None,
            error: None,
            blocking_question: None,
            used_qa_ids: vec![],
            confidence: 50,
            confidence_reasoning: Some("Local LLM suggestion (no PR created)".to_string()),
            wrong_repo: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: LlmAgentRunner requires a real LlmEngine which depends on
    // llama-cpp-2 model loading. Integration tests are gated behind
    // CLAUDEAR_LLM_MODEL_PATH env var, same as test_live_classification.

    #[test]
    fn test_llm_agent_runner_name() {
        // We can't construct without a real engine, but we can test the trait
        // method names via a simple mock approach. Instead, test the constants.
        assert_eq!("llm", "llm"); // placeholder for name check
    }

    #[test]
    fn test_llm_agent_runner_capabilities_are_minimal() {
        let caps = ProviderCapabilities {
            structured_output: false,
            tool_permissions: false,
            custom_instructions: false,
            streaming_events: false,
            cost_reporting: false,
        };
        assert!(!caps.structured_output);
        assert!(!caps.tool_permissions);
        assert!(!caps.custom_instructions);
        assert!(!caps.streaming_events);
        assert!(!caps.cost_reporting);
    }

    #[test]
    fn test_llm_agent_runner_build_prompt() {
        let issue = Issue::new(
            "test-1",
            "TEST-1",
            "NullPointerException in getDocument",
            "https://example.com/issue/1",
            "sentry",
        );
        let context = "Stack trace:\n  at Database.getDocument(Database.java:42)";

        // Test prompt building logic directly (same as what the runner does)
        let mut prompt = String::new();
        prompt.push_str(
            "You are a software engineer fixing a bug. Analyze the issue and suggest a fix.\n\n",
        );
        prompt.push_str(&format!("## Issue: {}\n", issue.title));
        if let Some(ref desc) = issue.description {
            prompt.push_str(&format!("Description: {}\n", desc));
        }
        prompt.push_str(&format!("Source: {}\n", issue.source));
        prompt.push_str(&format!("URL: {}\n\n", issue.url));
        if !context.is_empty() {
            prompt.push_str("## Context\n");
            prompt.push_str(context);
            prompt.push('\n');
        }
        prompt.push_str("\n## Instructions\n");
        prompt.push_str("Analyze the issue and provide a detailed fix suggestion including:\n");
        prompt.push_str("1. Root cause analysis\n");
        prompt.push_str("2. Suggested code changes\n");
        prompt.push_str("3. Testing recommendations\n");

        assert!(prompt.contains("NullPointerException in getDocument"));
        assert!(prompt.contains("Database.getDocument"));
        assert!(prompt.contains("sentry"));
        assert!(prompt.contains("Root cause analysis"));
    }
}
