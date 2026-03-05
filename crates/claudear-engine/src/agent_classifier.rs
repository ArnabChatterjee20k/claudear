//! Agent-based repository classifier.
//!
//! Uses the configured agent (claude/codex) for repo classification instead of
//! the local LLM. Much faster but costs API credits.

use claudear_analysis::inference::{ClassificationRequest, RepoClassifier};
use claudear_integrations::runner::AgentRunner;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Agent-based repository classifier.
pub struct AgentRepoClassifier {
    agent: Arc<dyn AgentRunner>,
}

impl AgentRepoClassifier {
    /// Create a new classifier with the given agent runner.
    pub fn new(agent: Arc<dyn AgentRunner>) -> Self {
        Self { agent }
    }
}

impl RepoClassifier for AgentRepoClassifier {
    fn classify(&self, request: &ClassificationRequest) -> Option<(String, f32)> {
        let prompt = build_prompt(request);
        let temp_dir = PathBuf::from("/tmp/claudear-agent-classify");

        let start = Instant::now();

        // Bridge async agent call into sync context.
        // Use block_in_place to avoid "cannot start a runtime from within a runtime" panic
        // when the classifier is called from an async context (e.g., the watcher loop).
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.agent
                    .execute_with_attempt(&prompt, None, None, &temp_dir)
                    .await
            })
        });

        let elapsed = start.elapsed();

        match result {
            Ok(agent_result) => {
                let response = agent_result.output.trim().to_string();
                tracing::debug!(
                    response = %response,
                    elapsed_ms = elapsed.as_millis(),
                    "Agent classifier response"
                );

                let candidate_names: Vec<&str> =
                    request.candidates.iter().map(|(n, _)| n.as_str()).collect();
                parse_response(&response, &candidate_names)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Agent classifier execution failed");
                None
            }
        }
    }
}

/// Build the classification prompt (plain text, no model-specific tokens).
fn build_prompt(request: &ClassificationRequest) -> String {
    let mut parts: Vec<String> = Vec::new();

    parts.push(
        "You are a code repository classifier. Given an issue with its full context and a list of \
         candidate repositories with their profiles, determine which repository the issue belongs to.\n\
         Respond with ONLY the exact repository name (e.g. \"org/repo\"). If none match, respond \"NONE\"."
            .to_string(),
    );

    // Issue section
    parts.push("\n## Issue".to_string());
    parts.push(format!("Title: {}", request.title));
    parts.push(format!("Source: {}", request.source));
    if let Some(ref desc) = request.description {
        let truncated = if desc.len() > 500 {
            format!("{}...", &desc[..500])
        } else {
            desc.clone()
        };
        parts.push(format!("Description: {}", truncated));
    }

    // Metadata
    let metadata_keys = [
        "stacktrace",
        "culprit",
        "filename",
        "function",
        "project",
        "message",
    ];
    for key in &metadata_keys {
        if let Some(val) = request.metadata.get(*key) {
            let display_val = if *key == "stacktrace" && val.len() > 500 {
                format!("{}...", &val[..500])
            } else {
                val.clone()
            };
            parts.push(format!("{}: {}", key, display_val));
        }
    }

    // Extracted signals
    parts.push("\n## Extracted Signals".to_string());
    if !request.extracted_filenames.is_empty() {
        parts.push(format!(
            "Files referenced: {}",
            request.extracted_filenames.join(", ")
        ));
    }
    if !request.extracted_functions.is_empty() {
        parts.push(format!(
            "Functions referenced: {}",
            request.extracted_functions.join(", ")
        ));
    }
    if !request.extracted_keywords.is_empty() {
        parts.push(format!(
            "Keywords: {}",
            request.extracted_keywords.join(", ")
        ));
    }
    if !request.extracted_repos.is_empty() {
        parts.push(format!(
            "Referenced repos: {}",
            request.extracted_repos.join(", ")
        ));
    }

    // Candidate repositories
    parts.push("\n## Candidate Repositories".to_string());
    for (i, (name, profile)) in request.candidates.iter().enumerate() {
        parts.push(format!("\n### {}. {}", i + 1, name));
        parts.push(profile.clone());
    }

    parts.push("\nWhich repository does this issue belong to?".to_string());

    parts.join("\n")
}

/// Parse the agent response to extract a repo name and confidence.
fn parse_response(response: &str, candidates: &[&str]) -> Option<(String, f32)> {
    let trimmed = response.trim();

    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return None;
    }

    // Exact match
    for &candidate in candidates {
        if trimmed == candidate {
            return Some((candidate.to_string(), 1.0));
        }
    }

    // Case-insensitive match
    let lower = trimmed.to_lowercase();
    for &candidate in candidates {
        if lower == candidate.to_lowercase() {
            return Some((candidate.to_string(), 0.9));
        }
    }

    // Contains match (response contains a candidate name)
    for &candidate in candidates {
        if lower.contains(&candidate.to_lowercase()) {
            return Some((candidate.to_string(), 0.7));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use claudear_core::types::{AgentResult, Issue};
    use claudear_integrations::runner::ProviderCapabilities;
    use std::collections::HashMap;
    use std::path::Path;

    struct MockAgent {
        response: Result<AgentResult, String>,
    }

    impl MockAgent {
        fn with_output(output: &str) -> Self {
            Self {
                response: Ok(AgentResult {
                    success: true,
                    output: output.to_string(),
                    pr_url: None,
                    changelog: None,
                    error: None,
                    blocking_question: None,
                    used_qa_ids: vec![],
                    confidence: 80,
                    confidence_reasoning: None,
                    wrong_repo: None,
                }),
            }
        }

        fn with_error() -> Self {
            Self {
                response: Err("mock error".to_string()),
            }
        }
    }

    #[async_trait]
    impl AgentRunner for MockAgent {
        fn name(&self) -> &str {
            "mock"
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
        ) -> claudear_core::error::Result<AgentResult> {
            match &self.response {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(claudear_core::error::Error::runner(e)),
            }
        }
    }

    fn sample_request() -> ClassificationRequest {
        ClassificationRequest {
            title: "MySQL server has gone away".to_string(),
            description: Some("Connection lost during query execution".to_string()),
            source: "sentry".to_string(),
            metadata: {
                let mut m = HashMap::new();
                m.insert("project".to_string(), "cloud-staging".to_string());
                m
            },
            extracted_filenames: vec!["src/Database/Adapter/SQL.php".to_string()],
            extracted_functions: vec!["query".to_string()],
            extracted_keywords: vec!["MySQL".to_string()],
            extracted_repos: vec![],
            candidates: vec![
                ("appwrite/cloud".to_string(), "Cloud backend".to_string()),
                (
                    "utopia-php/database".to_string(),
                    "Database abstraction".to_string(),
                ),
            ],
        }
    }

    #[test]
    fn test_agent_classifier_builds_prompt() {
        let request = sample_request();
        let prompt = build_prompt(&request);

        assert!(prompt.contains("MySQL server has gone away"));
        assert!(prompt.contains("appwrite/cloud"));
        assert!(prompt.contains("utopia-php/database"));
        assert!(prompt.contains("src/Database/Adapter/SQL.php"));
        assert!(prompt.contains("cloud-staging"));
    }

    #[test]
    fn test_agent_classifier_parses_exact_match() {
        let candidates = vec!["appwrite/cloud", "utopia-php/database"];
        let result = parse_response("appwrite/cloud", &candidates);
        assert_eq!(result, Some(("appwrite/cloud".to_string(), 1.0)));
    }

    #[test]
    fn test_agent_classifier_parses_case_insensitive() {
        let candidates = vec!["appwrite/cloud", "utopia-php/database"];
        let result = parse_response("Appwrite/Cloud", &candidates);
        assert_eq!(result, Some(("appwrite/cloud".to_string(), 0.9)));
    }

    #[test]
    fn test_agent_classifier_handles_none_response() {
        let candidates = vec!["appwrite/cloud", "utopia-php/database"];
        let result = parse_response("NONE", &candidates);
        assert_eq!(result, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_agent_classifier_handles_agent_error() {
        let mock = MockAgent::with_error();
        let classifier = AgentRepoClassifier::new(Arc::new(mock));
        let request = sample_request();
        let result = classifier.classify(&request);
        assert!(result.is_none());
    }

    #[test]
    fn test_agent_classifier_prompt_excludes_model_tokens() {
        let request = sample_request();
        let prompt = build_prompt(&request);

        assert!(!prompt.contains("<|system|>"));
        assert!(!prompt.contains("<|user|>"));
        assert!(!prompt.contains("<|assistant|>"));
        assert!(!prompt.contains("<|end|>"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_agent_classifier_parses_output() {
        let mock = MockAgent::with_output("appwrite/cloud");
        let classifier = AgentRepoClassifier::new(Arc::new(mock));
        let request = sample_request();
        let result = classifier.classify(&request);
        assert_eq!(result, Some(("appwrite/cloud".to_string(), 1.0)));
    }
}
