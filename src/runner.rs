//! Claude CLI runner for executing fixes.

use crate::error::{Error, Result};
use crate::storage::FixAttemptTracker;
use crate::templates::{TemplateContext, TemplateLoader, TemplateRenderer};
use crate::types::{ActivityLogEntry, BlockingQuestion, ClaudeExecution, ClaudeResult, Issue};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

const DEFAULT_LOG_DIR: &str = "./logs";
const CLAUDE_LOG_SUBDIR: &str = "claude";
const EXECUTION_LOG_PREVIEW_LIMIT: usize = 2000;
const QUESTION_PROTOCOL_PREFIX: &str = "CLAUDEAR_QUESTION:";
const QUESTION_PROTOCOL_INSTRUCTIONS: &str = r#"
If you are blocked because you need human input, emit exactly one line in this format and then stop:
CLAUDEAR_QUESTION: {"question":"...","context":"...","options":["..."],"why":"..."}
Use valid JSON and keep question concise.
"#;

#[derive(Debug, Clone)]
struct ExecutionLogFiles {
    stdout: PathBuf,
    stderr: PathBuf,
}

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
            return Self::append_question_protocol(
                &self.template_renderer.render(&template, &template_context),
            );
        }

        // Fallback to simple format
        let prompt = format!(
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
        );

        Self::append_question_protocol(&prompt)
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

    /// Best-effort detection for Claude/API rate limit failures.
    pub fn is_rate_limit_error(message: &str) -> bool {
        let lower = message.to_lowercase();
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
        Self::is_rate_limit_error(message)
            || [
                "failed to spawn claude",
                "failed to wait for claude",
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

    fn sanitize_label(label: &str) -> String {
        let mut out = String::with_capacity(label.len().min(64));
        for ch in label.chars().take(64) {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        if out.is_empty() {
            "custom".to_string()
        } else {
            out
        }
    }

    fn resolve_log_root() -> PathBuf {
        std::env::var("CLAUDEAR_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_LOG_DIR))
    }

    fn create_execution_log_files(label: &str) -> Option<ExecutionLogFiles> {
        let root = Self::resolve_log_root();
        if root.as_os_str().is_empty() {
            return None;
        }

        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let dir = root.join(CLAUDE_LOG_SUBDIR).join(day);

        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                component = "claude",
                path = %dir.display(),
                error = %e,
                "Failed to create execution log directory"
            );
            return None;
        }

        let safe_label = Self::sanitize_label(label);
        let pid = std::process::id();
        let stem = format!("{}_{}_{}", timestamp, pid, safe_label);

        Some(ExecutionLogFiles {
            stdout: dir.join(format!("{}.stdout.log", stem)),
            stderr: dir.join(format!("{}.stderr.log", stem)),
        })
    }

    fn compose_failure_message(exit_code: i32, stdout_output: &str, stderr_output: &str) -> String {
        let stderr_trimmed = stderr_output.trim();
        let stdout_trimmed = stdout_output.trim();
        let combined = if stderr_trimmed.is_empty() {
            stdout_trimmed.to_string()
        } else if stdout_trimmed.is_empty() {
            stderr_trimmed.to_string()
        } else {
            format!("{}\n{}", stderr_trimmed, stdout_trimmed)
        };

        if Self::is_rate_limit_error(&combined) {
            return format!(
                "Claude rate limit hit: {}",
                Self::truncate(
                    if combined.is_empty() {
                        "Too many requests".to_string()
                    } else {
                        combined
                    }
                    .as_str(),
                    EXECUTION_LOG_PREVIEW_LIMIT
                )
            );
        }

        if !stderr_trimmed.is_empty() {
            return Self::truncate(stderr_trimmed, EXECUTION_LOG_PREVIEW_LIMIT);
        }

        if !stdout_trimmed.is_empty() {
            return format!(
                "Process exited with code {}. Output: {}",
                exit_code,
                Self::truncate(stdout_trimmed, EXECUTION_LOG_PREVIEW_LIMIT)
            );
        }

        format!("Process exited with code {}", exit_code)
    }

    fn append_question_protocol(prompt: &str) -> String {
        if prompt.contains(QUESTION_PROTOCOL_PREFIX) {
            prompt.to_string()
        } else {
            format!("{prompt}\n\n{}", QUESTION_PROTOCOL_INSTRUCTIONS.trim())
        }
    }

    fn extract_blocking_question(output: &str) -> Option<BlockingQuestion> {
        output.lines().find_map(|line| {
            let trimmed = line.trim();
            let payload = trimmed.strip_prefix(QUESTION_PROTOCOL_PREFIX)?.trim();
            if payload.is_empty() {
                return None;
            }
            serde_json::from_str::<BlockingQuestion>(payload).ok()
        })
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
        let prompt_with_protocol = Self::append_question_protocol(prompt);
        execution.prompt_used = Some(prompt_with_protocol.clone());
        execution.prompt_hash = Some(Self::hash_prompt(&prompt_with_protocol));
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
        args.push(prompt_with_protocol);

        let log_files = Self::create_execution_log_files(label);
        if let Some(ref files) = log_files {
            execution.stdout_log_path = Some(files.stdout.display().to_string());
            execution.stderr_log_path = Some(files.stderr.display().to_string());
            tracing::info!(
                component = "claude",
                label = label,
                stdout_log = %files.stdout.display(),
                stderr_log = %files.stderr.display(),
                "Capturing Claude output to execution log files"
            );
        }

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
        let stdout_log_path = log_files.as_ref().map(|f| f.stdout.clone());
        let stderr_log_path = log_files.as_ref().map(|f| f.stderr.clone());

        let (question_tx, question_rx) = oneshot::channel::<()>();
        let stdout_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut output = String::new();
            let mut question_tx = Some(question_tx);
            let mut writer = match stdout_log_path {
                Some(path) => match tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await
                {
                    Ok(file) => Some(file),
                    Err(e) => {
                        tracing::warn!(
                            component = "claude",
                            label = label_stdout.as_str(),
                            path = %path.display(),
                            error = %e,
                            "Failed to open stdout execution log file"
                        );
                        None
                    }
                },
                None => None,
            };
            let mut write_failed = false;

            loop {
                let next_line = lines.next_line().await;
                let line = match next_line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(
                            component = "claude",
                            label = label_stdout.as_str(),
                            error = %e,
                            "Failed reading Claude stdout stream"
                        );
                        break;
                    }
                };

                if !line.trim().is_empty() {
                    tracing::info!(
                        component = "claude",
                        label = label_stdout.as_str(),
                        "{}",
                        line
                    );
                }

                // Detect blocking question in real-time and signal parent.
                if question_tx.is_some() && line.trim().starts_with(QUESTION_PROTOCOL_PREFIX) {
                    tracing::info!(
                        component = "claude",
                        label = label_stdout.as_str(),
                        "Blocking question detected in stream, signalling early termination"
                    );
                    if let Some(tx) = question_tx.take() {
                        let _ = tx.send(());
                    }
                }

                if let Some(file) = writer.as_mut() {
                    if !write_failed
                        && (file.write_all(line.as_bytes()).await.is_err()
                            || file.write_all(b"\n").await.is_err())
                    {
                        write_failed = true;
                        tracing::warn!(
                            component = "claude",
                            label = label_stdout.as_str(),
                            "Failed writing Claude stdout to execution log file"
                        );
                    }
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
            let mut writer = match stderr_log_path {
                Some(path) => match tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await
                {
                    Ok(file) => Some(file),
                    Err(e) => {
                        tracing::warn!(
                            component = "claude",
                            label = label_stderr.as_str(),
                            path = %path.display(),
                            error = %e,
                            "Failed to open stderr execution log file"
                        );
                        None
                    }
                },
                None => None,
            };
            let mut write_failed = false;

            loop {
                let next_line = lines.next_line().await;
                let line = match next_line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(
                            component = "claude",
                            label = label_stderr.as_str(),
                            error = %e,
                            "Failed reading Claude stderr stream"
                        );
                        break;
                    }
                };

                if !line.trim().is_empty() {
                    tracing::error!(
                        component = "claude",
                        label = label_stderr.as_str(),
                        "{}",
                        line
                    );
                }

                if let Some(file) = writer.as_mut() {
                    if !write_failed
                        && (file.write_all(line.as_bytes()).await.is_err()
                            || file.write_all(b"\n").await.is_err())
                    {
                        write_failed = true;
                        tracing::warn!(
                            component = "claude",
                            label = label_stderr.as_str(),
                            "Failed writing Claude stderr to execution log file"
                        );
                    }
                }

                output.push_str(&line);
                output.push('\n');
            }
            output
        });

        let timeout_duration = std::time::Duration::from_secs(self.config.timeout_secs);

        enum WaitOutcome {
            Exited(std::result::Result<std::process::ExitStatus, std::io::Error>),
            QuestionDetected,
            TimedOut,
        }

        let outcome = tokio::select! {
            result = child.wait() => WaitOutcome::Exited(result),
            _ = question_rx => WaitOutcome::QuestionDetected,
            _ = tokio::time::sleep(timeout_duration) => WaitOutcome::TimedOut,
        };

        let (status, timed_out) = match outcome {
            WaitOutcome::Exited(Ok(status)) => (status, false),
            WaitOutcome::Exited(Err(e)) => {
                return Err(Error::runner(format!("Failed to wait for claude: {}", e)));
            }
            WaitOutcome::QuestionDetected => {
                tracing::info!(
                    component = "claude",
                    label = label,
                    "Killing subprocess early — blocking question detected in stream"
                );
                if let Err(e) = child.kill().await {
                    tracing::error!(component = "claude", error = %e, "Failed to kill process after question detected");
                }
                // Wait for the process to actually finish so pipes are drained.
                let exit_status = child.wait().await;
                let status = match exit_status {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(component = "claude", error = %e, "Failed to reap killed process");
                        // Fabricate a non-success status; the question is already captured.
                        std::process::ExitStatus::default()
                    }
                };
                (status, false)
            }
            WaitOutcome::TimedOut => {
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
                    blocking_question: None,
                    used_qa_ids: Vec::new(),
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

        let failure_msg = if status.success() {
            None
        } else {
            Some(Self::compose_failure_message(
                exit_code,
                &stdout_output,
                &stderr_output,
            ))
        };
        let is_rate_limited = failure_msg
            .as_ref()
            .map(|msg| Self::is_rate_limit_error(msg))
            .unwrap_or(false)
            || Self::is_rate_limit_error(&stdout_output)
            || Self::is_rate_limit_error(&stderr_output);

        // Complete and record the execution
        execution.complete(status.code(), timed_out);
        execution.stdout_preview =
            Some(Self::truncate(&stdout_output, EXECUTION_LOG_PREVIEW_LIMIT));
        execution.stderr_preview = if stderr_output.is_empty() {
            failure_msg
                .as_ref()
                .map(|msg| Self::truncate(msg, EXECUTION_LOG_PREVIEW_LIMIT))
        } else {
            Some(Self::truncate(&stderr_output, EXECUTION_LOG_PREVIEW_LIMIT))
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
            let error_msg = failure_msg
                .clone()
                .unwrap_or_else(|| format!("Process exited with code {}", exit_code));
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

            if is_rate_limited {
                let rate_limit_activity = ActivityLogEntry::new(
                    "rate_limit_hit",
                    format!("Claude rate limit hit for {}", label),
                )
                .with_source("claude".to_string())
                .with_metadata(json!({
                    "label": label,
                    "exit_code": exit_code,
                    "error": Self::truncate(&error_msg, 500),
                }));
                self.tracker.record_activity(&rate_limit_activity).ok();
            }
        }

        let blocking_question = Self::extract_blocking_question(&stdout_output)
            .or_else(|| Self::extract_blocking_question(&stderr_output));

        Ok(ClaudeResult {
            success: status.success(),
            output: stdout_output,
            pr_url,
            error: failure_msg,
            blocking_question,
            used_qa_ids: Vec::new(),
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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

    #[test]
    fn test_is_rate_limit_error_detection() {
        assert!(ClaudeRunner::is_rate_limit_error(
            "Claude API returned 429 Too Many Requests"
        ));
        assert!(ClaudeRunner::is_rate_limit_error("rate limit exceeded"));
        assert!(!ClaudeRunner::is_rate_limit_error("cargo test failed"));
    }

    #[test]
    fn test_is_hard_error_detection() {
        assert!(ClaudeRunner::is_hard_error(
            "Failed to spawn claude: No such file or directory"
        ));
        assert!(ClaudeRunner::is_hard_error(
            "Process timed out after 3600 seconds"
        ));
        assert!(ClaudeRunner::is_hard_error("429 too many requests"));
        assert!(!ClaudeRunner::is_hard_error("tests failed"));
    }

    #[test]
    fn test_extract_blocking_question_valid_payload() {
        let output = r#"some logs
CLAUDEAR_QUESTION: {"question":"Which branch should I target?","context":"Branch policy is unclear","options":["main","develop"],"why":"Need destination branch"}
done"#;

        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "Which branch should I target?");
        assert_eq!(parsed.context.as_deref(), Some("Branch policy is unclear"));
        assert_eq!(parsed.options, vec!["main", "develop"]);
        assert_eq!(parsed.why.as_deref(), Some("Need destination branch"));
    }

    #[test]
    fn test_extract_blocking_question_ignores_malformed_payload() {
        let output = "CLAUDEAR_QUESTION: {not valid json}";
        assert!(ClaudeRunner::extract_blocking_question(output).is_none());
    }

    #[test]
    fn test_extract_blocking_question_prefix_with_leading_whitespace() {
        let output =
            "  \t  CLAUDEAR_QUESTION: {\"question\":\"spaces before prefix\",\"options\":[]}";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "spaces before prefix");
    }

    #[test]
    fn test_extract_blocking_question_prefix_no_space_before_json() {
        let output = "CLAUDEAR_QUESTION:{\"question\":\"no space before json\",\"options\":[]}";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "no space before json");
    }

    #[test]
    fn test_extract_blocking_question_multiple_lines_returns_first_valid() {
        let output = r#"CLAUDEAR_QUESTION: {"question":"first question","options":[]}
CLAUDEAR_QUESTION: {"question":"second question","options":[]}"#;
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "first question");
    }

    #[test]
    fn test_extract_blocking_question_empty_string() {
        assert!(ClaudeRunner::extract_blocking_question("").is_none());
    }

    #[test]
    fn test_extract_blocking_question_only_whitespace() {
        assert!(ClaudeRunner::extract_blocking_question("   \n\t\n  ").is_none());
    }

    #[test]
    fn test_extract_blocking_question_only_required_field() {
        let output = "CLAUDEAR_QUESTION: {\"question\":\"minimal question\"}";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "minimal question");
        assert!(parsed.context.is_none());
        assert!(parsed.options.is_empty());
        assert!(parsed.why.is_none());
    }

    #[test]
    fn test_extract_blocking_question_empty_options_array() {
        let output = "CLAUDEAR_QUESTION: {\"question\":\"empty opts\",\"options\":[]}";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert!(parsed.options.is_empty());
    }

    #[test]
    fn test_extract_blocking_question_unicode_in_question() {
        let output = "CLAUDEAR_QUESTION: {\"question\":\"日本語テスト 🎉 résumé\",\"options\":[]}";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "日本語テスト 🎉 résumé");
    }

    #[test]
    fn test_extract_blocking_question_deep_in_multiline_output() {
        let mut lines: Vec<String> = (0..150).map(|i| format!("log line {}", i)).collect();
        lines.push(
            "CLAUDEAR_QUESTION: {\"question\":\"deep question\",\"options\":[\"a\"]}".to_string(),
        );
        lines.push("more output".to_string());
        let output = lines.join("\n");
        let parsed = ClaudeRunner::extract_blocking_question(&output).unwrap();
        assert_eq!(parsed.question, "deep question");
        assert_eq!(parsed.options, vec!["a"]);
    }

    #[test]
    fn test_extract_blocking_question_prefix_in_middle_of_line_does_not_match() {
        let output =
            "some text CLAUDEAR_QUESTION: {\"question\":\"should not match\",\"options\":[]}";
        // strip_prefix on the trimmed line requires the prefix to be at the start
        assert!(ClaudeRunner::extract_blocking_question(output).is_none());
    }

    #[test]
    fn test_extract_blocking_question_empty_json_object() {
        let output = "CLAUDEAR_QUESTION: {}";
        // `question` field is required — empty object should fail deserialization
        assert!(ClaudeRunner::extract_blocking_question(output).is_none());
    }

    #[test]
    fn test_extract_blocking_question_prefix_followed_by_only_whitespace() {
        let output = "CLAUDEAR_QUESTION:    ";
        assert!(ClaudeRunner::extract_blocking_question(output).is_none());
    }

    #[test]
    fn test_extract_blocking_question_very_large_json_payload() {
        let long_context = "x".repeat(5000);
        let json = format!(
            "{{\"question\":\"big question\",\"context\":\"{}\",\"options\":[]}}",
            long_context
        );
        let output = format!("CLAUDEAR_QUESTION: {}", json);
        let parsed = ClaudeRunner::extract_blocking_question(&output).unwrap();
        assert_eq!(parsed.question, "big question");
        assert_eq!(parsed.context.unwrap().len(), 5000);
    }

    #[test]
    fn test_extract_blocking_question_special_characters_in_fields() {
        let output = r#"CLAUDEAR_QUESTION: {"question":"What about <html> & \"quotes\"?","context":"path/to/file.rs","options":["a & b","c < d"],"why":"need to know \"this\""}"#;
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "What about <html> & \"quotes\"?");
        assert_eq!(parsed.context.as_deref(), Some("path/to/file.rs"));
        assert_eq!(parsed.options, vec!["a & b", "c < d"]);
        assert_eq!(parsed.why.as_deref(), Some("need to know \"this\""));
    }

    #[test]
    fn test_extract_blocking_question_newlines_inside_json_string_values() {
        // JSON allows \n inside string values (escaped)
        let output = r#"CLAUDEAR_QUESTION: {"question":"line1\nline2","options":["opt\none"]}"#;
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "line1\nline2");
        assert_eq!(parsed.options, vec!["opt\none"]);
    }

    #[test]
    fn test_append_question_protocol_already_contains_prefix() {
        let prompt = format!("Do something.\n{} test", QUESTION_PROTOCOL_PREFIX);
        let result = ClaudeRunner::append_question_protocol(&prompt);
        assert_eq!(result, prompt);
    }

    #[test]
    fn test_append_question_protocol_does_not_contain_prefix() {
        let prompt = "Fix the bug in main.rs";
        let result = ClaudeRunner::append_question_protocol(prompt);
        assert!(result.starts_with(prompt));
        assert!(result.contains(QUESTION_PROTOCOL_PREFIX));
        assert!(result.contains("CLAUDEAR_QUESTION:"));
    }

    #[test]
    fn test_append_question_protocol_empty_prompt() {
        let result = ClaudeRunner::append_question_protocol("");
        assert!(result.contains(QUESTION_PROTOCOL_PREFIX));
    }

    #[test]
    fn test_append_question_protocol_prefix_embedded_in_word() {
        // "CLAUDEAR_QUESTION:foo" still contains the prefix substring, so it should NOT re-append
        let prompt = "CLAUDEAR_QUESTION:foo";
        let result = ClaudeRunner::append_question_protocol(prompt);
        assert_eq!(result, prompt);
    }

    #[test]
    fn test_truncate_shorter_than_max() {
        let s = "hello";
        assert_eq!(ClaudeRunner::truncate(s, 100), "hello");
    }

    #[test]
    fn test_truncate_exactly_at_max() {
        let s = "hello";
        assert_eq!(ClaudeRunner::truncate(s, 5), "hello");
    }

    #[test]
    fn test_truncate_one_byte_over_max() {
        let s = "abcdef";
        // max_len=5 means we keep up to 2 chars (5-3=2) then "..."
        let result = ClaudeRunner::truncate(s, 5);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 5 + 3); // truncated part + "..."
        assert_eq!(result, "ab...");
    }

    #[test]
    fn test_truncate_multibyte_unicode_at_boundary() {
        // 'é' is 2 bytes in UTF-8; the function must cut at a valid char boundary
        let s = "aéb"; // bytes: a(1) + é(2) + b(1) = 4 bytes
                       // max_len=3 -> end = 0, safe_end finds last char index <= 0 which is 'a' at index 0
        let result = ClaudeRunner::truncate(s, 3);
        assert!(result.ends_with("..."));
        // Should not panic and should be valid UTF-8
        assert_eq!(result, "..."); // end = 0, safe_end = 0
    }

    #[test]
    fn test_truncate_empty_string_zero_max() {
        assert_eq!(ClaudeRunner::truncate("", 0), "");
    }

    #[test]
    fn test_truncate_max_len_of_3() {
        // max_len=3, end = 0, so safe_end = 0, result is "..."
        let result = ClaudeRunner::truncate("abcdef", 3);
        assert_eq!(result, "...");
    }

    #[test]
    fn test_truncate_max_len_of_2() {
        // max_len=2, saturating_sub(3) = 0, safe_end = 0
        let result = ClaudeRunner::truncate("abcdef", 2);
        assert_eq!(result, "...");
    }

    #[test]
    fn test_truncate_very_long_string() {
        let s = "a".repeat(10_000);
        let result = ClaudeRunner::truncate(&s, 100);
        assert!(result.ends_with("..."));
        // 97 'a' chars + "..." = 100 total
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn test_truncate_max_len_of_4() {
        // max_len=4, end=1, safe_end=0 (char at index 0 is 'a', which is <= 1), actually 'a' is at 0 and 'b' at 1
        let result = ClaudeRunner::truncate("abcdef", 4);
        // end = 4-3 = 1, last char index <= 1 is 'b' at index 1
        assert_eq!(result, "a...");
    }

    #[test]
    fn test_sanitize_label_normal_alphanumeric() {
        assert_eq!(ClaudeRunner::sanitize_label("hello123"), "hello123");
    }

    #[test]
    fn test_sanitize_label_special_characters() {
        assert_eq!(
            ClaudeRunner::sanitize_label("hello world.foo/bar"),
            "hello_world_foo_bar"
        );
    }

    #[test]
    fn test_sanitize_label_empty() {
        assert_eq!(ClaudeRunner::sanitize_label(""), "custom");
    }

    #[test]
    fn test_sanitize_label_longer_than_64_chars() {
        let label = "a".repeat(100);
        let result = ClaudeRunner::sanitize_label(&label);
        assert_eq!(result.len(), 64);
        assert_eq!(result, "a".repeat(64));
    }

    #[test]
    fn test_sanitize_label_unicode_characters() {
        assert_eq!(ClaudeRunner::sanitize_label("café☕日本"), "caf____");
    }

    #[test]
    fn test_sanitize_label_hyphens_and_underscores_preserved() {
        assert_eq!(
            ClaudeRunner::sanitize_label("my-label_name"),
            "my-label_name"
        );
    }

    #[test]
    fn test_sanitize_label_exactly_64_chars() {
        let label = "b".repeat(64);
        let result = ClaudeRunner::sanitize_label(&label);
        assert_eq!(result.len(), 64);
        assert_eq!(result, label);
    }

    #[test]
    fn test_sanitize_label_all_special_chars_non_empty() {
        // All chars replaced with '_', so result is non-empty
        assert_eq!(ClaudeRunner::sanitize_label("@#$"), "___");
    }

    #[test]
    fn test_compose_failure_message_both_empty() {
        let msg = ClaudeRunner::compose_failure_message(1, "", "");
        assert_eq!(msg, "Process exited with code 1");
    }

    #[test]
    fn test_compose_failure_message_only_stderr() {
        let msg = ClaudeRunner::compose_failure_message(1, "", "error occurred");
        assert_eq!(msg, "error occurred");
    }

    #[test]
    fn test_compose_failure_message_only_stdout() {
        let msg = ClaudeRunner::compose_failure_message(1, "some output", "");
        assert_eq!(msg, "Process exited with code 1. Output: some output");
    }

    #[test]
    fn test_compose_failure_message_both_present_stderr_takes_priority() {
        let msg = ClaudeRunner::compose_failure_message(1, "stdout text", "stderr text");
        // When both present, stderr is returned (it is non-empty)
        assert_eq!(msg, "stderr text");
    }

    #[test]
    fn test_compose_failure_message_rate_limit_in_stderr() {
        let msg = ClaudeRunner::compose_failure_message(1, "", "rate limit exceeded");
        assert!(msg.starts_with("Claude rate limit hit:"));
        assert!(msg.contains("rate limit"));
    }

    #[test]
    fn test_compose_failure_message_rate_limit_in_stdout() {
        let msg = ClaudeRunner::compose_failure_message(1, "429 too many requests", "");
        assert!(msg.starts_with("Claude rate limit hit:"));
    }

    #[test]
    fn test_compose_failure_message_rate_limit_in_combined() {
        let msg = ClaudeRunner::compose_failure_message(1, "some output", "rate limit hit");
        assert!(msg.starts_with("Claude rate limit hit:"));
    }

    #[test]
    fn test_compose_failure_message_very_long_stderr_truncated() {
        let long_stderr = "e".repeat(5000);
        let msg = ClaudeRunner::compose_failure_message(1, "", &long_stderr);
        assert!(msg.len() <= EXECUTION_LOG_PREVIEW_LIMIT + 3); // +3 for "..."
        assert!(msg.ends_with("..."));
    }

    #[test]
    fn test_compose_failure_message_whitespace_only_inputs() {
        // Whitespace-only is trimmed to empty
        let msg = ClaudeRunner::compose_failure_message(42, "   ", "  \n\t  ");
        assert_eq!(msg, "Process exited with code 42");
    }

    #[test]
    fn test_is_rate_limit_error_rate_limit() {
        assert!(ClaudeRunner::is_rate_limit_error("rate limit exceeded"));
    }

    #[test]
    fn test_is_rate_limit_error_ratelimit() {
        assert!(ClaudeRunner::is_rate_limit_error("ratelimit error"));
    }

    #[test]
    fn test_is_rate_limit_error_too_many_requests() {
        assert!(ClaudeRunner::is_rate_limit_error("too many requests"));
    }

    #[test]
    fn test_is_rate_limit_error_429() {
        assert!(ClaudeRunner::is_rate_limit_error("HTTP 429 returned"));
    }

    #[test]
    fn test_is_rate_limit_error_quota_exceeded() {
        assert!(ClaudeRunner::is_rate_limit_error("quota exceeded for api"));
    }

    #[test]
    fn test_is_rate_limit_error_resource_exhausted() {
        assert!(ClaudeRunner::is_rate_limit_error("resource exhausted"));
    }

    #[test]
    fn test_is_rate_limit_error_retry_after() {
        assert!(ClaudeRunner::is_rate_limit_error("retry-after: 30 seconds"));
    }

    #[test]
    fn test_is_rate_limit_error_try_again_later() {
        assert!(ClaudeRunner::is_rate_limit_error("please try again later"));
    }

    #[test]
    fn test_is_rate_limit_error_case_insensitivity() {
        assert!(ClaudeRunner::is_rate_limit_error("Rate Limit Exceeded"));
        assert!(ClaudeRunner::is_rate_limit_error("RATE LIMIT"));
        assert!(ClaudeRunner::is_rate_limit_error("RateLimit"));
    }

    #[test]
    fn test_is_rate_limit_error_empty_string() {
        assert!(!ClaudeRunner::is_rate_limit_error(""));
    }

    #[test]
    fn test_is_rate_limit_error_substring_match() {
        assert!(ClaudeRunner::is_rate_limit_error(
            "we hit a rate limit here"
        ));
    }

    #[test]
    fn test_is_rate_limit_error_unrelated_message() {
        assert!(!ClaudeRunner::is_rate_limit_error(
            "compilation error in main.rs"
        ));
    }

    #[test]
    fn test_is_hard_error_failed_to_spawn_claude() {
        assert!(ClaudeRunner::is_hard_error(
            "Failed to spawn claude: not found"
        ));
    }

    #[test]
    fn test_is_hard_error_failed_to_wait_for_claude() {
        assert!(ClaudeRunner::is_hard_error("failed to wait for claude"));
    }

    #[test]
    fn test_is_hard_error_failed_to_capture_stdout() {
        assert!(ClaudeRunner::is_hard_error("failed to capture stdout"));
    }

    #[test]
    fn test_is_hard_error_failed_to_capture_stderr() {
        assert!(ClaudeRunner::is_hard_error("failed to capture stderr"));
    }

    #[test]
    fn test_is_hard_error_process_timed_out() {
        assert!(ClaudeRunner::is_hard_error("process timed out"));
    }

    #[test]
    fn test_is_hard_error_timed_out_after() {
        assert!(ClaudeRunner::is_hard_error("timed out after 3600 seconds"));
    }

    #[test]
    fn test_is_hard_error_connection_reset() {
        assert!(ClaudeRunner::is_hard_error("connection reset by peer"));
    }

    #[test]
    fn test_is_hard_error_service_unavailable() {
        assert!(ClaudeRunner::is_hard_error("503 service unavailable"));
    }

    #[test]
    fn test_is_hard_error_internal_server_error() {
        assert!(ClaudeRunner::is_hard_error("500 internal server error"));
    }

    #[test]
    fn test_is_hard_error_network_error() {
        assert!(ClaudeRunner::is_hard_error("network error: timeout"));
    }

    #[test]
    fn test_is_hard_error_broken_pipe() {
        assert!(ClaudeRunner::is_hard_error("broken pipe"));
    }

    #[test]
    fn test_is_hard_error_rate_limit_is_also_hard() {
        assert!(ClaudeRunner::is_hard_error("rate limit exceeded"));
        assert!(ClaudeRunner::is_hard_error("429 too many requests"));
    }

    #[test]
    fn test_is_hard_error_case_insensitivity() {
        assert!(ClaudeRunner::is_hard_error("FAILED TO SPAWN CLAUDE"));
        assert!(ClaudeRunner::is_hard_error("Connection Reset"));
        assert!(ClaudeRunner::is_hard_error("Broken Pipe"));
    }

    #[test]
    fn test_is_hard_error_empty_string() {
        assert!(!ClaudeRunner::is_hard_error(""));
    }

    #[test]
    fn test_is_hard_error_normal_error_is_not_hard() {
        assert!(!ClaudeRunner::is_hard_error("tests failed"));
        assert!(!ClaudeRunner::is_hard_error("compilation error"));
        assert!(!ClaudeRunner::is_hard_error("undefined variable"));
    }

    #[test]
    fn test_hash_prompt_deterministic() {
        let h1 = ClaudeRunner::hash_prompt("same prompt");
        let h2 = ClaudeRunner::hash_prompt("same prompt");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_prompt_different_prompts_different_hashes() {
        let h1 = ClaudeRunner::hash_prompt("prompt one");
        let h2 = ClaudeRunner::hash_prompt("prompt two");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_prompt_empty_produces_hash() {
        let h = ClaudeRunner::hash_prompt("");
        assert!(!h.is_empty());
        assert_eq!(h.len(), 16);
    }

    #[test]
    fn test_hash_prompt_length_is_16() {
        let h = ClaudeRunner::hash_prompt("any prompt here");
        assert_eq!(h.len(), 16);
    }

    #[test]
    fn test_hash_prompt_only_hex_chars() {
        let h = ClaudeRunner::hash_prompt("test");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_prompt_unicode_works() {
        let h = ClaudeRunner::hash_prompt("こんにちは 🌍");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_claude_execution_new_defaults() {
        let exec = ClaudeExecution::new();
        assert_eq!(exec.id, 0);
        assert!(exec.attempt_id.is_none());
        assert!(exec.completed_at.is_none());
        assert!(exec.duration_secs.is_none());
        assert!(exec.exit_code.is_none());
        assert!(!exec.timed_out);
        assert!(exec.stdout_preview.is_none());
        assert!(exec.stderr_preview.is_none());
        assert!(exec.stdout_log_path.is_none());
        assert!(exec.stderr_log_path.is_none());
        assert!(exec.prompt_used.is_none());
        assert!(exec.prompt_hash.is_none());
        assert!(exec.model_version.is_none());
        assert!(exec.working_directory.is_none());
        assert!(exec.git_branch.is_none());
        assert!(exec.git_commit_before.is_none());
        assert!(exec.git_commit_after.is_none());
        assert!(exec.files_changed.is_none());
        assert!(exec.lines_added.is_none());
        assert!(exec.lines_removed.is_none());
    }

    #[test]
    fn test_claude_execution_with_attempt_id() {
        let exec = ClaudeExecution::new().with_attempt_id(42);
        assert_eq!(exec.attempt_id, Some(42));
    }

    #[test]
    fn test_claude_execution_complete_sets_fields() {
        let mut exec = ClaudeExecution::new();
        assert!(exec.completed_at.is_none());
        assert!(exec.exit_code.is_none());
        assert!(!exec.timed_out);

        exec.complete(Some(0), false);
        assert!(exec.completed_at.is_some());
        assert!(exec.duration_secs.is_some());
        assert_eq!(exec.exit_code, Some(0));
        assert!(!exec.timed_out);
    }

    #[test]
    fn test_claude_execution_complete_with_timeout() {
        let mut exec = ClaudeExecution::new();
        exec.complete(None, true);
        assert!(exec.timed_out);
        assert!(exec.exit_code.is_none());
        assert!(exec.completed_at.is_some());
        assert!(exec.duration_secs.is_some());
    }

    #[test]
    fn test_claude_execution_complete_with_nonzero_exit() {
        let mut exec = ClaudeExecution::new();
        exec.complete(Some(1), false);
        assert_eq!(exec.exit_code, Some(1));
        assert!(!exec.timed_out);
    }

    #[test]
    fn test_claude_execution_duration_is_non_negative() {
        let mut exec = ClaudeExecution::new();
        exec.complete(Some(0), false);
        assert!(exec.duration_secs.unwrap() >= 0.0);
    }

    #[test]
    fn test_claude_execution_default_matches_new() {
        let from_new = ClaudeExecution::new();
        let from_default = ClaudeExecution::default();
        assert_eq!(from_new.id, from_default.id);
        assert_eq!(from_new.attempt_id, from_default.attempt_id);
        assert_eq!(from_new.timed_out, from_default.timed_out);
        assert_eq!(from_new.exit_code, from_default.exit_code);
    }

    #[test]
    fn test_claude_runner_config_default_values() {
        let config = ClaudeRunnerConfig::default();
        assert_eq!(config.timeout_secs, 21600);
        assert!(config.model.is_none());
        assert!(config.instructions.is_none());
        assert!(config.permissions.is_empty());
        assert!(config.skip_permissions);
    }

    #[test]
    fn test_claude_runner_config_custom_timeout() {
        let config = ClaudeRunnerConfig {
            timeout_secs: 60,
            ..Default::default()
        };
        assert_eq!(config.timeout_secs, 60);
        // Other fields remain at default
        assert!(config.model.is_none());
        assert!(config.skip_permissions);
    }

    #[test]
    fn test_claude_runner_config_with_model() {
        let config = ClaudeRunnerConfig {
            model: Some("opus".to_string()),
            ..Default::default()
        };
        assert_eq!(config.model.as_deref(), Some("opus"));
    }

    #[test]
    fn test_claude_runner_config_with_permissions() {
        let config = ClaudeRunnerConfig {
            permissions: vec!["Bash".to_string(), "Read".to_string()],
            skip_permissions: false,
            ..Default::default()
        };
        assert_eq!(config.permissions.len(), 2);
        assert!(!config.skip_permissions);
    }

    #[test]
    fn test_claude_runner_config_with_instructions() {
        let config = ClaudeRunnerConfig {
            instructions: Some("Always write tests".to_string()),
            ..Default::default()
        };
        assert_eq!(config.instructions.as_deref(), Some("Always write tests"));
    }
}
