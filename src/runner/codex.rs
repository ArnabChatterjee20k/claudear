//! Codex CLI agent runner.

use super::{AgentRunner, ProviderCapabilities};
use crate::error::{Error, Result};
use crate::storage::FixAttemptTracker;
use crate::templates::{TemplateContext, TemplateLoader, TemplateRenderer};
use crate::types::{ActivityLogEntry, AgentExecution, AgentResult, Issue};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Configuration for the Codex agent runner.
#[derive(Debug, Clone)]
pub struct CodexRunnerConfig {
    /// Timeout for Codex process execution in seconds (default: 21600 = 6 hours).
    pub timeout_secs: u64,
    /// Model to use (e.g., "o3").
    pub model: Option<String>,
    /// Custom instructions.
    pub instructions: Option<String>,
    /// CLI binary name/path (default: "codex").
    pub binary: String,
    /// Sandbox mode: "network-off" or "network-on".
    pub sandbox: Option<String>,
}

impl Default for CodexRunnerConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 21600,
            model: None,
            instructions: None,
            binary: "codex".to_string(),
            sandbox: Some("network-off".to_string()),
        }
    }
}

/// Codex CLI agent runner.
pub struct CodexAgentRunner {
    config: CodexRunnerConfig,
    template_renderer: TemplateRenderer,
    tracker: Arc<dyn FixAttemptTracker>,
}

impl CodexAgentRunner {
    /// Create a new Codex agent runner.
    pub fn new(config: CodexRunnerConfig, tracker: Arc<dyn FixAttemptTracker>) -> Self {
        Self {
            config,
            template_renderer: TemplateRenderer::new(),
            tracker,
        }
    }

    fn build_prompt(&self, issue: &Issue, context: &str, project_dir: &Path) -> String {
        let template_loader = TemplateLoader::new(project_dir);
        if let Ok(template) = template_loader.get_template(issue) {
            let agent_md = template_loader.load_agent_md();
            let template_context =
                TemplateContext::new(issue.clone(), context.to_string()).with_agent_md(agent_md);
            return self.template_renderer.render(&template, &template_context);
        }

        format!(
            r#"You are fixing an issue from {}. Here is the issue context:

{}

Your task:
1. Analyze the issue/error and any stack traces
2. Find the relevant code in this codebase
3. Implement a fix for the issue
4. Write or update tests if applicable
5. Create a PR with your changes
6. Ensure all checks pass on the PR

The PR title should include the issue ID: {}
"#,
            issue.source, context, issue.short_id
        )
    }

    /// Extract a PR URL from Codex text output.
    fn extract_pr_url(output: &str) -> Option<String> {
        use std::sync::LazyLock;

        static GITHUB_PR_RE: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
            regex_lite::Regex::new(r"https://github\.com/[^\s/]+/[^\s/]+/pull/\d+[^\s]*").unwrap()
        });
        static GITLAB_MR_RE: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
            regex_lite::Regex::new(r"https://gitlab\.com/[^\s]+/-/merge_requests/\d+[^\s]*")
                .unwrap()
        });

        if let Some(m) = GITHUB_PR_RE.find(output) {
            return Some(m.as_str().to_string());
        }
        if let Some(m) = GITLAB_MR_RE.find(output) {
            return Some(m.as_str().to_string());
        }
        None
    }
}

#[async_trait]
impl AgentRunner for CodexAgentRunner {
    fn name(&self) -> &str {
        "codex"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            structured_output: false,
            tool_permissions: false,
            custom_instructions: true,
            streaming_events: false,
            cost_reporting: false,
        }
    }

    fn build_prompt_for_issue(
        &self,
        issue: &Issue,
        context: &str,
        project_dir: &Path,
    ) -> String {
        self.build_prompt(issue, context, project_dir)
    }

    async fn execute_with_attempt(
        &self,
        prompt: &str,
        issue: Option<&Issue>,
        attempt_id: Option<i64>,
        project_dir: &Path,
    ) -> Result<AgentResult> {
        let label = issue
            .map(|i| i.short_id.as_str())
            .unwrap_or("custom");

        let mut execution = AgentExecution::new();
        execution.provider = Some("codex".to_string());
        if let Some(id) = attempt_id {
            execution = execution.with_attempt_id(id);
        }
        execution.prompt_used = Some(prompt.to_string());
        execution.model_version = self.config.model.clone();
        execution.working_directory = Some(project_dir.display().to_string());

        tracing::info!(
            component = "codex",
            label = label,
            timeout_secs = self.config.timeout_secs,
            "Starting Codex execution"
        );

        let activity = ActivityLogEntry::new(
            "agent_started",
            format!("Codex execution started for {}", label),
        )
        .with_source("codex".to_string())
        .with_metadata(json!({
            "timeout_secs": self.config.timeout_secs,
            "working_dir": project_dir.display().to_string(),
            "label": label
        }));
        self.tracker.record_activity(&activity).ok();

        let mut args = Vec::new();
        if let Some(ref model) = self.config.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args.push("--quiet".to_string());
        args.push("--full-auto".to_string());
        args.push(prompt.to_string());

        let mut child = match tokio::process::Command::new(&self.config.binary)
            .args(&args)
            .current_dir(project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                return Err(Error::runner(format!(
                    "Failed to spawn {}: {}",
                    self.config.binary, e
                )));
            }
        };

        let stdout = child.stdout.take().ok_or_else(|| Error::runner("Failed to capture stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| Error::runner("Failed to capture stderr"))?;

        let stdout_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut output = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                output.push_str(&line);
                output.push('\n');
            }
            output
        });

        let stderr_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut output = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                output.push_str(&line);
                output.push('\n');
            }
            output
        });

        let timeout_duration = std::time::Duration::from_secs(self.config.timeout_secs);

        enum WaitOutcome {
            Exited(std::result::Result<std::process::ExitStatus, std::io::Error>),
            TimedOut,
        }

        let outcome = tokio::select! {
            result = child.wait() => WaitOutcome::Exited(result),
            _ = tokio::time::sleep(timeout_duration) => WaitOutcome::TimedOut,
        };

        let (status, timed_out) = match outcome {
            WaitOutcome::Exited(Ok(status)) => (status, false),
            WaitOutcome::Exited(Err(e)) => {
                return Err(Error::runner(format!(
                    "Failed to wait for {}: {}",
                    self.config.binary, e
                )));
            }
            WaitOutcome::TimedOut => {
                tracing::error!(
                    component = "codex",
                    label = label,
                    timeout_secs = self.config.timeout_secs,
                    "Process timed out, attempting to kill"
                );
                let _ = child.kill().await;

                execution.complete(None, true);
                self.tracker.record_execution(&execution).ok();

                return Ok(AgentResult {
                    success: false,
                    output: String::new(),
                    pr_url: None,
                    changelog: None,
                    error: Some(format!(
                        "Process timed out after {} seconds",
                        self.config.timeout_secs
                    )),
                    blocking_question: None,
                    used_qa_ids: Vec::new(),
                });
            }
        };

        let stdout_output = stdout_handle.await.unwrap_or_default();
        let stderr_output = stderr_handle.await.unwrap_or_default();

        let exit_code = status.code().unwrap_or(-1);
        let success = status.success();
        let pr_url = Self::extract_pr_url(&stdout_output);

        execution.complete(status.code(), timed_out);
        execution.stdout_preview = Some(
            stdout_output
                .chars()
                .take(2000)
                .collect::<String>(),
        );
        execution.stderr_preview = if stderr_output.is_empty() {
            None
        } else {
            Some(stderr_output.chars().take(2000).collect::<String>())
        };
        self.tracker.record_execution(&execution).ok();

        let activity = if success {
            ActivityLogEntry::new(
                "agent_completed",
                format!("Codex completed for {}", label),
            )
            .with_source("codex".to_string())
            .with_metadata(json!({
                "duration_secs": execution.duration_secs,
                "exit_code": exit_code,
                "has_pr": pr_url.is_some(),
                "label": label
            }))
        } else {
            ActivityLogEntry::new(
                "agent_failed",
                format!("Codex failed for {} (exit {})", label, exit_code),
            )
            .with_source("codex".to_string())
            .with_metadata(json!({
                "duration_secs": execution.duration_secs,
                "exit_code": exit_code,
                "label": label
            }))
        };
        self.tracker.record_activity(&activity).ok();

        let error = if success {
            None
        } else {
            let err = stderr_output.trim();
            if err.is_empty() {
                Some(format!("Process exited with code {}", exit_code))
            } else {
                Some(err.to_string())
            }
        };

        Ok(AgentResult {
            success,
            output: stdout_output,
            pr_url,
            changelog: None,
            error,
            blocking_question: None,
            used_qa_ids: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codex_runner_config_default() {
        let config = CodexRunnerConfig::default();
        assert_eq!(config.timeout_secs, 21600);
        assert_eq!(config.binary, "codex");
        assert!(config.model.is_none());
    }

    #[test]
    fn test_extract_pr_url_github() {
        let output = "Created PR at https://github.com/org/repo/pull/123\n";
        assert_eq!(
            CodexAgentRunner::extract_pr_url(output),
            Some("https://github.com/org/repo/pull/123".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_gitlab() {
        let output = "MR: https://gitlab.com/group/project/-/merge_requests/42\n";
        assert_eq!(
            CodexAgentRunner::extract_pr_url(output),
            Some("https://gitlab.com/group/project/-/merge_requests/42".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_none() {
        assert_eq!(CodexAgentRunner::extract_pr_url("no urls here"), None);
    }

    #[test]
    fn test_build_prompt() {
        use crate::storage::SqliteTracker;
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let runner = CodexAgentRunner::new(CodexRunnerConfig::default(), tracker);
        let issue = Issue::new("1", "TEST-1", "Bug", "https://example.com", "linear");
        let prompt = runner.build_prompt(&issue, "context", Path::new("/tmp"));
        assert!(prompt.contains("TEST-1"));
    }

    #[test]
    fn test_capabilities() {
        use crate::storage::SqliteTracker;
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let runner = CodexAgentRunner::new(CodexRunnerConfig::default(), tracker);
        let caps = runner.capabilities();
        assert!(!caps.structured_output);
        assert!(!caps.tool_permissions);
        assert!(caps.custom_instructions);
    }
}
