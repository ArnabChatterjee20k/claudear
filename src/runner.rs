//! Claude CLI runner for executing fixes.

use crate::error::{Error, Result};
use crate::storage::FixAttemptTracker;
use crate::templates::{TemplateContext, TemplateLoader, TemplateRenderer};
use crate::types::{ActivityLogEntry, ClaudeExecution, ClaudeResult, Issue};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Configuration for the Claude runner.
#[derive(Debug, Clone)]
pub struct ClaudeRunnerConfig {
    /// Timeout for Claude process execution in seconds (default: 21600 = 6 hours).
    pub timeout_secs: u64,
    /// Model to use (e.g., sonnet, opus, haiku, or full model ID).
    pub model: Option<String>,
    /// Custom instructions appended to Claude's system prompt.
    pub instructions: Option<String>,
    /// Tool permissions granted without prompting (--allowedTools).
    pub permissions: Vec<String>,
    /// Skip all permission prompts (default: true for backwards compat).
    pub skip_permissions: bool,
}

impl Default for ClaudeRunnerConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 21600, // 6 hours default
            model: None,
            instructions: None,
            permissions: Vec::new(),
            skip_permissions: true,
        }
    }
}

/// Runs Claude Code to fix issues.
pub struct ClaudeRunner {
    config: ClaudeRunnerConfig,
    template_renderer: TemplateRenderer,
    tracker: Arc<dyn FixAttemptTracker>,
}

impl ClaudeRunner {
    /// Create a new Claude runner.
    pub fn new(config: ClaudeRunnerConfig, tracker: Arc<dyn FixAttemptTracker>) -> Self {
        let template_renderer = TemplateRenderer::new();
        Self {
            config,
            template_renderer,
            tracker,
        }
    }

    /// Create a new Claude runner without template support (for testing).
    pub fn new_simple(config: ClaudeRunnerConfig) -> Self {
        use crate::storage::SqliteTracker;
        // Use a temporary in-memory tracker
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        Self::new(config, tracker)
    }

    /// Run Claude Code with a prompt to fix an issue in a specific repository.
    pub async fn run_fix(
        &self,
        issue: &Issue,
        context: &str,
        project_dir: &Path,
    ) -> Result<ClaudeResult> {
        let prompt = self.build_prompt(issue, context, project_dir);
        self.execute(&prompt, Some(issue), project_dir).await
    }

    /// Run Claude Code with the /issue skill (for Linear issues).
    pub async fn run_issue_skill(
        &self,
        issue_identifier: &str,
        issue_url: &str,
        project_dir: &Path,
    ) -> Result<ClaudeResult> {
        tracing::info!(component = "claude", issue_id = %issue_identifier, "Running /issue skill");

        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("LINEAR_ISSUE_ID".to_string(), issue_identifier.to_string());
        env.insert("LINEAR_ISSUE_URL".to_string(), issue_url.to_string());

        self.execute_with_env(
            &format!("/issue {}", issue_identifier),
            issue_identifier,
            env,
            project_dir,
        )
        .await
    }

    /// Run Claude Code with a custom prompt.
    pub async fn run_custom(&self, prompt: &str, project_dir: &Path) -> Result<ClaudeResult> {
        self.execute(prompt, None, project_dir).await
    }

    fn build_prompt(&self, issue: &Issue, context: &str, project_dir: &Path) -> String {
        // Try to use template system
        let template_loader = TemplateLoader::new(project_dir);
        if let Ok(template) = template_loader.get_template(issue) {
            let agent_md = template_loader.load_agent_md();
            let template_context =
                TemplateContext::new(issue.clone(), context.to_string()).with_agent_md(agent_md);
            return self.template_renderer.render(&template, &template_context);
        }

        // Fallback to simple format
        format!(
            r#"You are fixing an issue from {}. Here is the issue context:

{}

Your task:
1. Analyze the issue/error and any stack traces
2. Find the relevant code in this codebase
3. Implement a fix for the issue
4. Write or update tests if applicable
5. Create a PR with your changes

The PR title should include the issue ID: {}

After creating the PR, output the PR URL on a line by itself starting with "PR_URL: ".
"#,
            issue.source, context, issue.short_id
        )
    }

    /// Check if a project has an AGENT.md file.
    pub fn has_agent_md(&self, project_dir: &Path) -> bool {
        let template_loader = TemplateLoader::new(project_dir);
        template_loader.has_agent_md()
    }

    /// Get AGENT.md content if it exists.
    pub fn get_agent_md(&self, project_dir: &Path) -> Option<String> {
        let template_loader = TemplateLoader::new(project_dir);
        template_loader.load_agent_md()
    }

    /// Build a prompt for the given issue and context.
    ///
    /// This is public to allow callers to get the prompt string for analytics/logging.
    pub fn build_prompt_for_issue(
        &self,
        issue: &Issue,
        context: &str,
        project_dir: &Path,
    ) -> String {
        self.build_prompt(issue, context, project_dir)
    }

    async fn execute(
        &self,
        prompt: &str,
        issue: Option<&Issue>,
        project_dir: &Path,
    ) -> Result<ClaudeResult> {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        if let Some(issue) = issue {
            let source_upper = issue.source.to_uppercase();
            env.insert(format!("{}_ISSUE_ID", source_upper), issue.id.clone());
            env.insert(
                format!("{}_ISSUE_SHORT_ID", source_upper),
                issue.short_id.clone(),
            );
            env.insert(format!("{}_ISSUE_URL", source_upper), issue.url.clone());
        }

        let label = issue.map(|i| i.short_id.as_str()).unwrap_or("custom");
        self.execute_with_env(prompt, label, env, project_dir).await
    }

    async fn execute_with_env(
        &self,
        prompt: &str,
        label: &str,
        env: HashMap<String, String>,
        project_dir: &Path,
    ) -> Result<ClaudeResult> {
        self.execute_with_env_and_attempt(prompt, label, env, None, project_dir)
            .await
    }

    /// Execute Claude with optional attempt ID for analytics tracking.
    pub async fn execute_with_attempt(
        &self,
        prompt: &str,
        issue: Option<&Issue>,
        attempt_id: Option<i64>,
        project_dir: &Path,
    ) -> Result<ClaudeResult> {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        if let Some(issue) = issue {
            let source_upper = issue.source.to_uppercase();
            env.insert(format!("{}_ISSUE_ID", source_upper), issue.id.clone());
            env.insert(
                format!("{}_ISSUE_SHORT_ID", source_upper),
                issue.short_id.clone(),
            );
            env.insert(format!("{}_ISSUE_URL", source_upper), issue.url.clone());
        }

        let label = issue.map(|i| i.short_id.as_str()).unwrap_or("custom");
        self.execute_with_env_and_attempt(prompt, label, env, attempt_id, project_dir)
            .await
    }

    async fn execute_with_env_and_attempt(
        &self,
        prompt: &str,
        label: &str,
        env: HashMap<String, String>,
        attempt_id: Option<i64>,
        project_dir: &Path,
    ) -> Result<ClaudeResult> {
        // Create execution record for analytics
        let mut execution = ClaudeExecution::new();
        if let Some(id) = attempt_id {
            execution = execution.with_attempt_id(id);
        }
        execution.prompt_used = Some(prompt.to_string());
        execution.prompt_hash = Some(Self::hash_prompt(prompt));
        execution.model_version = Some(
            self.config
                .model
                .clone()
                .unwrap_or_else(|| "claude-code".to_string()),
        );
        execution.working_directory = Some(project_dir.display().to_string());

        tracing::info!(
            component = "claude",
            label = label,
            timeout_secs = self.config.timeout_secs,
            "Starting execution"
        );

        // Log claude_started activity
        let activity = ActivityLogEntry::new(
            "claude_started",
            format!("Claude execution started for {}", label),
        )
        .with_source("claude".to_string())
        .with_metadata(json!({
            "timeout_secs": self.config.timeout_secs,
            "working_dir": project_dir.display().to_string(),
            "label": label
        }));
        self.tracker.record_activity(&activity).ok();

        let mut args = vec!["--print".to_string()];
        if self.config.skip_permissions {
            args.push("--dangerously-skip-permissions".to_string());
        }
        if let Some(ref model) = self.config.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(ref instructions) = self.config.instructions {
            args.push("--append-system-prompt".to_string());
            args.push(instructions.clone());
        }
        for perm in &self.config.permissions {
            args.push("--allowedTools".to_string());
            args.push(perm.clone());
        }
        args.push(prompt.to_string());

        let mut child = Command::new("claude")
            .args(&args)
            .current_dir(project_dir)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::runner(format!("Failed to spawn claude: {}", e)))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::runner("Failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::runner("Failed to capture stderr"))?;

        let label_stdout = label.to_string();
        let label_stderr = label.to_string();

        // Stream stdout
        let stdout_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut output = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    tracing::info!(
                        component = "claude",
                        label = label_stdout.as_str(),
                        "{}",
                        line
                    );
                }
                output.push_str(&line);
                output.push('\n');
            }
            output
        });

        // Stream stderr
        let stderr_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut output = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    tracing::error!(
                        component = "claude",
                        label = label_stderr.as_str(),
                        "{}",
                        line
                    );
                }
                output.push_str(&line);
                output.push('\n');
            }
            output
        });

        // Wait for process with timeout
        let timeout_duration = std::time::Duration::from_secs(self.config.timeout_secs);
        let wait_result = tokio::time::timeout(timeout_duration, child.wait()).await;

        let (status, timed_out) = match wait_result {
            Ok(Ok(status)) => (status, false),
            Ok(Err(e)) => {
                return Err(Error::runner(format!("Failed to wait for claude: {}", e)));
            }
            Err(_) => {
                // Timeout occurred - try to kill the process
                tracing::error!(
                    component = "claude",
                    label = label,
                    timeout_secs = self.config.timeout_secs,
                    "Process timed out, attempting to kill"
                );
                if let Err(e) = child.kill().await {
                    tracing::error!(component = "claude", error = %e, "Failed to kill timed-out process");
                }

                // Log claude_timed_out activity
                let activity = ActivityLogEntry::new(
                    "claude_timed_out",
                    format!("Claude timed out for {}", label),
                )
                .with_source("claude".to_string())
                .with_metadata(json!({
                    "timeout_secs": self.config.timeout_secs,
                    "label": label
                }));
                self.tracker.record_activity(&activity).ok();

                // Record the timed-out execution
                execution.complete(None, true);
                execution.stderr_preview = Some(format!(
                    "Process timed out after {} seconds",
                    self.config.timeout_secs
                ));
                if let Err(e) = self.tracker.record_execution(&execution) {
                    tracing::warn!(error = %e, "Failed to record timed-out execution to database");
                }

                // Return a result indicating timeout
                return Ok(ClaudeResult {
                    success: false,
                    output: String::new(),
                    pr_url: None,
                    error: Some(format!(
                        "Process timed out after {} seconds",
                        self.config.timeout_secs
                    )),
                });
            }
        };

        let stdout_output = stdout_handle.await.unwrap_or_default();
        let stderr_output = stderr_handle.await.unwrap_or_default();

        let exit_code = status.code().unwrap_or(-1);
        tracing::info!(
            component = "claude",
            label = label,
            exit_code = exit_code,
            timed_out = timed_out,
            "Process completed"
        );

        let pr_url = Self::extract_pr_url(&stdout_output);

        if let Some(ref url) = pr_url {
            tracing::info!(
                component = "claude",
                label = label,
                pr_url = url,
                "PR URL extracted"
            );
        }

        // Complete and record the execution
        execution.complete(status.code(), timed_out);
        execution.stdout_preview = Some(Self::truncate(&stdout_output, 2000));
        execution.stderr_preview = if stderr_output.is_empty() {
            None
        } else {
            Some(Self::truncate(&stderr_output, 2000))
        };

        // Record the execution to the database (don't fail the main operation if this fails)
        if let Err(e) = self.tracker.record_execution(&execution) {
            tracing::warn!(error = %e, "Failed to record execution to database");
        }

        // Log completion activity
        if status.success() {
            let activity = ActivityLogEntry::new(
                "claude_completed",
                format!("Claude completed for {}", label),
            )
            .with_source("claude".to_string())
            .with_metadata(json!({
                "duration_secs": execution.duration_secs,
                "exit_code": exit_code,
                "has_pr": pr_url.is_some(),
                "label": label
            }));
            self.tracker.record_activity(&activity).ok();
        } else {
            let error_msg = if stderr_output.is_empty() {
                format!("Process exited with code {}", exit_code)
            } else {
                stderr_output.clone()
            };
            let activity = ActivityLogEntry::new(
                "claude_failed",
                format!(
                    "Claude failed for {}: {}",
                    label,
                    Self::truncate(&error_msg, 100)
                ),
            )
            .with_source("claude".to_string())
            .with_metadata(json!({
                "duration_secs": execution.duration_secs,
                "exit_code": exit_code,
                "error": Self::truncate(&error_msg, 500),
                "label": label
            }));
            self.tracker.record_activity(&activity).ok();
        }

        Ok(ClaudeResult {
            success: status.success(),
            output: stdout_output,
            pr_url,
            error: if status.success() {
                None
            } else {
                Some(if stderr_output.is_empty() {
                    format!("Process exited with code {}", exit_code)
                } else {
                    stderr_output
                })
            },
        })
    }

    /// Compute a SHA256 hash of the prompt for grouping similar prompts.
    fn hash_prompt(prompt: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prompt.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)[..16].to_string() // First 16 hex chars
    }

    /// Truncate a string to approximately max_len bytes, adding "..." if truncated.
    /// Ensures the cut happens at a valid UTF-8 char boundary.
    fn truncate(s: &str, max_len: usize) -> String {
        if s.len() <= max_len {
            s.to_string()
        } else {
            let end = max_len.saturating_sub(3);
            // Find the nearest char boundary at or before `end`
            let safe_end = s
                .char_indices()
                .take_while(|(i, _)| *i <= end)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            format!("{}...", &s[..safe_end])
        }
    }

    /// Extract PR URL from output.
    fn extract_pr_url(output: &str) -> Option<String> {
        // Try explicit PR_URL format first
        if let Some(captures) = regex_lite::Regex::new(r"PR_URL:\s*(https://[^\s]+)")
            .ok()
            .and_then(|re| re.captures(output))
        {
            return captures.get(1).map(|m| m.as_str().to_string());
        }

        // Try GitHub PR URL pattern
        if let Some(captures) = regex_lite::Regex::new(r"https://github\.com/[^\s]+/pull/\d+")
            .ok()
            .and_then(|re| re.find(output))
        {
            return Some(captures.as_str().to_string());
        }

        // Try GitLab MR URL pattern
        if let Some(captures) =
            regex_lite::Regex::new(r"https://gitlab\.com/[^\s]+/-/merge_requests/\d+")
                .ok()
                .and_then(|re| re.find(output))
        {
            return Some(captures.as_str().to_string());
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_pr_url_explicit() {
        let output = "Some output\nPR_URL: https://github.com/org/repo/pull/123\nMore output";
        assert_eq!(
            ClaudeRunner::extract_pr_url(output),
            Some("https://github.com/org/repo/pull/123".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_github() {
        let output = "Created PR at https://github.com/myorg/myrepo/pull/456 successfully";
        assert_eq!(
            ClaudeRunner::extract_pr_url(output),
            Some("https://github.com/myorg/myrepo/pull/456".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_gitlab() {
        let output = "MR created: https://gitlab.com/group/project/-/merge_requests/789";
        assert_eq!(
            ClaudeRunner::extract_pr_url(output),
            Some("https://gitlab.com/group/project/-/merge_requests/789".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_none() {
        let output = "No PR URL in this output";
        assert_eq!(ClaudeRunner::extract_pr_url(output), None);
    }

    #[test]
    fn test_build_prompt() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        let context = "Issue description here";
        let project_dir = std::path::Path::new("/tmp");

        let prompt = runner.build_prompt(&issue, context, project_dir);
        assert!(prompt.contains("Linear"));
        assert!(prompt.contains("Issue description here") || prompt.contains("context"));
        assert!(prompt.contains("PROJ-123"));
        assert!(prompt.contains("PR_URL"));
    }

    #[test]
    fn test_extract_pr_url_with_trailing_punctuation() {
        let output = "Created PR: https://github.com/org/repo/pull/123.";
        // The regex will stop at whitespace, so trailing period might be included
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_some());
        let url = url.unwrap();
        assert!(url.starts_with("https://github.com/org/repo/pull/123"));
    }

    #[test]
    fn test_extract_pr_url_multiple_urls() {
        let output = "First PR: https://github.com/org/repo1/pull/100\nSecond PR: https://github.com/org/repo2/pull/200";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_some());
        // Should find the first one
        assert!(url.unwrap().contains("/pull/"));
    }

    #[test]
    fn test_extract_pr_url_explicit_takes_precedence() {
        let output = "Random text https://github.com/org/repo/pull/999\nPR_URL: https://github.com/org/main/pull/123";
        let url = ClaudeRunner::extract_pr_url(output);
        // PR_URL should take precedence
        assert_eq!(
            url,
            Some("https://github.com/org/main/pull/123".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_with_query_params() {
        // GitHub URLs might have query params, but our regex stops at whitespace
        let output = "PR at https://github.com/org/repo/pull/123?diff=split created";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_some());
    }

    #[test]
    fn test_extract_pr_url_gitlab_nested_groups() {
        let output = "MR: https://gitlab.com/group/subgroup/project/-/merge_requests/42";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(
            url,
            Some("https://gitlab.com/group/subgroup/project/-/merge_requests/42".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_empty_string() {
        assert_eq!(ClaudeRunner::extract_pr_url(""), None);
    }

    #[test]
    fn test_extract_pr_url_similar_but_not_valid() {
        // Not a valid PR URL
        let output = "See https://github.com/org/repo/issues/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_none());
    }

    #[test]
    fn test_build_prompt_sentry_source() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new(
            "456",
            "SENTRY-456",
            "TypeError in main.js",
            "https://sentry.io/123",
            "sentry",
        );
        let context = "Stack trace here";

        let project_dir = std::path::Path::new("/tmp");
        let prompt = runner.build_prompt(&issue, context, project_dir);
        // Prompt should be non-empty and contain either source name or context
        assert!(!prompt.is_empty());
        // Either templates are used (which have different content) or fallback
        assert!(
            prompt.contains("sentry")
                || prompt.contains("Sentry")
                || prompt.contains("Stack trace")
                || prompt.contains("TypeError")
        );
    }

    #[test]
    fn test_claude_result_success() {
        let result = ClaudeResult {
            success: true,
            output: "Success output".to_string(),
            pr_url: Some("https://github.com/org/repo/pull/123".to_string()),
            error: None,
        };

        assert!(result.success);
        assert!(result.pr_url.is_some());
        assert!(result.error.is_none());
    }

    #[test]
    fn test_claude_result_failure() {
        let result = ClaudeResult {
            success: false,
            output: "Error occurred".to_string(),
            pr_url: None,
            error: Some("Error message".to_string()),
        };

        assert!(!result.success);
        assert!(result.pr_url.is_none());
        assert!(result.error.is_some());
    }

    #[test]
    fn test_runner_config() {
        let config = ClaudeRunnerConfig::default();

        let runner = ClaudeRunner::new_simple(config);
        // Verify runner can be created
        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "linear");
        let project_dir = std::path::Path::new("/path/to/project");
        let prompt = runner.build_prompt(&issue, "context", project_dir);
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_claude_result_default_fields() {
        let result = ClaudeResult {
            success: false,
            output: String::new(),
            pr_url: None,
            error: None,
        };

        assert!(!result.success);
        assert!(result.output.is_empty());
        assert!(result.pr_url.is_none());
        assert!(result.error.is_none());
    }

    #[test]
    fn test_claude_runner_config_debug() {
        let config = ClaudeRunnerConfig {
            timeout_secs: 3600,
            ..Default::default()
        };
        let debug = format!("{:?}", config);
        assert!(debug.contains("timeout_secs"));
    }

    #[test]
    fn test_extract_pr_url_whitespace_only() {
        assert_eq!(ClaudeRunner::extract_pr_url("   \n\t  "), None);
    }

    #[test]
    fn test_extract_pr_url_github_enterprise() {
        // GitHub enterprise URLs might have different domains
        let output = "PR at https://github.mycompany.com/org/repo/pull/99";
        // This should NOT match since it's not github.com
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_none());
    }

    #[test]
    fn test_extract_pr_url_with_newlines() {
        let output = "PR created\n\nhttps://github.com/org/repo/pull/42\n\nDone";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_some());
        assert!(url.unwrap().contains("/pull/42"));
    }

    #[test]
    fn test_extract_pr_url_pr_url_colon_space() {
        let output = "PR_URL:   https://github.com/org/repo/pull/1  ";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(url, Some("https://github.com/org/repo/pull/1".to_string()));
    }

    #[test]
    fn test_build_prompt_github_source() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new(
            "789",
            "#789",
            "Add feature",
            "https://github.com/org/repo/issues/789",
            "github",
        );
        let context = "Feature description";
        let project_dir = std::path::Path::new("/tmp");

        let prompt = runner.build_prompt(&issue, context, project_dir);
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_build_prompt_unknown_source() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new(
            "123",
            "JIRA-123",
            "Bug fix",
            "https://jira.example.com/123",
            "jira",
        );
        let context = "Bug details";
        let project_dir = std::path::Path::new("/tmp");

        let prompt = runner.build_prompt(&issue, context, project_dir);
        // Should still produce a prompt even for unknown source
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_has_agent_md() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let project_dir = std::path::Path::new("/nonexistent/path");
        // No AGENT.md should exist at nonexistent path
        assert!(!runner.has_agent_md(project_dir));
    }

    #[test]
    fn test_get_agent_md_nonexistent() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let project_dir = std::path::Path::new("/nonexistent/path");
        // Should return None for nonexistent AGENT.md
        assert!(runner.get_agent_md(project_dir).is_none());
    }

    #[test]
    fn test_claude_result_with_long_output() {
        let output = "x".repeat(10000);
        let result = ClaudeResult {
            success: true,
            output,
            pr_url: None,
            error: None,
        };

        assert!(result.success);
        assert_eq!(result.output.len(), 10000);
    }

    #[test]
    fn test_claude_result_with_empty_error() {
        let result = ClaudeResult {
            success: false,
            output: "Output".to_string(),
            pr_url: None,
            error: Some(String::new()),
        };

        // Even empty error is Some("")
        assert!(result.error.is_some());
        assert!(result.error.unwrap().is_empty());
    }

    #[test]
    fn test_extract_pr_url_case_sensitivity() {
        // PR_URL: pattern should be case sensitive, but GitHub URL pattern is separate
        let output = "pr_url: https://github.com/org/repo/pull/1";
        let url = ClaudeRunner::extract_pr_url(output);
        // The GitHub URL regex will match regardless of the pr_url: prefix
        // This test verifies the behavior: PR_URL: must be uppercase to be the explicit format
        // But the GitHub URL fallback will still match
        assert!(url.is_some());
    }

    #[test]
    fn test_extract_pr_url_gitlab_with_path() {
        let output = "MR: https://gitlab.com/a/b/c/d/-/merge_requests/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(
            url,
            Some("https://gitlab.com/a/b/c/d/-/merge_requests/123".to_string())
        );
    }

    #[test]
    fn test_claude_runner_config_clone() {
        let config = ClaudeRunnerConfig {
            timeout_secs: 3600,
            ..Default::default()
        };
        let cloned = config.clone();
        assert_eq!(cloned.timeout_secs, config.timeout_secs);
        assert_eq!(cloned.skip_permissions, config.skip_permissions);
    }

    #[test]
    fn test_build_prompt_empty_context() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new("1", "TEST-1", "Test", "https://example.com", "linear");
        let project_dir = std::path::Path::new("/tmp");
        let prompt = runner.build_prompt(&issue, "", project_dir);
        // Should still produce a prompt even with empty context
        assert!(!prompt.is_empty());
        assert!(prompt.contains("PROJ") || prompt.contains("TEST-1") || prompt.contains("Linear"));
    }

    #[test]
    fn test_build_prompt_special_characters() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new(
            "1",
            "TEST-1",
            "Fix \"quoted\" issue & <special>",
            "https://example.com",
            "linear",
        );
        let context = "Context with special chars: <>&\"'";
        let project_dir = std::path::Path::new("/tmp");

        let prompt = runner.build_prompt(&issue, context, project_dir);
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_extract_pr_url_no_scheme() {
        let output = "github.com/org/repo/pull/123";
        let url = ClaudeRunner::extract_pr_url(output);
        // Should not match without https://
        assert!(url.is_none());
    }

    #[test]
    fn test_extract_pr_url_http_not_https() {
        // HTTP URLs should not match (security)
        let output = "PR at http://github.com/org/repo/pull/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_none());
    }

    #[test]
    fn test_extract_pr_url_multiline_pr_url() {
        let output = "Creating PR...\nPR_URL: https://github.com/org/repo/pull/42\nDone!";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(url, Some("https://github.com/org/repo/pull/42".to_string()));
    }

    #[test]
    fn test_build_prompt_multiline_context() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new("1", "TEST-1", "Bug", "https://example.com", "linear");
        let context = "Line 1\nLine 2\nLine 3\n";
        let project_dir = std::path::Path::new("/tmp");

        let prompt = runner.build_prompt(&issue, context, project_dir);
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_extract_pr_url_gitlab_self_hosted() {
        // Only gitlab.com should match, not self-hosted
        let output = "MR: https://gitlab.mycompany.com/group/project/-/merge_requests/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_none());
    }

    #[test]
    fn test_extract_pr_url_github_pr_zero() {
        // PR number 0 is technically invalid but should match regex
        let output = "PR: https://github.com/org/repo/pull/0";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_some());
    }

    #[test]
    fn test_extract_pr_url_very_long_pr_number() {
        let output = "PR: https://github.com/org/repo/pull/999999999999";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(
            url,
            Some("https://github.com/org/repo/pull/999999999999".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_org_with_dashes() {
        let output = "PR: https://github.com/my-org-name/my-repo-name/pull/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(
            url,
            Some("https://github.com/my-org-name/my-repo-name/pull/123".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_org_with_dots() {
        let output = "PR: https://github.com/org.name/repo.name/pull/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(
            url,
            Some("https://github.com/org.name/repo.name/pull/123".to_string())
        );
    }

    #[test]
    fn test_claude_result_pr_url_with_trailing_slash() {
        // Test PR URL doesn't capture trailing slash
        let output = "PR_URL: https://github.com/org/repo/pull/123/ Done";
        let url = ClaudeRunner::extract_pr_url(output);
        // The regex matches up to whitespace, so trailing slash would be captured
        assert!(url.is_some());
    }

    #[test]
    fn test_build_prompt_unicode_context() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());

        let issue = Issue::new(
            "1",
            "TEST-1",
            "Fix 日本語 issue",
            "https://example.com",
            "linear",
        );
        let context = "Context with emoji 🎉 and unicode ñ";
        let project_dir = std::path::Path::new("/tmp");

        let prompt = runner.build_prompt(&issue, context, project_dir);
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_claude_runner_new_with_tracker() {
        use crate::storage::SqliteTracker;
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let config = ClaudeRunnerConfig::default();
        let runner = ClaudeRunner::new(config, tracker);
        let project_dir = std::path::Path::new("/tmp");
        assert!(!runner.has_agent_md(project_dir));
    }

    #[test]
    fn test_issue_creation_for_runner() {
        let issue = Issue::new("id123", "SHORT-123", "Title", "https://url.com", "linear");
        assert_eq!(issue.id, "id123");
        assert_eq!(issue.short_id, "SHORT-123");
        assert_eq!(issue.title, "Title");
        assert_eq!(issue.url, "https://url.com");
        assert_eq!(issue.source, "linear");
    }
}
