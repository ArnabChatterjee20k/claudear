//! Claude CLI runner for executing fixes.

use crate::error::{Error, Result};
use crate::storage::FixAttemptTracker;
use crate::templates::{TemplateContext, TemplateLoader, TemplateRenderer};
use crate::types::{ActivityLogEntry, BlockingQuestion, ClaudeExecution, ClaudeResult, Issue};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

const DEFAULT_LOG_DIR: &str = "./logs";
const CLAUDE_LOG_SUBDIR: &str = "claude";
const EXECUTION_LOG_PREVIEW_LIMIT: usize = 2000;

/// Resolve the root directory for execution logs.
/// Used by both the runner and the API to validate log file paths.
pub fn resolve_log_root() -> PathBuf {
    std::env::var("CLAUDEAR_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_LOG_DIR))
}

/// JSON schema for Claude's structured output. Constrained decoding guarantees the
/// final response matches this schema, replacing both the CLAUDEAR_QUESTION protocol
/// and the PR_URL instruction.
const RESULT_SCHEMA: &str = r#"{
    "type": "object",
    "required": ["summary", "success"],
    "additionalProperties": false,
    "properties": {
        "summary": {
            "type": "string",
            "description": "Brief summary of what was done or why you stopped"
        },
        "success": {
            "type": "boolean",
            "description": "Whether the task was completed successfully"
        },
        "pr_url": {
            "type": ["string", "null"],
            "description": "URL of the created pull request, if one was created"
        },
        "blocking_question": {
            "type": ["object", "null"],
            "description": "If you need human input to proceed, provide the question here instead of attempting the task",
            "required": ["question"],
            "properties": {
                "question": { "type": "string" },
                "context": { "type": ["string", "null"] },
                "options": { "type": "array", "items": { "type": "string" } },
                "why": { "type": ["string", "null"] }
            },
            "additionalProperties": false
        }
    }
}"#;

/// Content block types within a CLI `assistant` event's `message.content[]`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum CliContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(other)]
    Other,
}

/// The `message` object carried by a CLI `assistant` event.
#[derive(Debug, Deserialize)]
struct CliMessage {
    #[serde(default)]
    content: Vec<CliContentBlock>,
}

/// Token usage reported by the Claude CLI result event.
#[derive(Debug, Deserialize)]
struct CliUsage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cache_read_input_tokens: Option<i64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<i64>,
}

/// Top-level NDJSON events emitted by `claude --output-format stream-json`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum StreamEvent {
    #[serde(rename = "system")]
    System {},
    #[serde(rename = "assistant")]
    Assistant {
        #[serde(default)]
        message: Option<CliMessage>,
    },
    #[serde(rename = "user")]
    User {},
    #[serde(rename = "result")]
    Result {
        #[serde(default)]
        structured_output: Option<serde_json::Value>,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        num_turns: Option<i64>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        duration_api_ms: Option<i64>,
        #[serde(default)]
        usage: Option<CliUsage>,
    },
    /// Forward-compat: ignore unknown event types.
    #[serde(other)]
    Unknown,
}

/// Deserialization target for the structured result produced by `--json-schema`.
#[derive(Debug, Deserialize)]
struct StructuredResult {
    #[serde(default)]
    summary: String,
    success: bool,
    #[serde(default)]
    pr_url: Option<String>,
    #[serde(default)]
    blocking_question: Option<BlockingQuestion>,
}

#[derive(Debug, Clone)]
struct ExecutionLogFiles {
    stdout: PathBuf,
    stderr: PathBuf,
    events: PathBuf,
}

/// Aggregated result from parsing the stdout stream.
#[derive(Debug, Default)]
struct StdoutParseResult {
    text_output: String,
    structured_result: Option<serde_json::Value>,
    cost_usd: Option<f64>,
    num_turns: Option<i64>,
    session_id: Option<String>,
    duration_api_ms: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
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
    /// Skip all permission prompts (default: false).
    pub skip_permissions: bool,
}

impl Default for ClaudeRunnerConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 21600, // 6 hours default
            model: None,
            instructions: None,
            permissions: Vec::new(),
            skip_permissions: false,
        }
    }
}

/// Runs Claude Code to fix issues.
pub struct ClaudeRunner {
    config: ClaudeRunnerConfig,
    template_renderer: TemplateRenderer,
    tracker: Arc<dyn FixAttemptTracker>,
    /// Cached base environment variables, captured once at construction time.
    /// Cloned per-invocation instead of re-reading the entire process environment.
    base_env: HashMap<String, String>,
}

impl ClaudeRunner {
    /// Create a new Claude runner.
    pub fn new(config: ClaudeRunnerConfig, tracker: Arc<dyn FixAttemptTracker>) -> Self {
        let template_renderer = TemplateRenderer::new();
        let base_env: HashMap<String, String> = std::env::vars().collect();
        Self {
            config,
            template_renderer,
            tracker,
            base_env,
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

        let mut env = self.base_env.clone();
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
6. Ensure all checks pass on the PR

The PR title should include the issue ID: {}
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

    /// Best-effort detection for Claude/API rate limit failures.
    pub fn is_rate_limit_error(message: &str) -> bool {
        let lower = message.to_lowercase();
        Self::is_rate_limit_error_lower(&lower)
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
        Self::is_rate_limit_error_lower(&lower)
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

        let now = chrono::Utc::now();
        let day = now.format("%Y-%m-%d").to_string();
        let timestamp = now.format("%Y%m%dT%H%M%S%.3fZ").to_string();
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
            events: dir.join(format!("{}.events.jsonl", stem)),
        })
    }

    fn compose_failure_message(exit_code: i32, stdout_output: &str, stderr_output: &str) -> String {
        use std::borrow::Cow;

        let stderr_trimmed = stderr_output.trim();
        let stdout_trimmed = stdout_output.trim();
        let combined: Cow<'_, str> = if stderr_trimmed.is_empty() {
            Cow::Borrowed(stdout_trimmed)
        } else if stdout_trimmed.is_empty() {
            Cow::Borrowed(stderr_trimmed)
        } else {
            Cow::Owned(format!("{}\n{}", stderr_trimmed, stdout_trimmed))
        };

        if Self::is_rate_limit_error(&combined) {
            let msg = if combined.is_empty() {
                "Too many requests"
            } else {
                &combined
            };
            return format!(
                "Claude rate limit hit: {}",
                Self::truncate(msg, EXECUTION_LOG_PREVIEW_LIMIT)
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

    /// Legacy fallback: extract a blocking question from raw text output.
    /// Used when structured_result is None (killed processes, old CLI versions, etc.).
    fn extract_blocking_question(output: &str) -> Option<BlockingQuestion> {
        // Look for CLAUDEAR_QUESTION: prefix (legacy protocol)
        let legacy_prefix = "CLAUDEAR_QUESTION:";
        output.lines().find_map(|line| {
            let trimmed = line.trim();
            let payload = trimmed.strip_prefix(legacy_prefix)?.trim();
            if payload.is_empty() {
                return None;
            }
            serde_json::from_str::<BlockingQuestion>(payload).ok()
        })
    }

    async fn append_execution_event(
        writer: &Option<Arc<Mutex<tokio::fs::File>>>,
        label: &str,
        event: &str,
        data: serde_json::Value,
    ) {
        let Some(writer) = writer else {
            return;
        };

        let payload = json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "label": label,
            "event": event,
            "data": data,
        });

        let serialized = match serde_json::to_string(&payload) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    component = "claude",
                    label = label,
                    error = %e,
                    "Failed to serialize execution event payload"
                );
                return;
            }
        };

        let mut guard = writer.lock().await;
        if let Err(e) = guard.write_all(serialized.as_bytes()).await {
            tracing::warn!(
                component = "claude",
                label = label,
                event = event,
                error = %e,
                "Failed to write execution event payload"
            );
            return;
        }
        if let Err(e) = guard.write_all(b"\n").await {
            tracing::warn!(
                component = "claude",
                label = label,
                event = event,
                error = %e,
                "Failed to terminate execution event payload line"
            );
        }
    }

    /// Build the per-invocation environment and derive a human-readable label
    /// from the optional issue. Shared by `execute` and `execute_with_attempt`
    /// to avoid duplicating the env-var / label logic.
    fn prepare_env_and_label<'a>(
        &self,
        issue: Option<&'a Issue>,
    ) -> (HashMap<String, String>, &'a str) {
        let mut env = self.base_env.clone();

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
        (env, label)
    }

    async fn execute(
        &self,
        prompt: &str,
        issue: Option<&Issue>,
        project_dir: &Path,
    ) -> Result<ClaudeResult> {
        let (env, label) = self.prepare_env_and_label(issue);
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
        let (env, label) = self.prepare_env_and_label(issue);
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

        let mut args = vec![
            "--verbose".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--json-schema".to_string(),
            RESULT_SCHEMA.to_string(),
        ];
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
        args.push("--print".to_string());
        args.push(prompt.to_string());

        let log_files = Self::create_execution_log_files(label);
        if let Some(ref files) = log_files {
            execution.stdout_log_path = Some(files.stdout.display().to_string());
            execution.stderr_log_path = Some(files.stderr.display().to_string());
            execution.event_log_path = Some(files.events.display().to_string());
            tracing::info!(
                component = "claude",
                label = label,
                stdout_log = %files.stdout.display(),
                stderr_log = %files.stderr.display(),
                events_log = %files.events.display(),
                "Capturing Claude output to execution log files"
            );
        }

        let event_writer: Option<Arc<Mutex<tokio::fs::File>>> =
            match log_files.as_ref().map(|files| files.events.clone()) {
                Some(path) => match tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await
                {
                    Ok(file) => Some(Arc::new(Mutex::new(file))),
                    Err(e) => {
                        tracing::warn!(
                            component = "claude",
                            label = label,
                            path = %path.display(),
                            error = %e,
                            "Failed to open execution events log file"
                        );
                        None
                    }
                },
                None => None,
            };

        let cli_args_without_prompt: Vec<String> = args
            .iter()
            .take(args.len().saturating_sub(1))
            .cloned()
            .collect();
        Self::append_execution_event(
            &event_writer,
            label,
            "execution_initialized",
            json!({
                "attempt_id": attempt_id,
                "timeout_secs": self.config.timeout_secs,
                "working_dir": project_dir.display().to_string(),
                "model": self.config.model.clone(),
                "skip_permissions": self.config.skip_permissions,
                "permissions": self.config.permissions.clone(),
                "cli_args_without_prompt": cli_args_without_prompt,
                "prompt_hash": execution.prompt_hash.clone(),
            }),
        )
        .await;

        let mut child = match Command::new("claude")
            .args(&args)
            .current_dir(project_dir)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                Self::append_execution_event(
                    &event_writer,
                    label,
                    "spawn_failed",
                    json!({
                        "error": e.to_string(),
                    }),
                )
                .await;
                return Err(Error::runner(format!("Failed to spawn claude: {}", e)));
            }
        };

        Self::append_execution_event(
            &event_writer,
            label,
            "subprocess_spawned",
            json!({
                "pid": child.id(),
            }),
        )
        .await;

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                Self::append_execution_event(
                    &event_writer,
                    label,
                    "stdout_capture_failed",
                    json!({
                        "error": "Failed to capture stdout",
                    }),
                )
                .await;
                return Err(Error::runner("Failed to capture stdout"));
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                Self::append_execution_event(
                    &event_writer,
                    label,
                    "stderr_capture_failed",
                    json!({
                        "error": "Failed to capture stderr",
                    }),
                )
                .await;
                return Err(Error::runner("Failed to capture stderr"));
            }
        };

        let label_stdout = label.to_string();
        let label_stderr = label.to_string();
        let stdout_log_path = log_files.as_ref().map(|f| f.stdout.clone());
        let stderr_log_path = log_files.as_ref().map(|f| f.stderr.clone());
        let stdout_event_writer = event_writer.clone();
        let stderr_event_writer = event_writer.clone();

        let stdout_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut text_output = String::new();
            let mut line_number: u64 = 0;
            let mut structured_result: Option<serde_json::Value> = None;
            let mut result_cost_usd: Option<f64> = None;
            let mut result_num_turns: Option<i64> = None;
            let mut result_session_id: Option<String> = None;
            let mut result_duration_api_ms: Option<i64> = None;
            let mut result_input_tokens: Option<i64> = None;
            let mut result_output_tokens: Option<i64> = None;
            let mut result_cache_read_tokens: Option<i64> = None;
            let mut result_cache_creation_tokens: Option<i64> = None;
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
                        ClaudeRunner::append_execution_event(
                            &stdout_event_writer,
                            label_stdout.as_str(),
                            "stdout_read_error",
                            json!({
                                "error": e.to_string(),
                            }),
                        )
                        .await;
                        tracing::warn!(
                            component = "claude",
                            label = label_stdout.as_str(),
                            error = %e,
                            "Failed reading Claude stdout stream"
                        );
                        break;
                    }
                };

                line_number = line_number.saturating_add(1);
                ClaudeRunner::append_execution_event(
                    &stdout_event_writer,
                    label_stdout.as_str(),
                    "stdout_line",
                    json!({
                        "line_number": line_number,
                        "line": line.as_str(),
                    }),
                )
                .await;

                // Parse NDJSON stream events; accumulate decoded text.
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    match serde_json::from_str::<StreamEvent>(trimmed) {
                        Ok(StreamEvent::Assistant { message }) => {
                            if let Some(msg) = message {
                                for block in &msg.content {
                                    match block {
                                        CliContentBlock::Text { ref text } => {
                                            text_output.push_str(text);
                                            if let Some(file) = writer.as_mut() {
                                                if !write_failed
                                                    && file
                                                        .write_all(text.as_bytes())
                                                        .await
                                                        .is_err()
                                                {
                                                    write_failed = true;
                                                    ClaudeRunner::append_execution_event(
                                                        &stdout_event_writer,
                                                        label_stdout.as_str(),
                                                        "stdout_log_write_failed",
                                                        json!({}),
                                                    )
                                                    .await;
                                                    tracing::warn!(
                                                        component = "claude",
                                                        label = label_stdout.as_str(),
                                                        "Failed writing decoded text to execution log file"
                                                    );
                                                }
                                            }
                                        }
                                        CliContentBlock::ToolUse { ref id, ref name } => {
                                            tracing::info!(
                                                component = "claude",
                                                label = label_stdout.as_str(),
                                                tool_use_id = id.as_str(),
                                                tool_name = name.as_str(),
                                                "Tool use started"
                                            );
                                        }
                                        CliContentBlock::Other => {}
                                    }
                                }
                            }
                        }
                        Ok(StreamEvent::Result {
                            structured_output,
                            total_cost_usd,
                            num_turns,
                            session_id,
                            duration_api_ms,
                            usage,
                        }) => {
                            if let Some(obj) = structured_output {
                                structured_result = Some(obj);
                            }
                            if let Some(cost) = total_cost_usd {
                                result_cost_usd = Some(cost);
                            }
                            result_num_turns = num_turns;
                            result_session_id = session_id;
                            result_duration_api_ms = duration_api_ms;
                            if let Some(u) = usage {
                                result_input_tokens = u.input_tokens;
                                result_output_tokens = u.output_tokens;
                                result_cache_read_tokens = u.cache_read_input_tokens;
                                result_cache_creation_tokens = u.cache_creation_input_tokens;
                            }
                        }
                        Ok(_) => { /* System, User, Unknown — ignored */ }
                        Err(_) => {
                            // Not a valid stream event — try parsing as the final
                            // JSON wrapper. The Claude CLI emits the result at the
                            // END of the stream, so always overwrite with the latest
                            // valid result (last-write-wins).
                            if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(trimmed)
                            {
                                if let Some(result_str) =
                                    wrapper.get("result").and_then(|v| v.as_str())
                                {
                                    if let Ok(parsed) =
                                        serde_json::from_str::<serde_json::Value>(result_str)
                                    {
                                        structured_result = Some(parsed);
                                    }
                                } else if let Some(obj) = wrapper.get("structured_output") {
                                    structured_result = Some(obj.clone());
                                }
                            }
                        }
                    }
                }
            }

            ClaudeRunner::append_execution_event(
                &stdout_event_writer,
                label_stdout.as_str(),
                "stdout_stream_closed",
                json!({
                    "line_count": line_number,
                    "has_structured_result": structured_result.is_some(),
                }),
            )
            .await;
            StdoutParseResult {
                text_output,
                structured_result,
                cost_usd: result_cost_usd,
                num_turns: result_num_turns,
                session_id: result_session_id,
                duration_api_ms: result_duration_api_ms,
                input_tokens: result_input_tokens,
                output_tokens: result_output_tokens,
                cache_read_input_tokens: result_cache_read_tokens,
                cache_creation_input_tokens: result_cache_creation_tokens,
            }
        });

        // Stream stderr
        let stderr_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut output = String::new();
            let mut line_number: u64 = 0;
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
                        ClaudeRunner::append_execution_event(
                            &stderr_event_writer,
                            label_stderr.as_str(),
                            "stderr_read_error",
                            json!({
                                "error": e.to_string(),
                            }),
                        )
                        .await;
                        tracing::warn!(
                            component = "claude",
                            label = label_stderr.as_str(),
                            error = %e,
                            "Failed reading Claude stderr stream"
                        );
                        break;
                    }
                };

                line_number = line_number.saturating_add(1);
                ClaudeRunner::append_execution_event(
                    &stderr_event_writer,
                    label_stderr.as_str(),
                    "stderr_line",
                    json!({
                        "line_number": line_number,
                        "line": line.as_str(),
                    }),
                )
                .await;

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
                        ClaudeRunner::append_execution_event(
                            &stderr_event_writer,
                            label_stderr.as_str(),
                            "stderr_log_write_failed",
                            json!({}),
                        )
                        .await;
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
            ClaudeRunner::append_execution_event(
                &stderr_event_writer,
                label_stderr.as_str(),
                "stderr_stream_closed",
                json!({
                    "line_count": line_number,
                }),
            )
            .await;
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
            WaitOutcome::Exited(Ok(status)) => {
                Self::append_execution_event(
                    &event_writer,
                    label,
                    "subprocess_exited",
                    json!({
                        "exit_code": status.code(),
                    }),
                )
                .await;
                (status, false)
            }
            WaitOutcome::Exited(Err(e)) => {
                Self::append_execution_event(
                    &event_writer,
                    label,
                    "wait_failed",
                    json!({
                        "error": e.to_string(),
                    }),
                )
                .await;
                return Err(Error::runner(format!("Failed to wait for claude: {}", e)));
            }
            WaitOutcome::TimedOut => {
                Self::append_execution_event(
                    &event_writer,
                    label,
                    "subprocess_timed_out",
                    json!({
                        "timeout_secs": self.config.timeout_secs,
                    }),
                )
                .await;
                // Timeout occurred - try to kill the process
                tracing::error!(
                    component = "claude",
                    label = label,
                    timeout_secs = self.config.timeout_secs,
                    "Process timed out, attempting to kill"
                );
                Self::append_execution_event(
                    &event_writer,
                    label,
                    "kill_requested_after_timeout",
                    json!({}),
                )
                .await;
                if let Err(e) = child.kill().await {
                    Self::append_execution_event(
                        &event_writer,
                        label,
                        "kill_failed_after_timeout",
                        json!({
                            "error": e.to_string(),
                        }),
                    )
                    .await;
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
                    Self::append_execution_event(
                        &event_writer,
                        label,
                        "execution_record_failed",
                        json!({
                            "error": e.to_string(),
                        }),
                    )
                    .await;
                    tracing::warn!(error = %e, "Failed to record timed-out execution to database");
                } else {
                    Self::append_execution_event(
                        &event_writer,
                        label,
                        "execution_recorded",
                        json!({
                            "timed_out": true,
                        }),
                    )
                    .await;
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

        let stdout_result = match stdout_handle.await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!(
                    component = "claude",
                    label = label,
                    error = %e,
                    "stdout reader task failed"
                );
                StdoutParseResult::default()
            }
        };
        let StdoutParseResult {
            text_output,
            structured_result,
            cost_usd: result_cost_usd,
            num_turns: result_num_turns,
            session_id: result_session_id,
            duration_api_ms: result_duration_api_ms,
            input_tokens: result_input_tokens,
            output_tokens: result_output_tokens,
            cache_read_input_tokens: result_cache_read_tokens,
            cache_creation_input_tokens: result_cache_creation_tokens,
        } = stdout_result;
        let stderr_output = stderr_handle.await.unwrap_or_default();

        let exit_code = status.code().unwrap_or(-1);
        tracing::info!(
            component = "claude",
            label = label,
            exit_code = exit_code,
            timed_out = timed_out,
            has_structured_result = structured_result.is_some(),
            "Process completed"
        );
        Self::append_execution_event(
            &event_writer,
            label,
            "process_completed",
            json!({
                "exit_code": exit_code,
                "timed_out": timed_out,
                "stdout_bytes": text_output.len(),
                "stderr_bytes": stderr_output.len(),
                "has_structured_result": structured_result.is_some(),
            }),
        )
        .await;

        // Extract fields from structured result, falling back to legacy extraction.
        let legacy_fallback = || {
            let pr = Self::extract_pr_url(&text_output);
            let bq = Self::extract_blocking_question(&text_output)
                .or_else(|| Self::extract_blocking_question(&stderr_output));
            (status.success(), text_output.clone(), pr, bq)
        };

        let (mut result_success, result_output, pr_url, blocking_question) = structured_result
            .as_ref()
            .and_then(|val| serde_json::from_value::<StructuredResult>(val.clone()).ok())
            .map(|sr| {
                let sr_success = sr.success;
                let sr_output = if sr.summary.is_empty() {
                    text_output.clone()
                } else {
                    sr.summary
                };
                let sr_pr_url = sr
                    .pr_url
                    .filter(|url| url.starts_with("https://"))
                    .or_else(|| Self::extract_pr_url(&text_output));
                let sr_question = sr.blocking_question;
                (sr_success, sr_output, sr_pr_url, sr_question)
            })
            .unwrap_or_else(legacy_fallback);

        // Process-level failure always overrides model's self-reported success.
        // The CLI could crash after the model responded (e.g., during git push).
        if !status.success() {
            result_success = false;
        }

        if let Some(ref url) = pr_url {
            tracing::info!(
                component = "claude",
                label = label,
                pr_url = url,
                "PR URL extracted"
            );
            Self::append_execution_event(
                &event_writer,
                label,
                "pr_url_extracted",
                json!({
                    "pr_url": url,
                }),
            )
            .await;
        }

        let failure_msg = if status.success() {
            None
        } else {
            Some(Self::compose_failure_message(
                exit_code,
                &text_output,
                &stderr_output,
            ))
        };
        let is_rate_limited = failure_msg
            .as_ref()
            .map(|msg| Self::is_rate_limit_error(msg))
            .unwrap_or(false)
            || Self::is_rate_limit_error(&text_output)
            || Self::is_rate_limit_error(&stderr_output);

        // Complete and record the execution
        execution.complete(status.code(), timed_out);
        execution.stdout_preview = Some(Self::truncate(&text_output, EXECUTION_LOG_PREVIEW_LIMIT));
        execution.stderr_preview = if stderr_output.is_empty() {
            failure_msg
                .as_ref()
                .map(|msg| Self::truncate(msg, EXECUTION_LOG_PREVIEW_LIMIT))
        } else {
            Some(Self::truncate(&stderr_output, EXECUTION_LOG_PREVIEW_LIMIT))
        };
        execution.total_cost_usd = result_cost_usd;
        execution.num_turns = result_num_turns;
        execution.session_id = result_session_id;
        execution.duration_api_ms = result_duration_api_ms;
        execution.input_tokens = result_input_tokens;
        execution.output_tokens = result_output_tokens;
        execution.cache_read_input_tokens = result_cache_read_tokens;
        execution.cache_creation_input_tokens = result_cache_creation_tokens;

        // Record the execution to the database (don't fail the main operation if this fails)
        if let Err(e) = self.tracker.record_execution(&execution) {
            Self::append_execution_event(
                &event_writer,
                label,
                "execution_record_failed",
                json!({
                    "error": e.to_string(),
                }),
            )
            .await;
            tracing::warn!(error = %e, "Failed to record execution to database");
        } else {
            Self::append_execution_event(
                &event_writer,
                label,
                "execution_recorded",
                json!({
                    "timed_out": false,
                    "exit_code": execution.exit_code,
                }),
            )
            .await;
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

        if let Some(question) = blocking_question.as_ref() {
            Self::append_execution_event(
                &event_writer,
                label,
                "blocking_question_parsed",
                json!({
                    "question": question.question.as_str(),
                    "has_context": question.context.is_some(),
                    "options": question.options.clone(),
                    "has_why": question.why.is_some(),
                }),
            )
            .await;
        }

        Ok(ClaudeResult {
            success: result_success,
            output: result_output,
            pr_url,
            error: failure_msg,
            blocking_question,
            used_qa_ids: Vec::new(),
        })
    }

    /// Compute a SHA256 hash of the prompt for grouping similar prompts.
    /// Returns the first 16 hex characters (8 bytes of the digest).
    fn hash_prompt(prompt: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prompt.as_bytes());
        let result = hasher.finalize();
        // Format only the first 8 bytes directly instead of all 32.
        let mut buf = String::with_capacity(16);
        for byte in &result[..8] {
            use std::fmt::Write;
            let _ = write!(buf, "{:02x}", byte);
        }
        buf
    }

    /// Truncate a string to approximately max_len bytes, adding "..." if truncated.
    /// Ensures the cut happens at a valid UTF-8 char boundary.
    fn truncate(s: &str, max_len: usize) -> String {
        if s.len() <= max_len {
            s.to_string()
        } else {
            let end = max_len.saturating_sub(3);
            // Fast path: if `end` is already on a char boundary (common for ASCII),
            // skip the O(n) char_indices walk entirely.
            let safe_end = if s.is_char_boundary(end) {
                end
            } else {
                // Find the nearest char boundary at or before `end`
                s.char_indices()
                    .take_while(|(i, _)| *i <= end)
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            };
            format!("{}...", &s[..safe_end])
        }
    }

    /// Extract PR URL from output.
    fn extract_pr_url(output: &str) -> Option<String> {
        use std::sync::LazyLock;

        static PR_URL_EXPLICIT_RE: LazyLock<regex_lite::Regex> =
            LazyLock::new(|| regex_lite::Regex::new(r"PR_URL:\s*(https://[^\s]+)").unwrap());
        static GITHUB_PR_RE: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
            regex_lite::Regex::new(r"https://github\.com/[^\s/]+/[^\s/]+/pull/\d+[^\s]*").unwrap()
        });
        static GITLAB_MR_RE: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
            regex_lite::Regex::new(r"https://gitlab\.com/[^\s]+/-/merge_requests/\d+[^\s]*")
                .unwrap()
        });
        // Also match self-hosted GitLab instances
        static GITLAB_MR_GENERIC_RE: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
            regex_lite::Regex::new(
                r"https://[a-zA-Z0-9._-]+(?:\.[a-zA-Z]{2,})/[^\s/]+/[^\s/]+/-/merge_requests/\d+",
            )
            .unwrap()
        });

        // Try explicit PR_URL format first
        if let Some(captures) = PR_URL_EXPLICIT_RE.captures(output) {
            return captures.get(1).map(|m| m.as_str().to_string());
        }

        // Try GitHub PR URL pattern
        if let Some(m) = GITHUB_PR_RE.find(output) {
            return Some(m.as_str().to_string());
        }

        // Try GitLab MR URL pattern (gitlab.com)
        if let Some(m) = GITLAB_MR_RE.find(output) {
            return Some(m.as_str().to_string());
        }

        // Try self-hosted GitLab MR URL pattern
        if let Some(m) = GITLAB_MR_GENERIC_RE.find(output) {
            return Some(m.as_str().to_string());
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CLI stream event parsing tests ──────────────────────────────

    #[test]
    fn test_parse_cli_assistant_text_event() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello world"}]}}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        match event {
            StreamEvent::Assistant { message: Some(msg) } => {
                assert_eq!(msg.content.len(), 1);
                match &msg.content[0] {
                    CliContentBlock::Text { text } => assert_eq!(text, "hello world"),
                    other => panic!("Unexpected content block: {:?}", other),
                }
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_cli_assistant_tool_use_event() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_1","name":"Bash"}]}}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        match event {
            StreamEvent::Assistant { message: Some(msg) } => {
                assert_eq!(msg.content.len(), 1);
                match &msg.content[0] {
                    CliContentBlock::ToolUse { id, name } => {
                        assert_eq!(id, "tu_1");
                        assert_eq!(name, "Bash");
                    }
                    other => panic!("Unexpected content block: {:?}", other),
                }
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_cli_assistant_multiple_content_blocks() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Running command..."},{"type":"tool_use","id":"tu_2","name":"Read"}]}}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        match event {
            StreamEvent::Assistant { message: Some(msg) } => {
                assert_eq!(msg.content.len(), 2);
                assert!(
                    matches!(&msg.content[0], CliContentBlock::Text { text } if text == "Running command...")
                );
                assert!(
                    matches!(&msg.content[1], CliContentBlock::ToolUse { id, name } if id == "tu_2" && name == "Read")
                );
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_cli_system_event() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc"}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(event, StreamEvent::System {}));
    }

    #[test]
    fn test_parse_cli_user_event() {
        let line = r#"{"type":"user","message":{"role":"user"}}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(event, StreamEvent::User {}));
    }

    #[test]
    fn test_parse_cli_result_with_structured_output() {
        let line = r#"{"type":"result","structured_output":{"summary":"done","success":true}}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        match event {
            StreamEvent::Result {
                structured_output: Some(obj),
                ..
            } => {
                assert_eq!(obj["summary"], "done");
                assert_eq!(obj["success"], true);
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_cli_result_without_structured_output() {
        let line = r#"{"type":"result"}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        match event {
            StreamEvent::Result {
                structured_output: None,
                ..
            } => {}
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_cli_result_with_cost_and_usage() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":2557,"duration_api_ms":2546,"num_turns":1,"result":"done","session_id":"sess-123","total_cost_usd":0.027,"usage":{"input_tokens":3,"cache_creation_input_tokens":2833,"cache_read_input_tokens":18758,"output_tokens":4}}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        match event {
            StreamEvent::Result {
                total_cost_usd: Some(cost),
                num_turns: Some(turns),
                session_id: Some(ref sid),
                duration_api_ms: Some(api_ms),
                usage: Some(ref u),
                ..
            } => {
                assert!((cost - 0.027).abs() < 1e-6);
                assert_eq!(turns, 1);
                assert_eq!(sid, "sess-123");
                assert_eq!(api_ms, 2546);
                assert_eq!(u.input_tokens, Some(3));
                assert_eq!(u.output_tokens, Some(4));
                assert_eq!(u.cache_read_input_tokens, Some(18758));
                assert_eq!(u.cache_creation_input_tokens, Some(2833));
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_cli_assistant_unknown_content_block() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        match event {
            StreamEvent::Assistant { message: Some(msg) } => {
                assert_eq!(msg.content.len(), 1);
                assert!(matches!(&msg.content[0], CliContentBlock::Other));
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_cli_unknown_event_forward_compat() {
        let line = r#"{"type":"some_future_event","data":"anything"}"#;
        let event: StreamEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(event, StreamEvent::Unknown));
    }

    // ── Structured result extraction tests ───────────────────────────

    #[test]
    fn test_structured_result_full() {
        let json = r#"{"summary":"Fixed the bug and created PR","success":true,"pr_url":"https://github.com/org/repo/pull/42","blocking_question":null}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        assert!(sr.success);
        assert_eq!(sr.summary, "Fixed the bug and created PR");
        assert_eq!(
            sr.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
        assert!(sr.blocking_question.is_none());
    }

    #[test]
    fn test_structured_result_with_blocking_question() {
        let json = r#"{"summary":"Need clarification","success":false,"pr_url":null,"blocking_question":{"question":"Which branch?","context":"Multiple candidates","options":["main","develop"],"why":"Ambiguous target"}}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        assert!(!sr.success);
        assert!(sr.pr_url.is_none());
        let bq = sr.blocking_question.unwrap();
        assert_eq!(bq.question, "Which branch?");
        assert_eq!(bq.context.as_deref(), Some("Multiple candidates"));
        assert_eq!(bq.options, vec!["main", "develop"]);
        assert_eq!(bq.why.as_deref(), Some("Ambiguous target"));
    }

    #[test]
    fn test_structured_result_minimal() {
        let json = r#"{"summary":"Done","success":true}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        assert!(sr.success);
        assert_eq!(sr.summary, "Done");
        assert!(sr.pr_url.is_none());
        assert!(sr.blocking_question.is_none());
    }

    #[test]
    fn test_structured_result_empty_summary() {
        let json = r#"{"summary":"","success":false}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        assert!(!sr.success);
        assert!(sr.summary.is_empty());
    }

    // ── Schema validity test ─────────────────────────────────────────

    #[test]
    fn test_result_schema_is_valid_json() {
        let parsed: serde_json::Value = serde_json::from_str(RESULT_SCHEMA).unwrap();
        assert_eq!(parsed["type"], "object");
        assert!(parsed["properties"]["summary"].is_object());
        assert!(parsed["properties"]["success"].is_object());
        assert!(parsed["properties"]["pr_url"].is_object());
        assert!(parsed["properties"]["blocking_question"].is_object());
    }

    // ── Prompt content tests ─────────────────────────────────────────

    #[test]
    fn test_build_prompt_no_question_protocol() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "TEST-1", "Bug", "https://example.com", "linear");
        let prompt = runner.build_prompt(&issue, "context", std::path::Path::new("/tmp"));
        assert!(!prompt.contains("CLAUDEAR_QUESTION"));
    }

    #[test]
    fn test_build_prompt_no_pr_url_instruction() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "TEST-1", "Bug", "https://example.com", "linear");
        let prompt = runner.build_prompt(&issue, "context", std::path::Path::new("/tmp"));
        assert!(!prompt.contains("PR_URL:"));
    }

    // ── Existing tests (kept) ────────────────────────────────────────

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
    }

    #[test]
    fn test_extract_pr_url_with_trailing_punctuation() {
        let output = "Created PR: https://github.com/org/repo/pull/123.";
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
        assert!(url.unwrap().contains("/pull/"));
    }

    #[test]
    fn test_extract_pr_url_explicit_takes_precedence() {
        let output = "Random text https://github.com/org/repo/pull/999\nPR_URL: https://github.com/org/main/pull/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(
            url,
            Some("https://github.com/org/main/pull/123".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_with_query_params() {
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
        assert!(!prompt.is_empty());
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
        let output = "PR at https://github.mycompany.com/org/repo/pull/99";
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
        let prompt =
            runner.build_prompt(&issue, "Feature description", std::path::Path::new("/tmp"));
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
        let prompt = runner.build_prompt(&issue, "Bug details", std::path::Path::new("/tmp"));
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_has_agent_md() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        assert!(!runner.has_agent_md(std::path::Path::new("/nonexistent/path")));
    }

    #[test]
    fn test_get_agent_md_nonexistent() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        assert!(runner
            .get_agent_md(std::path::Path::new("/nonexistent/path"))
            .is_none());
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
        assert!(result.error.is_some());
        assert!(result.error.unwrap().is_empty());
    }

    #[test]
    fn test_extract_pr_url_case_sensitivity() {
        let output = "pr_url: https://github.com/org/repo/pull/1";
        let url = ClaudeRunner::extract_pr_url(output);
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
        let prompt = runner.build_prompt(&issue, "", std::path::Path::new("/tmp"));
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
        let prompt = runner.build_prompt(
            &issue,
            "Context with special chars: <>&\"'",
            std::path::Path::new("/tmp"),
        );
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_extract_pr_url_no_scheme() {
        assert!(ClaudeRunner::extract_pr_url("github.com/org/repo/pull/123").is_none());
    }

    #[test]
    fn test_extract_pr_url_http_not_https() {
        assert!(
            ClaudeRunner::extract_pr_url("PR at http://github.com/org/repo/pull/123").is_none()
        );
    }

    #[test]
    fn test_extract_pr_url_multiline_pr_url() {
        let output = "Creating PR...\nPR_URL: https://github.com/org/repo/pull/42\nDone!";
        assert_eq!(
            ClaudeRunner::extract_pr_url(output),
            Some("https://github.com/org/repo/pull/42".to_string())
        );
    }

    #[test]
    fn test_build_prompt_multiline_context() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "TEST-1", "Bug", "https://example.com", "linear");
        let prompt = runner.build_prompt(
            &issue,
            "Line 1\nLine 2\nLine 3\n",
            std::path::Path::new("/tmp"),
        );
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_extract_pr_url_gitlab_self_hosted() {
        let url = ClaudeRunner::extract_pr_url(
            "MR: https://gitlab.mycompany.com/group/project/-/merge_requests/123",
        );
        assert_eq!(
            url.as_deref(),
            Some("https://gitlab.mycompany.com/group/project/-/merge_requests/123")
        );
    }

    #[test]
    fn test_extract_pr_url_github_pr_zero() {
        assert!(ClaudeRunner::extract_pr_url("PR: https://github.com/org/repo/pull/0").is_some());
    }

    #[test]
    fn test_extract_pr_url_very_long_pr_number() {
        assert_eq!(
            ClaudeRunner::extract_pr_url("PR: https://github.com/org/repo/pull/999999999999"),
            Some("https://github.com/org/repo/pull/999999999999".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_org_with_dashes() {
        assert_eq!(
            ClaudeRunner::extract_pr_url(
                "PR: https://github.com/my-org-name/my-repo-name/pull/123"
            ),
            Some("https://github.com/my-org-name/my-repo-name/pull/123".to_string())
        );
    }

    #[test]
    fn test_extract_pr_url_org_with_dots() {
        assert_eq!(
            ClaudeRunner::extract_pr_url("PR: https://github.com/org.name/repo.name/pull/123"),
            Some("https://github.com/org.name/repo.name/pull/123".to_string())
        );
    }

    #[test]
    fn test_claude_result_pr_url_with_trailing_slash() {
        assert!(
            ClaudeRunner::extract_pr_url("PR_URL: https://github.com/org/repo/pull/123/ Done")
                .is_some()
        );
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
        let prompt = runner.build_prompt(
            &issue,
            "Context with emoji 🎉 and unicode ñ",
            std::path::Path::new("/tmp"),
        );
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_claude_runner_new_with_tracker() {
        use crate::storage::SqliteTracker;
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let runner = ClaudeRunner::new(ClaudeRunnerConfig::default(), tracker);
        assert!(!runner.has_agent_md(std::path::Path::new("/tmp")));
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

    // ── Legacy blocking question extraction (fallback) ───────────────

    #[test]
    fn test_extract_blocking_question_valid_payload() {
        let output = "some logs\nCLAUDEAR_QUESTION: {\"question\":\"Which branch?\",\"context\":\"unclear\",\"options\":[\"main\",\"develop\"],\"why\":\"need branch\"}\ndone";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "Which branch?");
        assert_eq!(parsed.context.as_deref(), Some("unclear"));
        assert_eq!(parsed.options, vec!["main", "develop"]);
        assert_eq!(parsed.why.as_deref(), Some("need branch"));
    }

    #[test]
    fn test_extract_blocking_question_ignores_malformed() {
        assert!(
            ClaudeRunner::extract_blocking_question("CLAUDEAR_QUESTION: {not valid json}")
                .is_none()
        );
    }

    #[test]
    fn test_extract_blocking_question_empty() {
        assert!(ClaudeRunner::extract_blocking_question("").is_none());
    }

    #[test]
    fn test_extract_blocking_question_only_required_field() {
        let output = "CLAUDEAR_QUESTION: {\"question\":\"minimal\"}";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "minimal");
        assert!(parsed.context.is_none());
        assert!(parsed.options.is_empty());
        assert!(parsed.why.is_none());
    }

    // ── Truncate tests ───────────────────────────────────────────────

    #[test]
    fn test_truncate_shorter_than_max() {
        assert_eq!(ClaudeRunner::truncate("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_exactly_at_max() {
        assert_eq!(ClaudeRunner::truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_one_byte_over_max() {
        let result = ClaudeRunner::truncate("abcdef", 5);
        assert!(result.ends_with("..."));
        assert_eq!(result, "ab...");
    }

    #[test]
    fn test_truncate_multibyte_unicode_at_boundary() {
        let result = ClaudeRunner::truncate("aéb", 3);
        assert!(result.ends_with("..."));
        assert_eq!(result, "...");
    }

    #[test]
    fn test_truncate_empty_string_zero_max() {
        assert_eq!(ClaudeRunner::truncate("", 0), "");
    }

    #[test]
    fn test_truncate_max_len_of_3() {
        assert_eq!(ClaudeRunner::truncate("abcdef", 3), "...");
    }

    #[test]
    fn test_truncate_max_len_of_2() {
        assert_eq!(ClaudeRunner::truncate("abcdef", 2), "...");
    }

    #[test]
    fn test_truncate_very_long_string() {
        let s = "a".repeat(10_000);
        let result = ClaudeRunner::truncate(&s, 100);
        assert!(result.ends_with("..."));
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn test_truncate_max_len_of_4() {
        assert_eq!(ClaudeRunner::truncate("abcdef", 4), "a...");
    }

    // ── Sanitize label tests ─────────────────────────────────────────

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
        let result = ClaudeRunner::sanitize_label(&"a".repeat(100));
        assert_eq!(result.len(), 64);
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
        assert_eq!(ClaudeRunner::sanitize_label(&label), label);
    }

    #[test]
    fn test_sanitize_label_all_special_chars_non_empty() {
        assert_eq!(ClaudeRunner::sanitize_label("@#$"), "___");
    }

    // ── Compose failure message tests ────────────────────────────────

    #[test]
    fn test_compose_failure_message_both_empty() {
        assert_eq!(
            ClaudeRunner::compose_failure_message(1, "", ""),
            "Process exited with code 1"
        );
    }

    #[test]
    fn test_compose_failure_message_only_stderr() {
        assert_eq!(
            ClaudeRunner::compose_failure_message(1, "", "error occurred"),
            "error occurred"
        );
    }

    #[test]
    fn test_compose_failure_message_only_stdout() {
        assert_eq!(
            ClaudeRunner::compose_failure_message(1, "some output", ""),
            "Process exited with code 1. Output: some output"
        );
    }

    #[test]
    fn test_compose_failure_message_both_present_stderr_takes_priority() {
        assert_eq!(
            ClaudeRunner::compose_failure_message(1, "stdout text", "stderr text"),
            "stderr text"
        );
    }

    #[test]
    fn test_compose_failure_message_rate_limit_in_stderr() {
        let msg = ClaudeRunner::compose_failure_message(1, "", "rate limit exceeded");
        assert!(msg.starts_with("Claude rate limit hit:"));
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
        let msg = ClaudeRunner::compose_failure_message(1, "", &"e".repeat(5000));
        assert!(msg.len() <= EXECUTION_LOG_PREVIEW_LIMIT + 3);
        assert!(msg.ends_with("..."));
    }

    #[test]
    fn test_compose_failure_message_whitespace_only_inputs() {
        assert_eq!(
            ClaudeRunner::compose_failure_message(42, "   ", "  \n\t  "),
            "Process exited with code 42"
        );
    }

    // ── Rate limit / hard error detection tests ──────────────────────

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

    // ── Hash prompt tests ────────────────────────────────────────────

    #[test]
    fn test_hash_prompt_deterministic() {
        assert_eq!(
            ClaudeRunner::hash_prompt("same prompt"),
            ClaudeRunner::hash_prompt("same prompt")
        );
    }

    #[test]
    fn test_hash_prompt_different_prompts_different_hashes() {
        assert_ne!(
            ClaudeRunner::hash_prompt("prompt one"),
            ClaudeRunner::hash_prompt("prompt two")
        );
    }

    #[test]
    fn test_hash_prompt_empty_produces_hash() {
        let h = ClaudeRunner::hash_prompt("");
        assert!(!h.is_empty());
        assert_eq!(h.len(), 16);
    }

    #[test]
    fn test_hash_prompt_length_is_16() {
        assert_eq!(ClaudeRunner::hash_prompt("any prompt here").len(), 16);
    }

    #[test]
    fn test_hash_prompt_only_hex_chars() {
        assert!(ClaudeRunner::hash_prompt("test")
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_prompt_unicode_works() {
        let h = ClaudeRunner::hash_prompt("こんにちは 🌍");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ── ClaudeExecution tests ────────────────────────────────────────

    #[test]
    fn test_claude_execution_new_defaults() {
        let exec = ClaudeExecution::new();
        assert_eq!(exec.id, 0);
        assert!(exec.attempt_id.is_none());
        assert!(exec.completed_at.is_none());
        assert!(exec.duration_secs.is_none());
        assert!(exec.exit_code.is_none());
        assert!(!exec.timed_out);
    }

    #[test]
    fn test_claude_execution_with_attempt_id() {
        assert_eq!(
            ClaudeExecution::new().with_attempt_id(42).attempt_id,
            Some(42)
        );
    }

    #[test]
    fn test_claude_execution_complete_sets_fields() {
        let mut exec = ClaudeExecution::new();
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
    }

    // ── ClaudeRunnerConfig tests ─────────────────────────────────────

    #[test]
    fn test_claude_runner_config_default_values() {
        let config = ClaudeRunnerConfig::default();
        assert_eq!(config.timeout_secs, 21600);
        assert!(config.model.is_none());
        assert!(config.instructions.is_none());
        assert!(config.permissions.is_empty());
        assert!(!config.skip_permissions);
    }

    #[test]
    fn test_claude_runner_config_custom_timeout() {
        let config = ClaudeRunnerConfig {
            timeout_secs: 60,
            ..Default::default()
        };
        assert_eq!(config.timeout_secs, 60);
        assert!(config.model.is_none());
        assert!(!config.skip_permissions);
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

    // ── resolve_log_root() tests ────────────────────────────────────

    #[test]
    fn test_resolve_log_root_default_without_env_var() {
        // Remove the env var if it happens to be set
        let prev = std::env::var("CLAUDEAR_LOG_DIR").ok();
        std::env::remove_var("CLAUDEAR_LOG_DIR");

        let root = resolve_log_root();
        assert_eq!(root, PathBuf::from(DEFAULT_LOG_DIR));

        // Restore previous value
        if let Some(val) = prev {
            std::env::set_var("CLAUDEAR_LOG_DIR", val);
        }
    }

    #[test]
    fn test_resolve_log_root_with_env_var() {
        let prev = std::env::var("CLAUDEAR_LOG_DIR").ok();
        std::env::set_var("CLAUDEAR_LOG_DIR", "/tmp/custom-logs");

        let root = resolve_log_root();
        assert_eq!(root, PathBuf::from("/tmp/custom-logs"));

        // Restore
        if let Some(val) = prev {
            std::env::set_var("CLAUDEAR_LOG_DIR", val);
        } else {
            std::env::remove_var("CLAUDEAR_LOG_DIR");
        }
    }

    #[test]
    fn test_resolve_log_root_private_matches_public() {
        // The private method ClaudeRunner::resolve_log_root() should match
        // the public function resolve_log_root() since they share the same logic.
        let prev = std::env::var("CLAUDEAR_LOG_DIR").ok();
        std::env::remove_var("CLAUDEAR_LOG_DIR");

        let public_root = resolve_log_root();
        let private_root = ClaudeRunner::resolve_log_root();
        assert_eq!(public_root, private_root);

        if let Some(val) = prev {
            std::env::set_var("CLAUDEAR_LOG_DIR", val);
        }
    }

    // ── StdoutParseResult default tests ──────────────────────────────

    #[test]
    fn test_stdout_parse_result_default() {
        let result = StdoutParseResult::default();
        assert!(result.text_output.is_empty());
        assert!(result.structured_result.is_none());
        assert!(result.cost_usd.is_none());
        assert!(result.num_turns.is_none());
        assert!(result.session_id.is_none());
        assert!(result.duration_api_ms.is_none());
        assert!(result.input_tokens.is_none());
        assert!(result.output_tokens.is_none());
        assert!(result.cache_read_input_tokens.is_none());
        assert!(result.cache_creation_input_tokens.is_none());
    }

    // ── ExecutionLogFiles struct tests ────────────────────────────────

    #[test]
    fn test_execution_log_files_clone() {
        let files = ExecutionLogFiles {
            stdout: PathBuf::from("/tmp/test.stdout.log"),
            stderr: PathBuf::from("/tmp/test.stderr.log"),
            events: PathBuf::from("/tmp/test.events.jsonl"),
        };
        let cloned = files.clone();
        assert_eq!(cloned.stdout, files.stdout);
        assert_eq!(cloned.stderr, files.stderr);
        assert_eq!(cloned.events, files.events);
    }

    #[test]
    fn test_execution_log_files_debug() {
        let files = ExecutionLogFiles {
            stdout: PathBuf::from("/tmp/test.stdout.log"),
            stderr: PathBuf::from("/tmp/test.stderr.log"),
            events: PathBuf::from("/tmp/test.events.jsonl"),
        };
        let debug = format!("{:?}", files);
        assert!(debug.contains("stdout"));
        assert!(debug.contains("stderr"));
        assert!(debug.contains("events"));
    }

    // ── create_execution_log_files tests ─────────────────────────────

    #[test]
    fn test_create_execution_log_files_produces_valid_paths() {
        let prev = std::env::var("CLAUDEAR_LOG_DIR").ok();
        let tmp_dir = std::env::temp_dir().join("claudear_test_exec_logs");
        std::env::set_var("CLAUDEAR_LOG_DIR", tmp_dir.to_str().unwrap());

        let files = ClaudeRunner::create_execution_log_files("test-label");
        assert!(files.is_some());

        let files = files.unwrap();
        assert!(files.stdout.to_str().unwrap().contains("test-label"));
        assert!(files.stdout.to_str().unwrap().ends_with(".stdout.log"));
        assert!(files.stderr.to_str().unwrap().ends_with(".stderr.log"));
        assert!(files.events.to_str().unwrap().ends_with(".events.jsonl"));

        // All paths should share the same parent directory
        assert_eq!(files.stdout.parent(), files.stderr.parent());
        assert_eq!(files.stderr.parent(), files.events.parent());

        // The path should include the CLAUDE_LOG_SUBDIR
        assert!(files.stdout.to_str().unwrap().contains(CLAUDE_LOG_SUBDIR));

        // Clean up
        let _ = std::fs::remove_dir_all(&tmp_dir);
        if let Some(val) = prev {
            std::env::set_var("CLAUDEAR_LOG_DIR", val);
        } else {
            std::env::remove_var("CLAUDEAR_LOG_DIR");
        }
    }

    #[test]
    fn test_create_execution_log_files_sanitizes_label_in_filenames() {
        let prev = std::env::var("CLAUDEAR_LOG_DIR").ok();
        let tmp_dir = std::env::temp_dir().join("claudear_test_sanitize_logs");
        std::env::set_var("CLAUDEAR_LOG_DIR", tmp_dir.to_str().unwrap());

        let files = ClaudeRunner::create_execution_log_files("hello world/foo@bar");
        assert!(files.is_some());
        let files = files.unwrap();
        let stem = files
            .stdout
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        // The label portion should not contain spaces, slashes, or @
        assert!(!stem.contains(' '));
        assert!(!stem.contains('/'));
        assert!(!stem.contains('@'));

        let _ = std::fs::remove_dir_all(&tmp_dir);
        if let Some(val) = prev {
            std::env::set_var("CLAUDEAR_LOG_DIR", val);
        } else {
            std::env::remove_var("CLAUDEAR_LOG_DIR");
        }
    }

    // ── prepare_env_and_label tests ──────────────────────────────────

    #[test]
    fn test_prepare_env_and_label_with_issue() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("id-42", "PROJ-42", "A bug", "https://ex.com", "linear");
        let (env, label) = runner.prepare_env_and_label(Some(&issue));

        assert_eq!(label, "PROJ-42");
        assert_eq!(env.get("LINEAR_ISSUE_ID"), Some(&"id-42".to_string()));
        assert_eq!(
            env.get("LINEAR_ISSUE_SHORT_ID"),
            Some(&"PROJ-42".to_string())
        );
        assert_eq!(
            env.get("LINEAR_ISSUE_URL"),
            Some(&"https://ex.com".to_string())
        );
    }

    #[test]
    fn test_prepare_env_and_label_without_issue() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let (env, label) = runner.prepare_env_and_label(None);

        assert_eq!(label, "custom");
        // Should not contain any issue-specific env vars
        assert!(env.get("LINEAR_ISSUE_ID").is_none());
    }

    #[test]
    fn test_prepare_env_and_label_sentry_source() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("s-1", "SENTRY-1", "Error", "https://sentry.io/1", "sentry");
        let (env, label) = runner.prepare_env_and_label(Some(&issue));

        assert_eq!(label, "SENTRY-1");
        assert_eq!(env.get("SENTRY_ISSUE_ID"), Some(&"s-1".to_string()));
        assert_eq!(
            env.get("SENTRY_ISSUE_SHORT_ID"),
            Some(&"SENTRY-1".to_string())
        );
        assert_eq!(
            env.get("SENTRY_ISSUE_URL"),
            Some(&"https://sentry.io/1".to_string())
        );
    }

    #[test]
    fn test_prepare_env_and_label_github_source() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new(
            "789",
            "#789",
            "Feature",
            "https://github.com/o/r/issues/789",
            "github",
        );
        let (env, label) = runner.prepare_env_and_label(Some(&issue));

        assert_eq!(label, "#789");
        assert_eq!(env.get("GITHUB_ISSUE_ID"), Some(&"789".to_string()));
        assert_eq!(env.get("GITHUB_ISSUE_SHORT_ID"), Some(&"#789".to_string()));
    }

    // ── build_prompt_for_issue public wrapper test ────────────────────

    #[test]
    fn test_build_prompt_for_issue_matches_build_prompt() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "TEST-1", "Bug", "https://example.com", "linear");
        let project_dir = Path::new("/tmp");

        let from_public = runner.build_prompt_for_issue(&issue, "ctx", project_dir);
        let from_private = runner.build_prompt(&issue, "ctx", project_dir);
        assert_eq!(from_public, from_private);
    }

    // ── has_agent_md / get_agent_md with a real AGENT.md ─────────────

    #[test]
    fn test_has_agent_md_with_existing_file() {
        let tmp_dir = std::env::temp_dir().join("claudear_test_agent_md");
        let _ = std::fs::create_dir_all(&tmp_dir);
        std::fs::write(tmp_dir.join("AGENT.md"), "# Agent instructions\nDo things.").unwrap();

        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        assert!(runner.has_agent_md(&tmp_dir));

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_get_agent_md_with_existing_file() {
        let tmp_dir = std::env::temp_dir().join("claudear_test_agent_md_get");
        let _ = std::fs::create_dir_all(&tmp_dir);
        let content = "# Agent\nCustom instructions here.";
        std::fs::write(tmp_dir.join("AGENT.md"), content).unwrap();

        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let loaded = runner.get_agent_md(&tmp_dir);
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap(), content);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_get_agent_md_returns_none_for_missing() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        assert!(runner
            .get_agent_md(Path::new("/nonexistent/path/xyz"))
            .is_none());
    }

    // ── Deserialization edge cases ───────────────────────────────────

    #[test]
    fn test_cli_usage_all_none() {
        let json = r#"{}"#;
        let usage: CliUsage = serde_json::from_str(json).unwrap();
        assert!(usage.input_tokens.is_none());
        assert!(usage.output_tokens.is_none());
        assert!(usage.cache_read_input_tokens.is_none());
        assert!(usage.cache_creation_input_tokens.is_none());
    }

    #[test]
    fn test_cli_usage_partial_fields() {
        let json = r#"{"input_tokens": 100, "output_tokens": 50}"#;
        let usage: CliUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert!(usage.cache_read_input_tokens.is_none());
        assert!(usage.cache_creation_input_tokens.is_none());
    }

    #[test]
    fn test_cli_usage_all_fields() {
        let json = r#"{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40}"#;
        let usage: CliUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(20));
        assert_eq!(usage.cache_read_input_tokens, Some(30));
        assert_eq!(usage.cache_creation_input_tokens, Some(40));
    }

    #[test]
    fn test_cli_message_empty_content() {
        let json = r#"{"content":[]}"#;
        let msg: CliMessage = serde_json::from_str(json).unwrap();
        assert!(msg.content.is_empty());
    }

    #[test]
    fn test_cli_message_missing_content_field_defaults() {
        let json = r#"{}"#;
        let msg: CliMessage = serde_json::from_str(json).unwrap();
        assert!(msg.content.is_empty());
    }

    #[test]
    fn test_stream_event_assistant_no_message() {
        let json = r#"{"type":"assistant"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Assistant { message: None } => {}
            other => panic!("Expected Assistant with no message, got: {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_result_all_fields() {
        let json = r#"{
            "type": "result",
            "structured_output": {"summary": "all done", "success": true},
            "total_cost_usd": 0.123,
            "num_turns": 5,
            "session_id": "sess-abc",
            "duration_api_ms": 9876,
            "usage": {"input_tokens": 100, "output_tokens": 200, "cache_read_input_tokens": 300, "cache_creation_input_tokens": 400}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Result {
                structured_output: Some(ref so),
                total_cost_usd: Some(cost),
                num_turns: Some(turns),
                session_id: Some(ref sid),
                duration_api_ms: Some(api_ms),
                usage: Some(ref u),
            } => {
                assert_eq!(so["summary"], "all done");
                assert!((cost - 0.123).abs() < 1e-6);
                assert_eq!(turns, 5);
                assert_eq!(sid, "sess-abc");
                assert_eq!(api_ms, 9876);
                assert_eq!(u.input_tokens, Some(100));
                assert_eq!(u.output_tokens, Some(200));
                assert_eq!(u.cache_read_input_tokens, Some(300));
                assert_eq!(u.cache_creation_input_tokens, Some(400));
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_result_no_optional_fields() {
        let json = r#"{"type":"result"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Result {
                structured_output: None,
                total_cost_usd: None,
                num_turns: None,
                session_id: None,
                duration_api_ms: None,
                usage: None,
            } => {}
            other => panic!("Expected Result with all None, got: {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_ignores_extra_fields() {
        // Ensure forward-compat: extra fields in known events are silently ignored
        let json =
            r#"{"type":"system","subtype":"init","session_id":"xyz","extra_field":"ignored"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::System {}));
    }

    #[test]
    fn test_stream_event_user_ignores_extra_fields() {
        let json = r#"{"type":"user","message":{"role":"user","content":"hello"},"extra":true}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::User {}));
    }

    // ── StructuredResult edge cases ──────────────────────────────────

    #[test]
    fn test_structured_result_non_https_pr_url() {
        let json =
            r#"{"summary":"done","success":true,"pr_url":"http://github.com/org/repo/pull/1"}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        assert!(sr.success);
        // The pr_url is stored as-is in StructuredResult; filtering to https happens in the runner
        assert_eq!(
            sr.pr_url.as_deref(),
            Some("http://github.com/org/repo/pull/1")
        );
    }

    #[test]
    fn test_structured_result_empty_pr_url_string() {
        let json = r#"{"summary":"done","success":true,"pr_url":""}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        assert_eq!(sr.pr_url.as_deref(), Some(""));
    }

    #[test]
    fn test_structured_result_blocking_question_minimal() {
        let json =
            r#"{"summary":"stuck","success":false,"blocking_question":{"question":"help?"}}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        let bq = sr.blocking_question.unwrap();
        assert_eq!(bq.question, "help?");
        assert!(bq.context.is_none());
        assert!(bq.options.is_empty());
        assert!(bq.why.is_none());
    }

    #[test]
    fn test_structured_result_blocking_question_empty_options() {
        let json = r#"{"summary":"stuck","success":false,"blocking_question":{"question":"q","context":null,"options":[],"why":null}}"#;
        let sr: StructuredResult = serde_json::from_str(json).unwrap();
        let bq = sr.blocking_question.unwrap();
        assert_eq!(bq.question, "q");
        assert!(bq.options.is_empty());
    }

    // ── extract_blocking_question edge cases ─────────────────────────

    #[test]
    fn test_extract_blocking_question_prefix_only_empty_payload() {
        // "CLAUDEAR_QUESTION:" followed by whitespace only => None
        let output = "CLAUDEAR_QUESTION:   ";
        assert!(ClaudeRunner::extract_blocking_question(output).is_none());
    }

    #[test]
    fn test_extract_blocking_question_prefix_without_colon() {
        let output = "CLAUDEAR_QUESTION {\"question\":\"test\"}";
        assert!(ClaudeRunner::extract_blocking_question(output).is_none());
    }

    #[test]
    fn test_extract_blocking_question_multiple_lines_picks_first() {
        let output = "CLAUDEAR_QUESTION: {\"question\":\"first\"}\nCLAUDEAR_QUESTION: {\"question\":\"second\"}";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "first");
    }

    #[test]
    fn test_extract_blocking_question_with_surrounding_whitespace() {
        let output = "   CLAUDEAR_QUESTION:  {\"question\":\"trimmed\"}  ";
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "trimmed");
    }

    #[test]
    fn test_extract_blocking_question_no_prefix_present() {
        let output = "Just some regular output\nwithout any question markers\n";
        assert!(ClaudeRunner::extract_blocking_question(output).is_none());
    }

    #[test]
    fn test_extract_blocking_question_with_all_fields() {
        let output = r#"CLAUDEAR_QUESTION: {"question":"Which DB?","context":"Found postgres and mysql","options":["postgres","mysql","sqlite"],"why":"Cannot determine from config"}"#;
        let parsed = ClaudeRunner::extract_blocking_question(output).unwrap();
        assert_eq!(parsed.question, "Which DB?");
        assert_eq!(parsed.context.as_deref(), Some("Found postgres and mysql"));
        assert_eq!(parsed.options, vec!["postgres", "mysql", "sqlite"]);
        assert_eq!(parsed.why.as_deref(), Some("Cannot determine from config"));
    }

    // ── compose_failure_message additional edge cases ─────────────────

    #[test]
    fn test_compose_failure_message_rate_limit_combined_empty_gives_default() {
        // When both are empty but still trigger rate limit through combined being empty,
        // this should not happen, but let's verify the fallback.
        let msg = ClaudeRunner::compose_failure_message(1, "", "");
        assert!(!msg.starts_with("Claude rate limit hit:"));
        assert_eq!(msg, "Process exited with code 1");
    }

    #[test]
    fn test_compose_failure_message_whitespace_stderr_ignored() {
        let msg = ClaudeRunner::compose_failure_message(2, "actual output", "   ");
        assert!(msg.contains("actual output"));
        assert!(msg.contains("Process exited with code 2"));
    }

    #[test]
    fn test_compose_failure_message_very_long_stdout_truncated() {
        let long_stdout = "o".repeat(5000);
        let msg = ClaudeRunner::compose_failure_message(1, &long_stdout, "");
        assert!(msg.len() <= EXECUTION_LOG_PREVIEW_LIMIT + 50); // +50 for prefix
        assert!(msg.contains("Process exited with code 1"));
    }

    #[test]
    fn test_compose_failure_message_rate_limit_429_in_stderr() {
        let msg = ClaudeRunner::compose_failure_message(1, "", "HTTP 429");
        assert!(msg.starts_with("Claude rate limit hit:"));
    }

    #[test]
    fn test_compose_failure_message_exit_code_zero_with_stderr() {
        // Even exit code 0 can produce a failure message if called
        let msg = ClaudeRunner::compose_failure_message(0, "", "some stderr");
        assert_eq!(msg, "some stderr");
    }

    #[test]
    fn test_compose_failure_message_negative_exit_code() {
        let msg = ClaudeRunner::compose_failure_message(-1, "", "");
        assert_eq!(msg, "Process exited with code -1");
    }

    // ── truncate additional edge cases ───────────────────────────────

    #[test]
    fn test_truncate_max_len_of_0_non_empty_input() {
        // max_len = 0 means we want 0 chars + "..." => the "..." itself
        let result = ClaudeRunner::truncate("hello", 0);
        assert_eq!(result, "...");
    }

    #[test]
    fn test_truncate_emoji_boundary() {
        // Emoji is 4 bytes. With max_len=5, we have room for 2 chars before "...",
        // but the emoji won't fit in 2 bytes, so we should get safe truncation.
        let input = "\u{1F600}abc"; // grinning face (4 bytes) + "abc"
        let result = ClaudeRunner::truncate(input, 5);
        assert!(result.ends_with("..."));
        // Must be valid UTF-8
        assert!(result.len() <= 8); // at most the emoji (4) + "..." (3)
    }

    #[test]
    fn test_truncate_all_multibyte() {
        let input = "\u{00e9}\u{00e9}\u{00e9}\u{00e9}"; // "eeee" with accents, 2 bytes each = 8 bytes
        let result = ClaudeRunner::truncate(input, 6);
        assert!(result.ends_with("..."));
        // Should safely truncate at a char boundary
        assert!(result.is_char_boundary(0));
    }

    #[test]
    fn test_truncate_max_len_of_1() {
        let result = ClaudeRunner::truncate("abcdef", 1);
        assert_eq!(result, "...");
    }

    // ── hash_prompt additional tests ─────────────────────────────────

    #[test]
    fn test_hash_prompt_very_long_string() {
        let long = "x".repeat(100_000);
        let h = ClaudeRunner::hash_prompt(&long);
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_prompt_whitespace_differences_produce_different_hashes() {
        assert_ne!(
            ClaudeRunner::hash_prompt("hello world"),
            ClaudeRunner::hash_prompt("hello  world")
        );
    }

    #[test]
    fn test_hash_prompt_case_sensitive() {
        assert_ne!(
            ClaudeRunner::hash_prompt("Hello"),
            ClaudeRunner::hash_prompt("hello")
        );
    }

    // ── is_rate_limit_error additional tests ─────────────────────────

    #[test]
    fn test_is_rate_limit_error_mixed_case_embedded() {
        assert!(ClaudeRunner::is_rate_limit_error(
            "Error: The API returned a RateLimit error"
        ));
    }

    #[test]
    fn test_is_rate_limit_error_quota_in_longer_message() {
        assert!(ClaudeRunner::is_rate_limit_error(
            "Your project quota exceeded the monthly limit"
        ));
    }

    #[test]
    fn test_is_rate_limit_error_not_triggered_by_partial_substring() {
        // "rate" alone should not trigger it
        assert!(!ClaudeRunner::is_rate_limit_error("first rate code"));
    }

    // ── is_hard_error additional tests ───────────────────────────────

    #[test]
    fn test_is_hard_error_all_needles() {
        // Verify every hard error needle is recognized
        let needles = [
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
        ];
        for needle in needles {
            assert!(
                ClaudeRunner::is_hard_error(needle),
                "Expected hard error for: {}",
                needle
            );
        }
    }

    #[test]
    fn test_is_hard_error_normal_compilation_errors_not_hard() {
        let soft_errors = [
            "error[E0308]: mismatched types",
            "npm ERR! code ELIFECYCLE",
            "FAILED: 3 tests",
            "assertion failed at line 42",
            "segfault",
        ];
        for msg in soft_errors {
            assert!(
                !ClaudeRunner::is_hard_error(msg),
                "Should NOT be hard error: {}",
                msg
            );
        }
    }

    // ── sanitize_label additional tests ──────────────────────────────

    #[test]
    fn test_sanitize_label_numbers_only() {
        assert_eq!(ClaudeRunner::sanitize_label("12345"), "12345");
    }

    #[test]
    fn test_sanitize_label_single_char() {
        assert_eq!(ClaudeRunner::sanitize_label("a"), "a");
        assert_eq!(ClaudeRunner::sanitize_label("@"), "_");
    }

    #[test]
    fn test_sanitize_label_mixed_valid_and_invalid() {
        assert_eq!(ClaudeRunner::sanitize_label("PROJ-123/fix"), "PROJ-123_fix");
    }

    #[test]
    fn test_sanitize_label_65_chars_truncated() {
        let label = "a".repeat(65);
        let result = ClaudeRunner::sanitize_label(&label);
        assert_eq!(result.len(), 64);
    }

    // ── ClaudeRunnerConfig full construction ─────────────────────────

    #[test]
    fn test_claude_runner_config_fully_specified() {
        let config = ClaudeRunnerConfig {
            timeout_secs: 300,
            model: Some("claude-3-opus".to_string()),
            instructions: Some("Be concise.".to_string()),
            permissions: vec!["Bash".to_string(), "Read".to_string(), "Write".to_string()],
            skip_permissions: true,
        };
        assert_eq!(config.timeout_secs, 300);
        assert_eq!(config.model.as_deref(), Some("claude-3-opus"));
        assert_eq!(config.instructions.as_deref(), Some("Be concise."));
        assert_eq!(config.permissions.len(), 3);
        assert!(config.skip_permissions);

        // Clone preserves all fields
        let cloned = config.clone();
        assert_eq!(cloned.timeout_secs, 300);
        assert_eq!(cloned.model, config.model);
        assert_eq!(cloned.instructions, config.instructions);
        assert_eq!(cloned.permissions, config.permissions);
        assert_eq!(cloned.skip_permissions, config.skip_permissions);
    }

    // ── new_simple constructor test ──────────────────────────────────

    #[test]
    fn test_new_simple_creates_working_runner() {
        let config = ClaudeRunnerConfig {
            timeout_secs: 100,
            model: Some("haiku".to_string()),
            instructions: Some("test".to_string()),
            permissions: vec!["Bash".to_string()],
            skip_permissions: true,
        };
        let runner = ClaudeRunner::new_simple(config);
        // Verify the runner works by calling methods that depend on proper initialization
        assert!(!runner.has_agent_md(Path::new("/nonexistent")));
        let issue = Issue::new("1", "T-1", "Bug", "https://example.com", "linear");
        let prompt = runner.build_prompt(&issue, "ctx", Path::new("/tmp"));
        assert!(!prompt.is_empty());
    }

    // ── RESULT_SCHEMA detailed validation ────────────────────────────

    #[test]
    fn test_result_schema_required_fields() {
        let parsed: serde_json::Value = serde_json::from_str(RESULT_SCHEMA).unwrap();
        let required = parsed["required"].as_array().unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"summary"));
        assert!(required_strs.contains(&"success"));
    }

    #[test]
    fn test_result_schema_no_additional_properties() {
        let parsed: serde_json::Value = serde_json::from_str(RESULT_SCHEMA).unwrap();
        assert_eq!(parsed["additionalProperties"], false);
    }

    #[test]
    fn test_result_schema_blocking_question_sub_schema() {
        let parsed: serde_json::Value = serde_json::from_str(RESULT_SCHEMA).unwrap();
        let bq = &parsed["properties"]["blocking_question"];
        let bq_required = bq["required"].as_array().unwrap();
        let bq_required_strs: Vec<&str> = bq_required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(bq_required_strs.contains(&"question"));
        assert!(bq["properties"]["question"].is_object());
        assert!(bq["properties"]["context"].is_object());
        assert!(bq["properties"]["options"].is_object());
        assert!(bq["properties"]["why"].is_object());
        assert_eq!(bq["additionalProperties"], false);
    }

    // ── CliContentBlock deserialization tests ─────────────────────────

    #[test]
    fn test_cli_content_block_text_deserialization() {
        let json = r#"{"type":"text","text":"hello"}"#;
        let block: CliContentBlock = serde_json::from_str(json).unwrap();
        match block {
            CliContentBlock::Text { text } => assert_eq!(text, "hello"),
            other => panic!("Expected Text, got: {:?}", other),
        }
    }

    #[test]
    fn test_cli_content_block_tool_use_deserialization() {
        let json = r#"{"type":"tool_use","id":"tool-abc","name":"Grep"}"#;
        let block: CliContentBlock = serde_json::from_str(json).unwrap();
        match block {
            CliContentBlock::ToolUse { id, name } => {
                assert_eq!(id, "tool-abc");
                assert_eq!(name, "Grep");
            }
            other => panic!("Expected ToolUse, got: {:?}", other),
        }
    }

    #[test]
    fn test_cli_content_block_other_types() {
        // "thinking", "tool_result", or any future type should map to Other
        for type_name in &["thinking", "tool_result", "image", "some_future_type"] {
            let json = format!(r#"{{"type":"{}","data":"whatever"}}"#, type_name);
            let block: CliContentBlock = serde_json::from_str(&json).unwrap();
            assert!(
                matches!(block, CliContentBlock::Other),
                "Expected Other for type: {}",
                type_name
            );
        }
    }

    #[test]
    fn test_cli_content_block_text_empty_string() {
        let json = r#"{"type":"text","text":""}"#;
        let block: CliContentBlock = serde_json::from_str(json).unwrap();
        match block {
            CliContentBlock::Text { text } => assert!(text.is_empty()),
            other => panic!("Expected Text, got: {:?}", other),
        }
    }

    #[test]
    fn test_cli_content_block_text_with_special_chars() {
        let json = r#"{"type":"text","text":"line1\nline2\ttab\"quote\\"}"#;
        let block: CliContentBlock = serde_json::from_str(json).unwrap();
        match block {
            CliContentBlock::Text { text } => {
                assert!(text.contains('\n'));
                assert!(text.contains('\t'));
                assert!(text.contains('"'));
                assert!(text.contains('\\'));
            }
            other => panic!("Expected Text, got: {:?}", other),
        }
    }

    // ── CliMessage with multiple content blocks ──────────────────────

    #[test]
    fn test_cli_message_mixed_content() {
        let json = r#"{"content":[
            {"type":"text","text":"Analyzing..."},
            {"type":"tool_use","id":"t1","name":"Bash"},
            {"type":"thinking","thinking":"hmm"},
            {"type":"text","text":"Done."}
        ]}"#;
        let msg: CliMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content.len(), 4);
        assert!(
            matches!(&msg.content[0], CliContentBlock::Text { text } if text == "Analyzing...")
        );
        assert!(matches!(&msg.content[1], CliContentBlock::ToolUse { name, .. } if name == "Bash"));
        assert!(matches!(&msg.content[2], CliContentBlock::Other));
        assert!(matches!(&msg.content[3], CliContentBlock::Text { text } if text == "Done."));
    }

    // ── StreamEvent realistic stream simulation ──────────────────────

    #[test]
    fn test_parse_realistic_ndjson_stream() {
        // Simulate a sequence of NDJSON events as Claude CLI would emit
        let lines = vec![
            r#"{"type":"system","subtype":"init","session_id":"sess-001"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I'll analyze the issue."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_1","name":"Bash"}]}}"#,
            r#"{"type":"user","message":{"role":"user"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"The fix is applied."}]}}"#,
            r#"{"type":"result","structured_output":{"summary":"Fixed the bug","success":true,"pr_url":"https://github.com/org/repo/pull/42"},"total_cost_usd":0.05,"num_turns":3,"session_id":"sess-001","duration_api_ms":15000,"usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":2000,"cache_creation_input_tokens":100}}"#,
        ];

        let mut text_output = String::new();
        let mut structured_result: Option<serde_json::Value> = None;
        let mut cost = None;
        let mut turns = None;

        for line in lines {
            let event: StreamEvent = serde_json::from_str(line).unwrap();
            match event {
                StreamEvent::Assistant { message: Some(msg) } => {
                    for block in &msg.content {
                        if let CliContentBlock::Text { text } = block {
                            text_output.push_str(text);
                        }
                    }
                }
                StreamEvent::Result {
                    structured_output,
                    total_cost_usd,
                    num_turns,
                    ..
                } => {
                    structured_result = structured_output;
                    cost = total_cost_usd;
                    turns = num_turns;
                }
                _ => {}
            }
        }

        assert_eq!(text_output, "I'll analyze the issue.The fix is applied.");
        assert!(structured_result.is_some());
        let sr: StructuredResult = serde_json::from_value(structured_result.unwrap()).unwrap();
        assert!(sr.success);
        assert_eq!(sr.summary, "Fixed the bug");
        assert_eq!(
            sr.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
        assert!((cost.unwrap() - 0.05).abs() < 1e-6);
        assert_eq!(turns, Some(3));
    }

    // ── extract_pr_url self-hosted GitLab test ───────────────────────

    #[test]
    fn test_extract_pr_url_self_hosted_gitlab_multiple_segments() {
        let output = "MR: https://git.internal.company.io/engineering/backend/-/merge_requests/55";
        let url = ClaudeRunner::extract_pr_url(output);
        assert_eq!(
            url.as_deref(),
            Some("https://git.internal.company.io/engineering/backend/-/merge_requests/55")
        );
    }

    #[test]
    fn test_extract_pr_url_does_not_match_merge_request_without_dash_slash() {
        // Ensure patterns require the /-/ separator for GitLab
        let output = "https://gitlab.com/group/project/merge_requests/123";
        let url = ClaudeRunner::extract_pr_url(output);
        assert!(url.is_none());
    }

    // ── Constant values tests ────────────────────────────────────────

    #[test]
    fn test_default_log_dir_constant() {
        assert_eq!(DEFAULT_LOG_DIR, "./logs");
    }

    #[test]
    fn test_claude_log_subdir_constant() {
        assert_eq!(CLAUDE_LOG_SUBDIR, "claude");
    }

    #[test]
    fn test_execution_log_preview_limit_constant() {
        assert_eq!(EXECUTION_LOG_PREVIEW_LIMIT, 2000);
    }

    // ── build_prompt fallback format verification ────────────────────

    #[test]
    fn test_build_prompt_fallback_contains_issue_source() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "LIN-1", "Bug", "https://ex.com", "linear");
        let prompt = runner.build_prompt(
            &issue,
            "some context",
            Path::new("/tmp/nonexistent_project_dir_xyz"),
        );
        // The fallback template should reference the source
        assert!(
            prompt.contains("linear") || prompt.contains("Linear"),
            "Prompt should contain the issue source"
        );
    }

    #[test]
    fn test_build_prompt_fallback_contains_context() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "LIN-1", "Bug", "https://ex.com", "linear");
        let prompt = runner.build_prompt(
            &issue,
            "detailed error trace here",
            Path::new("/tmp/nonexistent_project_dir_xyz"),
        );
        assert!(
            prompt.contains("detailed error trace here"),
            "Prompt should contain the provided context"
        );
    }

    #[test]
    fn test_build_prompt_fallback_contains_short_id() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "PROJ-999", "Bug", "https://ex.com", "linear");
        let prompt = runner.build_prompt(
            &issue,
            "context",
            Path::new("/tmp/nonexistent_project_dir_xyz"),
        );
        assert!(
            prompt.contains("PROJ-999"),
            "Prompt should contain the issue short_id"
        );
    }

    #[test]
    fn test_build_prompt_fallback_instructs_pr_creation() {
        let runner = ClaudeRunner::new_simple(ClaudeRunnerConfig::default());
        let issue = Issue::new("1", "T-1", "Bug", "https://ex.com", "linear");
        let prompt =
            runner.build_prompt(&issue, "ctx", Path::new("/tmp/nonexistent_project_dir_xyz"));
        // The fallback should instruct the model to create a PR
        assert!(
            prompt.to_lowercase().contains("pr") || prompt.to_lowercase().contains("pull request"),
            "Prompt should mention PR creation"
        );
    }
}
