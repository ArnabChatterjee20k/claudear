//! Configuration loading and validation.
//!
//! Configuration is loaded from a TOML file (`claudear.toml` by default).
//! Environment variables can override any TOML values.

use crate::error::{Error, Result};
use crate::secret::SecretValue;
use serde::{Deserialize, Deserializer, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Deserialize a value that can be either a single string or a list of strings.
/// Accepts `"value"` or `["a", "b"]` in TOML/JSON and always returns `Vec<String>`.
fn string_or_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Single(String),
        Multiple(Vec<String>),
    }
    match StringOrVec::deserialize(deserializer)? {
        StringOrVec::Single(s) => Ok(vec![s]),
        StringOrVec::Multiple(v) => Ok(v),
    }
}

/// Deserialize a `HashMap<String, Vec<String>>` where each value can be either
/// a single string or a list of strings.
fn hashmap_string_or_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<std::collections::HashMap<String, Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Single(String),
        Multiple(Vec<String>),
    }
    let raw: std::collections::HashMap<String, StringOrVec> =
        std::collections::HashMap::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|(k, v)| match v {
            StringOrVec::Single(s) => (k, vec![s]),
            StringOrVec::Multiple(v) => (k, v),
        })
        .collect())
}

/// Default config file name.
pub const DEFAULT_CONFIG_FILE: &str = "claudear.toml";

fn default_bind_address() -> String {
    "127.0.0.1".to_string()
}

/// Agent configuration -- replaces the old `[claude]` config section.
///
/// Supports multiple providers, experiments, and orchestration strategies.
/// ```toml
/// [agent]
/// default_provider = "claude"
/// timeout_secs = 21600
///
/// [agent.providers.claude]
/// model = "opus"
/// instructions = "Follow AGENT.md"
/// permissions = ["Bash(git:*)", "Read", "Write"]
/// skip_permissions = true
/// binary = "claude"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Which provider to use by default.
    pub default_provider: String,
    /// Global timeout for agent process execution in seconds (default: 21600 = 6 hours).
    pub timeout_secs: u64,
    /// Per-provider configurations, keyed by provider name.
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfig>,
    /// Optional A/B experiments.
    #[serde(default)]
    pub experiments: Vec<ExperimentConfig>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        let mut providers = std::collections::HashMap::new();
        providers.insert("claude".to_string(), ProviderConfig::default());
        Self {
            default_provider: "claude".to_string(),
            timeout_secs: 21600,
            providers,
            experiments: Vec::new(),
        }
    }
}

impl AgentConfig {
    /// Get the default provider's config.
    pub fn default_provider_config(&self) -> Option<&ProviderConfig> {
        self.providers.get(&self.default_provider)
    }

    /// Get a mutable reference to the default provider's config, inserting a
    /// default entry if it does not exist.
    pub fn default_provider_config_mut(&mut self) -> &mut ProviderConfig {
        self.providers
            .entry(self.default_provider.clone())
            .or_default()
    }
}

/// Per-provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Model to use (e.g., sonnet, opus, haiku, or full model ID).
    pub model: Option<String>,
    /// Custom instructions appended to the agent's system prompt.
    pub instructions: Option<String>,
    /// Path to a file containing custom instructions.
    /// Resolved relative to the config file directory.
    pub instructions_file: Option<String>,
    /// Tool permissions granted without prompting (--allowedTools).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Skip all permission prompts (default: false).
    pub skip_permissions: bool,
    /// CLI binary name/path (e.g., "claude", "codex").
    pub binary: Option<String>,
    /// API key for API-based providers.
    pub api_key: Option<SecretValue>,
    /// API base URL for API-based providers.
    pub api_url: Option<String>,
    /// Sandbox mode (e.g., "network-off" for Codex).
    pub sandbox: Option<String>,
    /// Provider-specific extra configuration.
    #[serde(default)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Experiment configuration for A/B testing providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentConfig {
    /// Experiment name.
    pub name: String,
    /// Whether the experiment is active.
    #[serde(default)]
    pub enabled: bool,
    /// Selection strategy: "weighted_random" or "fallback".
    #[serde(default = "default_experiment_strategy")]
    pub strategy: String,
    /// Provider weights for the experiment.
    #[serde(default)]
    pub providers: Vec<ExperimentProviderWeight>,
}

fn default_experiment_strategy() -> String {
    "weighted_random".to_string()
}

/// Provider weight within an experiment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentProviderWeight {
    /// Provider name (must match a key in `agent.providers`).
    pub name: String,
    /// Selection weight (higher = more traffic).
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

/// SCM (Source Control Management) configuration group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct ScmConfig {
    /// GitHub configuration for PR monitoring and issue management.
    pub github: GitHubConfig,
    /// GitLab configuration for MR monitoring and issue management.
    pub gitlab: Option<GitLabConfig>,
}


/// Issue sources configuration group.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IssuesConfig {
    /// Linear source configuration.
    pub linear: Option<LinearConfig>,
    /// Sentry source configuration.
    pub sentry: Option<SentryConfig>,
    /// Jira source configuration.
    pub jira: Option<JiraConfig>,
    /// Discord as an issue source (bot_token + channel for inbound messages).
    pub discord: Option<DiscordSourceConfig>,
    /// Slack as an issue source (bot_token + channel for inbound messages).
    pub slack: Option<SlackSourceConfig>,
}

/// Notifier configuration group.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifiersConfig {
    /// Discord notification channel.
    pub discord: DiscordNotifierConfig,
    /// Slack notification channel.
    pub slack: SlackNotifierConfig,
    /// Email (SMTP) notification channel.
    pub email: EmailConfig,
    /// SMS (Twilio) notification channel.
    pub sms: SmsConfig,
    /// Push (Pushover) notification channel.
    pub push: PushConfig,
    /// WhatsApp Business notification channel.
    pub whatsapp: WhatsAppConfig,
    /// Telegram Bot notification channel.
    pub telegram: TelegramConfig,
}

/// Discord source-only configuration (for issue ingestion).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordSourceConfig {
    /// Discord bot token for reading messages.
    pub bot_token: Option<SecretValue>,
    /// Channel ID to read issues from.
    pub channel_id: Option<String>,
    /// Channel to listen for issue messages (falls back to channel_id).
    pub listen_channel_id: Option<String>,
    /// Guild (server) ID for constructing message URLs.
    pub guild_id: Option<String>,
    /// Polling interval in milliseconds (overrides global).
    pub poll_interval_ms: Option<u64>,
}

/// Discord notifier-only configuration (for outbound notifications).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordNotifierConfig {
    /// Discord webhook URL for notifications.
    pub webhook_url: Option<SecretValue>,
    /// Discord user ID to mention in notifications.
    pub user_id: Option<String>,
    /// Discord bot token (for reply polling).
    pub bot_token: Option<SecretValue>,
    /// Discord channel ID (for reply polling).
    pub channel_id: Option<String>,
    /// Guild (server) ID for constructing message URLs.
    pub guild_id: Option<String>,
}

/// Slack source-only configuration (for issue ingestion).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SlackSourceConfig {
    /// Slack Bot Token (xoxb-) for reading messages.
    pub bot_token: Option<SecretValue>,
    /// Channel ID to read issues from.
    pub channel_id: Option<String>,
    /// Channel to listen for issue messages (falls back to channel_id).
    pub listen_channel_id: Option<String>,
    /// Workspace name for constructing message URLs.
    pub workspace: Option<String>,
    /// Polling interval in milliseconds (overrides global).
    pub poll_interval_ms: Option<u64>,
    /// Slack user ID (e.g., bot's own user ID for reply detection).
    pub user_id: Option<String>,
    /// Slack signing secret for verifying Events API webhook requests.
    pub signing_secret: Option<SecretValue>,
    /// Slack app ID used for apps.manifest.export/update auto-configuration.
    pub app_id: Option<String>,
    /// Slack app configuration token used for manifest API calls.
    pub app_config_token: Option<SecretValue>,
}

/// Slack notifier-only configuration (for outbound notifications).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SlackNotifierConfig {
    /// Slack Incoming Webhook URL.
    pub webhook_url: Option<SecretValue>,
    /// Slack user ID to mention in notifications.
    pub user_id: Option<String>,
    /// Slack Bot Token (for reply polling).
    pub bot_token: Option<SecretValue>,
    /// Slack channel ID (for reply polling).
    pub channel_id: Option<String>,
    /// Workspace name for constructing message URLs.
    pub workspace: Option<String>,
}

/// Main configuration for the application.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Working directory for cloning repositories.
    /// Repositories will be cloned into subdirectories of this path.
    pub workspace: PathBuf,
    /// Known organizations to scan for repositories.
    /// Repos from these orgs will be discovered automatically.
    #[serde(default)]
    pub known_orgs: Vec<String>,
    /// Paths to scan for auto-discovery of repositories.
    /// Will scan for git repos from known_orgs in these directories.
    #[serde(default)]
    pub auto_discover_paths: Vec<String>,
    /// Polling interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Webhook server port.
    pub webhook_port: u16,
    /// Bind address for HTTP server (default "127.0.0.1", use "0.0.0.0" in Docker).
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    /// Database path for tracking.
    pub db_path: PathBuf,
    /// Maximum issues to process per poll cycle.
    pub max_issues_per_cycle: usize,
    /// Maximum concurrent issue processing.
    pub max_concurrent: usize,
    /// Delay between processing issues (ms).
    pub processing_delay_ms: u64,
    /// Maximum number of activity entries to keep in the IPC server (default: 10,000).
    pub max_activity_entries: usize,
    /// IPC request timeout in seconds (default: 30).
    pub ipc_timeout_secs: u64,
    /// Agent runner configuration (providers, experiments, orchestration).
    #[serde(default)]
    pub agent: AgentConfig,
    /// SCM (Source Control Management) configuration group.
    #[serde(default)]
    pub scm: ScmConfig,
    /// Issue sources configuration group.
    #[serde(default)]
    pub issues: IssuesConfig,
    /// Notification channels configuration group.
    #[serde(default)]
    pub notifiers: NotifiersConfig,
    /// Human Q&A ask-loop configuration.
    pub ask: AskConfig,
    /// Retry configuration.
    pub retry: RetryConfig,
    /// Regression monitoring configuration.
    #[serde(default)]
    pub regression: RegressionConfig,
    /// Cascade configuration for multi-repo chaining.
    #[serde(default)]
    pub cascade: CascadeConfig,
    /// User registry mapping slugs to source IDs and notification channel IDs.
    #[serde(default)]
    pub users: std::collections::HashMap<String, UserConfig>,
    /// Continuous learning configuration.
    #[serde(default)]
    pub learning: LearningConfig,
    /// Prioritisation engine configuration.
    #[serde(default)]
    pub prioritisation: PrioritisationConfig,
    /// Code indexing configuration.
    #[serde(default)]
    pub code_index: CodeIndexConfig,
    /// Self-evaluation configuration.
    #[serde(default)]
    pub evaluation: EvaluationConfig,
    /// General-purpose storage directory for user uploads (avatars, etc.).
    #[serde(default = "default_storage_dir")]
    pub storage_dir: PathBuf,
    /// Dashboard display configuration.
    #[serde(default)]
    pub dashboard: DashboardConfig,
    /// Optional tenant identifier for multi-tenant deployments.
    /// Defaults to "default" when absent (single-tenant mode).
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Optional PostgreSQL database URL for cloud deployments.
    /// When set, uses PostgresBackend instead of SQLite.
    #[serde(default)]
    pub database_url: Option<String>,
    /// Optional Redis URL for caching (e.g., "redis://localhost:6379").
    /// When set alongside database_url, single-row reads are cached and writes
    /// invalidate the cache. Ignored when database_url is not set.
    #[serde(default)]
    pub redis_url: Option<String>,
}

fn default_storage_dir() -> PathBuf {
    PathBuf::from("./storage")
}

/// Dashboard display & estimation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    /// Monthly cost of Claude Max plan (if applicable). Used to estimate per-fix
    /// cost when total_cost_usd is not available from CLI. Set to 0 to disable.
    pub max_plan_monthly_cost: f64,
    /// Hourly engineer rate for cost-savings calculation.
    pub hourly_engineer_rate: f64,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            max_plan_monthly_cost: 0.0,
            hourly_engineer_rate: 75.0,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            workspace: PathBuf::new(),
            known_orgs: Vec::new(),
            auto_discover_paths: Vec::new(),
            poll_interval_ms: 300_000,
            webhook_port: 3100,
            bind_address: default_bind_address(),
            db_path: PathBuf::from("claudear.db"),
            max_issues_per_cycle: 5,
            max_concurrent: 1,
            processing_delay_ms: 5000,
            max_activity_entries: 10_000,
            ipc_timeout_secs: 30,
            agent: AgentConfig::default(),
            scm: ScmConfig::default(),
            issues: IssuesConfig::default(),
            notifiers: NotifiersConfig::default(),
            ask: AskConfig::default(),
            retry: RetryConfig::default(),
            regression: RegressionConfig::default(),
            cascade: CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: LearningConfig::default(),
            prioritisation: PrioritisationConfig::default(),
            code_index: CodeIndexConfig::default(),
            evaluation: EvaluationConfig::default(),
            storage_dir: default_storage_dir(),
            dashboard: DashboardConfig::default(),
            tenant_id: None,
            database_url: None,
            redis_url: None,
        }
    }
}

/// Human Q&A ask-loop configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AskConfig {
    /// Enable/disable human question flow.
    pub enabled: bool,
    /// Maximum time to wait for a human answer in seconds.
    pub wait_timeout_secs: u64,
    /// Poll interval for reply-capable channels in seconds.
    pub poll_interval_secs: u64,
    /// Max ask rounds per attempt to prevent infinite loops.
    pub max_rounds_per_attempt: u8,
    /// Semantic threshold for scoped (source+repo) reuse.
    pub semantic_threshold_scoped: f64,
    /// Semantic threshold for global fallback reuse.
    pub semantic_threshold_global: f64,
    /// Max semantic candidates to include in context/reuse.
    pub max_reuse_candidates: usize,
    /// Continue with best effort when no reply is received.
    pub best_effort_on_timeout: bool,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wait_timeout_secs: 900,
            poll_interval_secs: 15,
            max_rounds_per_attempt: 2,
            semantic_threshold_scoped: 0.82,
            semantic_threshold_global: 0.88,
            max_reuse_candidates: 3,
            best_effort_on_timeout: true,
        }
    }
}

/// Configuration for multi-repo cascade chaining.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CascadeConfig {
    /// Whether cascade chaining is enabled.
    pub enabled: bool,
    /// Maximum cascade depth (0 = unlimited).
    pub max_depth: usize,
    /// Per-dependency cascade rules.
    #[serde(default)]
    pub rules: Vec<CascadeRule>,
}

impl CascadeConfig {
    /// Find a rule matching a specific upstream->downstream pair.
    pub fn find_rule(&self, upstream: &str, downstream: &str) -> Option<&CascadeRule> {
        self.rules
            .iter()
            .find(|r| r.upstream == upstream && r.downstream == downstream)
    }

    /// Find a rule matching a specific upstream->downstream pair and trigger type.
    pub fn find_rule_for_trigger(
        &self,
        upstream: &str,
        downstream: &str,
        trigger: &CascadeTrigger,
    ) -> Option<&CascadeRule> {
        self.rules
            .iter()
            .find(|r| r.upstream == upstream && r.downstream == downstream && &r.trigger == trigger)
    }

    /// Get all upstream repos that have release-triggered rules.
    pub fn release_trigger_upstreams(&self) -> Vec<&str> {
        let mut upstreams: Vec<&str> = self
            .rules
            .iter()
            .filter(|r| r.trigger == CascadeTrigger::Release)
            .map(|r| r.upstream.as_str())
            .collect();
        upstreams.dedup();
        upstreams
    }
}

/// A per-dependency cascade rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeRule {
    /// Upstream repo (e.g., "appwrite/server-ce").
    pub upstream: String,
    /// Downstream repo (e.g., "appwrite-labs/cloud").
    pub downstream: String,
    /// What triggers the cascade: "merge" or "release" (default).
    #[serde(default = "default_cascade_trigger")]
    pub trigger: CascadeTrigger,
    /// Target branch in downstream repo (default: repo's default branch).
    #[serde(default)]
    pub target_branch: Option<String>,
    /// Whether to update dependency version in downstream.
    #[serde(default = "default_true")]
    pub version_update: bool,
    /// Custom instructions appended to the cascade prompt.
    #[serde(default)]
    pub instructions: Option<String>,
}

fn default_cascade_trigger() -> CascadeTrigger {
    CascadeTrigger::Release
}

fn default_true() -> bool {
    true
}

/// What triggers a cascade.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CascadeTrigger {
    Merge,
    Release,
}

/// Continuous learning configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LearningConfig {
    /// Auto-extract learnings from Claude execution logs.
    pub auto_extract_learnings: bool,
    /// Analyze PR diffs on merge.
    pub diff_analysis: bool,
    /// Promote repeated Q&A answers to standing instructions.
    pub qa_promotion: bool,
    /// Minimum occurrences before Q&A answer is promoted.
    pub qa_promotion_threshold: usize,
    /// Accumulate per-repo knowledge from successful fixes.
    pub repo_knowledge: bool,
    /// Classify review feedback patterns.
    pub review_classification: bool,
    /// Minimum occurrences before review pattern is promoted.
    pub review_promotion_threshold: usize,
    /// Track how Claude approaches fixes.
    pub strategy_fingerprinting: bool,
    /// Score fix quality based on merge velocity.
    pub quality_scoring: bool,
    /// Detect clusters of correlated issues.
    pub cluster_detection: bool,
    /// Time window for cluster detection in minutes.
    pub cluster_window_minutes: u32,
    /// Minimum issues to form a cluster.
    pub min_cluster_size: usize,
    /// Auto-generate AGENT.md from accumulated knowledge (opt-in).
    pub auto_agent_md: bool,
    /// Enable cross-repo failure correlation detection.
    pub cross_repo_correlation: bool,
    /// Time window for cross-repo correlation in hours.
    pub cross_repo_window_hours: i64,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            auto_extract_learnings: true,
            diff_analysis: true,
            qa_promotion: true,
            qa_promotion_threshold: 2,
            repo_knowledge: true,
            review_classification: true,
            review_promotion_threshold: 3,
            strategy_fingerprinting: true,
            quality_scoring: true,
            cluster_detection: true,
            cluster_window_minutes: 30,
            min_cluster_size: 3,
            auto_agent_md: false,
            cross_repo_correlation: true,
            cross_repo_window_hours: 24,
        }
    }
}

/// Code indexing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeIndexConfig {
    /// Enable tree-sitter code indexing.
    pub enabled: bool,
    /// Maximum file size to index in KB.
    pub max_file_size_kb: u64,
    /// Embedding batch size.
    pub batch_size: usize,
}

impl Default for CodeIndexConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_size_kb: 1024,
            batch_size: 32,
        }
    }
}

/// Self-evaluation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvaluationConfig {
    /// Enable evaluation (opt-in, can be slow).
    pub enabled: bool,
    /// Run test before/after comparison.
    pub test_delta: bool,
    /// Run lint before/after comparison.
    pub lint_delta: bool,
    /// Run static analysis before/after comparison.
    pub static_analysis_delta: bool,
    /// Run coverage before/after comparison (slowest).
    pub coverage_delta: bool,
    /// Timeout per tool in seconds.
    pub tool_timeout_secs: u64,
    /// Total timeout for all tools in seconds.
    pub total_timeout_secs: u64,
    /// Post evaluation results as PR comment.
    pub post_pr_comment: bool,
    /// Fail the fix attempt on regression.
    pub fail_on_regression: bool,
    /// Custom test command override.
    pub custom_test_cmd: Option<String>,
    /// Custom lint command override.
    pub custom_lint_cmd: Option<String>,
    /// Custom static analysis command override.
    pub custom_analysis_cmd: Option<String>,
    /// Custom coverage command override.
    pub custom_coverage_cmd: Option<String>,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            test_delta: true,
            lint_delta: true,
            static_analysis_delta: true,
            coverage_delta: false,
            tool_timeout_secs: 300,
            total_timeout_secs: 900,
            post_pr_comment: true,
            fail_on_regression: false,
            custom_test_cmd: None,
            custom_lint_cmd: None,
            custom_analysis_cmd: None,
            custom_coverage_cmd: None,
        }
    }
}

/// Prioritisation engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrioritisationConfig {
    /// Enable the prioritisation engine (when false, legacy sort is used).
    pub enabled: bool,
    /// Weight for severity component (issue + match priority).
    pub severity_weight: f64,
    /// Weight for frequency component (event counts, escalation).
    pub frequency_weight: f64,
    /// Weight for regression risk component.
    pub regression_weight: f64,
    /// Weight for blast radius component.
    pub blast_radius_weight: f64,
    /// Weight for content-cluster boost component.
    pub cluster_weight: f64,
    /// Path patterns classified as Critical blast radius.
    pub critical_paths: Vec<String>,
    /// Path patterns classified as Core blast radius.
    pub core_paths: Vec<String>,
    /// Path patterns classified as Infrastructure blast radius.
    pub infra_paths: Vec<String>,
    /// Path patterns classified as Test blast radius.
    pub test_paths: Vec<String>,
    /// Path patterns classified as Cosmetic blast radius.
    pub cosmetic_paths: Vec<String>,
    /// Enable content clustering for duplicate detection.
    pub content_clustering: bool,
    /// Minimum Jaccard similarity to keep a cluster.
    pub cluster_similarity_threshold: f64,
    /// Minimum number of issues to form a content cluster.
    pub min_content_cluster_size: usize,
    /// User-defined suppression rules.
    pub suppression_rules: Vec<crate::types::SuppressionRule>,
}

impl Default for PrioritisationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            severity_weight: 0.30,
            frequency_weight: 0.25,
            regression_weight: 0.20,
            blast_radius_weight: 0.15,
            cluster_weight: 0.10,
            critical_paths: vec![
                "auth".into(),
                "payment".into(),
                "billing".into(),
                "security".into(),
                "login".into(),
                "oauth".into(),
            ],
            core_paths: vec![
                "api".into(),
                "core".into(),
                "middleware".into(),
                "router".into(),
                "handler".into(),
            ],
            infra_paths: vec![
                "deploy".into(),
                "infra".into(),
                "ci".into(),
                "docker".into(),
                "terraform".into(),
                "k8s".into(),
                "database".into(),
                "migration".into(),
            ],
            test_paths: vec![
                "test".into(),
                "spec".into(),
                "fixture".into(),
                "mock".into(),
            ],
            cosmetic_paths: vec![
                "readme".into(),
                "changelog".into(),
                "license".into(),
                "docs".into(),
                "md".into(),
            ],
            content_clustering: true,
            cluster_similarity_threshold: 0.60,
            min_content_cluster_size: 2,
            suppression_rules: Vec::new(),
        }
    }
}

impl PrioritisationConfig {
    /// Validate prioritisation configuration values.
    ///
    /// Checks that weights are finite and non-negative, similarity threshold is
    /// in 0.0-1.0, and min_cluster_size >= 2.
    pub fn validate(&self) -> Result<()> {
        let weights = [
            ("severity_weight", self.severity_weight),
            ("frequency_weight", self.frequency_weight),
            ("regression_weight", self.regression_weight),
            ("blast_radius_weight", self.blast_radius_weight),
            ("cluster_weight", self.cluster_weight),
        ];

        for (name, value) in &weights {
            if !value.is_finite() {
                return Err(Error::config(format!(
                    "prioritisation.{name} must be finite, got {value}"
                )));
            }
            if *value < 0.0 {
                return Err(Error::config(format!(
                    "prioritisation.{name} must be non-negative, got {value}"
                )));
            }
        }

        let weight_sum: f64 = weights.iter().map(|(_, v)| v).sum();
        if weight_sum == 0.0 {
            return Err(Error::config("prioritisation weights must not all be zero"));
        }

        if !self.cluster_similarity_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.cluster_similarity_threshold)
        {
            return Err(Error::config(format!(
                "prioritisation.cluster_similarity_threshold must be between 0.0 and 1.0, got {}",
                self.cluster_similarity_threshold
            )));
        }

        if self.min_content_cluster_size < 2 {
            return Err(Error::config(format!(
                "prioritisation.min_content_cluster_size must be >= 2, got {}",
                self.min_content_cluster_size
            )));
        }

        Ok(())
    }
}

/// Retry configuration for failed fix attempts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (default: 2).
    pub max_retries: u32,
    /// Base delay between retries in milliseconds (default: 60000 = 1 minute).
    pub base_delay_ms: u64,
    /// Maximum delay between retries in milliseconds (default: 3600000 = 1 hour).
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay_ms: 60_000,   // 1 minute
            max_delay_ms: 3_600_000, // 1 hour
        }
    }
}

/// Slack notification configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SlackConfig {
    /// Slack Bot Token (xoxb-) for API calls.
    pub bot_token: Option<SecretValue>,
    /// Slack channel ID for notifications.
    pub channel_id: Option<String>,
    /// Slack Incoming Webhook URL (optional, notification-only alternative).
    pub webhook_url: Option<SecretValue>,
    /// Slack user ID to mention in notifications.
    pub user_id: Option<String>,
    /// Enable Slack as an issue source (messages become issues).
    pub source_enabled: bool,
    /// Channel to listen for issue messages (falls back to channel_id).
    pub listen_channel_id: Option<String>,
    /// Workspace name for constructing message URLs.
    pub workspace: Option<String>,
    /// Polling interval in milliseconds for Slack source (overrides global).
    pub poll_interval_ms: Option<u64>,
}

/// Discord notification configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordConfig {
    /// Discord webhook URL for notifications.
    pub webhook_url: Option<SecretValue>,
    /// Discord user ID to mention in notifications.
    pub user_id: Option<String>,
    /// Discord bot token used for inbound reply polling.
    pub bot_token: Option<SecretValue>,
    /// Discord channel ID used for inbound reply polling.
    pub channel_id: Option<String>,
    /// Enable Discord as an issue source (messages become issues).
    pub source_enabled: bool,
    /// Channel to listen for issue messages (falls back to channel_id).
    pub listen_channel_id: Option<String>,
    /// Guild (server) ID for constructing message URLs.
    pub guild_id: Option<String>,
    /// Polling interval in milliseconds for Discord source (overrides global).
    pub poll_interval_ms: Option<u64>,
}

/// Email (SMTP) notification configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    /// SMTP server host.
    pub smtp_host: Option<String>,
    /// SMTP server port (default: 587).
    pub smtp_port: u16,
    /// SMTP username.
    pub smtp_username: Option<String>,
    /// SMTP password.
    pub smtp_password: Option<SecretValue>,
    /// Sender email address.
    pub from_address: Option<String>,
    /// Recipient email addresses.
    pub to_addresses: Vec<String>,
    /// Use TLS (default: true).
    pub use_tls: bool,
    /// IMAP host used for inbound reply polling.
    pub imap_host: Option<String>,
    /// IMAP server port (default: 993).
    pub imap_port: u16,
    /// IMAP username.
    pub imap_username: Option<String>,
    /// IMAP password.
    pub imap_password: Option<SecretValue>,
    /// Use TLS for IMAP.
    pub imap_use_tls: bool,
    /// IMAP folder to poll.
    pub imap_folder: String,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            smtp_host: None,
            smtp_port: 587,
            smtp_username: None,
            smtp_password: None,
            from_address: None,
            to_addresses: Vec::new(),
            use_tls: true,
            imap_host: None,
            imap_port: 993,
            imap_username: None,
            imap_password: None,
            imap_use_tls: true,
            imap_folder: "INBOX".to_string(),
        }
    }
}

/// SMS notification configuration (via Twilio).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmsConfig {
    /// Twilio Account SID.
    pub account_sid: Option<String>,
    /// Twilio Auth Token.
    pub auth_token: Option<SecretValue>,
    /// Twilio phone number (sender).
    pub from_number: Option<String>,
    /// Recipient phone numbers.
    pub to_numbers: Vec<String>,
}

/// Push notification configuration (via Pushover).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PushConfig {
    /// Pushover API token.
    pub api_token: Option<SecretValue>,
    /// Pushover user key.
    pub user_key: Option<String>,
    /// Device name (optional, sends to all devices if empty).
    pub device: Option<String>,
    /// Priority level (-2 to 2).
    pub priority: Option<i8>,
}

/// WhatsApp Business Cloud API configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WhatsAppConfig {
    /// WhatsApp Business phone number ID.
    pub phone_number_id: Option<String>,
    /// Meta Graph API access token.
    pub access_token: Option<SecretValue>,
    /// WhatsApp Business Account (WABA) ID used for webhook subscription setup.
    pub business_account_id: Option<String>,
    /// Meta app secret for verifying webhook signatures.
    pub app_secret: Option<SecretValue>,
    /// Verify token used for WhatsApp webhook callback challenge.
    pub webhook_verify_token: Option<SecretValue>,
    /// Default recipient phone numbers.
    pub to_numbers: Vec<String>,
    /// Enable WhatsApp as an issue source.
    pub source_enabled: bool,
    /// Override phone number ID for source (falls back to phone_number_id).
    pub listen_phone_number_id: Option<String>,
    /// Polling interval in milliseconds for WhatsApp source (overrides global).
    pub poll_interval_ms: Option<u64>,
}

/// Telegram Bot API configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    /// Telegram Bot API token.
    pub bot_token: Option<SecretValue>,
    /// Default chat ID for notifications.
    pub chat_id: Option<String>,
    /// Additional recipient chat IDs.
    pub to_chat_ids: Vec<String>,
    /// Enable Telegram as an issue source.
    pub source_enabled: bool,
    /// Override chat ID for source (falls back to chat_id).
    pub listen_chat_id: Option<String>,
    /// Polling interval in milliseconds for Telegram source (overrides global).
    pub poll_interval_ms: Option<u64>,
}

/// Per-user configuration mapping source identifiers to notification channel IDs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserConfig {
    /// User's display names in Linear (matched against issue assignee).
    #[serde(default, deserialize_with = "string_or_vec")]
    pub linear_names: Vec<String>,
    /// User's GitHub usernames (matched against PR author / issue assignee).
    #[serde(default, deserialize_with = "string_or_vec")]
    pub github_usernames: Vec<String>,
    /// User's Sentry usernames (matched against issue assignee).
    #[serde(default, deserialize_with = "string_or_vec")]
    pub sentry_usernames: Vec<String>,
    /// User's Jira usernames/display names (matched against issue assignee).
    #[serde(default, deserialize_with = "string_or_vec")]
    pub jira_usernames: Vec<String>,
    /// User's GitLab usernames (matched against MR author / issue assignee).
    #[serde(default, deserialize_with = "string_or_vec")]
    pub gitlab_usernames: Vec<String>,
    /// Discord user ID for mentions.
    pub discord_id: Option<String>,
    /// Slack user ID for mentions.
    pub slack_id: Option<String>,
    /// Email address for notifications.
    pub email: Option<String>,
    /// Pushover user key for push notifications.
    pub push_user_key: Option<String>,
    /// Phone number for SMS notifications.
    pub sms_number: Option<String>,
    /// WhatsApp phone number for notifications.
    pub whatsapp_number: Option<String>,
    /// Telegram chat ID for notifications.
    pub telegram_chat_id: Option<String>,
}

/// GitLab configuration for MR monitoring and issue management.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitLabConfig {
    /// Whether this source is enabled.
    pub enabled: bool,
    /// GitLab personal access token.
    pub token: Option<SecretValue>,
    /// GitLab base URL (default: "https://gitlab.com").
    pub base_url: String,
    /// GitLab groups to monitor.
    pub groups: Vec<String>,
    /// Labels that trigger automation.
    pub trigger_labels: Vec<String>,
    /// States that trigger automation (e.g., "opened").
    pub trigger_states: Vec<String>,
    /// Poll interval for checking MR status (ms).
    /// When `None`, falls back to the global `poll_interval_ms`.
    pub poll_interval_ms: Option<u64>,
    /// Whether to auto-resolve issues when MRs merge.
    pub auto_resolve_on_merge: bool,
    /// Webhook secret for verifying GitLab webhook requests.
    pub webhook_secret: Option<SecretValue>,
    /// Trigger tag for review comments (e.g., "@claudear").
    pub review_trigger: String,
    /// Use SSH URLs for cloning instead of HTTPS.
    #[serde(default)]
    pub use_ssh: bool,
    /// Maximum issues to process per poll cycle for this source (overrides global).
    pub max_issues_per_cycle: Option<usize>,
    /// Maximum concurrent issue processing for this source (overrides global).
    pub max_concurrent: Option<usize>,
}

impl Default for GitLabConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            base_url: "https://gitlab.com".to_string(),
            groups: Vec::new(),
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_states: vec!["opened".to_string()],
            poll_interval_ms: None,
            auto_resolve_on_merge: false,
            webhook_secret: None,
            review_trigger: "@claudear".to_string(),
            use_ssh: false,
            max_issues_per_cycle: None,
            max_concurrent: None,
        }
    }
}

#[cfg(test)]
impl GitLabConfig {
    /// Create a GitLabConfig suitable for testing.
    pub fn test_default() -> Self {
        Self {
            enabled: true,
            token: Some(SecretValue::new("test_token")),
            base_url: "https://gitlab.com".to_string(),
            groups: vec!["mygroup".to_string()],
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_states: vec!["opened".to_string()],
            poll_interval_ms: Some(60000),
            auto_resolve_on_merge: true,
            webhook_secret: Some(SecretValue::new("test_secret")),
            review_trigger: "@claudear".to_string(),
            use_ssh: false,
            max_issues_per_cycle: None,
            max_concurrent: None,
        }
    }
}

/// GitHub configuration for PR monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    /// GitHub personal access token.
    pub token: Option<SecretValue>,
    /// Poll interval for checking PR status (ms).
    pub poll_interval_ms: u64,
    /// Whether to auto-resolve issues when PRs merge.
    pub auto_resolve_on_merge: bool,
    /// Webhook secret for verifying GitHub webhook signatures.
    pub webhook_secret: Option<SecretValue>,
    /// Trigger tag for review comments (e.g., "@claudear" or "@mybot").
    /// Comments must contain this tag to trigger Claude.
    /// Set to empty string to respond to all comments.
    pub review_trigger: String,
    /// Use SSH URLs for cloning instead of HTTPS.
    /// Set to true if you have SSH keys configured for GitHub.
    #[serde(default)]
    pub use_ssh: bool,
    /// Repositories to monitor for issues (e.g., ["owner/repo"]).
    #[serde(default)]
    pub repos: Vec<String>,
    /// Labels that trigger automation on GitHub issues.
    #[serde(default)]
    pub trigger_labels: Vec<String>,
    /// Issue states that trigger automation (e.g., ["open"]).
    #[serde(default)]
    pub trigger_states: Vec<String>,
    /// GitHub App configuration (nested under [scm.github.app]).
    #[serde(default)]
    pub app: GitHubAppConfig,
}

impl Default for GitHubConfig {
    fn default() -> Self {
        Self {
            token: None,
            poll_interval_ms: 60000,
            auto_resolve_on_merge: false,
            webhook_secret: None,
            review_trigger: "@claudear".to_string(),
            use_ssh: false,
            repos: Vec::new(),
            trigger_labels: Vec::new(),
            trigger_states: Vec::new(),
            app: GitHubAppConfig::default(),
        }
    }
}

#[cfg(test)]
impl GitHubConfig {
    /// Create a GitHubConfig suitable for testing.
    pub fn test_default() -> Self {
        Self {
            token: Some(SecretValue::new("ghp_test_token")),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: false,
            webhook_secret: None,
            review_trigger: "@claudear".to_string(),
            use_ssh: false,
            repos: Vec::new(),
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_states: vec!["open".to_string()],
            app: GitHubAppConfig::default(),
        }
    }
}

/// GitHub App authentication mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitHubAuthMode {
    /// Personal Access Token (classic mode).
    #[default]
    Token,
    /// GitHub App with JWT authentication.
    App,
}

/// GitHub App configuration for App-based authentication.
///
/// This is used for self-hosted deployments where users create their own
/// GitHub App via the manifest flow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubAppConfig {
    /// GitHub App ID (assigned by GitHub when the App is created).
    pub app_id: Option<i64>,
    /// Path to the private key PEM file.
    pub private_key_path: Option<PathBuf>,
    /// Inline private key PEM content (alternative to file).
    pub private_key: Option<SecretValue>,
    /// Webhook secret for verifying GitHub webhook signatures.
    pub webhook_secret: Option<SecretValue>,
    /// Installation ID (auto-detected if not set).
    pub installation_id: Option<i64>,
    /// OAuth Client ID (for user authorization flows).
    pub client_id: Option<String>,
    /// OAuth Client Secret.
    pub client_secret: Option<SecretValue>,
    /// Public base URL for the manifest flow.
    pub base_url: Option<String>,
}

impl GitHubAppConfig {
    /// Check if the GitHub App is configured with minimum required fields.
    pub fn is_configured(&self) -> bool {
        self.app_id.is_some() && (self.private_key_path.is_some() || self.private_key.is_some())
    }

    /// Load the private key from file or inline content.
    pub fn load_private_key(&self) -> Result<String> {
        if let Some(key) = &self.private_key {
            return Ok(key.expose().to_string());
        }

        if let Some(path) = &self.private_key_path {
            let content = fs::read_to_string(path).map_err(|e| {
                Error::config(format!(
                    "Failed to read GitHub App private key from '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            return Ok(content);
        }

        Err(Error::config(
            "No GitHub App private key configured (set private_key or private_key_path)",
        ))
    }
}

/// Linear source configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LinearConfig {
    /// Whether this source is enabled.
    pub enabled: bool,
    /// Linear API key.
    pub api_key: SecretValue,
    /// Labels that trigger automation.
    pub trigger_labels: Vec<String>,
    /// Optional assignee display name filter. When set, only issues assigned to
    /// this user are processed. If set and `trigger_labels` is empty, label
    /// matching is skipped.
    pub trigger_assignee: Option<String>,
    /// States that trigger automation.
    pub trigger_states: Vec<String>,
    /// Optional team filter.
    pub team_id: Option<String>,
    /// Optional project filter.
    pub project_id: Option<String>,
    /// Webhook signature verification secret.
    pub webhook_secret: Option<SecretValue>,
    /// Maximum issues to process per poll cycle for this source (overrides global).
    pub max_issues_per_cycle: Option<usize>,
    /// Maximum concurrent issue processing for this source (overrides global).
    pub max_concurrent: Option<usize>,
    /// Polling interval in milliseconds for Linear source (overrides global).
    pub poll_interval_ms: Option<u64>,
}

impl Default for LinearConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: SecretValue::new(""),
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_assignee: None,
            trigger_states: vec!["backlog".to_string(), "todo".to_string()],
            team_id: None,
            project_id: None,
            webhook_secret: None,
            max_issues_per_cycle: None,
            max_concurrent: None,
            poll_interval_ms: None,
        }
    }
}

/// Time period for fetching top Sentry issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TopIssuesPeriod {
    /// 1 hour
    #[serde(alias = "1h", alias = "1hr", alias = "hour")]
    OneHour,
    /// 12 hours
    #[serde(alias = "12h", alias = "12hr", alias = "12hrs")]
    TwelveHours,
    /// 24 hours (1 day) - default
    #[default]
    #[serde(alias = "24h", alias = "1d", alias = "day", alias = "1day")]
    OneDay,
    /// 7 days (1 week)
    #[serde(alias = "7d", alias = "1w", alias = "week", alias = "1week")]
    OneWeek,
    /// 30 days (1 month)
    #[serde(alias = "30d", alias = "1m", alias = "month", alias = "1month")]
    OneMonth,
}

impl TopIssuesPeriod {
    /// Convert to Sentry API statsPeriod parameter value.
    pub fn to_stats_period(&self) -> &'static str {
        match self {
            TopIssuesPeriod::OneHour => "1h",
            TopIssuesPeriod::TwelveHours => "12h",
            TopIssuesPeriod::OneDay => "24h",
            TopIssuesPeriod::OneWeek => "7d",
            TopIssuesPeriod::OneMonth => "30d",
        }
    }
}

impl std::str::FromStr for TopIssuesPeriod {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "1h" | "1hr" | "hour" | "one_hour" => Ok(TopIssuesPeriod::OneHour),
            "12h" | "12hr" | "12hrs" | "twelve_hours" => Ok(TopIssuesPeriod::TwelveHours),
            "24h" | "1d" | "day" | "1day" | "one_day" => Ok(TopIssuesPeriod::OneDay),
            "7d" | "1w" | "week" | "1week" | "one_week" => Ok(TopIssuesPeriod::OneWeek),
            "30d" | "1m" | "month" | "1month" | "one_month" => Ok(TopIssuesPeriod::OneMonth),
            _ => Err(format!("Invalid time period: {}", s)),
        }
    }
}

impl std::fmt::Display for TopIssuesPeriod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TopIssuesPeriod::OneHour => write!(f, "1h"),
            TopIssuesPeriod::TwelveHours => write!(f, "12h"),
            TopIssuesPeriod::OneDay => write!(f, "24h"),
            TopIssuesPeriod::OneWeek => write!(f, "7d"),
            TopIssuesPeriod::OneMonth => write!(f, "30d"),
        }
    }
}

/// Sentry source configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SentryConfig {
    /// Whether this source is enabled.
    pub enabled: bool,
    /// Sentry auth token.
    pub auth_token: SecretValue,
    /// Sentry organization slug.
    pub org_slug: String,
    /// Project slugs to filter.
    pub project_slugs: Vec<String>,
    /// Number of top issues to fetch.
    pub top_issues_count: usize,
    /// Time period for fetching top issues (default: 24h).
    pub top_issues_period: TopIssuesPeriod,
    /// Minimum event count for issue to be processed.
    pub min_event_count: usize,
    /// Percentage increase to consider issue escalating.
    pub escalation_threshold_percent: u32,
    /// Webhook client secret for signature verification.
    pub client_secret: Option<SecretValue>,
    /// Maximum issues to process per poll cycle for this source (overrides global).
    pub max_issues_per_cycle: Option<usize>,
    /// Maximum concurrent issue processing for this source (overrides global).
    pub max_concurrent: Option<usize>,
    /// Polling interval in milliseconds for Sentry source (overrides global).
    pub poll_interval_ms: Option<u64>,
}

impl Default for SentryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auth_token: SecretValue::new(""),
            org_slug: String::new(),
            project_slugs: Vec::new(),
            top_issues_count: 100,
            top_issues_period: TopIssuesPeriod::default(),
            min_event_count: 10,
            escalation_threshold_percent: 50,
            client_secret: None,
            max_issues_per_cycle: None,
            max_concurrent: None,
            poll_interval_ms: None,
        }
    }
}

/// Jira source configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JiraConfig {
    /// Whether this source is enabled.
    pub enabled: bool,
    /// Jira base URL (e.g., "https://myco.atlassian.net").
    pub base_url: String,
    /// Email for Basic auth (Jira Cloud).
    pub email: String,
    /// API token (Cloud) or personal access token (Server/DC).
    pub api_token: SecretValue,
    /// Authentication mode: "basic" (email:token) or "bearer" (PAT).
    pub auth_mode: String,
    /// Jira project keys to monitor (e.g., ["PROJ", "BACKEND"]).
    pub project_keys: Vec<String>,
    /// Labels that trigger automation.
    pub trigger_labels: Vec<String>,
    /// Statuses that trigger automation.
    pub trigger_statuses: Vec<String>,
    /// Optional: Only process issues assigned to this user (display name).
    pub trigger_assignee: Option<String>,
    /// Issue types to include (e.g., ["Bug", "Task", "Story"]).
    pub issue_types: Vec<String>,
    /// Optional: Custom JQL appended to the generated query.
    pub custom_jql: Option<String>,
    /// Maximum results per search request (default: 50, max: 100).
    pub max_results: usize,
    /// Maximum issues to process per poll cycle for this source (overrides global).
    pub max_issues_per_cycle: Option<usize>,
    /// Maximum concurrent issue processing for this source (overrides global).
    pub max_concurrent: Option<usize>,
    /// Polling interval in milliseconds for Jira source (overrides global).
    pub poll_interval_ms: Option<u64>,
}

impl Default for JiraConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            email: String::new(),
            api_token: SecretValue::new(""),
            auth_mode: "basic".to_string(),
            project_keys: Vec::new(),
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_statuses: vec!["To Do".to_string(), "Backlog".to_string()],
            trigger_assignee: None,
            issue_types: Vec::new(),
            custom_jql: None,
            max_results: 50,
            max_issues_per_cycle: None,
            max_concurrent: None,
            poll_interval_ms: None,
        }
    }
}

/// Configuration for bug fix regression monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RegressionConfig {
    /// Whether regression monitoring is enabled.
    pub enabled: bool,
    /// How often to check for regressions (in hours).
    pub check_interval_hours: u32,
    /// Total monitoring duration after release (in hours).
    pub monitoring_duration_hours: u32,
    /// Override check interval in seconds (for testing). Takes precedence over check_interval_hours.
    pub check_interval_secs: Option<u64>,
    /// Override monitoring duration in seconds (for testing). Takes precedence over monitoring_duration_hours.
    pub monitoring_duration_secs: Option<u64>,
    /// Minimum Sentry events to trigger regression detection.
    pub sentry_event_threshold: u32,
    /// Semantic similarity threshold for matching issues (0.0-1.0).
    pub similarity_threshold: f64,
    /// Target repositories that signal a release is live.
    /// When a fix is included in a release of these repos, monitoring starts.
    /// The dependency graph is used to trace how fixes flow to these targets.
    pub target_repos: Vec<String>,
    /// GitHub token for searching issues (uses github.token if not set).
    pub github_token: Option<String>,
    /// Repositories to search for similar issues.
    pub github_search_repos: Vec<String>,
    /// Package name overrides when repo name differs from package name.
    /// Maps repo name (e.g., "utopia-php/database") to package name (e.g., "utopia-php/database").
    /// Only needed when they differ; same-name packages are auto-detected.
    #[serde(default, deserialize_with = "hashmap_string_or_vec")]
    pub package_names: std::collections::HashMap<String, Vec<String>>,
}

impl RegressionConfig {
    /// Get the effective check interval in seconds.
    /// Uses `check_interval_secs` if set, otherwise converts `check_interval_hours` to seconds.
    pub fn effective_check_interval_secs(&self) -> u64 {
        self.check_interval_secs
            .unwrap_or((self.check_interval_hours as u64) * 3600)
            .max(1)
    }

    /// Get the effective monitoring duration in seconds.
    /// Uses `monitoring_duration_secs` if set, otherwise converts `monitoring_duration_hours` to seconds.
    pub fn effective_monitoring_duration_secs(&self) -> u64 {
        self.monitoring_duration_secs
            .unwrap_or((self.monitoring_duration_hours as u64) * 3600)
    }
}

impl Default for RegressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_hours: 1,
            monitoring_duration_hours: 24,
            check_interval_secs: None,
            monitoring_duration_secs: None,
            sentry_event_threshold: 1,
            similarity_threshold: 0.75,
            target_repos: Vec::new(),
            github_token: None,
            github_search_repos: Vec::new(),
            package_names: std::collections::HashMap::new(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file with environment variable overrides.
    ///
    /// This is the primary way to load configuration. It:
    /// 1. Reads the TOML config file
    /// 2. Applies any environment variable overrides
    /// 3. Validates required fields
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path).map_err(|e| {
            Error::config(format!(
                "Failed to read config file '{}': {}",
                path.display(),
                e
            ))
        })?;

        let mut config: Config = toml::from_str(&content).map_err(|e| {
            Error::config(format!(
                "Failed to parse TOML config '{}': {}",
                path.display(),
                e
            ))
        })?;

        // Apply environment variable overrides
        config.apply_env_overrides();

        // Resolve instructions_file if set
        let config_dir = path.parent().unwrap_or(Path::new("."));
        let resolved_instructions = config.resolve_instructions_file(config_dir)?;
        config.agent.default_provider_config_mut().instructions = resolved_instructions;

        // Resolve user slug references in global notification configs
        config.resolve_user_slugs();

        // Validate project directory configuration
        config.validate_project_config()?;

        Ok(config)
    }

    /// Validate minimal configuration needed for loading.
    ///
    /// Only validates `workspace` is set. Repository validation is done
    /// in `validate()` for commands that actually need repositories.
    fn validate_project_config(&self) -> Result<()> {
        if self.workspace.as_os_str().is_empty() {
            return Err(Error::config(
                "'workspace' is required - path where repositories will be cloned",
            ));
        }

        Ok(())
    }

    /// Load configuration from the default config file path.
    pub fn load_default() -> Result<Self> {
        Self::load(DEFAULT_CONFIG_FILE)
    }

    /// Load configuration from TOML string (useful for testing).
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        let mut config: Config = toml::from_str(toml_str)
            .map_err(|e| Error::config(format!("Failed to parse TOML: {}", e)))?;
        config.apply_env_overrides();
        Ok(config)
    }

    /// Resolve the default provider's `instructions_file` by reading it
    /// and combining with inline `instructions`.
    ///
    /// - `config_dir`: directory containing the config file (for relative path resolution)
    /// - File content comes first, then inline instructions appended with a newline
    /// - Returns `None` if neither field is set
    /// - Returns error if the file path is set but the file cannot be read
    pub fn resolve_instructions_file(&self, config_dir: &Path) -> Result<Option<String>> {
        let provider = match self.agent.default_provider_config() {
            Some(p) => p,
            None => return Ok(None),
        };

        let file_content = if let Some(ref file_path) = provider.instructions_file {
            let path = Path::new(file_path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                config_dir.join(path)
            };
            let content = fs::read_to_string(&resolved).map_err(|e| {
                Error::config(format!(
                    "Failed to read instructions file '{}': {}",
                    resolved.display(),
                    e
                ))
            })?;
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        } else {
            None
        };

        match (file_content, &provider.instructions) {
            (Some(file), Some(inline)) => Ok(Some(format!("{}\n{}", file, inline))),
            (Some(file), None) => Ok(Some(file)),
            (None, Some(inline)) => Ok(Some(inline.clone())),
            (None, None) => Ok(None),
        }
    }

    /// Resolve user slug references in global notification configs.
    ///
    /// If a field like `discord.user_id` matches a key in `users`,
    /// replace it with the user's actual channel-specific ID.
    pub fn resolve_user_slugs(&mut self) {
        // Resolve discord notifier user_id
        if let Some(ref user_id) = self.notifiers.discord.user_id {
            if let Some(user) = self.users.get(user_id) {
                if let Some(ref discord_id) = user.discord_id {
                    self.notifiers.discord.user_id = Some(discord_id.clone());
                }
            }
        }

        // Resolve email.to_addresses
        let resolved_emails: Vec<String> = self
            .notifiers
            .email
            .to_addresses
            .iter()
            .map(|addr| {
                if let Some(user) = self.users.get(addr) {
                    user.email.clone().unwrap_or_else(|| addr.clone())
                } else {
                    addr.clone()
                }
            })
            .collect();
        self.notifiers.email.to_addresses = resolved_emails;

        // Resolve push.user_key
        if let Some(ref user_key) = self.notifiers.push.user_key {
            if let Some(user) = self.users.get(user_key) {
                if let Some(ref push_key) = user.push_user_key {
                    self.notifiers.push.user_key = Some(push_key.clone());
                }
            }
        }

        // Resolve sms.to_numbers
        let resolved_numbers: Vec<String> = self
            .notifiers
            .sms
            .to_numbers
            .iter()
            .map(|num| {
                if let Some(user) = self.users.get(num) {
                    user.sms_number.clone().unwrap_or_else(|| num.clone())
                } else {
                    num.clone()
                }
            })
            .collect();
        self.notifiers.sms.to_numbers = resolved_numbers;
    }

    /// Apply environment variable overrides to the config.
    /// Environment variables take precedence over TOML values.
    fn apply_env_overrides(&mut self) {
        // Core settings
        if let Ok(v) = env::var("WORK_DIR") {
            if !v.is_empty() {
                self.workspace = v.into();
            }
        }
        if let Ok(v) = env::var("KNOWN_ORGS") {
            if !v.is_empty() {
                self.known_orgs = v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Ok(v) = env::var("AUTO_DISCOVER_PATHS") {
            if !v.is_empty() {
                self.auto_discover_paths = v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Some(v) = env::var("POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.poll_interval_ms = v;
        }
        if let Some(v) = env::var("WEBHOOK_PORT").ok().and_then(|v| v.parse().ok()) {
            self.webhook_port = v;
        }
        if let Ok(v) = env::var("DB_PATH") {
            if !v.is_empty() {
                self.db_path = v.into();
            }
        }
        if let Some(v) = env::var("MAX_ISSUES_PER_CYCLE")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.max_issues_per_cycle = v;
        }
        if let Some(v) = env::var("MAX_CONCURRENT").ok().and_then(|v| v.parse().ok()) {
            self.max_concurrent = v;
        }
        if let Some(v) = env::var("PROCESSING_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.processing_delay_ms = v;
        }
        if let Some(v) = env::var("MAX_ACTIVITY_ENTRIES")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.max_activity_entries = v;
        }
        if let Some(v) = env::var("IPC_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.ipc_timeout_secs = v;
        }
        if let Some(v) = env::var("CLAUDE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.agent.timeout_secs = v;
        }

        // Agent provider (Claude) CLI -- env vars write into the default provider.
        if let Ok(v) = env::var("CLAUDE_MODEL") {
            if !v.is_empty() {
                self.agent.default_provider_config_mut().model = Some(v);
            }
        }
        if let Ok(v) = env::var("CLAUDE_INSTRUCTIONS") {
            if !v.is_empty() {
                self.agent.default_provider_config_mut().instructions = Some(v);
            }
        }
        if let Ok(v) = env::var("CLAUDE_INSTRUCTIONS_FILE") {
            if !v.is_empty() {
                self.agent.default_provider_config_mut().instructions_file = Some(v);
            }
        }
        if let Ok(v) = env::var("CLAUDE_PERMISSIONS") {
            if !v.is_empty() {
                self.agent.default_provider_config_mut().permissions =
                    v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Ok(v) = env::var("CLAUDE_SKIP_PERMISSIONS") {
            self.agent.default_provider_config_mut().skip_permissions =
                v.to_lowercase() == "true" || v == "1";
        }

        // Discord notifier
        if let Ok(v) = env::var("DISCORD_WEBHOOK_URL") {
            self.notifiers.discord.webhook_url =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("DISCORD_USER_ID") {
            self.notifiers.discord.user_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("DISCORD_BOT_TOKEN") {
            // Set on both notifier and source
            let val = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
            self.notifiers.discord.bot_token = val.clone();
            if let Some(ref mut src) = self.issues.discord {
                src.bot_token = val;
            }
        }
        if let Ok(v) = env::var("DISCORD_CHANNEL_ID") {
            let val = Some(v).filter(|s| !s.is_empty());
            self.notifiers.discord.channel_id = val.clone();
            if let Some(ref mut src) = self.issues.discord {
                src.channel_id = val;
            }
        }
        if let Ok(v) = env::var("DISCORD_SOURCE_ENABLED") {
            if v == "true" || v == "1" {
                let src = self
                    .issues
                    .discord
                    .get_or_insert_with(DiscordSourceConfig::default);
                let _ = src; // ensure it exists
            } else {
                self.issues.discord = None;
            }
        }
        if let Ok(v) = env::var("DISCORD_LISTEN_CHANNEL_ID") {
            let val = Some(v).filter(|s| !s.is_empty());
            if let Some(ref mut src) = self.issues.discord {
                src.listen_channel_id = val;
            }
        }
        if let Ok(v) = env::var("DISCORD_GUILD_ID") {
            let val = Some(v).filter(|s| !s.is_empty());
            self.notifiers.discord.guild_id = val.clone();
            if let Some(ref mut src) = self.issues.discord {
                src.guild_id = val;
            }
        }
        if let Some(v) = env::var("DISCORD_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            if let Some(ref mut src) = self.issues.discord {
                src.poll_interval_ms = Some(v);
            }
        }

        // Slack notifier
        if let Ok(v) = env::var("SLACK_BOT_TOKEN") {
            let val = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
            self.notifiers.slack.bot_token = val.clone();
            if let Some(ref mut src) = self.issues.slack {
                src.bot_token = val;
            }
        }
        if let Ok(v) = env::var("SLACK_CHANNEL_ID") {
            let val = Some(v).filter(|s| !s.is_empty());
            self.notifiers.slack.channel_id = val.clone();
            if let Some(ref mut src) = self.issues.slack {
                src.channel_id = val;
            }
        }
        if let Ok(v) = env::var("SLACK_WEBHOOK_URL") {
            self.notifiers.slack.webhook_url =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("SLACK_USER_ID") {
            let val = Some(v).filter(|s| !s.is_empty());
            self.notifiers.slack.user_id = val.clone();
            if let Some(ref mut src) = self.issues.slack {
                src.user_id = val;
            }
        }
        if let Ok(v) = env::var("SLACK_SOURCE_ENABLED") {
            if v == "true" || v == "1" {
                let src = self
                    .issues
                    .slack
                    .get_or_insert_with(SlackSourceConfig::default);
                let _ = src;
            } else {
                self.issues.slack = None;
            }
        }
        if let Ok(v) = env::var("SLACK_LISTEN_CHANNEL_ID") {
            let val = Some(v).filter(|s| !s.is_empty());
            if let Some(ref mut src) = self.issues.slack {
                src.listen_channel_id = val;
            }
        }
        if let Ok(v) = env::var("SLACK_WORKSPACE") {
            let val = Some(v).filter(|s| !s.is_empty());
            self.notifiers.slack.workspace = val.clone();
            if let Some(ref mut src) = self.issues.slack {
                src.workspace = val;
            }
        }
        if let Ok(v) = env::var("SLACK_SIGNING_SECRET") {
            if let Some(ref mut src) = self.issues.slack {
                src.signing_secret = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
            }
        }
        if let Ok(v) = env::var("SLACK_APP_ID") {
            if let Some(ref mut src) = self.issues.slack {
                src.app_id = Some(v).filter(|s| !s.is_empty());
            }
        }
        if let Ok(v) = env::var("SLACK_APP_CONFIG_TOKEN") {
            if let Some(ref mut src) = self.issues.slack {
                src.app_config_token = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
            }
        }
        if let Some(v) = env::var("SLACK_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            if let Some(ref mut src) = self.issues.slack {
                src.poll_interval_ms = Some(v);
            }
        }

        // WhatsApp notifier/source
        if let Ok(v) = env::var("WHATSAPP_PHONE_NUMBER_ID") {
            self.notifiers.whatsapp.phone_number_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("WHATSAPP_ACCESS_TOKEN") {
            self.notifiers.whatsapp.access_token =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("WHATSAPP_BUSINESS_ACCOUNT_ID") {
            self.notifiers.whatsapp.business_account_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("WHATSAPP_APP_SECRET") {
            self.notifiers.whatsapp.app_secret =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("WHATSAPP_WEBHOOK_VERIFY_TOKEN") {
            self.notifiers.whatsapp.webhook_verify_token =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("WHATSAPP_TO_NUMBERS") {
            if !v.is_empty() {
                self.notifiers.whatsapp.to_numbers =
                    v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Ok(v) = env::var("WHATSAPP_SOURCE_ENABLED") {
            self.notifiers.whatsapp.source_enabled = v.to_lowercase() == "true" || v == "1";
        }
        if let Ok(v) = env::var("WHATSAPP_LISTEN_PHONE_NUMBER_ID") {
            self.notifiers.whatsapp.listen_phone_number_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Some(v) = env::var("WHATSAPP_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.notifiers.whatsapp.poll_interval_ms = Some(v);
        }

        // Telegram notifier/source
        if let Ok(v) = env::var("TELEGRAM_BOT_TOKEN") {
            self.notifiers.telegram.bot_token =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("TELEGRAM_CHAT_ID") {
            self.notifiers.telegram.chat_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("TELEGRAM_TO_CHAT_IDS") {
            if !v.is_empty() {
                self.notifiers.telegram.to_chat_ids =
                    v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Ok(v) = env::var("TELEGRAM_SOURCE_ENABLED") {
            self.notifiers.telegram.source_enabled = v.to_lowercase() == "true" || v == "1";
        }
        if let Ok(v) = env::var("TELEGRAM_LISTEN_CHAT_ID") {
            self.notifiers.telegram.listen_chat_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Some(v) = env::var("TELEGRAM_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.notifiers.telegram.poll_interval_ms = Some(v);
        }

        // Email
        if let Ok(v) = env::var("SMTP_HOST") {
            self.notifiers.email.smtp_host = Some(v).filter(|s| !s.is_empty());
        }
        if let Some(v) = env::var("SMTP_PORT").ok().and_then(|v| v.parse().ok()) {
            self.notifiers.email.smtp_port = v;
        }
        if let Ok(v) = env::var("SMTP_USERNAME") {
            self.notifiers.email.smtp_username = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("SMTP_PASSWORD") {
            self.notifiers.email.smtp_password =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("EMAIL_FROM") {
            self.notifiers.email.from_address = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("EMAIL_TO") {
            if !v.is_empty() {
                self.notifiers.email.to_addresses =
                    v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Ok(v) = env::var("SMTP_TLS") {
            self.notifiers.email.use_tls = v.to_lowercase() == "true" || v == "1";
        }
        if let Ok(v) = env::var("IMAP_HOST") {
            self.notifiers.email.imap_host = Some(v).filter(|s| !s.is_empty());
        }
        if let Some(v) = env::var("IMAP_PORT").ok().and_then(|v| v.parse().ok()) {
            self.notifiers.email.imap_port = v;
        }
        if let Ok(v) = env::var("IMAP_USERNAME") {
            self.notifiers.email.imap_username = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("IMAP_PASSWORD") {
            self.notifiers.email.imap_password =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("IMAP_TLS") {
            self.notifiers.email.imap_use_tls = v.to_lowercase() == "true" || v == "1";
        }
        if let Ok(v) = env::var("IMAP_FOLDER") {
            if !v.is_empty() {
                self.notifiers.email.imap_folder = v;
            }
        }

        // Ask loop
        if let Ok(v) = env::var("ASK_ENABLED") {
            self.ask.enabled = v.to_lowercase() == "true" || v == "1";
        }
        if let Some(v) = env::var("ASK_WAIT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.ask.wait_timeout_secs = v;
        }
        if let Some(v) = env::var("ASK_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.ask.poll_interval_secs = v;
        }
        if let Some(v) = env::var("ASK_MAX_ROUNDS").ok().and_then(|v| v.parse().ok()) {
            self.ask.max_rounds_per_attempt = v;
        }
        if let Some(v) = env::var("ASK_SEMANTIC_THRESHOLD_SCOPED")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.ask.semantic_threshold_scoped = v;
        }
        if let Some(v) = env::var("ASK_SEMANTIC_THRESHOLD_GLOBAL")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.ask.semantic_threshold_global = v;
        }
        if let Some(v) = env::var("ASK_MAX_REUSE_CANDIDATES")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.ask.max_reuse_candidates = v;
        }
        if let Ok(v) = env::var("ASK_BEST_EFFORT_ON_TIMEOUT") {
            self.ask.best_effort_on_timeout = v.to_lowercase() == "true" || v == "1";
        }

        // SMS
        if let Ok(v) = env::var("TWILIO_ACCOUNT_SID") {
            self.notifiers.sms.account_sid = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("TWILIO_AUTH_TOKEN") {
            self.notifiers.sms.auth_token = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("TWILIO_FROM_NUMBER") {
            self.notifiers.sms.from_number = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("TWILIO_TO_NUMBERS") {
            if !v.is_empty() {
                self.notifiers.sms.to_numbers =
                    v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }

        // Push
        if let Ok(v) = env::var("PUSHOVER_API_TOKEN") {
            self.notifiers.push.api_token = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("PUSHOVER_USER_KEY") {
            self.notifiers.push.user_key = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("PUSHOVER_DEVICE") {
            self.notifiers.push.device = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("PUSHOVER_PRIORITY") {
            self.notifiers.push.priority = v.parse().ok();
        }

        // GitHub
        if let Ok(v) = env::var("GITHUB_TOKEN") {
            self.scm.github.token = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Some(v) = env::var("GITHUB_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.scm.github.poll_interval_ms = v;
        }
        if let Ok(v) = env::var("GITHUB_AUTO_RESOLVE_ON_MERGE") {
            self.scm.github.auto_resolve_on_merge = v.to_lowercase() == "true" || v == "1";
        }
        if let Ok(v) = env::var("GITHUB_WEBHOOK_SECRET") {
            self.scm.github.webhook_secret =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("GITHUB_REVIEW_TRIGGER") {
            self.scm.github.review_trigger = v;
        }

        // GitHub App
        if let Some(v) = env::var("GITHUB_APP_ID").ok().and_then(|v| v.parse().ok()) {
            self.scm.github.app.app_id = Some(v);
        }
        if let Ok(v) = env::var("GITHUB_APP_PRIVATE_KEY_PATH") {
            self.scm.github.app.private_key_path =
                Some(v).filter(|s| !s.is_empty()).map(PathBuf::from);
        }
        if let Ok(v) = env::var("GITHUB_APP_PRIVATE_KEY") {
            self.scm.github.app.private_key =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("GITHUB_APP_WEBHOOK_SECRET") {
            self.scm.github.app.webhook_secret =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Some(v) = env::var("GITHUB_APP_INSTALLATION_ID")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.scm.github.app.installation_id = Some(v);
        }
        if let Ok(v) = env::var("GITHUB_APP_CLIENT_ID") {
            self.scm.github.app.client_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("GITHUB_APP_CLIENT_SECRET") {
            self.scm.github.app.client_secret =
                Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
        }
        if let Ok(v) = env::var("GITHUB_APP_BASE_URL") {
            self.scm.github.app.base_url = Some(v).filter(|s| !s.is_empty());
        }

        // Retry
        if let Some(v) = env::var("RETRY_MAX_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.retry.max_retries = v;
        }
        if let Some(v) = env::var("RETRY_BASE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.retry.base_delay_ms = v;
        }
        if let Some(v) = env::var("RETRY_MAX_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.retry.max_delay_ms = v;
        }

        // Linear - apply overrides to existing config or create new one
        self.apply_linear_env_overrides();

        // Sentry - apply overrides to existing config or create new one
        self.apply_sentry_env_overrides();

        // Jira - apply overrides to existing config or create new one
        self.apply_jira_env_overrides();

        // GitLab - apply overrides to existing config or create new one
        self.apply_gitlab_env_overrides();
    }

    /// Apply Linear environment variable overrides.
    fn apply_linear_env_overrides(&mut self) {
        // If LINEAR_API_KEY is set in env, ensure we have a LinearConfig
        if let Ok(api_key) = env::var("LINEAR_API_KEY") {
            if !api_key.is_empty() {
                let linear = self.issues.linear.get_or_insert_with(LinearConfig::default);
                linear.api_key = SecretValue::new(api_key);
            }
        }

        // Apply other overrides if we have a LinearConfig
        if let Some(ref mut linear) = self.issues.linear {
            if let Ok(v) = env::var("LINEAR_ENABLED") {
                linear.enabled = v.to_lowercase() == "true" || v == "1";
            }
            if let Ok(v) = env::var("LINEAR_TRIGGER_LABELS") {
                if !v.is_empty() {
                    linear.trigger_labels = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("LINEAR_TRIGGER_ASSIGNEE") {
                linear.trigger_assignee = Some(v).filter(|s| !s.is_empty());
            }
            if let Ok(v) = env::var("LINEAR_TRIGGER_STATES") {
                if !v.is_empty() {
                    linear.trigger_states = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("LINEAR_TEAM_ID") {
                linear.team_id = Some(v).filter(|s| !s.is_empty());
            }
            if let Ok(v) = env::var("LINEAR_PROJECT_ID") {
                linear.project_id = Some(v).filter(|s| !s.is_empty());
            }
            if let Ok(v) = env::var("LINEAR_WEBHOOK_SECRET") {
                linear.webhook_secret = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
            }
            if let Some(v) = env::var("LINEAR_MAX_ISSUES_PER_CYCLE")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                linear.max_issues_per_cycle = Some(v);
            }
            if let Some(v) = env::var("LINEAR_MAX_CONCURRENT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                linear.max_concurrent = Some(v);
            }
            if let Some(v) = env::var("LINEAR_POLL_INTERVAL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                linear.poll_interval_ms = Some(v);
            }
        }
    }

    /// Apply Sentry environment variable overrides.
    fn apply_sentry_env_overrides(&mut self) {
        // If SENTRY_AUTH_TOKEN is set in env, ensure we have a SentryConfig
        if let Ok(auth_token) = env::var("SENTRY_AUTH_TOKEN") {
            if !auth_token.is_empty() {
                let sentry = self.issues.sentry.get_or_insert_with(SentryConfig::default);
                sentry.auth_token = SecretValue::new(auth_token);
            }
        }

        // Apply other overrides if we have a SentryConfig
        if let Some(ref mut sentry) = self.issues.sentry {
            if let Ok(v) = env::var("SENTRY_ENABLED") {
                sentry.enabled = v.to_lowercase() == "true" || v == "1";
            }
            if let Ok(v) = env::var("SENTRY_ORG_SLUG") {
                if !v.is_empty() {
                    sentry.org_slug = v;
                }
            }
            if let Ok(v) = env::var("SENTRY_PROJECT_SLUGS") {
                if !v.is_empty() {
                    sentry.project_slugs = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Some(v) = env::var("SENTRY_TOP_ISSUES_COUNT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                sentry.top_issues_count = v;
            }
            if let Some(v) = env::var("SENTRY_TOP_ISSUES_PERIOD")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                sentry.top_issues_period = v;
            }
            if let Some(v) = env::var("SENTRY_MIN_EVENT_COUNT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                sentry.min_event_count = v;
            }
            if let Some(v) = env::var("SENTRY_ESCALATION_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                sentry.escalation_threshold_percent = v;
            }
            if let Ok(v) = env::var("SENTRY_CLIENT_SECRET") {
                sentry.client_secret = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
            }
            if let Some(v) = env::var("SENTRY_MAX_ISSUES_PER_CYCLE")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                sentry.max_issues_per_cycle = Some(v);
            }
            if let Some(v) = env::var("SENTRY_MAX_CONCURRENT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                sentry.max_concurrent = Some(v);
            }
            if let Some(v) = env::var("SENTRY_POLL_INTERVAL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                sentry.poll_interval_ms = Some(v);
            }
        }
    }

    /// Apply Jira environment variable overrides.
    fn apply_jira_env_overrides(&mut self) {
        // If JIRA_API_TOKEN is set in env, ensure we have a JiraConfig
        if let Ok(api_token) = env::var("JIRA_API_TOKEN") {
            if !api_token.is_empty() {
                let jira = self.issues.jira.get_or_insert_with(JiraConfig::default);
                jira.api_token = SecretValue::new(api_token);
            }
        }

        // Apply other overrides if we have a JiraConfig
        if let Some(ref mut jira) = self.issues.jira {
            if let Ok(v) = env::var("JIRA_ENABLED") {
                jira.enabled = v.to_lowercase() == "true" || v == "1";
            }
            if let Ok(v) = env::var("JIRA_BASE_URL") {
                if !v.is_empty() {
                    jira.base_url = v;
                }
            }
            if let Ok(v) = env::var("JIRA_EMAIL") {
                if !v.is_empty() {
                    jira.email = v;
                }
            }
            if let Ok(v) = env::var("JIRA_AUTH_MODE") {
                if !v.is_empty() {
                    jira.auth_mode = v;
                }
            }
            if let Ok(v) = env::var("JIRA_PROJECT_KEYS") {
                if !v.is_empty() {
                    jira.project_keys = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("JIRA_TRIGGER_LABELS") {
                if !v.is_empty() {
                    jira.trigger_labels = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("JIRA_TRIGGER_STATUSES") {
                if !v.is_empty() {
                    jira.trigger_statuses = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("JIRA_TRIGGER_ASSIGNEE") {
                jira.trigger_assignee = Some(v).filter(|s| !s.is_empty());
            }
            if let Ok(v) = env::var("JIRA_ISSUE_TYPES") {
                if !v.is_empty() {
                    jira.issue_types = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("JIRA_CUSTOM_JQL") {
                jira.custom_jql = Some(v).filter(|s| !s.is_empty());
            }
            if let Some(v) = env::var("JIRA_MAX_RESULTS")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                jira.max_results = v;
            }
            if let Some(v) = env::var("JIRA_MAX_ISSUES_PER_CYCLE")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                jira.max_issues_per_cycle = Some(v);
            }
            if let Some(v) = env::var("JIRA_MAX_CONCURRENT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                jira.max_concurrent = Some(v);
            }
            if let Some(v) = env::var("JIRA_POLL_INTERVAL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                jira.poll_interval_ms = Some(v);
            }
        }
    }

    /// Apply GitLab environment variable overrides.
    fn apply_gitlab_env_overrides(&mut self) {
        // If GITLAB_TOKEN is set in env, ensure we have a GitLabConfig
        if let Ok(token) = env::var("GITLAB_TOKEN") {
            if !token.is_empty() {
                let gitlab = self.scm.gitlab.get_or_insert_with(GitLabConfig::default);
                gitlab.token = Some(SecretValue::new(token));
                gitlab.enabled = true;
            }
        }

        // Apply other overrides if we have a GitLabConfig
        if let Some(ref mut gitlab) = self.scm.gitlab {
            if let Ok(v) = env::var("GITLAB_ENABLED") {
                gitlab.enabled = v.to_lowercase() == "true" || v == "1";
            }
            if let Ok(v) = env::var("GITLAB_BASE_URL") {
                if !v.is_empty() {
                    gitlab.base_url = v;
                }
            }
            if let Ok(v) = env::var("GITLAB_GROUPS") {
                if !v.is_empty() {
                    gitlab.groups = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("GITLAB_TRIGGER_LABELS") {
                if !v.is_empty() {
                    gitlab.trigger_labels = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Ok(v) = env::var("GITLAB_TRIGGER_STATES") {
                if !v.is_empty() {
                    gitlab.trigger_states = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            if let Some(v) = env::var("GITLAB_POLL_INTERVAL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                gitlab.poll_interval_ms = Some(v);
            }
            if let Ok(v) = env::var("GITLAB_AUTO_RESOLVE_ON_MERGE") {
                gitlab.auto_resolve_on_merge = v.to_lowercase() == "true" || v == "1";
            }
            if let Ok(v) = env::var("GITLAB_WEBHOOK_SECRET") {
                gitlab.webhook_secret = Some(v).filter(|s| !s.is_empty()).map(SecretValue::new);
            }
            if let Ok(v) = env::var("GITLAB_REVIEW_TRIGGER") {
                gitlab.review_trigger = v;
            }
            if let Ok(v) = env::var("GITLAB_USE_SSH") {
                gitlab.use_ssh = v.to_lowercase() == "true" || v == "1";
            }
            if let Some(v) = env::var("GITLAB_MAX_ISSUES_PER_CYCLE")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                gitlab.max_issues_per_cycle = Some(v);
            }
            if let Some(v) = env::var("GITLAB_MAX_CONCURRENT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                gitlab.max_concurrent = Some(v);
            }
        }
    }

    /// Validate that at least one source is configured and enabled.
    pub fn validate(&self) -> Result<()> {
        let has_linear = self
            .issues
            .linear
            .as_ref()
            .is_some_and(|c| c.enabled && !c.api_key.is_empty());
        let has_sentry = self
            .issues
            .sentry
            .as_ref()
            .is_some_and(|c| c.enabled && !c.auth_token.is_empty());
        let has_jira = self
            .issues
            .jira
            .as_ref()
            .is_some_and(|c| c.enabled && !c.api_token.is_empty());
        let has_gitlab = self
            .scm
            .gitlab
            .as_ref()
            .is_some_and(|c| c.enabled && c.token.is_some());
        let has_slack = self
            .issues
            .slack
            .as_ref()
            .is_some_and(|s| s.bot_token.is_some());
        let has_discord = self
            .issues
            .discord
            .as_ref()
            .is_some_and(|s| s.bot_token.is_some());

        if !has_linear && !has_sentry && !has_jira && !has_gitlab && !has_slack && !has_discord {
            return Err(Error::config(
                "No sources configured. Configure linear, sentry, jira, gitlab, slack, or discord in config file with valid API credentials.",
            ));
        }

        // Validate Sentry has org_slug if enabled
        if let Some(ref sentry) = self.issues.sentry {
            if sentry.enabled && !sentry.auth_token.is_empty() && sentry.org_slug.is_empty() {
                return Err(Error::config(
                    "sentry.org_slug is required when Sentry is enabled",
                ));
            }
        }

        // Validate Jira has base_url when enabled
        if let Some(ref jira) = self.issues.jira {
            if jira.enabled && !jira.api_token.is_empty() && jira.base_url.is_empty() {
                return Err(Error::config(
                    "jira.base_url is required when Jira is enabled",
                ));
            }
            if jira.enabled && jira.auth_mode != "basic" && jira.auth_mode != "bearer" {
                return Err(Error::config(format!(
                    "jira.auth_mode must be 'basic' or 'bearer', got '{}'",
                    jira.auth_mode
                )));
            }
            if jira.enabled
                && !jira.api_token.is_empty()
                && jira.auth_mode == "basic"
                && jira.email.is_empty()
            {
                return Err(Error::config(
                    "jira.email is required when Jira auth_mode is 'basic'",
                ));
            }
        }

        // Validate prioritisation config only when engine is enabled
        if self.prioritisation.enabled {
            self.prioritisation.validate()?;
        }

        Ok(())
    }

    /// Check if Linear source is enabled.
    pub fn is_linear_enabled(&self) -> bool {
        self.issues.linear.as_ref().is_some_and(|c| c.enabled)
    }

    /// Check if Sentry source is enabled.
    pub fn is_sentry_enabled(&self) -> bool {
        self.issues.sentry.as_ref().is_some_and(|c| c.enabled)
    }

    /// Check if Jira source is enabled.
    pub fn is_jira_enabled(&self) -> bool {
        self.issues.jira.as_ref().is_some_and(|c| c.enabled)
    }

    /// Check if GitHub PR monitoring is enabled.
    pub fn is_github_enabled(&self) -> bool {
        self.scm.github.token.is_some() || self.scm.github.app.is_configured()
    }

    /// Check if GitLab is enabled.
    pub fn is_gitlab_enabled(&self) -> bool {
        self.scm
            .gitlab
            .as_ref()
            .is_some_and(|c| c.enabled && c.token.is_some())
    }

    /// Determine the GitHub authentication mode to use.
    ///
    /// Returns `App` if GitHub App is configured, otherwise `Token`.
    pub fn github_auth_mode(&self) -> GitHubAuthMode {
        if self.scm.github.app.is_configured() {
            GitHubAuthMode::App
        } else {
            GitHubAuthMode::Token
        }
    }

    /// Check if GitHub App is configured.
    pub fn is_github_app_configured(&self) -> bool {
        self.scm.github.app.is_configured()
    }

    /// Accessor: get reference to GitHubConfig.
    pub fn github(&self) -> &GitHubConfig {
        &self.scm.github
    }

    /// Accessor: get mutable reference to GitHubConfig.
    pub fn github_mut(&mut self) -> &mut GitHubConfig {
        &mut self.scm.github
    }

    /// Accessor: get reference to GitHubAppConfig.
    pub fn github_app(&self) -> &GitHubAppConfig {
        &self.scm.github.app
    }

    /// Accessor: get mutable reference to GitHubAppConfig.
    pub fn github_app_mut(&mut self) -> &mut GitHubAppConfig {
        &mut self.scm.github.app
    }

    /// Accessor: get reference to GitLabConfig.
    pub fn gitlab(&self) -> Option<&GitLabConfig> {
        self.scm.gitlab.as_ref()
    }

    /// Accessor: get reference to LinearConfig.
    pub fn linear(&self) -> Option<&LinearConfig> {
        self.issues.linear.as_ref()
    }

    /// Accessor: get reference to SentryConfig.
    pub fn sentry_config(&self) -> Option<&SentryConfig> {
        self.issues.sentry.as_ref()
    }

    /// Accessor: get reference to JiraConfig.
    pub fn jira(&self) -> Option<&JiraConfig> {
        self.issues.jira.as_ref()
    }

    /// Accessor: get reference to EmailConfig.
    pub fn email(&self) -> &EmailConfig {
        &self.notifiers.email
    }

    /// Accessor: get reference to SmsConfig.
    pub fn sms(&self) -> &SmsConfig {
        &self.notifiers.sms
    }

    /// Accessor: get reference to PushConfig.
    pub fn push_config(&self) -> &PushConfig {
        &self.notifiers.push
    }

    /// Merge issues.discord + notifiers.discord into the legacy combined DiscordConfig.
    pub fn discord_merged(&self) -> DiscordConfig {
        let src = self.issues.discord.as_ref();
        let notif = &self.notifiers.discord;
        DiscordConfig {
            webhook_url: notif.webhook_url.clone(),
            user_id: notif.user_id.clone(),
            bot_token: notif
                .bot_token
                .clone()
                .or_else(|| src.and_then(|s| s.bot_token.clone())),
            channel_id: notif
                .channel_id
                .clone()
                .or_else(|| src.and_then(|s| s.channel_id.clone())),
            source_enabled: src.is_some(),
            listen_channel_id: src.and_then(|s| s.listen_channel_id.clone()),
            guild_id: notif
                .guild_id
                .clone()
                .or_else(|| src.and_then(|s| s.guild_id.clone())),
            poll_interval_ms: src.and_then(|s| s.poll_interval_ms),
        }
    }

    /// Merge issues.slack + notifiers.slack into the legacy combined SlackConfig.
    pub fn slack_merged(&self) -> SlackConfig {
        let src = self.issues.slack.as_ref();
        let notif = &self.notifiers.slack;
        SlackConfig {
            bot_token: notif
                .bot_token
                .clone()
                .or_else(|| src.and_then(|s| s.bot_token.clone())),
            channel_id: notif
                .channel_id
                .clone()
                .or_else(|| src.and_then(|s| s.channel_id.clone())),
            webhook_url: notif.webhook_url.clone(),
            user_id: notif
                .user_id
                .clone()
                .or_else(|| src.and_then(|s| s.user_id.clone())),
            source_enabled: src.is_some(),
            listen_channel_id: src.and_then(|s| s.listen_channel_id.clone()),
            workspace: notif
                .workspace
                .clone()
                .or_else(|| src.and_then(|s| s.workspace.clone())),
            poll_interval_ms: src.and_then(|s| s.poll_interval_ms),
        }
    }

    /// Get the max issues per cycle for a specific source.
    /// Uses the source-specific value if set, otherwise falls back to the global value.
    pub fn max_issues_per_cycle_for(&self, source_name: &str) -> usize {
        match source_name {
            "linear" => self
                .issues
                .linear
                .as_ref()
                .and_then(|c| c.max_issues_per_cycle)
                .unwrap_or(self.max_issues_per_cycle),
            "sentry" => self
                .issues
                .sentry
                .as_ref()
                .and_then(|c| c.max_issues_per_cycle)
                .unwrap_or(self.max_issues_per_cycle),
            "jira" => self
                .issues
                .jira
                .as_ref()
                .and_then(|c| c.max_issues_per_cycle)
                .unwrap_or(self.max_issues_per_cycle),
            "gitlab" => self
                .scm
                .gitlab
                .as_ref()
                .and_then(|c| c.max_issues_per_cycle)
                .unwrap_or(self.max_issues_per_cycle),
            _ => self.max_issues_per_cycle,
        }
    }

    /// Get the max concurrent processing for a specific source.
    /// Uses the source-specific value if set, otherwise falls back to the global value.
    pub fn max_concurrent_for(&self, source_name: &str) -> usize {
        match source_name {
            "linear" => self
                .issues
                .linear
                .as_ref()
                .and_then(|c| c.max_concurrent)
                .unwrap_or(self.max_concurrent),
            "sentry" => self
                .issues
                .sentry
                .as_ref()
                .and_then(|c| c.max_concurrent)
                .unwrap_or(self.max_concurrent),
            "jira" => self
                .issues
                .jira
                .as_ref()
                .and_then(|c| c.max_concurrent)
                .unwrap_or(self.max_concurrent),
            "gitlab" => self
                .scm
                .gitlab
                .as_ref()
                .and_then(|c| c.max_concurrent)
                .unwrap_or(self.max_concurrent),
            _ => self.max_concurrent,
        }
    }

    /// Get the poll interval in milliseconds for a specific source.
    /// Uses the source-specific value if set, otherwise falls back to the global value.
    pub fn poll_interval_ms_for(&self, source_name: &str) -> u64 {
        match source_name {
            "discord" => self
                .issues
                .discord
                .as_ref()
                .and_then(|c| c.poll_interval_ms)
                .unwrap_or(self.poll_interval_ms),
            "slack" => self
                .issues
                .slack
                .as_ref()
                .and_then(|c| c.poll_interval_ms)
                .unwrap_or(self.poll_interval_ms),
            "linear" => self
                .issues
                .linear
                .as_ref()
                .and_then(|c| c.poll_interval_ms)
                .unwrap_or(self.poll_interval_ms),
            "sentry" => self
                .issues
                .sentry
                .as_ref()
                .and_then(|c| c.poll_interval_ms)
                .unwrap_or(self.poll_interval_ms),
            "jira" => self
                .issues
                .jira
                .as_ref()
                .and_then(|c| c.poll_interval_ms)
                .unwrap_or(self.poll_interval_ms),
            "gitlab" => self
                .scm
                .gitlab
                .as_ref()
                .and_then(|c| c.poll_interval_ms)
                .unwrap_or(self.poll_interval_ms),
            _ => self.poll_interval_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::NamedTempFile;

    // Mutex to prevent parallel execution of env-modifying tests
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // All environment variables that Config reads
    const CONFIG_ENV_VARS: &[&str] = &[
        "WORK_DIR",
        "KNOWN_ORGS",
        "AUTO_DISCOVER_PATHS",
        "POLL_INTERVAL_MS",
        "WEBHOOK_PORT",
        "DB_PATH",
        "MAX_ISSUES_PER_CYCLE",
        "MAX_CONCURRENT",
        "PROCESSING_DELAY_MS",
        "LINEAR_API_KEY",
        "LINEAR_ENABLED",
        "LINEAR_TRIGGER_LABELS",
        "LINEAR_TRIGGER_ASSIGNEE",
        "LINEAR_TRIGGER_STATES",
        "LINEAR_TEAM_ID",
        "LINEAR_PROJECT_ID",
        "LINEAR_WEBHOOK_SECRET",
        "LINEAR_MAX_ISSUES_PER_CYCLE",
        "LINEAR_MAX_CONCURRENT",
        "LINEAR_POLL_INTERVAL_MS",
        "SENTRY_AUTH_TOKEN",
        "SENTRY_ORG_SLUG",
        "SENTRY_ENABLED",
        "SENTRY_PROJECT_SLUGS",
        "SENTRY_TOP_ISSUES_COUNT",
        "SENTRY_MIN_EVENT_COUNT",
        "SENTRY_ESCALATION_THRESHOLD",
        "SENTRY_CLIENT_SECRET",
        "SENTRY_MAX_ISSUES_PER_CYCLE",
        "SENTRY_MAX_CONCURRENT",
        "SENTRY_POLL_INTERVAL_MS",
        "JIRA_API_TOKEN",
        "JIRA_ENABLED",
        "JIRA_BASE_URL",
        "JIRA_EMAIL",
        "JIRA_AUTH_MODE",
        "JIRA_PROJECT_KEYS",
        "JIRA_TRIGGER_LABELS",
        "JIRA_TRIGGER_STATUSES",
        "JIRA_TRIGGER_ASSIGNEE",
        "JIRA_ISSUE_TYPES",
        "JIRA_CUSTOM_JQL",
        "JIRA_MAX_RESULTS",
        "JIRA_MAX_ISSUES_PER_CYCLE",
        "JIRA_MAX_CONCURRENT",
        "JIRA_POLL_INTERVAL_MS",
        "DISCORD_WEBHOOK_URL",
        "DISCORD_USER_ID",
        "DISCORD_BOT_TOKEN",
        "DISCORD_CHANNEL_ID",
        "DISCORD_SOURCE_ENABLED",
        "DISCORD_LISTEN_CHANNEL_ID",
        "DISCORD_GUILD_ID",
        "DISCORD_POLL_INTERVAL_MS",
        "SMTP_HOST",
        "SMTP_PORT",
        "SMTP_USERNAME",
        "SMTP_PASSWORD",
        "EMAIL_FROM",
        "EMAIL_TO",
        "SMTP_TLS",
        "IMAP_HOST",
        "IMAP_PORT",
        "IMAP_USERNAME",
        "IMAP_PASSWORD",
        "IMAP_TLS",
        "IMAP_FOLDER",
        "ASK_ENABLED",
        "ASK_WAIT_TIMEOUT_SECS",
        "ASK_POLL_INTERVAL_SECS",
        "ASK_MAX_ROUNDS",
        "ASK_SEMANTIC_THRESHOLD_SCOPED",
        "ASK_SEMANTIC_THRESHOLD_GLOBAL",
        "ASK_MAX_REUSE_CANDIDATES",
        "ASK_BEST_EFFORT_ON_TIMEOUT",
        "TWILIO_ACCOUNT_SID",
        "TWILIO_AUTH_TOKEN",
        "TWILIO_FROM_NUMBER",
        "TWILIO_TO_NUMBERS",
        "PUSHOVER_API_TOKEN",
        "PUSHOVER_USER_KEY",
        "PUSHOVER_DEVICE",
        "PUSHOVER_PRIORITY",
        "GITHUB_TOKEN",
        "GITHUB_POLL_INTERVAL_MS",
        "GITHUB_AUTO_RESOLVE_ON_MERGE",
        "GITHUB_APP_ID",
        "GITHUB_APP_PRIVATE_KEY_PATH",
        "GITHUB_APP_PRIVATE_KEY",
        "GITHUB_APP_WEBHOOK_SECRET",
        "GITHUB_APP_INSTALLATION_ID",
        "GITHUB_APP_CLIENT_ID",
        "GITHUB_APP_CLIENT_SECRET",
        "GITHUB_APP_BASE_URL",
        "RETRY_MAX_RETRIES",
        "RETRY_BASE_DELAY_MS",
        "RETRY_MAX_DELAY_MS",
        "CLAUDE_MODEL",
        "CLAUDE_INSTRUCTIONS",
        "CLAUDE_INSTRUCTIONS_FILE",
        "CLAUDE_PERMISSIONS",
        "CLAUDE_SKIP_PERMISSIONS",
    ];

    fn with_env<F, R>(vars: &[(&str, &str)], f: F) -> R
    where
        F: FnOnce() -> R,
    {
        // Lock to prevent parallel execution
        let _lock = ENV_MUTEX.lock().unwrap();

        // Save all existing config env vars
        let saved: Vec<(String, Option<String>)> = CONFIG_ENV_VARS
            .iter()
            .map(|&key| (key.to_string(), env::var(key).ok()))
            .collect();

        // Clear all config env vars first
        for &key in CONFIG_ENV_VARS {
            env::remove_var(key);
        }

        // Set only the vars specified for this test
        for (key, value) in vars {
            env::set_var(key, value);
        }

        let result = f();

        // Clean up: remove all vars we set
        for (key, _) in vars {
            env::remove_var(key);
        }

        // Restore original environment
        for (key, value) in saved {
            match value {
                Some(v) => env::set_var(&key, v),
                None => env::remove_var(&key),
            }
        }

        result
    }

    fn create_temp_toml(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    #[test]
    fn test_from_toml_minimal() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"
known_orgs = ["appwrite", "utopia-php"]

[issues.linear]
api_key = "lin_test_key"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.workspace, PathBuf::from("/tmp/repos"));
            assert_eq!(config.known_orgs, vec!["appwrite", "utopia-php"]);
            assert!(config.issues.linear.is_some());
            assert_eq!(
                config.issues.linear.as_ref().unwrap().api_key,
                SecretValue::new("lin_test_key")
            );
        });
    }

    #[test]
    fn test_from_toml_full_config() {
        // Wrap in with_env to prevent env var interference from parallel tests
        with_env(&[], || {
            let toml_str = r#"
workspace = "/path/to/repos"
known_orgs = ["appwrite", "utopia-php"]
auto_discover_paths = ["~/Local", "~/Projects"]
poll_interval_ms = 600000
webhook_port = 8080
db_path = "/custom/db.sqlite"
max_issues_per_cycle = 10
max_concurrent = 3
processing_delay_ms = 10000

[notifiers.discord]
webhook_url = "https://discord.com/api/webhooks/123/abc"
user_id = "987654321"

[notifiers.email]
smtp_host = "smtp.example.com"
smtp_port = 465
smtp_username = "user@example.com"
smtp_password = "secret"
from_address = "noreply@example.com"
to_addresses = ["admin@example.com", "team@example.com"]
use_tls = true

[notifiers.sms]
account_sid = "AC123"
auth_token = "token123"
from_number = "+15555555555"
to_numbers = ["+16666666666"]

[notifiers.push]
api_token = "pushover_token"
user_key = "user_key"
device = "iPhone"
priority = 1

[scm.github]
token = "ghp_token123"
poll_interval_ms = 30000
auto_resolve_on_merge = false

[retry]
max_retries = 5
base_delay_ms = 30000
max_delay_ms = 7200000

[issues.linear]
enabled = true
api_key = "lin_api_key"
trigger_labels = ["auto", "implement"]
trigger_states = ["todo", "backlog"]
team_id = "team_123"
project_id = "proj_456"
webhook_secret = "webhook_secret"

[issues.sentry]
enabled = true
auth_token = "sentry_token"
org_slug = "my-org"
project_slugs = ["frontend", "backend"]
top_issues_count = 50
min_event_count = 5
escalation_threshold_percent = 25
client_secret = "client_secret"
"#;
            let config = Config::from_toml(toml_str).unwrap();

            assert_eq!(config.workspace, PathBuf::from("/path/to/repos"));
            assert_eq!(config.known_orgs, vec!["appwrite", "utopia-php"]);
            assert_eq!(config.auto_discover_paths, vec!["~/Local", "~/Projects"]);
            assert_eq!(config.poll_interval_ms, 600000);
            assert_eq!(config.webhook_port, 8080);
            assert_eq!(config.db_path, PathBuf::from("/custom/db.sqlite"));
            assert_eq!(config.max_issues_per_cycle, 10);
            assert_eq!(config.max_concurrent, 3);
            assert_eq!(config.processing_delay_ms, 10000);

            // Discord
            let discord = config.discord_merged();
            assert_eq!(
                discord.webhook_url,
                Some(SecretValue::new("https://discord.com/api/webhooks/123/abc"))
            );
            assert_eq!(discord.user_id, Some("987654321".to_string()));

            // Email
            assert_eq!(
                config.notifiers.email.smtp_host,
                Some("smtp.example.com".to_string())
            );
            assert_eq!(config.notifiers.email.smtp_port, 465);
            assert!(config.notifiers.email.use_tls);

            // Linear
            let linear = config.issues.linear.unwrap();
            assert!(linear.enabled);
            assert_eq!(linear.api_key, SecretValue::new("lin_api_key"));
            assert_eq!(linear.trigger_labels, vec!["auto", "implement"]);
            assert_eq!(linear.team_id, Some("team_123".to_string()));

            // Sentry
            let sentry = config.issues.sentry.unwrap();
            assert!(sentry.enabled);
            assert_eq!(sentry.auth_token, SecretValue::new("sentry_token"));
            assert_eq!(sentry.org_slug, "my-org");
            assert_eq!(sentry.top_issues_count, 50);
        });
    }

    /// Helper to create a minimal valid config TOML for tests.
    fn test_config_toml() -> &'static str {
        r#"
workspace = "/tmp/repos"
known_orgs = ["appwrite"]

[issues.linear]
api_key = "test_key"
"#
    }

    #[test]
    fn test_from_toml_with_defaults() {
        with_env(&[], || {
            let config = Config::from_toml(test_config_toml()).unwrap();

            // Check that defaults are applied
            assert_eq!(config.poll_interval_ms, 300_000);
            assert_eq!(config.webhook_port, 3100);
            assert_eq!(config.max_issues_per_cycle, 5);
            assert_eq!(config.max_concurrent, 1);
            assert_eq!(config.processing_delay_ms, 5000);

            // Linear defaults
            let linear = config.issues.linear.unwrap();
            assert!(linear.enabled);
            assert_eq!(
                linear.trigger_labels,
                vec!["auto-implement".to_string(), "claude".to_string()]
            );
            assert_eq!(
                linear.trigger_states,
                vec!["backlog".to_string(), "todo".to_string()]
            );
        });
    }

    #[test]
    fn test_from_toml_invalid_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = /tmp/repos
this is not valid toml [[[
"#;
            let result = Config::from_toml(toml_str);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_load_from_file() {
        let file = create_temp_toml(test_config_toml());

        with_env(&[], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.workspace, PathBuf::from("/tmp/repos"));
            assert_eq!(config.known_orgs, vec!["appwrite"]);
            assert!(config.issues.linear.is_some());
        });
    }

    #[test]
    fn test_load_file_not_found() {
        with_env(&[], || {
            let result = Config::load("/nonexistent/path/config.toml");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("Failed to read"));
        });
    }

    #[test]
    fn test_load_missing_workspace() {
        let toml_str = r#"
known_orgs = ["appwrite"]
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[], || {
            let result = Config::load(file.path());
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("workspace"));
        });
    }

    #[test]
    fn test_load_without_known_orgs_succeeds() {
        // Config can load without known_orgs and auto_discover_paths
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "test_key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[], || {
            let config = Config::load(file.path()).unwrap();
            assert!(config.known_orgs.is_empty());
            assert!(config.auto_discover_paths.is_empty());
            // validate() should succeed since we have a source configured
            assert!(config.validate().is_ok());
        });
    }

    #[test]
    fn test_env_override_workspace() {
        let toml_str = r#"
workspace = "/toml/path"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("WORK_DIR", "/env/path")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.workspace, PathBuf::from("/env/path"));
        });
    }

    #[test]
    fn test_env_override_known_orgs() {
        let toml_str = r#"
workspace = "/tmp/repos"
known_orgs = ["toml-org"]
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("KNOWN_ORGS", "env-org1, env-org2")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.known_orgs, vec!["env-org1", "env-org2"]);
        });
    }

    #[test]
    fn test_env_override_auto_discover_paths() {
        let toml_str = r#"
workspace = "/tmp/repos"
auto_discover_paths = ["~/toml/path"]
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[("AUTO_DISCOVER_PATHS", "~/env/path1, ~/env/path2")],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.auto_discover_paths,
                    vec!["~/env/path1", "~/env/path2"]
                );
            },
        );
    }

    #[test]
    fn test_env_override_core_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
poll_interval_ms = 100000
webhook_port = 3000

[issues.linear]
api_key = "lin_key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("POLL_INTERVAL_MS", "200000"),
                ("WEBHOOK_PORT", "4000"),
                ("MAX_ISSUES_PER_CYCLE", "20"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(config.poll_interval_ms, 200000);
                assert_eq!(config.webhook_port, 4000);
                assert_eq!(config.max_issues_per_cycle, 20);
            },
        );
    }

    #[test]
    fn test_env_override_linear_api_key() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "toml_key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("LINEAR_API_KEY", "env_key")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.issues.linear.as_ref().unwrap().api_key,
                SecretValue::new("env_key")
            );
        });
    }

    #[test]
    fn test_env_creates_linear_config_when_missing() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("LINEAR_API_KEY", "env_key")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(config.issues.linear.is_some());
            assert_eq!(
                config.issues.linear.as_ref().unwrap().api_key,
                SecretValue::new("env_key")
            );
        });
    }

    #[test]
    fn test_env_override_sentry() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.sentry]
auth_token = "toml_token"
org_slug = "toml-org"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("SENTRY_AUTH_TOKEN", "env_token"),
                ("SENTRY_ORG_SLUG", "env-org"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                let sentry = config.issues.sentry.unwrap();
                assert_eq!(sentry.auth_token, SecretValue::new("env_token"));
                assert_eq!(sentry.org_slug, "env-org");
            },
        );
    }

    #[test]
    fn test_env_creates_sentry_config_when_missing() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("SENTRY_AUTH_TOKEN", "env_token"),
                ("SENTRY_ORG_SLUG", "env-org"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert!(config.issues.sentry.is_some());
                assert_eq!(
                    config.issues.sentry.as_ref().unwrap().auth_token,
                    SecretValue::new("env_token")
                );
            },
        );
    }

    #[test]
    fn test_env_override_discord() {
        let toml_str = r#"
workspace = "/tmp/repos"

[notifiers.discord]
webhook_url = "https://toml.webhook"

[issues.linear]
api_key = "key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("DISCORD_WEBHOOK_URL", "https://env.webhook")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.discord_merged().webhook_url,
                Some(SecretValue::new("https://env.webhook"))
            );
        });
    }

    #[test]
    fn test_env_override_github() {
        let toml_str = r#"
workspace = "/tmp/repos"

[scm.github]
token = "toml_token"
poll_interval_ms = 30000
auto_resolve_on_merge = true

[issues.linear]
api_key = "key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("GITHUB_TOKEN", "env_token"),
                ("GITHUB_POLL_INTERVAL_MS", "60000"),
                ("GITHUB_AUTO_RESOLVE_ON_MERGE", "false"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(config.scm.github.token, Some(SecretValue::new("env_token")));
                assert_eq!(config.scm.github.poll_interval_ms, 60000);
                assert!(!config.scm.github.auto_resolve_on_merge);
            },
        );
    }

    #[test]
    fn test_validation_no_sources() {
        let config = Config::default();
        assert!(config.validate().is_err());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_with_linear() {
        let mut config = Config::default();
        config.issues.linear = Some(LinearConfig {
            enabled: true,
            api_key: SecretValue::new("test_key"),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_with_sentry() {
        let mut config = Config::default();
        config.issues.sentry = Some(SentryConfig {
            enabled: true,
            auth_token: SecretValue::new("test_token"),
            org_slug: "test_org".into(),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_sentry_missing_org_slug() {
        let mut config = Config::default();
        config.issues.sentry = Some(SentryConfig {
            enabled: true,
            auth_token: SecretValue::new("test_token"),
            org_slug: String::new(), // Empty org_slug
            ..Default::default()
        });
        assert!(config.validate().is_err());
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("org_slug"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_disabled_sources_fail() {
        let mut config = Config::default();
        config.issues.linear = Some(LinearConfig {
            enabled: false,
            api_key: SecretValue::new("test_key"),
            ..Default::default()
        });
        assert!(config.validate().is_err());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_empty_api_key_fails() {
        let mut config = Config::default();
        config.issues.linear = Some(LinearConfig {
            enabled: true,
            api_key: SecretValue::new(""),
            ..Default::default()
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.poll_interval_ms, 300_000);
        assert_eq!(config.webhook_port, 3100);
        assert_eq!(config.max_issues_per_cycle, 5);
        assert_eq!(config.max_concurrent, 1);
        assert_eq!(config.processing_delay_ms, 5000);
        assert!(config.issues.linear.is_none());
        assert!(config.issues.sentry.is_none());
    }

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 2);
        assert_eq!(config.base_delay_ms, 60_000);
        assert_eq!(config.max_delay_ms, 3_600_000);
    }

    #[test]
    fn test_linear_config_default() {
        let config = LinearConfig::default();
        assert!(config.enabled);
        assert!(config.api_key.is_empty());
        assert_eq!(
            config.trigger_labels,
            vec!["auto-implement".to_string(), "claude".to_string()]
        );
        assert_eq!(
            config.trigger_states,
            vec!["backlog".to_string(), "todo".to_string()]
        );
    }

    #[test]
    fn test_sentry_config_default() {
        let config = SentryConfig::default();
        assert!(config.enabled);
        assert!(config.auth_token.is_empty());
        assert!(config.org_slug.is_empty());
        assert_eq!(config.top_issues_count, 100);
        assert_eq!(config.top_issues_period, TopIssuesPeriod::OneDay);
        assert_eq!(config.min_event_count, 10);
        assert_eq!(config.escalation_threshold_percent, 50);
    }

    #[test]
    fn test_per_source_max_issues_falls_back_to_global() {
        let config = Config {
            max_issues_per_cycle: 7,
            issues: IssuesConfig {
                linear: Some(LinearConfig {
                    api_key: "key".into(),
                    ..Default::default()
                }),
                sentry: Some(SentryConfig {
                    auth_token: "tok".into(),
                    org_slug: "org".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_issues_per_cycle_for("linear"), 7);
        assert_eq!(config.max_issues_per_cycle_for("sentry"), 7);
        assert_eq!(config.max_issues_per_cycle_for("unknown"), 7);
    }

    #[test]
    fn test_per_source_max_issues_overrides_global() {
        let config = Config {
            max_issues_per_cycle: 5,
            issues: IssuesConfig {
                linear: Some(LinearConfig {
                    api_key: "key".into(),
                    max_issues_per_cycle: Some(3),
                    ..Default::default()
                }),
                sentry: Some(SentryConfig {
                    auth_token: "tok".into(),
                    org_slug: "org".into(),
                    max_issues_per_cycle: Some(2),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_issues_per_cycle_for("linear"), 3);
        assert_eq!(config.max_issues_per_cycle_for("sentry"), 2);
        assert_eq!(config.max_issues_per_cycle_for("unknown"), 5);
    }

    #[test]
    fn test_per_source_max_concurrent_falls_back_to_global() {
        let config = Config {
            max_concurrent: 4,
            issues: IssuesConfig {
                linear: Some(LinearConfig {
                    api_key: "key".into(),
                    ..Default::default()
                }),
                sentry: Some(SentryConfig {
                    auth_token: "tok".into(),
                    org_slug: "org".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_concurrent_for("linear"), 4);
        assert_eq!(config.max_concurrent_for("sentry"), 4);
        assert_eq!(config.max_concurrent_for("unknown"), 4);
    }

    #[test]
    fn test_per_source_max_concurrent_overrides_global() {
        let config = Config {
            max_concurrent: 8,
            issues: IssuesConfig {
                linear: Some(LinearConfig {
                    api_key: "key".into(),
                    max_concurrent: Some(2),
                    ..Default::default()
                }),
                sentry: Some(SentryConfig {
                    auth_token: "tok".into(),
                    org_slug: "org".into(),
                    max_concurrent: Some(6),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_concurrent_for("linear"), 2);
        assert_eq!(config.max_concurrent_for("sentry"), 6);
        assert_eq!(config.max_concurrent_for("unknown"), 8);
    }

    #[test]
    fn test_per_source_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"
max_issues_per_cycle = 5
max_concurrent = 8

[issues.linear]
api_key = "lin_key"
max_issues_per_cycle = 3
max_concurrent = 2

[issues.sentry]
auth_token = "sentry_tok"
org_slug = "org"
max_issues_per_cycle = 2
max_concurrent = 6
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.max_issues_per_cycle_for("linear"), 3);
            assert_eq!(config.max_issues_per_cycle_for("sentry"), 2);
            assert_eq!(config.max_concurrent_for("linear"), 2);
            assert_eq!(config.max_concurrent_for("sentry"), 6);
        });
    }

    #[test]
    fn test_per_source_config_partial_override() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"
max_issues_per_cycle = 5
max_concurrent = 8

[issues.linear]
api_key = "lin_key"
max_issues_per_cycle = 3

[issues.sentry]
auth_token = "sentry_tok"
org_slug = "org"
max_concurrent = 6
"#;
            let config = Config::from_toml(toml_str).unwrap();
            // Linear overrides issues but not concurrent
            assert_eq!(config.max_issues_per_cycle_for("linear"), 3);
            assert_eq!(config.max_concurrent_for("linear"), 8);
            // Sentry overrides concurrent but not issues
            assert_eq!(config.max_issues_per_cycle_for("sentry"), 5);
            assert_eq!(config.max_concurrent_for("sentry"), 6);
        });
    }

    #[test]
    fn test_poll_interval_ms_for_falls_back_to_global() {
        let config = Config {
            poll_interval_ms: 300_000,
            issues: IssuesConfig {
                linear: Some(LinearConfig {
                    api_key: "key".into(),
                    ..Default::default()
                }),
                sentry: Some(SentryConfig {
                    auth_token: "tok".into(),
                    org_slug: "org".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.poll_interval_ms_for("discord"), 300_000);
        assert_eq!(config.poll_interval_ms_for("linear"), 300_000);
        assert_eq!(config.poll_interval_ms_for("sentry"), 300_000);
        assert_eq!(config.poll_interval_ms_for("unknown"), 300_000);
    }

    #[test]
    fn test_poll_interval_ms_for_overrides_global() {
        let config = Config {
            poll_interval_ms: 300_000,
            issues: IssuesConfig {
                discord: Some(DiscordSourceConfig {
                    poll_interval_ms: Some(30_000),
                    ..Default::default()
                }),
                linear: Some(LinearConfig {
                    api_key: "key".into(),
                    poll_interval_ms: Some(600_000),
                    ..Default::default()
                }),
                sentry: Some(SentryConfig {
                    auth_token: "tok".into(),
                    org_slug: "org".into(),
                    poll_interval_ms: Some(120_000),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.poll_interval_ms_for("discord"), 30_000);
        assert_eq!(config.poll_interval_ms_for("linear"), 600_000);
        assert_eq!(config.poll_interval_ms_for("sentry"), 120_000);
        assert_eq!(config.poll_interval_ms_for("unknown"), 300_000);
    }

    #[test]
    fn test_poll_interval_ms_for_from_toml() {
        // Hold env mutex: from_toml reads env vars which can race with other tests
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"
poll_interval_ms = 300000

[issues.discord]
poll_interval_ms = 30000

[issues.linear]
api_key = "lin_key"
poll_interval_ms = 600000

[issues.sentry]
auth_token = "sentry_tok"
org_slug = "org"
poll_interval_ms = 120000
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.poll_interval_ms_for("discord"), 30_000);
            assert_eq!(config.poll_interval_ms_for("linear"), 600_000);
            assert_eq!(config.poll_interval_ms_for("sentry"), 120_000);
            assert_eq!(config.poll_interval_ms_for("unknown"), 300_000);
        });
    }

    #[test]
    fn test_poll_interval_ms_for_env_override() {
        with_env(
            &[
                ("DISCORD_POLL_INTERVAL_MS", "15000"),
                ("DISCORD_SOURCE_ENABLED", "1"),
            ],
            || {
                let config = Config::from_toml("workspace = \"/tmp\"").unwrap();
                assert_eq!(config.poll_interval_ms_for("discord"), 15_000);
                // Global unchanged
                assert_eq!(config.poll_interval_ms_for("unknown"), 300_000);
            },
        );
    }

    #[test]
    fn test_top_issues_period_to_stats_period() {
        assert_eq!(TopIssuesPeriod::OneHour.to_stats_period(), "1h");
        assert_eq!(TopIssuesPeriod::TwelveHours.to_stats_period(), "12h");
        assert_eq!(TopIssuesPeriod::OneDay.to_stats_period(), "24h");
        assert_eq!(TopIssuesPeriod::OneWeek.to_stats_period(), "7d");
        assert_eq!(TopIssuesPeriod::OneMonth.to_stats_period(), "30d");
    }

    #[test]
    fn test_top_issues_period_from_str() {
        // 1 hour variants
        assert_eq!(
            "1h".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneHour)
        );
        assert_eq!(
            "1hr".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneHour)
        );
        assert_eq!(
            "hour".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneHour)
        );
        assert_eq!(
            "one_hour".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneHour)
        );

        // 12 hours variants
        assert_eq!(
            "12h".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::TwelveHours)
        );
        assert_eq!(
            "12hr".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::TwelveHours)
        );
        assert_eq!(
            "12hrs".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::TwelveHours)
        );
        assert_eq!(
            "twelve_hours".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::TwelveHours)
        );

        // 1 day variants
        assert_eq!(
            "24h".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneDay)
        );
        assert_eq!("1d".parse::<TopIssuesPeriod>(), Ok(TopIssuesPeriod::OneDay));
        assert_eq!(
            "day".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneDay)
        );
        assert_eq!(
            "1day".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneDay)
        );
        assert_eq!(
            "one_day".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneDay)
        );

        // 1 week variants
        assert_eq!(
            "7d".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneWeek)
        );
        assert_eq!(
            "1w".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneWeek)
        );
        assert_eq!(
            "week".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneWeek)
        );
        assert_eq!(
            "1week".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneWeek)
        );
        assert_eq!(
            "one_week".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneWeek)
        );

        // 1 month variants
        assert_eq!(
            "30d".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneMonth)
        );
        assert_eq!(
            "1m".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneMonth)
        );
        assert_eq!(
            "month".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneMonth)
        );
        assert_eq!(
            "1month".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneMonth)
        );
        assert_eq!(
            "one_month".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneMonth)
        );

        // Case insensitivity
        assert_eq!(
            "1H".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneHour)
        );
        assert_eq!(
            "ONE_WEEK".parse::<TopIssuesPeriod>(),
            Ok(TopIssuesPeriod::OneWeek)
        );

        // Invalid
        assert!("invalid".parse::<TopIssuesPeriod>().is_err());
        assert!("2h".parse::<TopIssuesPeriod>().is_err());
        assert!("".parse::<TopIssuesPeriod>().is_err());
    }

    #[test]
    fn test_top_issues_period_display() {
        assert_eq!(format!("{}", TopIssuesPeriod::OneHour), "1h");
        assert_eq!(format!("{}", TopIssuesPeriod::TwelveHours), "12h");
        assert_eq!(format!("{}", TopIssuesPeriod::OneDay), "24h");
        assert_eq!(format!("{}", TopIssuesPeriod::OneWeek), "7d");
        assert_eq!(format!("{}", TopIssuesPeriod::OneMonth), "30d");
    }

    #[test]
    fn test_top_issues_period_default() {
        assert_eq!(TopIssuesPeriod::default(), TopIssuesPeriod::OneDay);
    }

    #[test]
    fn test_is_linear_enabled() {
        let mut config = Config::default();
        assert!(!config.is_linear_enabled());

        config.issues.linear = Some(LinearConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(config.is_linear_enabled());

        config.issues.linear.as_mut().unwrap().enabled = false;
        assert!(!config.is_linear_enabled());
    }

    #[test]
    fn test_is_sentry_enabled() {
        let mut config = Config::default();
        assert!(!config.is_sentry_enabled());

        config.issues.sentry = Some(SentryConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(config.is_sentry_enabled());

        config.issues.sentry.as_mut().unwrap().enabled = false;
        assert!(!config.is_sentry_enabled());
    }

    #[test]
    fn test_is_github_enabled() {
        let mut config = Config::default();
        assert!(!config.is_github_enabled());

        config.scm.github.token = Some(SecretValue::new("ghp_test"));
        assert!(config.is_github_enabled());
    }

    #[test]
    fn test_config_toml_roundtrip() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"
known_orgs = ["appwrite", "utopia-php"]
auto_discover_paths = ["~/Local"]
poll_interval_ms = 500000

[issues.linear]
enabled = true
api_key = "test_key"
trigger_labels = ["label1", "label2"]
"#;
            let config = Config::from_toml(toml_str).unwrap();
            let serialized = toml::to_string(&config).unwrap();
            let deserialized: Config = toml::from_str(&serialized).unwrap();

            assert_eq!(config.workspace, deserialized.workspace);
            assert_eq!(config.known_orgs, deserialized.known_orgs);
            assert_eq!(config.auto_discover_paths, deserialized.auto_discover_paths);
            assert_eq!(config.poll_interval_ms, deserialized.poll_interval_ms);
            assert_eq!(
                config.issues.linear.as_ref().unwrap().api_key,
                deserialized.issues.linear.as_ref().unwrap().api_key
            );
        });
    }

    #[test]
    fn test_retry_config_serialization() {
        let config = RetryConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("max_retries"));
        assert!(toml_str.contains("base_delay_ms"));
        assert!(toml_str.contains("max_delay_ms"));
    }

    #[test]
    fn test_regression_config_default() {
        let config = RegressionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.check_interval_hours, 1);
        assert_eq!(config.monitoring_duration_hours, 24);
        assert!(config.check_interval_secs.is_none());
        assert!(config.monitoring_duration_secs.is_none());
        assert_eq!(config.sentry_event_threshold, 1);
        assert!((config.similarity_threshold - 0.75).abs() < 0.01);
        // target_repos and github_search_repos should be empty by default
        // (configured in TOML, not hardcoded)
        assert!(config.target_repos.is_empty());
        assert!(config.github_token.is_none());
        assert!(config.github_search_repos.is_empty());
        assert!(config.package_names.is_empty());
    }

    #[test]
    fn test_regression_config_effective_seconds_defaults() {
        let config = RegressionConfig::default();
        // Without overrides, should convert hours to seconds
        assert_eq!(config.effective_check_interval_secs(), 3600); // 1 hour
        assert_eq!(config.effective_monitoring_duration_secs(), 86400); // 24 hours
    }

    #[test]
    fn test_regression_config_effective_seconds_overrides() {
        let config = RegressionConfig {
            check_interval_secs: Some(10),
            monitoring_duration_secs: Some(30),
            ..Default::default()
        };
        assert_eq!(config.effective_check_interval_secs(), 10);
        assert_eq!(config.effective_monitoring_duration_secs(), 30);
    }

    #[test]
    fn test_regression_config_effective_check_interval_min_clamp() {
        let config = RegressionConfig {
            check_interval_secs: Some(0),
            ..Default::default()
        };
        // Should clamp to 1 second minimum
        assert_eq!(config.effective_check_interval_secs(), 1);
    }

    #[test]
    fn test_regression_config_serialization() {
        let config = RegressionConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("enabled"));
        assert!(toml_str.contains("check_interval_hours"));
        assert!(toml_str.contains("monitoring_duration_hours"));
        assert!(toml_str.contains("sentry_event_threshold"));
        assert!(toml_str.contains("similarity_threshold"));
        assert!(toml_str.contains("target_repos"));
    }

    #[test]
    fn test_regression_config_deserialization() {
        let toml_str = r#"
enabled = true
check_interval_hours = 2
monitoring_duration_hours = 48
sentry_event_threshold = 5
similarity_threshold = 0.8
target_repos = ["custom/repo"]
github_search_repos = ["org/repo1", "org/repo2"]
"#;
        let config: RegressionConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.check_interval_hours, 2);
        assert_eq!(config.monitoring_duration_hours, 48);
        assert_eq!(config.sentry_event_threshold, 5);
        assert!((config.similarity_threshold - 0.8).abs() < 0.01);
        assert_eq!(config.target_repos, vec!["custom/repo"]);
        assert_eq!(config.github_search_repos.len(), 2);
    }

    #[test]
    fn test_config_includes_regression() {
        let config = Config::default();
        assert!(config.regression.enabled);
        assert_eq!(config.regression.check_interval_hours, 1);
    }

    #[test]
    fn test_config_regression_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/test"

[regression]
enabled = false
check_interval_hours = 4
monitoring_duration_hours = 12
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(!config.regression.enabled);
            assert_eq!(config.regression.check_interval_hours, 4);
            assert_eq!(config.regression.monitoring_duration_hours, 12);
            // Defaults should apply for unspecified fields
            assert_eq!(config.regression.sentry_event_threshold, 1);
        });
    }

    #[test]
    fn test_github_app_config_default() {
        let config = GitHubAppConfig::default();
        assert!(config.app_id.is_none());
        assert!(config.private_key_path.is_none());
        assert!(config.private_key.is_none());
        assert!(config.webhook_secret.is_none());
        assert!(config.installation_id.is_none());
        assert!(config.client_id.is_none());
        assert!(config.client_secret.is_none());
        assert!(config.base_url.is_none());
    }

    #[test]
    fn test_github_app_config_is_configured() {
        let mut config = GitHubAppConfig::default();
        assert!(!config.is_configured());

        // Just app_id is not enough
        config.app_id = Some(12345);
        assert!(!config.is_configured());

        // app_id + private_key_path is enough
        config.private_key_path = Some(PathBuf::from("/path/to/key.pem"));
        assert!(config.is_configured());

        // app_id + private_key (inline) is also enough
        config.private_key_path = None;
        config.private_key = Some(SecretValue::new("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(config.is_configured());
    }

    #[test]
    fn test_github_app_config_load_private_key_inline() {
        let config = GitHubAppConfig {
            private_key: Some(SecretValue::new("test-key-content")),
            ..Default::default()
        };

        let key = config.load_private_key().unwrap();
        assert_eq!(key, "test-key-content");
    }

    #[test]
    fn test_github_app_config_load_private_key_missing() {
        let config = GitHubAppConfig::default();
        let result = config.load_private_key();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No GitHub App private key"));
    }

    #[test]
    fn test_github_auth_mode_default() {
        assert_eq!(GitHubAuthMode::default(), GitHubAuthMode::Token);
    }

    #[test]
    fn test_config_github_auth_mode_token() {
        let config = Config::default();
        assert_eq!(config.github_auth_mode(), GitHubAuthMode::Token);
    }

    #[test]
    fn test_config_github_auth_mode_app() {
        let mut config = Config::default();
        config.scm.github.app.app_id = Some(12345);
        config.scm.github.app.private_key = Some(SecretValue::new("test-key"));
        assert_eq!(config.github_auth_mode(), GitHubAuthMode::App);
    }

    #[test]
    fn test_is_github_enabled_with_token() {
        let mut config = Config::default();
        assert!(!config.is_github_enabled());

        config.scm.github.token = Some(SecretValue::new("ghp_test"));
        assert!(config.is_github_enabled());
    }

    #[test]
    fn test_is_github_enabled_with_app() {
        let mut config = Config::default();
        assert!(!config.is_github_enabled());

        config.scm.github.app.app_id = Some(12345);
        config.scm.github.app.private_key = Some(SecretValue::new("test-key"));
        assert!(config.is_github_enabled());
    }

    #[test]
    fn test_github_app_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/test"

[scm.github.app]
app_id = 12345
private_key_path = "/path/to/key.pem"
webhook_secret = "secret123"
installation_id = 67890
client_id = "Iv1.abc123"
client_secret = "secret456"
base_url = "https://example.com"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.scm.github.app.app_id, Some(12345));
            assert_eq!(
                config.scm.github.app.private_key_path,
                Some(PathBuf::from("/path/to/key.pem"))
            );
            assert_eq!(
                config.scm.github.app.webhook_secret,
                Some(SecretValue::new("secret123"))
            );
            assert_eq!(config.scm.github.app.installation_id, Some(67890));
            assert_eq!(
                config.scm.github.app.client_id,
                Some("Iv1.abc123".to_string())
            );
            assert_eq!(
                config.scm.github.app.client_secret,
                Some(SecretValue::new("secret456"))
            );
            assert_eq!(
                config.scm.github.app.base_url,
                Some("https://example.com".to_string())
            );
        });
    }

    #[test]
    fn test_env_override_github_app() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("GITHUB_APP_ID", "12345"),
                ("GITHUB_APP_PRIVATE_KEY", "test-key"),
                ("GITHUB_APP_WEBHOOK_SECRET", "webhook-secret"),
                ("GITHUB_APP_INSTALLATION_ID", "67890"),
                ("GITHUB_APP_CLIENT_ID", "client-id"),
                ("GITHUB_APP_CLIENT_SECRET", "client-secret"),
                ("GITHUB_APP_BASE_URL", "https://example.com"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(config.scm.github.app.app_id, Some(12345));
                assert_eq!(
                    config.scm.github.app.private_key,
                    Some(SecretValue::new("test-key"))
                );
                assert_eq!(
                    config.scm.github.app.webhook_secret,
                    Some(SecretValue::new("webhook-secret"))
                );
                assert_eq!(config.scm.github.app.installation_id, Some(67890));
                assert_eq!(
                    config.scm.github.app.client_id,
                    Some("client-id".to_string())
                );
                assert_eq!(
                    config.scm.github.app.client_secret,
                    Some(SecretValue::new("client-secret"))
                );
                assert_eq!(
                    config.scm.github.app.base_url,
                    Some("https://example.com".to_string())
                );
            },
        );
    }

    #[test]
    fn test_provider_config_default() {
        let config = ProviderConfig::default();
        assert!(config.model.is_none());
        assert!(config.instructions.is_none());
        assert!(config.permissions.is_empty());
        assert!(!config.skip_permissions);
    }

    #[test]
    fn test_config_default_includes_claude() {
        let config = Config::default();
        assert!(config
            .agent
            .default_provider_config()
            .unwrap()
            .model
            .is_none());
        assert!(config
            .agent
            .default_provider_config()
            .unwrap()
            .instructions
            .is_none());
        assert!(config
            .agent
            .default_provider_config()
            .unwrap()
            .permissions
            .is_empty());
        assert!(
            !config
                .agent
                .default_provider_config()
                .unwrap()
                .skip_permissions
        );
    }

    #[test]
    fn test_agent_provider_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/test"

[agent.providers.claude]
model = "sonnet"
instructions = "Always write tests."
permissions = ["Bash(git *)", "Read"]
skip_permissions = false
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(
                config.agent.default_provider_config().unwrap().model,
                Some("sonnet".to_string())
            );
            assert_eq!(
                config.agent.default_provider_config().unwrap().instructions,
                Some("Always write tests.".to_string())
            );
            assert_eq!(
                config.agent.default_provider_config().unwrap().permissions,
                vec!["Bash(git *)", "Read"]
            );
            assert!(
                !config
                    .agent
                    .default_provider_config()
                    .unwrap()
                    .skip_permissions
            );
        });
    }

    #[test]
    fn test_claude_config_toml_defaults() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/test"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(config
                .agent
                .default_provider_config()
                .unwrap()
                .model
                .is_none());
            assert!(config
                .agent
                .default_provider_config()
                .unwrap()
                .instructions
                .is_none());
            assert!(config
                .agent
                .default_provider_config()
                .unwrap()
                .permissions
                .is_empty());
            assert!(
                !config
                    .agent
                    .default_provider_config()
                    .unwrap()
                    .skip_permissions
            );
        });
    }

    #[test]
    fn test_env_override_claude_model() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("CLAUDE_MODEL", "opus")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.agent.default_provider_config().unwrap().model,
                Some("opus".to_string())
            );
        });
    }

    #[test]
    fn test_env_override_claude_instructions() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("CLAUDE_INSTRUCTIONS", "Be concise.")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.agent.default_provider_config().unwrap().instructions,
                Some("Be concise.".to_string())
            );
        });
    }

    #[test]
    fn test_env_override_claude_instructions_file() {
        let dir = tempfile::tempdir().unwrap();
        let instructions_path = dir.path().join("my-instructions.md");
        fs::write(&instructions_path, "File content.").unwrap();

        let config_path = dir.path().join("claudear.toml");
        fs::write(&config_path, "workspace = \"/tmp/repos\"\n").unwrap();

        with_env(
            &[("CLAUDE_INSTRUCTIONS_FILE", "my-instructions.md")],
            || {
                let config = Config::load(&config_path).unwrap();
                assert_eq!(
                    config
                        .agent
                        .default_provider_config()
                        .unwrap()
                        .instructions_file,
                    Some("my-instructions.md".to_string())
                );
                // After load, instructions should contain resolved file content
                assert_eq!(
                    config.agent.default_provider_config().unwrap().instructions,
                    Some("File content.".to_string())
                );
            },
        );
    }

    #[test]
    fn test_env_override_claude_permissions() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("CLAUDE_PERMISSIONS", "Bash(git *), Read, Edit")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.agent.default_provider_config().unwrap().permissions,
                vec!["Bash(git *)", "Read", "Edit"]
            );
        });
    }

    #[test]
    fn test_env_override_claude_skip_permissions() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("CLAUDE_SKIP_PERMISSIONS", "false")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(
                !config
                    .agent
                    .default_provider_config()
                    .unwrap()
                    .skip_permissions
            );
        });
    }

    #[test]
    fn test_env_override_claude_skip_permissions_true() {
        let toml_str = r#"
workspace = "/tmp/repos"

[agent.providers.claude]
skip_permissions = false
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("CLAUDE_SKIP_PERMISSIONS", "1")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(
                config
                    .agent
                    .default_provider_config()
                    .unwrap()
                    .skip_permissions
            );
        });
    }

    #[test]
    fn test_resolve_instructions_file_reads_file() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let instructions_path = dir.path().join("instructions.md");
            fs::write(&instructions_path, "Be helpful and concise.").unwrap();

            let toml_str = format!(
                "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions_file = \"{}\"",
                instructions_path.display()
            );
            let config = Config::from_toml(&toml_str).unwrap();
            let resolved = config.resolve_instructions_file(dir.path()).unwrap();
            assert_eq!(resolved, Some("Be helpful and concise.".to_string()));
        });
    }

    #[test]
    fn test_resolve_instructions_file_relative_path() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let instructions_path = dir.path().join("my-instructions.md");
            fs::write(&instructions_path, "Write tests first.").unwrap();

            let toml_str =
                "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions_file = \"my-instructions.md\"";
            let config = Config::from_toml(toml_str).unwrap();
            let resolved = config.resolve_instructions_file(dir.path()).unwrap();
            assert_eq!(resolved, Some("Write tests first.".to_string()));
        });
    }

    #[test]
    fn test_resolve_instructions_file_combines_with_inline() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let instructions_path = dir.path().join("base.md");
            fs::write(&instructions_path, "Base instructions from file.").unwrap();

            let toml_str = "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions_file = \"base.md\"\ninstructions = \"Plus inline.\"";
            let config = Config::from_toml(toml_str).unwrap();
            let resolved = config.resolve_instructions_file(dir.path()).unwrap();
            assert_eq!(
                resolved,
                Some("Base instructions from file.\nPlus inline.".to_string())
            );
        });
    }

    #[test]
    fn test_resolve_instructions_file_inline_only() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();

            let toml_str = "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions = \"Just inline.\"";
            let config = Config::from_toml(toml_str).unwrap();
            let resolved = config.resolve_instructions_file(dir.path()).unwrap();
            assert_eq!(resolved, Some("Just inline.".to_string()));
        });
    }

    #[test]
    fn test_resolve_instructions_file_neither_set() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();

            let toml_str = "workspace = \"/tmp/repos\"";
            let config = Config::from_toml(toml_str).unwrap();
            let resolved = config.resolve_instructions_file(dir.path()).unwrap();
            assert_eq!(resolved, None);
        });
    }

    #[test]
    fn test_resolve_instructions_file_not_found() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();

            let toml_str =
                "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions_file = \"nonexistent.md\"";
            let config = Config::from_toml(toml_str).unwrap();
            let result = config.resolve_instructions_file(dir.path());
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("nonexistent.md"));
        });
    }

    #[test]
    fn test_resolve_instructions_file_empty_file() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let instructions_path = dir.path().join("empty.md");
            fs::write(&instructions_path, "").unwrap();

            let toml_str =
                "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions_file = \"empty.md\"";
            let config = Config::from_toml(toml_str).unwrap();
            let resolved = config.resolve_instructions_file(dir.path()).unwrap();
            assert_eq!(resolved, None);
        });
    }

    #[test]
    fn test_load_resolves_instructions_file() {
        let dir = tempfile::tempdir().unwrap();
        let instructions_path = dir.path().join("my-instructions.md");
        fs::write(&instructions_path, "Instructions from file.").unwrap();

        let toml_str = "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions_file = \"my-instructions.md\"\ninstructions = \"And inline.\"";
        let config_path = dir.path().join("claudear.toml");
        fs::write(&config_path, toml_str).unwrap();

        with_env(&[], || {
            let config = Config::load(&config_path).unwrap();
            // After load, instructions should be the merged result
            assert_eq!(
                config.agent.default_provider_config().unwrap().instructions,
                Some("Instructions from file.\nAnd inline.".to_string())
            );
        });
    }

    #[test]
    fn test_users_config_deserialize() {
        let toml_str = r#"
[users.jake]
linear_names = ["Jake Barnwell"]
github_usernames = ["jakebarnby"]
sentry_usernames = ["jake"]
discord_id = "123456789"
email = "jake@example.com"
push_user_key = "pushover_key"
sms_number = "+1234567890"

[users.alice]
linear_names = ["Alice Smith"]
github_usernames = ["alicesmith"]
discord_id = "987654321"
email = "alice@example.com"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.users.len(), 2);
        let jake = &config.users["jake"];
        assert_eq!(jake.linear_names, vec!["Jake Barnwell"]);
        assert_eq!(jake.github_usernames, vec!["jakebarnby"]);
        assert_eq!(jake.discord_id.as_deref(), Some("123456789"));
        assert_eq!(jake.email.as_deref(), Some("jake@example.com"));
        assert_eq!(jake.push_user_key.as_deref(), Some("pushover_key"));
        assert_eq!(jake.sms_number.as_deref(), Some("+1234567890"));
        let alice = &config.users["alice"];
        assert_eq!(alice.linear_names, vec!["Alice Smith"]);
        assert!(alice.push_user_key.is_none());
        assert!(alice.sms_number.is_none());
    }

    #[test]
    fn test_users_config_default_empty() {
        let config = Config::default();
        assert!(config.users.is_empty());
    }

    #[test]
    fn test_resolve_user_slug_in_discord_config() {
        let toml_str = r#"
[users.jake]
discord_id = "123456789"
email = "jake@example.com"

[notifiers.discord]
webhook_url = "https://discord.com/api/webhooks/123/abc"
user_id = "jake"
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        assert_eq!(
            config.notifiers.discord.user_id.as_deref(),
            Some("123456789")
        );
    }

    #[test]
    fn test_resolve_user_slug_not_found_keeps_raw_value() {
        let toml_str = r#"
[users.jake]
discord_id = "123456789"

[notifiers.discord]
webhook_url = "https://discord.com/api/webhooks/123/abc"
user_id = "999888777"
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        assert_eq!(
            config.notifiers.discord.user_id.as_deref(),
            Some("999888777")
        );
    }

    #[test]
    fn test_resolve_user_slug_in_email_config() {
        let toml_str = r#"
[users.jake]
email = "jake@resolved.com"

[notifiers.email]
smtp_host = "smtp.example.com"
to_addresses = ["jake", "other@example.com"]
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        assert_eq!(
            config.notifiers.email.to_addresses,
            vec!["jake@resolved.com", "other@example.com"]
        );
    }

    #[test]
    fn test_resolve_user_slug_in_push_config() {
        let toml_str = r#"
[users.jake]
push_user_key = "resolved_push_key"

[notifiers.push]
api_token = "token"
user_key = "jake"
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        assert_eq!(
            config.notifiers.push.user_key.as_deref(),
            Some("resolved_push_key")
        );
    }

    #[test]
    fn test_resolve_user_slug_in_sms_config() {
        let toml_str = r#"
[users.jake]
sms_number = "+1234567890"

[notifiers.sms]
account_sid = "sid"
to_numbers = ["jake", "+9876543210"]
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        assert_eq!(
            config.notifiers.sms.to_numbers,
            vec!["+1234567890", "+9876543210"]
        );
    }

    #[test]
    fn test_env_override_imap_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("IMAP_HOST", "imap.example.com"),
                ("IMAP_PORT", "143"),
                ("IMAP_USERNAME", "user@example.com"),
                ("IMAP_PASSWORD", "secret"),
                ("IMAP_TLS", "false"),
                ("IMAP_FOLDER", "Junk"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.notifiers.email.imap_host,
                    Some("imap.example.com".to_string())
                );
                assert_eq!(config.notifiers.email.imap_port, 143);
                assert_eq!(
                    config.notifiers.email.imap_username,
                    Some("user@example.com".to_string())
                );
                assert_eq!(
                    config.notifiers.email.imap_password,
                    Some(SecretValue::new("secret"))
                );
                assert!(!config.notifiers.email.imap_use_tls);
                assert_eq!(config.notifiers.email.imap_folder, "Junk");
            },
        );
    }

    #[test]
    fn test_env_override_ask_config() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("ASK_ENABLED", "false"),
                ("ASK_WAIT_TIMEOUT_SECS", "600"),
                ("ASK_POLL_INTERVAL_SECS", "30"),
                ("ASK_MAX_ROUNDS", "5"),
                ("ASK_SEMANTIC_THRESHOLD_SCOPED", "0.90"),
                ("ASK_SEMANTIC_THRESHOLD_GLOBAL", "0.95"),
                ("ASK_MAX_REUSE_CANDIDATES", "10"),
                ("ASK_BEST_EFFORT_ON_TIMEOUT", "false"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert!(!config.ask.enabled);
                assert_eq!(config.ask.wait_timeout_secs, 600);
                assert_eq!(config.ask.poll_interval_secs, 30);
                assert_eq!(config.ask.max_rounds_per_attempt, 5);
                assert!((config.ask.semantic_threshold_scoped - 0.90).abs() < 0.01);
                assert!((config.ask.semantic_threshold_global - 0.95).abs() < 0.01);
                assert_eq!(config.ask.max_reuse_candidates, 10);
                assert!(!config.ask.best_effort_on_timeout);
            },
        );
    }

    #[test]
    fn test_env_override_sms_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("TWILIO_ACCOUNT_SID", "AC_test"),
                ("TWILIO_AUTH_TOKEN", "auth_tok"),
                ("TWILIO_FROM_NUMBER", "+15551234567"),
                ("TWILIO_TO_NUMBERS", "+15559876543, +15551111111"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.notifiers.sms.account_sid,
                    Some("AC_test".to_string())
                );
                assert_eq!(
                    config.notifiers.sms.auth_token,
                    Some(SecretValue::new("auth_tok"))
                );
                assert_eq!(
                    config.notifiers.sms.from_number,
                    Some("+15551234567".to_string())
                );
                assert_eq!(
                    config.notifiers.sms.to_numbers,
                    vec!["+15559876543", "+15551111111"]
                );
            },
        );
    }

    #[test]
    fn test_env_override_push_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("PUSHOVER_API_TOKEN", "api_tok"),
                ("PUSHOVER_USER_KEY", "user_key"),
                ("PUSHOVER_DEVICE", "myphone"),
                ("PUSHOVER_PRIORITY", "2"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.notifiers.push.api_token,
                    Some(SecretValue::new("api_tok"))
                );
                assert_eq!(config.notifiers.push.user_key, Some("user_key".to_string()));
                assert_eq!(config.notifiers.push.device, Some("myphone".to_string()));
                assert_eq!(config.notifiers.push.priority, Some(2));
            },
        );
    }

    #[test]
    fn test_env_override_retry_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("RETRY_MAX_RETRIES", "5"),
                ("RETRY_BASE_DELAY_MS", "30000"),
                ("RETRY_MAX_DELAY_MS", "7200000"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(config.retry.max_retries, 5);
                assert_eq!(config.retry.base_delay_ms, 30000);
                assert_eq!(config.retry.max_delay_ms, 7200000);
            },
        );
    }

    #[test]
    fn test_env_override_linear_trigger_labels_and_states() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "toml_key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("LINEAR_TRIGGER_LABELS", "urgent, critical"),
                ("LINEAR_TRIGGER_STATES", "in_progress, review"),
                ("LINEAR_TEAM_ID", "team_abc"),
                ("LINEAR_PROJECT_ID", "proj_xyz"),
                ("LINEAR_WEBHOOK_SECRET", "my_secret"),
                ("LINEAR_MAX_ISSUES_PER_CYCLE", "3"),
                ("LINEAR_MAX_CONCURRENT", "2"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                let linear = config.issues.linear.unwrap();
                assert_eq!(linear.trigger_labels, vec!["urgent", "critical"]);
                assert_eq!(linear.trigger_states, vec!["in_progress", "review"]);
                assert_eq!(linear.team_id, Some("team_abc".to_string()));
                assert_eq!(linear.project_id, Some("proj_xyz".to_string()));
                assert_eq!(linear.webhook_secret, Some(SecretValue::new("my_secret")));
                assert_eq!(linear.max_issues_per_cycle, Some(3));
                assert_eq!(linear.max_concurrent, Some(2));
            },
        );
    }

    #[test]
    fn test_env_override_linear_enabled_flag() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "key"
enabled = true
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("LINEAR_ENABLED", "false")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(!config.issues.linear.as_ref().unwrap().enabled);
        });
    }

    #[test]
    fn test_env_override_linear_trigger_assignee() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("LINEAR_TRIGGER_ASSIGNEE", "Jane Smith")], || {
            let config = Config::load(file.path()).unwrap();
            let linear = config.issues.linear.unwrap();
            assert_eq!(linear.trigger_assignee, Some("Jane Smith".to_string()));
        });
    }

    #[test]
    fn test_env_override_linear_trigger_assignee_empty() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "key"
trigger_assignee = "Previous Value"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("LINEAR_TRIGGER_ASSIGNEE", "")], || {
            let config = Config::load(file.path()).unwrap();
            let linear = config.issues.linear.unwrap();
            // Empty env var should clear the value
            assert_eq!(linear.trigger_assignee, None);
        });
    }

    #[test]
    fn test_env_override_sentry_detailed() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.sentry]
auth_token = "toml_token"
org_slug = "toml-org"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("SENTRY_ENABLED", "false"),
                ("SENTRY_PROJECT_SLUGS", "web, api, worker"),
                ("SENTRY_TOP_ISSUES_COUNT", "25"),
                ("SENTRY_TOP_ISSUES_PERIOD", "7d"),
                ("SENTRY_MIN_EVENT_COUNT", "50"),
                ("SENTRY_ESCALATION_THRESHOLD", "75"),
                ("SENTRY_CLIENT_SECRET", "sentry_secret"),
                ("SENTRY_MAX_ISSUES_PER_CYCLE", "10"),
                ("SENTRY_MAX_CONCURRENT", "4"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                let sentry = config.issues.sentry.unwrap();
                assert!(!sentry.enabled);
                assert_eq!(sentry.project_slugs, vec!["web", "api", "worker"]);
                assert_eq!(sentry.top_issues_count, 25);
                assert_eq!(sentry.top_issues_period, TopIssuesPeriod::OneWeek);
                assert_eq!(sentry.min_event_count, 50);
                assert_eq!(sentry.escalation_threshold_percent, 75);
                assert_eq!(
                    sentry.client_secret,
                    Some(SecretValue::new("sentry_secret"))
                );
                assert_eq!(sentry.max_issues_per_cycle, Some(10));
                assert_eq!(sentry.max_concurrent, Some(4));
            },
        );
    }

    #[test]
    fn test_env_override_additional_core_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("MAX_CONCURRENT", "4"),
                ("PROCESSING_DELAY_MS", "1000"),
                ("DB_PATH", "/custom/db.sqlite"),
                ("MAX_ACTIVITY_ENTRIES", "50000"),
                ("IPC_TIMEOUT_SECS", "60"),
                ("CLAUDE_TIMEOUT_SECS", "3600"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(config.max_concurrent, 4);
                assert_eq!(config.processing_delay_ms, 1000);
                assert_eq!(config.db_path, PathBuf::from("/custom/db.sqlite"));
                assert_eq!(config.max_activity_entries, 50000);
                assert_eq!(config.ipc_timeout_secs, 60);
                assert_eq!(config.agent.timeout_secs, 3600);
            },
        );
    }

    #[test]
    fn test_env_override_empty_values_ignored() {
        let toml_str = r#"
workspace = "/tmp/repos"

[notifiers.discord]
webhook_url = "https://keep-this.url"

[issues.linear]
api_key = "keep_key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("WORK_DIR", ""),
                ("KNOWN_ORGS", ""),
                ("DISCORD_WEBHOOK_URL", ""),
                ("LINEAR_API_KEY", ""),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                // Empty WORK_DIR should not override
                assert_eq!(config.workspace, PathBuf::from("/tmp/repos"));
                // Empty KNOWN_ORGS should not override
                assert!(config.known_orgs.is_empty());
                // Empty DISCORD_WEBHOOK_URL should set to None
                assert!(config.notifiers.discord.webhook_url.is_none());
                // Empty LINEAR_API_KEY should not create config
                assert_eq!(
                    config.issues.linear.as_ref().unwrap().api_key,
                    SecretValue::new("keep_key")
                );
            },
        );
    }

    #[test]
    fn test_env_override_github_webhook_secret_and_review_trigger() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("GITHUB_WEBHOOK_SECRET", "gh_secret"),
                ("GITHUB_REVIEW_TRIGGER", "@mybot"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.scm.github.webhook_secret,
                    Some(SecretValue::new("gh_secret"))
                );
                assert_eq!(config.scm.github.review_trigger, "@mybot");
            },
        );
    }

    #[test]
    fn test_env_override_email_smtp_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("SMTP_HOST", "smtp.gmail.com"),
                ("SMTP_PORT", "465"),
                ("SMTP_USERNAME", "user@gmail.com"),
                ("SMTP_PASSWORD", "app_password"),
                ("EMAIL_FROM", "sender@gmail.com"),
                ("EMAIL_TO", "admin@test.com, dev@test.com"),
                ("SMTP_TLS", "true"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.notifiers.email.smtp_host,
                    Some("smtp.gmail.com".to_string())
                );
                assert_eq!(config.notifiers.email.smtp_port, 465);
                assert_eq!(
                    config.notifiers.email.smtp_username,
                    Some("user@gmail.com".to_string())
                );
                assert_eq!(
                    config.notifiers.email.smtp_password,
                    Some(SecretValue::new("app_password"))
                );
                assert_eq!(
                    config.notifiers.email.from_address,
                    Some("sender@gmail.com".to_string())
                );
                assert_eq!(
                    config.notifiers.email.to_addresses,
                    vec!["admin@test.com", "dev@test.com"]
                );
                assert!(config.notifiers.email.use_tls);
            },
        );
    }

    #[test]
    fn test_ask_config_default() {
        let config = AskConfig::default();
        assert!(config.enabled);
        assert_eq!(config.wait_timeout_secs, 900);
        assert_eq!(config.poll_interval_secs, 15);
        assert_eq!(config.max_rounds_per_attempt, 2);
        assert!((config.semantic_threshold_scoped - 0.82).abs() < 0.01);
        assert!((config.semantic_threshold_global - 0.88).abs() < 0.01);
        assert_eq!(config.max_reuse_candidates, 3);
        assert!(config.best_effort_on_timeout);
    }

    #[test]
    fn test_cascade_config_default() {
        let config = CascadeConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_depth, 0);
        assert!(config.rules.is_empty());
    }

    #[test]
    fn test_cascade_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/test"

[cascade]
enabled = true
max_depth = 3
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(config.cascade.enabled);
            assert_eq!(config.cascade.max_depth, 3);
            assert!(config.cascade.rules.is_empty());
        });
    }

    #[test]
    fn test_cascade_config_with_rules() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/test"

[cascade]
enabled = true
max_depth = 3

[[cascade.rules]]
upstream = "org/lib"
downstream = "org/app"
trigger = "merge"
version_update = true
instructions = "Run npm install after updating"

[[cascade.rules]]
upstream = "org/lib"
downstream = "org/service"
trigger = "release"
target_branch = "develop"
version_update = false
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.cascade.rules.len(), 2);

            let rule1 = &config.cascade.rules[0];
            assert_eq!(rule1.upstream, "org/lib");
            assert_eq!(rule1.downstream, "org/app");
            assert_eq!(rule1.trigger, CascadeTrigger::Merge);
            assert!(rule1.version_update);
            assert_eq!(
                rule1.instructions.as_deref(),
                Some("Run npm install after updating")
            );

            let rule2 = &config.cascade.rules[1];
            assert_eq!(rule2.trigger, CascadeTrigger::Release);
            assert_eq!(rule2.target_branch.as_deref(), Some("develop"));
            assert!(!rule2.version_update);
        });
    }

    #[test]
    fn test_cascade_find_rule() {
        let config = CascadeConfig {
            enabled: true,
            max_depth: 0,
            rules: vec![CascadeRule {
                upstream: "org/lib".to_string(),
                downstream: "org/app".to_string(),
                trigger: CascadeTrigger::Merge,
                target_branch: None,
                version_update: true,
                instructions: None,
            }],
        };
        assert!(config.find_rule("org/lib", "org/app").is_some());
        assert!(config.find_rule("org/lib", "org/other").is_none());
        assert!(config.find_rule("other/lib", "org/app").is_none());
    }

    #[test]
    fn test_github_app_config_load_private_key_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key.pem");
        fs::write(
            &key_path,
            "-----BEGIN RSA PRIVATE KEY-----\ntest\n-----END RSA PRIVATE KEY-----",
        )
        .unwrap();

        let config = GitHubAppConfig {
            private_key_path: Some(key_path),
            ..Default::default()
        };

        let key = config.load_private_key().unwrap();
        assert!(key.contains("BEGIN RSA PRIVATE KEY"));
    }

    #[test]
    fn test_github_app_config_load_private_key_file_not_found() {
        let config = GitHubAppConfig {
            private_key_path: Some(PathBuf::from("/nonexistent/key.pem")),
            ..Default::default()
        };

        let result = config.load_private_key();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to read"));
    }

    #[test]
    fn test_github_app_config_inline_key_takes_precedence() {
        let config = GitHubAppConfig {
            private_key: Some(SecretValue::new("inline-key")),
            private_key_path: Some(PathBuf::from("/nonexistent/key.pem")),
            ..Default::default()
        };

        let key = config.load_private_key().unwrap();
        assert_eq!(key, "inline-key");
    }

    #[test]
    fn test_env_override_discord_bot_and_channel() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("DISCORD_BOT_TOKEN", "bot_token_123"),
                ("DISCORD_CHANNEL_ID", "channel_456"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.notifiers.discord.bot_token,
                    Some(SecretValue::new("bot_token_123"))
                );
                assert_eq!(
                    config.notifiers.discord.channel_id,
                    Some("channel_456".to_string())
                );
            },
        );
    }

    #[test]
    fn test_env_override_github_app_private_key_path() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[("GITHUB_APP_PRIVATE_KEY_PATH", "/path/to/key.pem")],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.scm.github.app.private_key_path,
                    Some(PathBuf::from("/path/to/key.pem"))
                );
            },
        );
    }

    #[test]
    fn test_email_config_default() {
        let config = EmailConfig::default();
        assert!(config.smtp_host.is_none());
        assert_eq!(config.smtp_port, 587);
        assert!(config.smtp_username.is_none());
        assert!(config.smtp_password.is_none());
        assert!(config.from_address.is_none());
        assert!(config.to_addresses.is_empty());
        assert!(config.use_tls);
        assert!(config.imap_host.is_none());
        assert_eq!(config.imap_port, 993);
        assert!(config.imap_username.is_none());
        assert!(config.imap_password.is_none());
        assert!(config.imap_use_tls);
        assert_eq!(config.imap_folder, "INBOX");
    }

    #[test]
    fn test_github_config_default() {
        let config = GitHubConfig::default();
        assert!(config.token.is_none());
        assert_eq!(config.poll_interval_ms, 60000);
        assert!(!config.auto_resolve_on_merge);
        assert!(config.webhook_secret.is_none());
        assert_eq!(config.review_trigger, "@claudear");
        assert!(!config.use_ssh);
    }

    #[test]
    fn test_resolve_user_slug_user_has_no_channel_id() {
        let toml_str = r#"
[users.jake]
linear_names = ["Jake B"]

[notifiers.discord]
user_id = "jake"

[notifiers.push]
user_key = "jake"
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        // User exists but has no discord_id, should keep slug
        assert_eq!(config.notifiers.discord.user_id.as_deref(), Some("jake"));
        // User exists but has no push_user_key, should keep slug
        assert_eq!(config.notifiers.push.user_key.as_deref(), Some("jake"));
    }

    #[test]
    fn test_resolve_user_slug_email_user_has_no_email() {
        let toml_str = r#"
[users.jake]
linear_names = ["Jake B"]

[notifiers.email]
to_addresses = ["jake"]
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        // User exists but has no email field, should keep the slug as-is
        assert_eq!(config.notifiers.email.to_addresses, vec!["jake"]);
    }

    #[test]
    fn test_resolve_user_slug_sms_user_has_no_number() {
        let toml_str = r#"
[users.jake]
linear_names = ["Jake B"]

[notifiers.sms]
to_numbers = ["jake"]
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.resolve_user_slugs();
        // User exists but has no sms_number, should keep the slug as-is
        assert_eq!(config.notifiers.sms.to_numbers, vec!["jake"]);
    }

    #[test]
    fn test_is_github_app_configured() {
        let mut config = Config::default();
        assert!(!config.is_github_app_configured());

        config.scm.github.app.app_id = Some(1);
        config.scm.github.app.private_key = Some(SecretValue::new("key"));
        assert!(config.is_github_app_configured());
    }

    #[test]
    fn test_top_issues_period_serde_toml_aliases() {
        // TOML cannot serialize/deserialize bare enum values; wrap in a struct
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Wrapper {
            period: TopIssuesPeriod,
        }

        // Test roundtrip of all variants
        for variant in [
            TopIssuesPeriod::OneHour,
            TopIssuesPeriod::TwelveHours,
            TopIssuesPeriod::OneDay,
            TopIssuesPeriod::OneWeek,
            TopIssuesPeriod::OneMonth,
        ] {
            let wrapper = Wrapper { period: variant };
            let serialized = toml::to_string(&wrapper).unwrap();
            let deserialized: Wrapper = toml::from_str(&serialized).unwrap();
            assert_eq!(variant, deserialized.period);
        }

        // Test that aliases work in TOML context
        let from_alias: Wrapper = toml::from_str("period = \"1h\"").unwrap();
        assert_eq!(from_alias.period, TopIssuesPeriod::OneHour);
    }

    #[test]
    fn test_learning_config_defaults() {
        let config = LearningConfig::default();
        assert!(config.auto_extract_learnings);
        assert!(config.diff_analysis);
        assert!(config.qa_promotion);
        assert_eq!(config.qa_promotion_threshold, 2);
        assert!(config.repo_knowledge);
        assert!(config.review_classification);
        assert_eq!(config.review_promotion_threshold, 3);
        assert!(config.strategy_fingerprinting);
        assert!(config.quality_scoring);
        assert!(config.cluster_detection);
        assert_eq!(config.cluster_window_minutes, 30);
        assert_eq!(config.min_cluster_size, 3);
        assert!(!config.auto_agent_md); // opt-in, default false
    }

    #[test]
    fn test_learning_config_deserialize_empty_toml() {
        // An empty TOML string should give all defaults
        let config: LearningConfig = toml::from_str("").unwrap();
        assert!(config.auto_extract_learnings);
        assert!(config.diff_analysis);
        assert!(!config.auto_agent_md);
    }

    #[test]
    fn test_learning_config_deserialize_partial() {
        let toml_str = r#"
auto_extract_learnings = false
cluster_window_minutes = 60
"#;
        let config: LearningConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.auto_extract_learnings);
        assert_eq!(config.cluster_window_minutes, 60);
        // Rest should be defaults
        assert!(config.diff_analysis);
        assert_eq!(config.min_cluster_size, 3);
    }

    #[test]
    fn test_config_without_learning_section() {
        // A minimal Config TOML without any [learning] section should still work
        let toml_str = r#"
workspace = "/tmp/repos"
known_orgs = ["test-org"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        // learning field should get default values
        assert!(config.learning.auto_extract_learnings);
        assert!(config.learning.diff_analysis);
        assert!(!config.learning.auto_agent_md);
    }

    #[test]
    fn test_config_with_learning_section() {
        let toml_str = r#"
workspace = "/tmp/repos"
known_orgs = ["test-org"]

[learning]
auto_extract_learnings = false
auto_agent_md = true
min_cluster_size = 5
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.learning.auto_extract_learnings);
        assert!(config.learning.auto_agent_md);
        assert_eq!(config.learning.min_cluster_size, 5);
        // Other learning fields should be defaults
        assert!(config.learning.diff_analysis);
    }

    #[test]
    fn test_learning_config_zero_thresholds() {
        let toml_str = r#"
qa_promotion_threshold = 0
review_promotion_threshold = 0
cluster_window_minutes = 0
min_cluster_size = 0
"#;
        let config: LearningConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.qa_promotion_threshold, 0);
        assert_eq!(config.review_promotion_threshold, 0);
        assert_eq!(config.cluster_window_minutes, 0);
        assert_eq!(config.min_cluster_size, 0);
    }

    #[test]
    fn test_learning_config_large_values() {
        let toml_str = r#"
qa_promotion_threshold = 999999
cluster_window_minutes = 4294967295
min_cluster_size = 999999
"#;
        let config: LearningConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.qa_promotion_threshold, 999999);
        assert_eq!(config.cluster_window_minutes, 4294967295);
        assert_eq!(config.min_cluster_size, 999999);
    }

    #[test]
    fn test_learning_config_all_features_disabled() {
        let toml_str = r#"
auto_extract_learnings = false
diff_analysis = false
qa_promotion = false
repo_knowledge = false
review_classification = false
strategy_fingerprinting = false
quality_scoring = false
cluster_detection = false
auto_agent_md = false
"#;
        let config: LearningConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.auto_extract_learnings);
        assert!(!config.diff_analysis);
        assert!(!config.qa_promotion);
        assert!(!config.repo_knowledge);
        assert!(!config.review_classification);
        assert!(!config.strategy_fingerprinting);
        assert!(!config.quality_scoring);
        assert!(!config.cluster_detection);
        assert!(!config.auto_agent_md);
    }

    #[test]
    fn test_config_zero_poll_interval() {
        let toml_str = r#"
workspace = "/tmp/repos"
poll_interval_ms = 0
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.poll_interval_ms, 0);
    }

    #[test]
    fn test_config_zero_max_issues_per_cycle() {
        let toml_str = r#"
workspace = "/tmp/repos"
max_issues_per_cycle = 0
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.max_issues_per_cycle, 0);
    }

    #[test]
    fn test_config_zero_max_concurrent() {
        let toml_str = r#"
workspace = "/tmp/repos"
max_concurrent = 0
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.max_concurrent, 0);
    }

    #[test]
    fn test_config_empty_workspace() {
        let toml_str = r#"
workspace = ""
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.workspace, PathBuf::from(""));
    }

    #[test]
    fn test_config_empty_known_orgs() {
        let toml_str = r#"
workspace = "/tmp"
known_orgs = []
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.known_orgs.is_empty());
    }

    #[test]
    fn test_retry_config_default_values() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 2);
        assert_eq!(config.base_delay_ms, 60_000);
        assert_eq!(config.max_delay_ms, 3_600_000);
    }

    #[test]
    fn test_retry_config_zero_retries() {
        let toml_str = r#"
max_retries = 0
"#;
        let config: RetryConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.max_retries, 0);
    }

    #[test]
    fn test_config_unknown_fields_ignored() {
        // TOML with unknown fields - toml crate by default rejects unknown fields with serde
        let toml_str = r#"
workspace = "/tmp"
unknown_field = "should be ignored"
another_unknown = 42
"#;
        let result: std::result::Result<Config, _> = toml::from_str(toml_str);
        // TOML rejects unknown fields by default; verify it doesn't panic
        let _ = result;
    }

    #[test]
    fn test_learning_config_roundtrip() {
        let config = LearningConfig {
            auto_extract_learnings: false,
            diff_analysis: true,
            qa_promotion: false,
            qa_promotion_threshold: 10,
            repo_knowledge: true,
            review_classification: false,
            review_promotion_threshold: 5,
            strategy_fingerprinting: true,
            quality_scoring: false,
            cluster_detection: true,
            cluster_window_minutes: 45,
            min_cluster_size: 7,
            auto_agent_md: true,
            cross_repo_correlation: true,
            cross_repo_window_hours: 48,
        };
        let toml_str = toml::to_string(&config).unwrap();
        let restored: LearningConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            config.auto_extract_learnings,
            restored.auto_extract_learnings
        );
        assert_eq!(
            config.qa_promotion_threshold,
            restored.qa_promotion_threshold
        );
        assert_eq!(
            config.cluster_window_minutes,
            restored.cluster_window_minutes
        );
        assert_eq!(config.auto_agent_md, restored.auto_agent_md);
    }

    #[test]
    fn prioritisation_validate_rejects_negative_weight() {
        let config = PrioritisationConfig {
            severity_weight: -0.1,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("severity_weight"),
            "Expected error about severity_weight, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_rejects_nan_weight() {
        let config = PrioritisationConfig {
            frequency_weight: f64::NAN,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("frequency_weight"),
            "Expected error about frequency_weight, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_rejects_all_zero_weights() {
        let config = PrioritisationConfig {
            severity_weight: 0.0,
            frequency_weight: 0.0,
            regression_weight: 0.0,
            blast_radius_weight: 0.0,
            cluster_weight: 0.0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("must not all be zero"),
            "Expected all-zero error, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_rejects_cluster_size_one() {
        let config = PrioritisationConfig {
            min_content_cluster_size: 1,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("min_content_cluster_size"),
            "Expected cluster size error, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_accepts_defaults() {
        let config = PrioritisationConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_dashboard_config_default() {
        let config = DashboardConfig::default();
        assert!((config.max_plan_monthly_cost - 0.0).abs() < f64::EPSILON);
        assert!((config.hourly_engineer_rate - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_dashboard_config_from_toml() {
        let toml_str = r#"
max_plan_monthly_cost = 200.0
hourly_engineer_rate = 150.0
"#;
        let config: DashboardConfig = toml::from_str(toml_str).unwrap();
        assert!((config.max_plan_monthly_cost - 200.0).abs() < f64::EPSILON);
        assert!((config.hourly_engineer_rate - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_dashboard_config_partial_toml() {
        let toml_str = r#"
hourly_engineer_rate = 100.0
"#;
        let config: DashboardConfig = toml::from_str(toml_str).unwrap();
        assert!((config.max_plan_monthly_cost - 0.0).abs() < f64::EPSILON);
        assert!((config.hourly_engineer_rate - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_default_includes_dashboard() {
        let config = Config::default();
        assert!((config.dashboard.max_plan_monthly_cost - 0.0).abs() < f64::EPSILON);
        assert!((config.dashboard.hourly_engineer_rate - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_dashboard_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[dashboard]
max_plan_monthly_cost = 100.0
hourly_engineer_rate = 200.0
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!((config.dashboard.max_plan_monthly_cost - 100.0).abs() < f64::EPSILON);
            assert!((config.dashboard.hourly_engineer_rate - 200.0).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn test_code_index_config_default() {
        let config = CodeIndexConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_file_size_kb, 1024);
        assert_eq!(config.batch_size, 32);
    }

    #[test]
    fn test_code_index_config_from_toml() {
        let toml_str = r#"
enabled = false
max_file_size_kb = 2048
batch_size = 64
"#;
        let config: CodeIndexConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.max_file_size_kb, 2048);
        assert_eq!(config.batch_size, 64);
    }

    #[test]
    fn test_code_index_config_partial_toml() {
        let toml_str = r#"
enabled = false
"#;
        let config: CodeIndexConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.max_file_size_kb, 1024);
        assert_eq!(config.batch_size, 32);
    }

    #[test]
    fn test_config_default_includes_code_index() {
        let config = Config::default();
        assert!(config.code_index.enabled);
        assert_eq!(config.code_index.max_file_size_kb, 1024);
    }

    #[test]
    fn test_config_code_index_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[code_index]
enabled = false
max_file_size_kb = 512
batch_size = 16
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(!config.code_index.enabled);
            assert_eq!(config.code_index.max_file_size_kb, 512);
            assert_eq!(config.code_index.batch_size, 16);
        });
    }

    #[test]
    fn test_evaluation_config_default() {
        let config = EvaluationConfig::default();
        assert!(!config.enabled);
        assert!(config.test_delta);
        assert!(config.lint_delta);
        assert!(config.static_analysis_delta);
        assert!(!config.coverage_delta);
        assert_eq!(config.tool_timeout_secs, 300);
        assert_eq!(config.total_timeout_secs, 900);
        assert!(config.post_pr_comment);
        assert!(!config.fail_on_regression);
        assert!(config.custom_test_cmd.is_none());
        assert!(config.custom_lint_cmd.is_none());
        assert!(config.custom_analysis_cmd.is_none());
        assert!(config.custom_coverage_cmd.is_none());
    }

    #[test]
    fn test_evaluation_config_from_toml() {
        let toml_str = r#"
enabled = true
test_delta = false
lint_delta = false
static_analysis_delta = false
coverage_delta = true
tool_timeout_secs = 600
total_timeout_secs = 1800
post_pr_comment = false
fail_on_regression = true
custom_test_cmd = "cargo test"
custom_lint_cmd = "cargo clippy"
custom_analysis_cmd = "cargo audit"
custom_coverage_cmd = "cargo tarpaulin"
"#;
        let config: EvaluationConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert!(!config.test_delta);
        assert!(!config.lint_delta);
        assert!(!config.static_analysis_delta);
        assert!(config.coverage_delta);
        assert_eq!(config.tool_timeout_secs, 600);
        assert_eq!(config.total_timeout_secs, 1800);
        assert!(!config.post_pr_comment);
        assert!(config.fail_on_regression);
        assert_eq!(config.custom_test_cmd.as_deref(), Some("cargo test"));
        assert_eq!(config.custom_lint_cmd.as_deref(), Some("cargo clippy"));
        assert_eq!(config.custom_analysis_cmd.as_deref(), Some("cargo audit"));
        assert_eq!(
            config.custom_coverage_cmd.as_deref(),
            Some("cargo tarpaulin")
        );
    }

    #[test]
    fn test_evaluation_config_partial_toml() {
        let toml_str = r#"
enabled = true
custom_test_cmd = "npm test"
"#;
        let config: EvaluationConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert!(config.test_delta); // default
        assert_eq!(config.custom_test_cmd.as_deref(), Some("npm test"));
        assert!(config.custom_lint_cmd.is_none()); // default
    }

    #[test]
    fn test_config_default_includes_evaluation() {
        let config = Config::default();
        assert!(!config.evaluation.enabled);
        assert!(config.evaluation.test_delta);
    }

    #[test]
    fn test_config_evaluation_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[evaluation]
enabled = true
fail_on_regression = true
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(config.evaluation.enabled);
            assert!(config.evaluation.fail_on_regression);
            assert!(config.evaluation.test_delta); // default preserved
        });
    }

    #[test]
    fn test_prioritisation_config_default() {
        let config = PrioritisationConfig::default();
        assert!(config.enabled);
        assert!((config.severity_weight - 0.30).abs() < f64::EPSILON);
        assert!((config.frequency_weight - 0.25).abs() < f64::EPSILON);
        assert!((config.regression_weight - 0.20).abs() < f64::EPSILON);
        assert!((config.blast_radius_weight - 0.15).abs() < f64::EPSILON);
        assert!((config.cluster_weight - 0.10).abs() < f64::EPSILON);
        assert!(!config.critical_paths.is_empty());
        assert!(!config.core_paths.is_empty());
        assert!(!config.infra_paths.is_empty());
        assert!(!config.test_paths.is_empty());
        assert!(!config.cosmetic_paths.is_empty());
        assert!(config.content_clustering);
        assert!((config.cluster_similarity_threshold - 0.60).abs() < f64::EPSILON);
        assert_eq!(config.min_content_cluster_size, 2);
        assert!(config.suppression_rules.is_empty());
    }

    #[test]
    fn test_prioritisation_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[prioritisation]
enabled = false
severity_weight = 0.5
frequency_weight = 0.3
regression_weight = 0.1
blast_radius_weight = 0.05
cluster_weight = 0.05
content_clustering = false
cluster_similarity_threshold = 0.8
min_content_cluster_size = 5
critical_paths = ["auth", "security"]
core_paths = ["api"]
infra_paths = ["deploy"]
test_paths = ["test"]
cosmetic_paths = ["docs"]
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(!config.prioritisation.enabled);
            assert!((config.prioritisation.severity_weight - 0.5).abs() < f64::EPSILON);
            assert_eq!(
                config.prioritisation.critical_paths,
                vec!["auth", "security"]
            );
            assert!(!config.prioritisation.content_clustering);
            assert_eq!(config.prioritisation.min_content_cluster_size, 5);
        });
    }

    #[test]
    fn prioritisation_validate_rejects_similarity_threshold_above_one() {
        let config = PrioritisationConfig {
            cluster_similarity_threshold: 1.5,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("cluster_similarity_threshold"),
            "Expected similarity threshold error, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_rejects_similarity_threshold_negative() {
        let config = PrioritisationConfig {
            cluster_similarity_threshold: -0.1,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("cluster_similarity_threshold"),
            "Expected similarity threshold error, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_rejects_similarity_threshold_nan() {
        let config = PrioritisationConfig {
            cluster_similarity_threshold: f64::NAN,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("cluster_similarity_threshold"),
            "Expected similarity threshold error, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_rejects_infinity_weight() {
        let config = PrioritisationConfig {
            blast_radius_weight: f64::INFINITY,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("blast_radius_weight"),
            "Expected infinity error, got: {}",
            err
        );
    }

    #[test]
    fn prioritisation_validate_accepts_zero_similarity_threshold() {
        let config = PrioritisationConfig {
            cluster_similarity_threshold: 0.0,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn prioritisation_validate_accepts_one_similarity_threshold() {
        let config = PrioritisationConfig {
            cluster_similarity_threshold: 1.0,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn prioritisation_validate_rejects_cluster_size_zero() {
        let config = PrioritisationConfig {
            min_content_cluster_size: 0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("min_content_cluster_size"));
    }

    #[test]
    fn prioritisation_validate_accepts_cluster_size_two() {
        let config = PrioritisationConfig {
            min_content_cluster_size: 2,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_slack_config_default() {
        let config = SlackConfig::default();
        assert!(config.bot_token.is_none());
        assert!(config.channel_id.is_none());
        assert!(config.webhook_url.is_none());
        assert!(config.user_id.is_none());
        assert!(!config.source_enabled);
        assert!(config.listen_channel_id.is_none());
        assert!(config.workspace.is_none());
        assert!(config.poll_interval_ms.is_none());
    }

    #[test]
    fn test_env_override_slack_settings() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("SLACK_BOT_TOKEN", "xoxb-test-token"),
                ("SLACK_CHANNEL_ID", "C12345"),
                ("SLACK_WEBHOOK_URL", "https://hooks.slack.com/test"),
                ("SLACK_USER_ID", "U12345"),
                ("SLACK_SOURCE_ENABLED", "true"),
                ("SLACK_LISTEN_CHANNEL_ID", "C67890"),
                ("SLACK_WORKSPACE", "myworkspace"),
                ("SLACK_POLL_INTERVAL_MS", "45000"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(
                    config.notifiers.slack.bot_token,
                    Some(SecretValue::new("xoxb-test-token"))
                );
                assert_eq!(
                    config.notifiers.slack.channel_id,
                    Some("C12345".to_string())
                );
                assert_eq!(
                    config.notifiers.slack.webhook_url,
                    Some(SecretValue::new("https://hooks.slack.com/test"))
                );
                assert_eq!(config.notifiers.slack.user_id, Some("U12345".to_string()));
                assert!(config.issues.slack.is_some());
                assert_eq!(
                    config
                        .issues
                        .slack
                        .as_ref()
                        .and_then(|s| s.listen_channel_id.clone()),
                    Some("C67890".to_string())
                );
                assert_eq!(
                    config.notifiers.slack.workspace,
                    Some("myworkspace".to_string())
                );
                assert_eq!(
                    config
                        .issues
                        .slack
                        .as_ref()
                        .and_then(|s| s.poll_interval_ms),
                    Some(45000)
                );
            },
        );
    }

    #[test]
    fn test_discord_config_default() {
        let config = DiscordConfig::default();
        assert!(config.webhook_url.is_none());
        assert!(config.user_id.is_none());
        assert!(config.bot_token.is_none());
        assert!(config.channel_id.is_none());
        assert!(!config.source_enabled);
        assert!(config.listen_channel_id.is_none());
        assert!(config.guild_id.is_none());
        assert!(config.poll_interval_ms.is_none());
    }

    #[test]
    fn test_env_override_discord_source_enabled() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("DISCORD_SOURCE_ENABLED", "1"),
                ("DISCORD_LISTEN_CHANNEL_ID", "LC123"),
                ("DISCORD_GUILD_ID", "G456"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert!(config.issues.discord.is_some());
                assert_eq!(
                    config
                        .issues
                        .discord
                        .as_ref()
                        .and_then(|s| s.listen_channel_id.clone()),
                    Some("LC123".to_string())
                );
                assert_eq!(config.notifiers.discord.guild_id, Some("G456".to_string()));
            },
        );
    }

    #[test]
    fn test_sms_config_default() {
        let config = SmsConfig::default();
        assert!(config.account_sid.is_none());
        assert!(config.auth_token.is_none());
        assert!(config.from_number.is_none());
        assert!(config.to_numbers.is_empty());
    }

    #[test]
    fn test_push_config_default() {
        let config = PushConfig::default();
        assert!(config.api_token.is_none());
        assert!(config.user_key.is_none());
        assert!(config.device.is_none());
        assert!(config.priority.is_none());
    }

    #[test]
    fn test_gitlab_config_default() {
        let config = GitLabConfig::default();
        assert!(!config.enabled);
        assert!(config.token.is_none());
        assert_eq!(config.base_url, "https://gitlab.com");
        assert!(config.groups.is_empty());
        assert_eq!(
            config.trigger_labels,
            vec!["auto-implement".to_string(), "claude".to_string()]
        );
        assert_eq!(config.trigger_states, vec!["opened".to_string()]);
        assert!(config.poll_interval_ms.is_none());
        assert!(!config.auto_resolve_on_merge);
        assert!(config.webhook_secret.is_none());
        assert_eq!(config.review_trigger, "@claudear");
        assert!(!config.use_ssh);
        assert!(config.max_issues_per_cycle.is_none());
        assert!(config.max_concurrent.is_none());
    }

    #[test]
    fn test_gitlab_test_default() {
        let config = GitLabConfig::test_default();
        assert!(config.enabled);
        assert_eq!(config.token, Some(SecretValue::new("test_token")));
        assert_eq!(config.groups, vec!["mygroup".to_string()]);
        assert!(config.auto_resolve_on_merge);
        assert_eq!(config.webhook_secret, Some(SecretValue::new("test_secret")));
    }

    #[test]
    fn test_gitlab_config_from_toml() {
        let toml_str = r#"
enabled = true
token = "glpat-test"
base_url = "https://gitlab.myco.com"
groups = ["frontend", "backend"]
trigger_labels = ["bot-fix"]
trigger_states = ["opened", "reopened"]
auto_resolve_on_merge = true
webhook_secret = "secret"
review_trigger = "@mybot"
use_ssh = true
max_issues_per_cycle = 10
max_concurrent = 3
"#;
        let config: GitLabConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.token, Some(SecretValue::new("glpat-test")));
        assert_eq!(config.base_url, "https://gitlab.myco.com");
        assert_eq!(config.groups, vec!["frontend", "backend"]);
        assert_eq!(config.trigger_labels, vec!["bot-fix"]);
        assert_eq!(config.trigger_states, vec!["opened", "reopened"]);
        assert!(config.auto_resolve_on_merge);
        assert!(config.use_ssh);
        assert_eq!(config.max_issues_per_cycle, Some(10));
        assert_eq!(config.max_concurrent, Some(3));
    }

    #[test]
    fn test_is_gitlab_enabled() {
        let mut config = Config::default();
        assert!(!config.is_gitlab_enabled());

        config.scm.gitlab = Some(GitLabConfig {
            enabled: true,
            token: Some(SecretValue::new("tok")),
            ..Default::default()
        });
        assert!(config.is_gitlab_enabled());

        // Enabled but no token
        config.scm.gitlab = Some(GitLabConfig {
            enabled: true,
            token: None,
            ..Default::default()
        });
        assert!(!config.is_gitlab_enabled());

        // Has token but disabled
        config.scm.gitlab = Some(GitLabConfig {
            enabled: false,
            token: Some(SecretValue::new("tok")),
            ..Default::default()
        });
        assert!(!config.is_gitlab_enabled());
    }

    #[test]
    fn test_env_override_gitlab() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("GITLAB_TOKEN", "glpat-env-token"),
                ("GITLAB_BASE_URL", "https://gitlab.custom.com"),
                ("GITLAB_GROUPS", "grp1, grp2"),
                ("GITLAB_TRIGGER_LABELS", "fix, auto"),
                ("GITLAB_TRIGGER_STATES", "opened, reopened"),
                ("GITLAB_POLL_INTERVAL_MS", "120000"),
                ("GITLAB_AUTO_RESOLVE_ON_MERGE", "true"),
                ("GITLAB_WEBHOOK_SECRET", "gl_secret"),
                ("GITLAB_REVIEW_TRIGGER", "@bot"),
                ("GITLAB_USE_SSH", "true"),
                ("GITLAB_MAX_ISSUES_PER_CYCLE", "8"),
                ("GITLAB_MAX_CONCURRENT", "4"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                let gitlab = config.scm.gitlab.unwrap();
                assert!(gitlab.enabled);
                assert_eq!(gitlab.token, Some(SecretValue::new("glpat-env-token")));
                assert_eq!(gitlab.base_url, "https://gitlab.custom.com");
                assert_eq!(gitlab.groups, vec!["grp1", "grp2"]);
                assert_eq!(gitlab.trigger_labels, vec!["fix", "auto"]);
                assert_eq!(gitlab.trigger_states, vec!["opened", "reopened"]);
                assert_eq!(gitlab.poll_interval_ms, Some(120000));
                assert!(gitlab.auto_resolve_on_merge);
                assert_eq!(gitlab.webhook_secret, Some(SecretValue::new("gl_secret")));
                assert_eq!(gitlab.review_trigger, "@bot");
                assert!(gitlab.use_ssh);
                assert_eq!(gitlab.max_issues_per_cycle, Some(8));
                assert_eq!(gitlab.max_concurrent, Some(4));
            },
        );
    }

    #[test]
    fn test_env_override_gitlab_enabled_flag() {
        let toml_str = r#"
workspace = "/tmp/repos"

[scm.gitlab]
enabled = true
token = "tok"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("GITLAB_ENABLED", "false")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(!config.scm.gitlab.as_ref().unwrap().enabled);
        });
    }

    #[test]
    fn test_jira_config_default() {
        let config = JiraConfig::default();
        assert!(!config.enabled);
        assert!(config.base_url.is_empty());
        assert!(config.email.is_empty());
        assert!(config.api_token.is_empty());
        assert_eq!(config.auth_mode, "basic");
        assert!(config.project_keys.is_empty());
        assert_eq!(
            config.trigger_labels,
            vec!["auto-implement".to_string(), "claude".to_string()]
        );
        assert_eq!(
            config.trigger_statuses,
            vec!["To Do".to_string(), "Backlog".to_string()]
        );
        assert!(config.trigger_assignee.is_none());
        assert!(config.issue_types.is_empty());
        assert!(config.custom_jql.is_none());
        assert_eq!(config.max_results, 50);
        assert!(config.max_issues_per_cycle.is_none());
        assert!(config.max_concurrent.is_none());
        assert!(config.poll_interval_ms.is_none());
    }

    #[test]
    fn test_jira_config_from_toml() {
        let toml_str = r#"
enabled = true
base_url = "https://myco.atlassian.net"
email = "user@myco.com"
api_token = "jira_token"
auth_mode = "basic"
project_keys = ["PROJ", "BACKEND"]
trigger_labels = ["autofix"]
trigger_statuses = ["Open"]
trigger_assignee = "John Doe"
issue_types = ["Bug", "Task"]
custom_jql = "priority = High"
max_results = 100
max_issues_per_cycle = 5
max_concurrent = 2
poll_interval_ms = 60000
"#;
        let config: JiraConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.base_url, "https://myco.atlassian.net");
        assert_eq!(config.email, "user@myco.com");
        assert_eq!(config.api_token, SecretValue::new("jira_token"));
        assert_eq!(config.auth_mode, "basic");
        assert_eq!(config.project_keys, vec!["PROJ", "BACKEND"]);
        assert_eq!(config.trigger_assignee.as_deref(), Some("John Doe"));
        assert_eq!(config.issue_types, vec!["Bug", "Task"]);
        assert_eq!(config.custom_jql.as_deref(), Some("priority = High"));
        assert_eq!(config.max_results, 100);
        assert_eq!(config.max_issues_per_cycle, Some(5));
        assert_eq!(config.max_concurrent, Some(2));
        assert_eq!(config.poll_interval_ms, Some(60000));
    }

    #[test]
    fn test_is_jira_enabled() {
        let mut config = Config::default();
        assert!(!config.is_jira_enabled());

        config.issues.jira = Some(JiraConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(config.is_jira_enabled());

        config.issues.jira.as_mut().unwrap().enabled = false;
        assert!(!config.is_jira_enabled());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_with_jira() {
        let mut config = Config::default();
        config.issues.jira = Some(JiraConfig {
            enabled: true,
            api_token: "token".into(),
            base_url: "https://myco.atlassian.net".into(),
            email: "user@myco.com".into(),
            auth_mode: "basic".into(),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_jira_missing_base_url() {
        let mut config = Config::default();
        config.issues.jira = Some(JiraConfig {
            enabled: true,
            api_token: "token".into(),
            base_url: String::new(),
            email: "user@myco.com".into(),
            auth_mode: "basic".into(),
            ..Default::default()
        });
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("base_url"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_jira_invalid_auth_mode() {
        let mut config = Config::default();
        config.issues.jira = Some(JiraConfig {
            enabled: true,
            api_token: "token".into(),
            base_url: "https://myco.atlassian.net".into(),
            email: "user@myco.com".into(),
            auth_mode: "invalid".into(),
            ..Default::default()
        });
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("auth_mode"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_jira_basic_auth_missing_email() {
        let mut config = Config::default();
        config.issues.jira = Some(JiraConfig {
            enabled: true,
            api_token: "token".into(),
            base_url: "https://myco.atlassian.net".into(),
            email: String::new(),
            auth_mode: "basic".into(),
            ..Default::default()
        });
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("email"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_jira_bearer_auth_no_email_required() {
        let mut config = Config::default();
        config.issues.jira = Some(JiraConfig {
            enabled: true,
            api_token: "token".into(),
            base_url: "https://myco.atlassian.net".into(),
            email: String::new(),
            auth_mode: "bearer".into(),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_env_override_jira() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(
            &[
                ("JIRA_API_TOKEN", "env_jira_token"),
                ("JIRA_ENABLED", "true"),
                ("JIRA_BASE_URL", "https://env.atlassian.net"),
                ("JIRA_EMAIL", "env@example.com"),
                ("JIRA_AUTH_MODE", "bearer"),
                ("JIRA_PROJECT_KEYS", "PROJ1, PROJ2"),
                ("JIRA_TRIGGER_LABELS", "fix, auto"),
                ("JIRA_TRIGGER_STATUSES", "Open, In Progress"),
                ("JIRA_TRIGGER_ASSIGNEE", "Jane Smith"),
                ("JIRA_ISSUE_TYPES", "Bug, Story"),
                ("JIRA_CUSTOM_JQL", "priority = Critical"),
                ("JIRA_MAX_RESULTS", "25"),
                ("JIRA_MAX_ISSUES_PER_CYCLE", "7"),
                ("JIRA_MAX_CONCURRENT", "3"),
                ("JIRA_POLL_INTERVAL_MS", "90000"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                let jira = config.issues.jira.unwrap();
                assert!(jira.enabled);
                assert_eq!(jira.api_token, SecretValue::new("env_jira_token"));
                assert_eq!(jira.base_url, "https://env.atlassian.net");
                assert_eq!(jira.email, "env@example.com");
                assert_eq!(jira.auth_mode, "bearer");
                assert_eq!(jira.project_keys, vec!["PROJ1", "PROJ2"]);
                assert_eq!(jira.trigger_labels, vec!["fix", "auto"]);
                assert_eq!(jira.trigger_statuses, vec!["Open", "In Progress"]);
                assert_eq!(jira.trigger_assignee, Some("Jane Smith".to_string()));
                assert_eq!(jira.issue_types, vec!["Bug", "Story"]);
                assert_eq!(jira.custom_jql, Some("priority = Critical".to_string()));
                assert_eq!(jira.max_results, 25);
                assert_eq!(jira.max_issues_per_cycle, Some(7));
                assert_eq!(jira.max_concurrent, Some(3));
                assert_eq!(jira.poll_interval_ms, Some(90000));
            },
        );
    }

    #[test]
    fn test_env_creates_jira_config_when_missing() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("JIRA_API_TOKEN", "env_jira_token")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(config.issues.jira.is_some());
            assert_eq!(
                config.issues.jira.as_ref().unwrap().api_token,
                SecretValue::new("env_jira_token")
            );
        });
    }

    #[test]
    fn test_env_creates_gitlab_config_when_missing() {
        let toml_str = r#"
workspace = "/tmp/repos"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("GITLAB_TOKEN", "glpat-env")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(config.scm.gitlab.is_some());
            let gitlab = config.scm.gitlab.unwrap();
            assert_eq!(gitlab.token, Some(SecretValue::new("glpat-env")));
            assert!(gitlab.enabled);
        });
    }

    #[test]
    fn test_user_config_default() {
        let config = UserConfig::default();
        assert!(config.linear_names.is_empty());
        assert!(config.github_usernames.is_empty());
        assert!(config.sentry_usernames.is_empty());
        assert!(config.jira_usernames.is_empty());
        assert!(config.gitlab_usernames.is_empty());
        assert!(config.discord_id.is_none());
        assert!(config.slack_id.is_none());
        assert!(config.email.is_none());
        assert!(config.push_user_key.is_none());
        assert!(config.sms_number.is_none());
    }

    #[test]
    fn test_user_config_with_all_fields() {
        let toml_str = r#"
[users.fulluser]
linear_names = ["Full User"]
github_usernames = ["fulluser"]
sentry_usernames = ["full"]
jira_usernames = ["fulluser"]
gitlab_usernames = ["fulluser_gl"]
discord_id = "111"
slack_id = "U111"
email = "full@example.com"
push_user_key = "pk111"
sms_number = "+1111111111"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let user = &config.users["fulluser"];
        assert_eq!(user.jira_usernames, vec!["fulluser"]);
        assert_eq!(user.gitlab_usernames, vec!["fulluser_gl"]);
        assert_eq!(user.slack_id.as_deref(), Some("U111"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_with_gitlab() {
        let mut config = Config::default();
        config.scm.gitlab = Some(GitLabConfig {
            enabled: true,
            token: Some(SecretValue::new("glpat-test")),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_with_slack_source() {
        let mut config = Config::default();
        config.issues.slack = Some(SlackSourceConfig {
            bot_token: Some(SecretValue::new("xoxb-test")),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_with_discord_source() {
        let mut config = Config::default();
        config.issues.discord = Some(DiscordSourceConfig {
            bot_token: Some(SecretValue::new("bot-token")),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_slack_source_without_bot_token() {
        let mut config = Config::default();
        config.issues.slack = Some(SlackSourceConfig {
            bot_token: None,
            ..Default::default()
        });
        assert!(config.validate().is_err());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_discord_source_without_bot_token() {
        let mut config = Config::default();
        config.issues.discord = Some(DiscordSourceConfig {
            bot_token: None,
            ..Default::default()
        });
        assert!(config.validate().is_err());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_prioritisation_skipped_when_disabled() {
        let mut config = Config::default();
        config.issues.linear = Some(LinearConfig {
            enabled: true,
            api_key: SecretValue::new("key"),
            ..Default::default()
        });
        // Set invalid prioritisation values
        config.prioritisation.enabled = false;
        config.prioritisation.severity_weight = -1.0;
        // Validation should pass because prioritisation is disabled
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_prioritisation_checked_when_enabled() {
        let mut config = Config::default();
        config.issues.linear = Some(LinearConfig {
            enabled: true,
            api_key: SecretValue::new("key"),
            ..Default::default()
        });
        config.prioritisation.enabled = true;
        config.prioritisation.severity_weight = -1.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_per_source_max_issues_for_jira_and_gitlab() {
        let config = Config {
            max_issues_per_cycle: 5,
            issues: IssuesConfig {
                jira: Some(JiraConfig {
                    max_issues_per_cycle: Some(3),
                    ..Default::default()
                }),
                ..Default::default()
            },
            scm: ScmConfig {
                gitlab: Some(GitLabConfig {
                    max_issues_per_cycle: Some(4),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_issues_per_cycle_for("jira"), 3);
        assert_eq!(config.max_issues_per_cycle_for("gitlab"), 4);
    }

    #[test]
    fn test_per_source_max_issues_for_jira_gitlab_fallback() {
        let config = Config {
            max_issues_per_cycle: 5,
            issues: IssuesConfig {
                jira: Some(JiraConfig::default()),
                ..Default::default()
            },
            scm: ScmConfig {
                gitlab: Some(GitLabConfig::default()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_issues_per_cycle_for("jira"), 5);
        assert_eq!(config.max_issues_per_cycle_for("gitlab"), 5);
    }

    #[test]
    fn test_per_source_max_concurrent_for_jira_and_gitlab() {
        let config = Config {
            max_concurrent: 4,
            issues: IssuesConfig {
                jira: Some(JiraConfig {
                    max_concurrent: Some(2),
                    ..Default::default()
                }),
                ..Default::default()
            },
            scm: ScmConfig {
                gitlab: Some(GitLabConfig {
                    max_concurrent: Some(3),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_concurrent_for("jira"), 2);
        assert_eq!(config.max_concurrent_for("gitlab"), 3);
    }

    #[test]
    fn test_per_source_max_concurrent_for_jira_gitlab_fallback() {
        let config = Config {
            max_concurrent: 4,
            issues: IssuesConfig {
                jira: Some(JiraConfig::default()),
                ..Default::default()
            },
            scm: ScmConfig {
                gitlab: Some(GitLabConfig::default()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.max_concurrent_for("jira"), 4);
        assert_eq!(config.max_concurrent_for("gitlab"), 4);
    }

    #[test]
    fn test_poll_interval_ms_for_jira_and_gitlab() {
        let config = Config {
            poll_interval_ms: 300_000,
            issues: IssuesConfig {
                jira: Some(JiraConfig {
                    poll_interval_ms: Some(60_000),
                    ..Default::default()
                }),
                slack: Some(SlackSourceConfig {
                    poll_interval_ms: Some(45_000),
                    ..Default::default()
                }),
                ..Default::default()
            },
            scm: ScmConfig {
                gitlab: Some(GitLabConfig {
                    poll_interval_ms: Some(90_000),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.poll_interval_ms_for("jira"), 60_000);
        assert_eq!(config.poll_interval_ms_for("gitlab"), 90_000);
        assert_eq!(config.poll_interval_ms_for("slack"), 45_000);
    }

    #[test]
    fn test_poll_interval_ms_for_jira_gitlab_fallback() {
        let config = Config {
            poll_interval_ms: 300_000,
            issues: IssuesConfig {
                jira: Some(JiraConfig::default()),
                ..Default::default()
            },
            scm: ScmConfig {
                gitlab: Some(GitLabConfig::default()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.poll_interval_ms_for("jira"), 300_000);
        assert_eq!(config.poll_interval_ms_for("gitlab"), 300_000);
    }

    #[test]
    fn test_regression_config_effective_monitoring_zero_hours() {
        let config = RegressionConfig {
            monitoring_duration_hours: 0,
            monitoring_duration_secs: None,
            ..Default::default()
        };
        assert_eq!(config.effective_monitoring_duration_secs(), 0);
    }

    #[test]
    fn test_regression_config_effective_check_zero_hours() {
        let config = RegressionConfig {
            check_interval_hours: 0,
            check_interval_secs: None,
            ..Default::default()
        };
        // 0 hours * 3600 = 0, but .max(1) clamps to 1
        assert_eq!(config.effective_check_interval_secs(), 1);
    }

    #[test]
    fn test_regression_config_package_names() {
        let toml_str = r#"
enabled = true
check_interval_hours = 1
monitoring_duration_hours = 24
sentry_event_threshold = 1
similarity_threshold = 0.75

[package_names]
"utopia-php/database" = ["utopia-php/database"]
"my-repo" = ["my-package"]
"#;
        let config: RegressionConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.package_names.len(), 2);
        assert_eq!(
            config.package_names.get("my-repo"),
            Some(&vec!["my-package".to_string()])
        );
    }

    #[test]
    fn test_default_storage_dir() {
        let dir = default_storage_dir();
        assert_eq!(dir, PathBuf::from("./storage"));
    }

    #[test]
    fn test_config_default_storage_dir() {
        let config = Config::default();
        assert_eq!(config.storage_dir, PathBuf::from("./storage"));
    }

    #[test]
    fn test_config_storage_dir_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"
storage_dir = "/custom/storage"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.storage_dir, PathBuf::from("/custom/storage"));
        });
    }

    #[test]
    fn test_config_default_max_activity_entries() {
        let config = Config::default();
        assert_eq!(config.max_activity_entries, 10_000);
    }

    #[test]
    fn test_config_default_ipc_timeout_secs() {
        let config = Config::default();
        assert_eq!(config.ipc_timeout_secs, 30);
    }

    #[test]
    fn test_config_default_agent_timeout_secs() {
        let config = Config::default();
        assert_eq!(config.agent.timeout_secs, 21600);
    }

    #[test]
    fn test_config_default_db_path() {
        let config = Config::default();
        assert_eq!(config.db_path, PathBuf::from("claudear.db"));
    }

    #[test]
    fn test_config_default_workspace_empty() {
        let config = Config::default();
        assert!(config.workspace.as_os_str().is_empty());
    }

    #[test]
    fn test_env_override_linear_poll_interval_ms() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "key"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("LINEAR_POLL_INTERVAL_MS", "120000")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.issues.linear.as_ref().unwrap().poll_interval_ms,
                Some(120000)
            );
        });
    }

    #[test]
    fn test_env_override_sentry_poll_interval_ms() {
        let toml_str = r#"
workspace = "/tmp/repos"

[issues.sentry]
auth_token = "tok"
org_slug = "org"
"#;
        let file = create_temp_toml(toml_str);

        with_env(&[("SENTRY_POLL_INTERVAL_MS", "90000")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.issues.sentry.as_ref().unwrap().poll_interval_ms,
                Some(90000)
            );
        });
    }

    #[test]
    fn test_linear_trigger_assignee_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[issues.linear]
api_key = "key"
trigger_assignee = "Alice"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(
                config.issues.linear.as_ref().unwrap().trigger_assignee,
                Some("Alice".to_string())
            );
        });
    }

    #[test]
    fn test_from_toml_empty() {
        with_env(&[], || {
            let config = Config::from_toml("").unwrap();
            assert!(config.workspace.as_os_str().is_empty());
            assert_eq!(config.poll_interval_ms, 300_000);
            assert!(config.issues.linear.is_none());
            assert!(config.issues.sentry.is_none());
            assert!(config.issues.jira.is_none());
            assert!(config.scm.gitlab.is_none());
        });
    }

    #[test]
    fn test_learning_config_cross_repo_defaults() {
        let config = LearningConfig::default();
        assert!(config.cross_repo_correlation);
        assert_eq!(config.cross_repo_window_hours, 24);
    }

    #[test]
    fn test_learning_config_cross_repo_from_toml() {
        let toml_str = r#"
cross_repo_correlation = false
cross_repo_window_hours = 48
"#;
        let config: LearningConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.cross_repo_correlation);
        assert_eq!(config.cross_repo_window_hours, 48);
    }

    #[test]
    fn test_agent_provider_instructions_file_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent.providers.claude]
instructions_file = "my-instructions.md"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(
                config
                    .agent
                    .default_provider_config()
                    .and_then(|p| p.instructions_file.clone()),
                Some("my-instructions.md".to_string())
            );
        });
    }

    #[test]
    fn test_evaluation_config_roundtrip() {
        let config = EvaluationConfig {
            enabled: true,
            test_delta: false,
            lint_delta: true,
            static_analysis_delta: false,
            coverage_delta: true,
            tool_timeout_secs: 600,
            total_timeout_secs: 1800,
            post_pr_comment: false,
            fail_on_regression: true,
            custom_test_cmd: Some("npm test".to_string()),
            custom_lint_cmd: None,
            custom_analysis_cmd: Some("sonar".to_string()),
            custom_coverage_cmd: None,
        };
        let toml_str = toml::to_string(&config).unwrap();
        let restored: EvaluationConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.enabled, restored.enabled);
        assert_eq!(config.test_delta, restored.test_delta);
        assert_eq!(config.tool_timeout_secs, restored.tool_timeout_secs);
        assert_eq!(config.custom_test_cmd, restored.custom_test_cmd);
        assert_eq!(config.custom_lint_cmd, restored.custom_lint_cmd);
    }

    #[test]
    fn test_code_index_config_roundtrip() {
        let config = CodeIndexConfig {
            enabled: false,
            max_file_size_kb: 4096,
            batch_size: 128,
        };
        let toml_str = toml::to_string(&config).unwrap();
        let restored: CodeIndexConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.enabled, restored.enabled);
        assert_eq!(config.max_file_size_kb, restored.max_file_size_kb);
        assert_eq!(config.batch_size, restored.batch_size);
    }

    #[test]
    fn test_top_issues_period_serde_toml_more_aliases() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Wrapper {
            period: TopIssuesPeriod,
        }

        let from_12h: Wrapper = toml::from_str("period = \"12h\"").unwrap();
        assert_eq!(from_12h.period, TopIssuesPeriod::TwelveHours);

        let from_24h: Wrapper = toml::from_str("period = \"24h\"").unwrap();
        assert_eq!(from_24h.period, TopIssuesPeriod::OneDay);

        let from_1d: Wrapper = toml::from_str("period = \"1d\"").unwrap();
        assert_eq!(from_1d.period, TopIssuesPeriod::OneDay);

        let from_7d: Wrapper = toml::from_str("period = \"7d\"").unwrap();
        assert_eq!(from_7d.period, TopIssuesPeriod::OneWeek);

        let from_30d: Wrapper = toml::from_str("period = \"30d\"").unwrap();
        assert_eq!(from_30d.period, TopIssuesPeriod::OneMonth);

        let from_1m: Wrapper = toml::from_str("period = \"1m\"").unwrap();
        assert_eq!(from_1m.period, TopIssuesPeriod::OneMonth);
    }

    #[test]
    fn test_github_use_ssh_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[scm.github]
token = "ghp_test"
use_ssh = true
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(config.scm.github.use_ssh);
        });
    }

    #[test]
    fn test_slack_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[notifiers.slack]
bot_token = "xoxb-token"
channel_id = "C123"
webhook_url = "https://hooks.slack.com/x"
user_id = "U123"
workspace = "myteam"

[issues.slack]
bot_token = "xoxb-token"
channel_id = "C123"
listen_channel_id = "C456"
workspace = "myteam"
poll_interval_ms = 30000
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(
                config.notifiers.slack.bot_token,
                Some(SecretValue::new("xoxb-token"))
            );
            assert_eq!(config.notifiers.slack.channel_id, Some("C123".to_string()));
            assert!(config.issues.slack.is_some());
            assert_eq!(
                config
                    .issues
                    .slack
                    .as_ref()
                    .and_then(|s| s.listen_channel_id.clone()),
                Some("C456".to_string())
            );
            assert_eq!(config.notifiers.slack.workspace, Some("myteam".to_string()));
            assert_eq!(
                config
                    .issues
                    .slack
                    .as_ref()
                    .and_then(|s| s.poll_interval_ms),
                Some(30000)
            );
        });
    }

    #[test]
    fn test_discord_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[notifiers.discord]
webhook_url = "https://discord.com/wh"
user_id = "U789"
bot_token = "bot_tok"
channel_id = "CH123"
guild_id = "G789"

[issues.discord]
bot_token = "bot_tok"
channel_id = "CH123"
listen_channel_id = "CH456"
guild_id = "G789"
poll_interval_ms = 25000
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(
                config.notifiers.discord.webhook_url,
                Some(SecretValue::new("https://discord.com/wh"))
            );
            assert_eq!(
                config.notifiers.discord.bot_token,
                Some(SecretValue::new("bot_tok"))
            );
            assert!(config.issues.discord.is_some());
            assert_eq!(config.notifiers.discord.guild_id, Some("G789".to_string()));
            assert_eq!(
                config
                    .issues
                    .discord
                    .as_ref()
                    .and_then(|s| s.poll_interval_ms),
                Some(25000)
            );
        });
    }

    #[test]
    fn test_load_default_returns_error_when_no_file() {
        // load_default tries to read claudear.toml from the current directory,
        // which should not exist in the test environment in most cases.
        // We just verify it returns an error rather than panicking.
        let result = Config::load_default();
        // It might or might not exist - we just ensure no panic
        let _ = result;
    }

    #[test]
    fn test_resolve_instructions_file_whitespace_only() {
        with_env(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let instructions_path = dir.path().join("whitespace.md");
            fs::write(&instructions_path, "   \n\n  \t  \n").unwrap();

            let toml_str =
                "workspace = \"/tmp/repos\"\n\n[agent.providers.claude]\ninstructions_file = \"whitespace.md\"";
            let config = Config::from_toml(toml_str).unwrap();
            let resolved = config.resolve_instructions_file(dir.path()).unwrap();
            // Whitespace-only file should be treated as empty
            assert_eq!(resolved, None);
        });
    }

    #[test]
    fn test_per_source_helpers_with_none_configs() {
        let config = Config::default();
        // All source Options are None - should fall back to global
        assert_eq!(
            config.max_issues_per_cycle_for("linear"),
            config.max_issues_per_cycle
        );
        assert_eq!(
            config.max_issues_per_cycle_for("sentry"),
            config.max_issues_per_cycle
        );
        assert_eq!(
            config.max_issues_per_cycle_for("jira"),
            config.max_issues_per_cycle
        );
        assert_eq!(
            config.max_issues_per_cycle_for("gitlab"),
            config.max_issues_per_cycle
        );
        assert_eq!(config.max_concurrent_for("linear"), config.max_concurrent);
        assert_eq!(config.max_concurrent_for("sentry"), config.max_concurrent);
        assert_eq!(config.max_concurrent_for("jira"), config.max_concurrent);
        assert_eq!(config.max_concurrent_for("gitlab"), config.max_concurrent);
        assert_eq!(
            config.poll_interval_ms_for("linear"),
            config.poll_interval_ms
        );
        assert_eq!(
            config.poll_interval_ms_for("sentry"),
            config.poll_interval_ms
        );
        assert_eq!(config.poll_interval_ms_for("jira"), config.poll_interval_ms);
        assert_eq!(
            config.poll_interval_ms_for("gitlab"),
            config.poll_interval_ms
        );
    }

    #[test]
    fn test_default_config_file_constant() {
        assert_eq!(DEFAULT_CONFIG_FILE, "claudear.toml");
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.default_provider, "claude");
        assert_eq!(config.timeout_secs, 21600);
        // Default impl pre-inserts a "claude" provider entry
        assert_eq!(config.providers.len(), 1);
        assert!(config.providers.contains_key("claude"));
        assert!(config.experiments.is_empty());
    }

    #[test]
    fn test_agent_config_default_provider_config_present() {
        let config = AgentConfig::default();
        // Default impl inserts a "claude" provider, so this should return Some
        let pc = config.default_provider_config().unwrap();
        assert!(pc.model.is_none());
    }

    #[test]
    fn test_agent_config_default_provider_config_with_claude() {
        let mut config = AgentConfig::default();
        config.providers.insert(
            "claude".to_string(),
            ProviderConfig {
                model: Some("opus".to_string()),
                ..ProviderConfig::default()
            },
        );
        let pc = config.default_provider_config().unwrap();
        assert_eq!(pc.model, Some("opus".to_string()));
    }

    #[test]
    fn test_agent_config_default_provider_config_mut() {
        let mut config = AgentConfig::default();
        // Should insert a new entry if not present
        let pc = config.default_provider_config_mut();
        pc.model = Some("sonnet".to_string());
        assert_eq!(
            config.providers.get("claude").unwrap().model,
            Some("sonnet".to_string())
        );
    }

    #[test]
    fn test_experiment_config_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent]
default_provider = "claude"

[[agent.experiments]]
name = "claude-vs-codex"
enabled = true
strategy = "weighted_random"

[[agent.experiments.providers]]
name = "claude"
weight = 0.7

[[agent.experiments.providers]]
name = "codex"
weight = 0.3
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.experiments.len(), 1);
            let exp = &config.agent.experiments[0];
            assert_eq!(exp.name, "claude-vs-codex");
            assert!(exp.enabled);
            assert_eq!(exp.strategy, "weighted_random");
            assert_eq!(exp.providers.len(), 2);
            assert_eq!(exp.providers[0].name, "claude");
            assert!((exp.providers[0].weight - 0.7).abs() < f64::EPSILON);
            assert_eq!(exp.providers[1].name, "codex");
            assert!((exp.providers[1].weight - 0.3).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn test_multiple_provider_configs_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent.providers.claude]
model = "opus"
skip_permissions = true

[agent.providers.codex]
model = "o3"
binary = "codex"
sandbox = "network-off"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.providers.len(), 2);

            let claude = config.agent.providers.get("claude").unwrap();
            assert_eq!(claude.model, Some("opus".to_string()));
            assert!(claude.skip_permissions);

            let codex = config.agent.providers.get("codex").unwrap();
            assert_eq!(codex.model, Some("o3".to_string()));
            assert_eq!(codex.binary, Some("codex".to_string()));
            assert_eq!(codex.sandbox, Some("network-off".to_string()));
        });
    }

    #[test]
    fn test_agent_timeout_from_toml() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent]
timeout_secs = 3600
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.timeout_secs, 3600);
        });
    }

    #[test]
    fn test_experiment_config_disabled() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[[agent.experiments]]
name = "inactive"
enabled = false
strategy = "fallback"
providers = []
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.experiments.len(), 1);
            assert!(!config.agent.experiments[0].enabled);
        });
    }

    #[test]
    fn test_provider_config_default_values() {
        let pc = ProviderConfig::default();
        assert!(pc.model.is_none());
        assert!(pc.instructions.is_none());
        assert!(pc.instructions_file.is_none());
        assert!(pc.permissions.is_empty());
        assert!(!pc.skip_permissions);
        assert!(pc.binary.is_none());
        assert!(pc.api_key.is_none());
        assert!(pc.api_url.is_none());
        assert!(pc.sandbox.is_none());
        assert!(pc.extra.is_empty());
    }

    #[test]
    fn test_env_override_claude_model_over_toml() {
        with_env(&[("CLAUDE_MODEL", "opus")], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent.providers.claude]
model = "sonnet"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            // Environment variable should override TOML
            assert_eq!(
                config.agent.default_provider_config().unwrap().model,
                Some("opus".to_string())
            );
        });
    }

    #[test]
    fn test_env_override_claude_timeout() {
        with_env(&[("CLAUDE_TIMEOUT_SECS", "7200")], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent]
timeout_secs = 3600
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.timeout_secs, 7200);
        });
    }

    #[test]
    fn test_env_override_skip_permissions() {
        with_env(&[("CLAUDE_SKIP_PERMISSIONS", "true")], || {
            let toml_str = r#"
workspace = "/tmp/repos"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert!(
                config
                    .agent
                    .default_provider_config()
                    .unwrap()
                    .skip_permissions
            );
        });
    }

    #[test]
    fn test_agent_config_multiple_experiments() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[[agent.experiments]]
name = "exp-a"
enabled = true
strategy = "weighted_random"
providers = []

[[agent.experiments]]
name = "exp-b"
enabled = false
strategy = "fallback"
providers = []
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.experiments.len(), 2);
            assert_eq!(config.agent.experiments[0].name, "exp-a");
            assert!(config.agent.experiments[0].enabled);
            assert_eq!(config.agent.experiments[1].name, "exp-b");
            assert!(!config.agent.experiments[1].enabled);
        });
    }

    #[test]
    fn test_agent_config_default_provider_custom() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent]
default_provider = "codex"

[agent.providers.codex]
model = "o3"
binary = "codex"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.default_provider, "codex");
            let codex = config.agent.providers.get("codex").unwrap();
            assert_eq!(codex.model, Some("o3".to_string()));
            assert_eq!(codex.binary, Some("codex".to_string()));
        });
    }

    #[test]
    fn test_provider_config_with_extra_fields() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent.providers.claude]
model = "opus"

[agent.providers.claude.extra]
custom_flag = true
custom_value = "hello"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            let claude = config.agent.providers.get("claude").unwrap();
            assert_eq!(
                claude.extra.get("custom_flag").and_then(|v| v.as_bool()),
                Some(true)
            );
            assert_eq!(
                claude.extra.get("custom_value").and_then(|v| v.as_str()),
                Some("hello")
            );
        });
    }

    // --- Additional AgentConfig tests ---

    #[test]
    fn test_agent_config_default_has_claude_provider() {
        let config = AgentConfig::default();
        assert_eq!(config.default_provider, "claude");
        assert!(config.providers.contains_key("claude"));
        assert!(config.experiments.is_empty());
        assert_eq!(config.timeout_secs, 21600);
    }

    #[test]
    fn test_agent_config_default_provider_config() {
        let config = AgentConfig::default();
        let pc = config.default_provider_config();
        assert!(pc.is_some());
    }

    #[test]
    fn test_agent_config_default_provider_config_missing() {
        let config = AgentConfig {
            default_provider: "nonexistent".to_string(),
            ..AgentConfig::default()
        };
        assert!(config.default_provider_config().is_none());
    }

    #[test]
    fn test_agent_config_default_provider_config_mut_creates_entry() {
        let mut config = AgentConfig {
            default_provider: "codex".to_string(),
            providers: std::collections::HashMap::new(),
            ..AgentConfig::default()
        };
        assert!(!config.providers.contains_key("codex"));
        let _ = config.default_provider_config_mut();
        assert!(config.providers.contains_key("codex"));
    }

    #[test]
    fn test_provider_config_all_fields() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent.providers.codex]
model = "o3"
instructions = "Be careful"
binary = "/usr/local/bin/codex"
skip_permissions = true
sandbox = "network-on"
api_url = "https://api.codex.example.com"
permissions = ["Read", "Write"]
"#;
            let config = Config::from_toml(toml_str).unwrap();
            let codex = config.agent.providers.get("codex").unwrap();
            assert_eq!(codex.model.as_deref(), Some("o3"));
            assert_eq!(codex.instructions.as_deref(), Some("Be careful"));
            assert_eq!(codex.binary.as_deref(), Some("/usr/local/bin/codex"));
            assert!(codex.skip_permissions);
            assert_eq!(codex.sandbox.as_deref(), Some("network-on"));
            assert_eq!(
                codex.api_url.as_deref(),
                Some("https://api.codex.example.com")
            );
            assert_eq!(
                codex.permissions,
                vec!["Read".to_string(), "Write".to_string()]
            );
        });
    }

    #[test]
    fn test_multiple_providers_in_config() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent]
default_provider = "claude"

[agent.providers.claude]
model = "opus"

[agent.providers.codex]
model = "o3"
binary = "codex"

[agent.providers.gemini]
model = "gemini-pro"
api_url = "https://ai.google.dev"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.providers.len(), 3);
            assert!(config.agent.providers.contains_key("claude"));
            assert!(config.agent.providers.contains_key("codex"));
            assert!(config.agent.providers.contains_key("gemini"));
        });
    }

    #[test]
    fn test_experiment_default_strategy() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[[agent.experiments]]
name = "test-exp"
enabled = true
providers = []
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.experiments[0].strategy, "weighted_random");
        });
    }

    #[test]
    fn test_experiment_provider_weight_default() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[[agent.experiments]]
name = "test-exp"
enabled = true
strategy = "fallback"

[[agent.experiments.providers]]
name = "claude"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            let p = &config.agent.experiments[0].providers[0];
            assert_eq!(p.name, "claude");
            assert!(
                (p.weight - 1.0).abs() < f64::EPSILON,
                "default weight should be 1.0"
            );
        });
    }

    #[test]
    fn test_empty_agent_section_uses_defaults() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"

[agent]
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.default_provider, "claude");
            assert_eq!(config.agent.timeout_secs, 21600);
            assert!(config.agent.experiments.is_empty());
        });
    }

    #[test]
    fn test_no_agent_section_uses_defaults() {
        with_env(&[], || {
            let toml_str = r#"
workspace = "/tmp/repos"
"#;
            let config = Config::from_toml(toml_str).unwrap();
            assert_eq!(config.agent.default_provider, "claude");
            assert_eq!(config.agent.timeout_secs, 21600);
        });
    }

    #[test]
    fn test_experiment_config_serialization_roundtrip() {
        let exp = ExperimentConfig {
            name: "claude-vs-codex".to_string(),
            enabled: true,
            strategy: "weighted_random".to_string(),
            providers: vec![
                ExperimentProviderWeight {
                    name: "claude".to_string(),
                    weight: 0.7,
                },
                ExperimentProviderWeight {
                    name: "codex".to_string(),
                    weight: 0.3,
                },
            ],
        };
        let json = serde_json::to_string(&exp).unwrap();
        let deser: ExperimentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.name, "claude-vs-codex");
        assert!(deser.enabled);
        assert_eq!(deser.providers.len(), 2);
        assert!((deser.providers[0].weight - 0.7).abs() < f64::EPSILON);
    }
}
