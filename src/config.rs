//! Configuration loading and validation.
//!
//! Configuration is loaded from a YAML file (`claudear.yaml` by default).
//! Environment variables can override any YAML values.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Default config file name.
pub const DEFAULT_CONFIG_FILE: &str = "claudear.yaml";

/// Claude CLI configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Model to use (e.g., sonnet, opus, haiku, or full model ID).
    pub model: Option<String>,
    /// Custom instructions appended to Claude's system prompt.
    pub instructions: Option<String>,
    /// Path to a file containing custom instructions.
    /// Resolved relative to the config file directory. If both this and
    /// `instructions` are set, file content comes first, then inline appended.
    pub instructions_file: Option<String>,
    /// Tool permissions granted without prompting (--allowedTools).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Skip all permission prompts (default: true for backwards compat).
    pub skip_permissions: bool,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            model: None,
            instructions: None,
            instructions_file: None,
            permissions: Vec::new(),
            skip_permissions: true,
        }
    }
}

/// Main configuration for the application.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Working directory for cloning repositories.
    /// Repositories will be cloned into subdirectories of this path.
    pub work_dir: PathBuf,
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
    /// Claude process execution timeout in seconds (default: 21600 = 6 hours).
    pub claude_timeout_secs: u64,
    /// Claude CLI configuration.
    pub claude: ClaudeConfig,
    /// Discord configuration.
    pub discord: DiscordConfig,
    /// Email configuration.
    pub email: EmailConfig,
    /// SMS configuration.
    pub sms: SmsConfig,
    /// Push notification configuration.
    pub push: PushConfig,
    /// GitHub configuration for PR monitoring.
    pub github: GitHubConfig,
    /// GitHub App configuration for App-based authentication.
    #[serde(default)]
    pub github_app: GitHubAppConfig,
    /// Retry configuration.
    pub retry: RetryConfig,
    /// Linear source configuration.
    pub linear: Option<LinearConfig>,
    /// Sentry source configuration.
    pub sentry: Option<SentryConfig>,
    /// Regression monitoring configuration.
    #[serde(default)]
    pub regression: RegressionConfig,
    /// Cascade configuration for multi-repo chaining.
    #[serde(default)]
    pub cascade: CascadeConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            work_dir: PathBuf::new(),
            known_orgs: Vec::new(),
            auto_discover_paths: Vec::new(),
            poll_interval_ms: 300_000,
            webhook_port: 3100,
            db_path: PathBuf::from("claudear.db"),
            max_issues_per_cycle: 5,
            max_concurrent: 1,
            processing_delay_ms: 5000,
            max_activity_entries: 10_000,
            ipc_timeout_secs: 30,
            claude_timeout_secs: 21600, // 6 hours
            claude: ClaudeConfig::default(),
            discord: DiscordConfig::default(),
            email: EmailConfig::default(),
            sms: SmsConfig::default(),
            push: PushConfig::default(),
            github: GitHubConfig::default(),
            github_app: GitHubAppConfig::default(),
            retry: RetryConfig::default(),
            linear: None,
            sentry: None,
            regression: RegressionConfig::default(),
            cascade: CascadeConfig::default(),
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

/// Discord notification configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordConfig {
    /// Discord webhook URL for notifications.
    pub webhook_url: Option<String>,
    /// Discord user ID to mention in notifications.
    pub user_id: Option<String>,
}

/// Email (SMTP) notification configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    /// SMTP server host.
    pub smtp_host: Option<String>,
    /// SMTP server port (default: 587).
    pub smtp_port: u16,
    /// SMTP username.
    pub smtp_username: Option<String>,
    /// SMTP password.
    pub smtp_password: Option<String>,
    /// Sender email address.
    pub from_address: Option<String>,
    /// Recipient email addresses.
    pub to_addresses: Vec<String>,
    /// Use TLS (default: true).
    pub use_tls: bool,
}

/// SMS notification configuration (via Twilio).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmsConfig {
    /// Twilio Account SID.
    pub account_sid: Option<String>,
    /// Twilio Auth Token.
    pub auth_token: Option<String>,
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
    pub api_token: Option<String>,
    /// Pushover user key.
    pub user_key: Option<String>,
    /// Device name (optional, sends to all devices if empty).
    pub device: Option<String>,
    /// Priority level (-2 to 2).
    pub priority: Option<i8>,
}

/// GitHub configuration for PR monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    /// GitHub personal access token.
    pub token: Option<String>,
    /// Poll interval for checking PR status (ms).
    pub poll_interval_ms: u64,
    /// Whether to auto-resolve issues when PRs merge.
    pub auto_resolve_on_merge: bool,
    /// Webhook secret for verifying GitHub webhook signatures.
    pub webhook_secret: Option<String>,
    /// Trigger tag for review comments (e.g., "/claudear" or "@mybot").
    /// Comments must contain this tag to trigger Claude.
    /// Set to empty string to respond to all comments.
    pub review_trigger: String,
    /// Use SSH URLs for cloning instead of HTTPS.
    /// Set to true if you have SSH keys configured for GitHub.
    #[serde(default)]
    pub use_ssh: bool,
}

impl Default for GitHubConfig {
    fn default() -> Self {
        Self {
            token: None,
            poll_interval_ms: 60000,
            auto_resolve_on_merge: false,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
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
    pub private_key: Option<String>,
    /// Webhook secret for verifying GitHub webhook signatures.
    pub webhook_secret: Option<String>,
    /// Installation ID (auto-detected if not set).
    pub installation_id: Option<i64>,
    /// OAuth Client ID (for user authorization flows).
    pub client_id: Option<String>,
    /// OAuth Client Secret.
    pub client_secret: Option<String>,
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
            return Ok(key.clone());
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
    pub api_key: String,
    /// Labels that trigger automation.
    pub trigger_labels: Vec<String>,
    /// States that trigger automation.
    pub trigger_states: Vec<String>,
    /// Optional team filter.
    pub team_id: Option<String>,
    /// Optional project filter.
    pub project_id: Option<String>,
    /// Webhook signature verification secret.
    pub webhook_secret: Option<String>,
    /// Maximum issues to process per poll cycle for this source (overrides global).
    pub max_issues_per_cycle: Option<usize>,
    /// Maximum concurrent issue processing for this source (overrides global).
    pub max_concurrent: Option<usize>,
}

impl Default for LinearConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: String::new(),
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_states: vec!["backlog".to_string(), "todo".to_string()],
            team_id: None,
            project_id: None,
            webhook_secret: None,
            max_issues_per_cycle: None,
            max_concurrent: None,
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
    pub auth_token: String,
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
    pub client_secret: Option<String>,
    /// Maximum issues to process per poll cycle for this source (overrides global).
    pub max_issues_per_cycle: Option<usize>,
    /// Maximum concurrent issue processing for this source (overrides global).
    pub max_concurrent: Option<usize>,
}

impl Default for SentryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auth_token: String::new(),
            org_slug: String::new(),
            project_slugs: Vec::new(),
            top_issues_count: 100,
            top_issues_period: TopIssuesPeriod::default(),
            min_event_count: 10,
            escalation_threshold_percent: 50,
            client_secret: None,
            max_issues_per_cycle: None,
            max_concurrent: None,
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
    #[serde(default)]
    pub package_names: std::collections::HashMap<String, String>,
}

impl Default for RegressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_hours: 1,
            monitoring_duration_hours: 24,
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
    /// Load configuration from a YAML file with environment variable overrides.
    ///
    /// This is the primary way to load configuration. It:
    /// 1. Reads the YAML config file
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

        let mut config: Config = serde_yaml::from_str(&content).map_err(|e| {
            Error::config(format!(
                "Failed to parse YAML config '{}': {}",
                path.display(),
                e
            ))
        })?;

        // Apply environment variable overrides
        config.apply_env_overrides();

        // Validate project directory configuration
        config.validate_project_config()?;

        Ok(config)
    }

    /// Validate minimal configuration needed for loading.
    ///
    /// Only validates `work_dir` is set. Repository validation is done
    /// in `validate()` for commands that actually need repositories.
    fn validate_project_config(&self) -> Result<()> {
        if self.work_dir.as_os_str().is_empty() {
            return Err(Error::config(
                "'work_dir' is required - path where repositories will be cloned",
            ));
        }

        Ok(())
    }

    /// Load configuration from the default config file path.
    pub fn load_default() -> Result<Self> {
        Self::load(DEFAULT_CONFIG_FILE)
    }

    /// Load configuration from YAML string (useful for testing).
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let config: Config = serde_yaml::from_str(yaml)
            .map_err(|e| Error::config(format!("Failed to parse YAML: {}", e)))?;
        Ok(config)
    }

    /// Resolve `claude.instructions_file` by reading the file and combining
    /// with inline `claude.instructions`.
    ///
    /// - `config_dir`: directory containing the config file (for relative path resolution)
    /// - File content comes first, then inline instructions appended with a newline
    /// - Returns `None` if neither field is set
    /// - Returns error if the file path is set but the file cannot be read
    pub fn resolve_instructions_file(&self, config_dir: &Path) -> Result<Option<String>> {
        let file_content = if let Some(ref file_path) = self.claude.instructions_file {
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

        match (file_content, &self.claude.instructions) {
            (Some(file), Some(inline)) => Ok(Some(format!("{}\n{}", file, inline))),
            (Some(file), None) => Ok(Some(file)),
            (None, Some(inline)) => Ok(Some(inline.clone())),
            (None, None) => Ok(None),
        }
    }

    /// Apply environment variable overrides to the config.
    /// Environment variables take precedence over YAML values.
    fn apply_env_overrides(&mut self) {
        // Core settings
        if let Ok(v) = env::var("WORK_DIR") {
            if !v.is_empty() {
                self.work_dir = v.into();
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
            self.claude_timeout_secs = v;
        }

        // Claude CLI
        if let Ok(v) = env::var("CLAUDE_MODEL") {
            if !v.is_empty() {
                self.claude.model = Some(v);
            }
        }
        if let Ok(v) = env::var("CLAUDE_INSTRUCTIONS") {
            if !v.is_empty() {
                self.claude.instructions = Some(v);
            }
        }
        if let Ok(v) = env::var("CLAUDE_INSTRUCTIONS_FILE") {
            if !v.is_empty() {
                self.claude.instructions_file = Some(v);
            }
        }
        if let Ok(v) = env::var("CLAUDE_PERMISSIONS") {
            if !v.is_empty() {
                self.claude.permissions = v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Ok(v) = env::var("CLAUDE_SKIP_PERMISSIONS") {
            self.claude.skip_permissions = v.to_lowercase() == "true" || v == "1";
        }

        // Discord
        if let Ok(v) = env::var("DISCORD_WEBHOOK_URL") {
            self.discord.webhook_url = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("DISCORD_USER_ID") {
            self.discord.user_id = Some(v).filter(|s| !s.is_empty());
        }

        // Email
        if let Ok(v) = env::var("SMTP_HOST") {
            self.email.smtp_host = Some(v).filter(|s| !s.is_empty());
        }
        if let Some(v) = env::var("SMTP_PORT").ok().and_then(|v| v.parse().ok()) {
            self.email.smtp_port = v;
        }
        if let Ok(v) = env::var("SMTP_USERNAME") {
            self.email.smtp_username = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("SMTP_PASSWORD") {
            self.email.smtp_password = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("EMAIL_FROM") {
            self.email.from_address = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("EMAIL_TO") {
            if !v.is_empty() {
                self.email.to_addresses = v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }
        if let Ok(v) = env::var("SMTP_TLS") {
            self.email.use_tls = v.to_lowercase() == "true" || v == "1";
        }

        // SMS
        if let Ok(v) = env::var("TWILIO_ACCOUNT_SID") {
            self.sms.account_sid = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("TWILIO_AUTH_TOKEN") {
            self.sms.auth_token = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("TWILIO_FROM_NUMBER") {
            self.sms.from_number = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("TWILIO_TO_NUMBERS") {
            if !v.is_empty() {
                self.sms.to_numbers = v.split(',').map(|s| s.trim().to_string()).collect();
            }
        }

        // Push
        if let Ok(v) = env::var("PUSHOVER_API_TOKEN") {
            self.push.api_token = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("PUSHOVER_USER_KEY") {
            self.push.user_key = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("PUSHOVER_DEVICE") {
            self.push.device = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("PUSHOVER_PRIORITY") {
            self.push.priority = v.parse().ok();
        }

        // GitHub
        if let Ok(v) = env::var("GITHUB_TOKEN") {
            self.github.token = Some(v).filter(|s| !s.is_empty());
        }
        if let Some(v) = env::var("GITHUB_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.github.poll_interval_ms = v;
        }
        if let Ok(v) = env::var("GITHUB_AUTO_RESOLVE_ON_MERGE") {
            self.github.auto_resolve_on_merge = v.to_lowercase() == "true" || v == "1";
        }
        if let Ok(v) = env::var("GITHUB_WEBHOOK_SECRET") {
            self.github.webhook_secret = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("GITHUB_REVIEW_TRIGGER") {
            self.github.review_trigger = v;
        }

        // GitHub App
        if let Some(v) = env::var("GITHUB_APP_ID").ok().and_then(|v| v.parse().ok()) {
            self.github_app.app_id = Some(v);
        }
        if let Ok(v) = env::var("GITHUB_APP_PRIVATE_KEY_PATH") {
            self.github_app.private_key_path = Some(v).filter(|s| !s.is_empty()).map(PathBuf::from);
        }
        if let Ok(v) = env::var("GITHUB_APP_PRIVATE_KEY") {
            self.github_app.private_key = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("GITHUB_APP_WEBHOOK_SECRET") {
            self.github_app.webhook_secret = Some(v).filter(|s| !s.is_empty());
        }
        if let Some(v) = env::var("GITHUB_APP_INSTALLATION_ID")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            self.github_app.installation_id = Some(v);
        }
        if let Ok(v) = env::var("GITHUB_APP_CLIENT_ID") {
            self.github_app.client_id = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("GITHUB_APP_CLIENT_SECRET") {
            self.github_app.client_secret = Some(v).filter(|s| !s.is_empty());
        }
        if let Ok(v) = env::var("GITHUB_APP_BASE_URL") {
            self.github_app.base_url = Some(v).filter(|s| !s.is_empty());
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
    }

    /// Apply Linear environment variable overrides.
    fn apply_linear_env_overrides(&mut self) {
        // If LINEAR_API_KEY is set in env, ensure we have a LinearConfig
        if let Ok(api_key) = env::var("LINEAR_API_KEY") {
            if !api_key.is_empty() {
                let linear = self.linear.get_or_insert_with(LinearConfig::default);
                linear.api_key = api_key;
            }
        }

        // Apply other overrides if we have a LinearConfig
        if let Some(ref mut linear) = self.linear {
            if let Ok(v) = env::var("LINEAR_ENABLED") {
                linear.enabled = v.to_lowercase() == "true" || v == "1";
            }
            if let Ok(v) = env::var("LINEAR_TRIGGER_LABELS") {
                if !v.is_empty() {
                    linear.trigger_labels = v.split(',').map(|s| s.trim().to_string()).collect();
                }
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
                linear.webhook_secret = Some(v).filter(|s| !s.is_empty());
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
        }
    }

    /// Apply Sentry environment variable overrides.
    fn apply_sentry_env_overrides(&mut self) {
        // If SENTRY_AUTH_TOKEN is set in env, ensure we have a SentryConfig
        if let Ok(auth_token) = env::var("SENTRY_AUTH_TOKEN") {
            if !auth_token.is_empty() {
                let sentry = self.sentry.get_or_insert_with(SentryConfig::default);
                sentry.auth_token = auth_token;
            }
        }

        // Apply other overrides if we have a SentryConfig
        if let Some(ref mut sentry) = self.sentry {
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
                sentry.client_secret = Some(v).filter(|s| !s.is_empty());
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
        }
    }

    /// Validate that at least one source is configured and enabled.
    pub fn validate(&self) -> Result<()> {
        let has_linear = self
            .linear
            .as_ref()
            .is_some_and(|c| c.enabled && !c.api_key.is_empty());
        let has_sentry = self
            .sentry
            .as_ref()
            .is_some_and(|c| c.enabled && !c.auth_token.is_empty());

        if !has_linear && !has_sentry {
            return Err(Error::config(
                "No sources configured. Configure linear or sentry in config file with valid API credentials.",
            ));
        }

        // Validate Sentry has org_slug if enabled
        if let Some(ref sentry) = self.sentry {
            if sentry.enabled && !sentry.auth_token.is_empty() && sentry.org_slug.is_empty() {
                return Err(Error::config(
                    "sentry.org_slug is required when Sentry is enabled",
                ));
            }
        }

        Ok(())
    }

    /// Check if Linear source is enabled.
    pub fn is_linear_enabled(&self) -> bool {
        self.linear.as_ref().is_some_and(|c| c.enabled)
    }

    /// Check if Sentry source is enabled.
    pub fn is_sentry_enabled(&self) -> bool {
        self.sentry.as_ref().is_some_and(|c| c.enabled)
    }

    /// Check if GitHub PR monitoring is enabled.
    pub fn is_github_enabled(&self) -> bool {
        self.github.token.is_some() || self.github_app.is_configured()
    }

    /// Determine the GitHub authentication mode to use.
    ///
    /// Returns `App` if GitHub App is configured, otherwise `Token`.
    pub fn github_auth_mode(&self) -> GitHubAuthMode {
        if self.github_app.is_configured() {
            GitHubAuthMode::App
        } else {
            GitHubAuthMode::Token
        }
    }

    /// Check if GitHub App is configured.
    pub fn is_github_app_configured(&self) -> bool {
        self.github_app.is_configured()
    }

    /// Get the max issues per cycle for a specific source.
    /// Uses the source-specific value if set, otherwise falls back to the global value.
    pub fn max_issues_per_cycle_for(&self, source_name: &str) -> usize {
        match source_name {
            "linear" => self
                .linear
                .as_ref()
                .and_then(|c| c.max_issues_per_cycle)
                .unwrap_or(self.max_issues_per_cycle),
            "sentry" => self
                .sentry
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
                .linear
                .as_ref()
                .and_then(|c| c.max_concurrent)
                .unwrap_or(self.max_concurrent),
            "sentry" => self
                .sentry
                .as_ref()
                .and_then(|c| c.max_concurrent)
                .unwrap_or(self.max_concurrent),
            _ => self.max_concurrent,
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
        "LINEAR_TRIGGER_STATES",
        "LINEAR_TEAM_ID",
        "LINEAR_PROJECT_ID",
        "LINEAR_WEBHOOK_SECRET",
        "LINEAR_MAX_ISSUES_PER_CYCLE",
        "LINEAR_MAX_CONCURRENT",
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
        "DISCORD_WEBHOOK_URL",
        "DISCORD_USER_ID",
        "SMTP_HOST",
        "SMTP_PORT",
        "SMTP_USERNAME",
        "SMTP_PASSWORD",
        "EMAIL_FROM",
        "EMAIL_TO",
        "SMTP_TLS",
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

    fn create_temp_yaml(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    #[test]
    fn test_from_yaml_minimal() {
        let yaml = r#"
work_dir: /tmp/repos
known_orgs:
  - appwrite
  - utopia-php
linear:
  api_key: lin_test_key
"#;
        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.work_dir, PathBuf::from("/tmp/repos"));
        assert_eq!(config.known_orgs, vec!["appwrite", "utopia-php"]);
        assert!(config.linear.is_some());
        assert_eq!(config.linear.as_ref().unwrap().api_key, "lin_test_key");
    }

    #[test]
    fn test_from_yaml_full_config() {
        let yaml = r#"
work_dir: /path/to/repos
known_orgs:
  - appwrite
  - utopia-php
auto_discover_paths:
  - ~/Local
  - ~/Projects
poll_interval_ms: 600000
webhook_port: 8080
db_path: /custom/db.sqlite
max_issues_per_cycle: 10
max_concurrent: 3
processing_delay_ms: 10000

discord:
  webhook_url: https://discord.com/api/webhooks/123/abc
  user_id: "987654321"

email:
  smtp_host: smtp.example.com
  smtp_port: 465
  smtp_username: user@example.com
  smtp_password: secret
  from_address: noreply@example.com
  to_addresses:
    - admin@example.com
    - team@example.com
  use_tls: true

sms:
  account_sid: AC123
  auth_token: token123
  from_number: "+15555555555"
  to_numbers:
    - "+16666666666"

push:
  api_token: pushover_token
  user_key: user_key
  device: iPhone
  priority: 1

github:
  token: ghp_token123
  poll_interval_ms: 30000
  auto_resolve_on_merge: false

retry:
  max_retries: 5
  base_delay_ms: 30000
  max_delay_ms: 7200000

linear:
  enabled: true
  api_key: lin_api_key
  trigger_labels:
    - auto
    - implement
  trigger_states:
    - todo
    - backlog
  team_id: team_123
  project_id: proj_456
  webhook_secret: webhook_secret

sentry:
  enabled: true
  auth_token: sentry_token
  org_slug: my-org
  project_slugs:
    - frontend
    - backend
  top_issues_count: 50
  min_event_count: 5
  escalation_threshold_percent: 25
  client_secret: client_secret
"#;
        let config = Config::from_yaml(yaml).unwrap();

        assert_eq!(config.work_dir, PathBuf::from("/path/to/repos"));
        assert_eq!(config.known_orgs, vec!["appwrite", "utopia-php"]);
        assert_eq!(config.auto_discover_paths, vec!["~/Local", "~/Projects"]);
        assert_eq!(config.poll_interval_ms, 600000);
        assert_eq!(config.webhook_port, 8080);
        assert_eq!(config.db_path, PathBuf::from("/custom/db.sqlite"));
        assert_eq!(config.max_issues_per_cycle, 10);
        assert_eq!(config.max_concurrent, 3);
        assert_eq!(config.processing_delay_ms, 10000);

        // Discord
        assert_eq!(
            config.discord.webhook_url,
            Some("https://discord.com/api/webhooks/123/abc".to_string())
        );
        assert_eq!(config.discord.user_id, Some("987654321".to_string()));

        // Email
        assert_eq!(config.email.smtp_host, Some("smtp.example.com".to_string()));
        assert_eq!(config.email.smtp_port, 465);
        assert!(config.email.use_tls);

        // Linear
        let linear = config.linear.unwrap();
        assert!(linear.enabled);
        assert_eq!(linear.api_key, "lin_api_key");
        assert_eq!(linear.trigger_labels, vec!["auto", "implement"]);
        assert_eq!(linear.team_id, Some("team_123".to_string()));

        // Sentry
        let sentry = config.sentry.unwrap();
        assert!(sentry.enabled);
        assert_eq!(sentry.auth_token, "sentry_token");
        assert_eq!(sentry.org_slug, "my-org");
        assert_eq!(sentry.top_issues_count, 50);
    }

    /// Helper to create a minimal valid config YAML for tests.
    fn test_config_yaml() -> &'static str {
        r#"
work_dir: /tmp/repos
known_orgs:
  - appwrite
linear:
  api_key: test_key
"#
    }

    #[test]
    fn test_from_yaml_with_defaults() {
        let config = Config::from_yaml(test_config_yaml()).unwrap();

        // Check that defaults are applied
        assert_eq!(config.poll_interval_ms, 300_000);
        assert_eq!(config.webhook_port, 3100);
        assert_eq!(config.max_issues_per_cycle, 5);
        assert_eq!(config.max_concurrent, 1);
        assert_eq!(config.processing_delay_ms, 5000);

        // Linear defaults
        let linear = config.linear.unwrap();
        assert!(linear.enabled);
        assert_eq!(
            linear.trigger_labels,
            vec!["auto-implement".to_string(), "claude".to_string()]
        );
        assert_eq!(
            linear.trigger_states,
            vec!["backlog".to_string(), "todo".to_string()]
        );
    }

    #[test]
    fn test_from_yaml_invalid_yaml() {
        let yaml = r#"
work_dir: /tmp/repos
known_orgs:
  this is wrong: not valid
"#;
        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_from_file() {
        let file = create_temp_yaml(test_config_yaml());

        with_env(&[], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.work_dir, PathBuf::from("/tmp/repos"));
            assert_eq!(config.known_orgs, vec!["appwrite"]);
            assert!(config.linear.is_some());
        });
    }

    #[test]
    fn test_load_file_not_found() {
        with_env(&[], || {
            let result = Config::load("/nonexistent/path/config.yaml");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("Failed to read"));
        });
    }

    #[test]
    fn test_load_missing_work_dir() {
        let yaml = r#"
known_orgs:
  - appwrite
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[], || {
            let result = Config::load(file.path());
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("work_dir"));
        });
    }

    #[test]
    fn test_load_without_known_orgs_succeeds() {
        // Config can load without known_orgs and auto_discover_paths
        let yaml = r#"
work_dir: /tmp/repos
linear:
  api_key: test_key
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[], || {
            let config = Config::load(file.path()).unwrap();
            assert!(config.known_orgs.is_empty());
            assert!(config.auto_discover_paths.is_empty());
            // validate() should succeed since we have a source configured
            assert!(config.validate().is_ok());
        });
    }

    #[test]
    fn test_env_override_work_dir() {
        let yaml = r#"
work_dir: /yaml/path
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("WORK_DIR", "/env/path")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.work_dir, PathBuf::from("/env/path"));
        });
    }

    #[test]
    fn test_env_override_known_orgs() {
        let yaml = r#"
work_dir: /tmp/repos
known_orgs:
  - yaml-org
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("KNOWN_ORGS", "env-org1, env-org2")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.known_orgs, vec!["env-org1", "env-org2"]);
        });
    }

    #[test]
    fn test_env_override_auto_discover_paths() {
        let yaml = r#"
work_dir: /tmp/repos
auto_discover_paths:
  - ~/yaml/path
"#;
        let file = create_temp_yaml(yaml);

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
        let yaml = r#"
work_dir: /tmp/repos
poll_interval_ms: 100000
webhook_port: 3000
linear:
  api_key: lin_key
"#;
        let file = create_temp_yaml(yaml);

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
        let yaml = r#"
work_dir: /tmp/repos
linear:
  api_key: yaml_key
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("LINEAR_API_KEY", "env_key")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.linear.as_ref().unwrap().api_key, "env_key");
        });
    }

    #[test]
    fn test_env_creates_linear_config_when_missing() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("LINEAR_API_KEY", "env_key")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(config.linear.is_some());
            assert_eq!(config.linear.as_ref().unwrap().api_key, "env_key");
        });
    }

    #[test]
    fn test_env_override_sentry() {
        let yaml = r#"
work_dir: /tmp/repos
sentry:
  auth_token: yaml_token
  org_slug: yaml-org
"#;
        let file = create_temp_yaml(yaml);

        with_env(
            &[
                ("SENTRY_AUTH_TOKEN", "env_token"),
                ("SENTRY_ORG_SLUG", "env-org"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                let sentry = config.sentry.unwrap();
                assert_eq!(sentry.auth_token, "env_token");
                assert_eq!(sentry.org_slug, "env-org");
            },
        );
    }

    #[test]
    fn test_env_creates_sentry_config_when_missing() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

        with_env(
            &[
                ("SENTRY_AUTH_TOKEN", "env_token"),
                ("SENTRY_ORG_SLUG", "env-org"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert!(config.sentry.is_some());
                assert_eq!(config.sentry.as_ref().unwrap().auth_token, "env_token");
            },
        );
    }

    #[test]
    fn test_env_override_discord() {
        let yaml = r#"
work_dir: /tmp/repos
discord:
  webhook_url: https://yaml.webhook
linear:
  api_key: key
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("DISCORD_WEBHOOK_URL", "https://env.webhook")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.discord.webhook_url,
                Some("https://env.webhook".to_string())
            );
        });
    }

    #[test]
    fn test_env_override_github() {
        let yaml = r#"
work_dir: /tmp/repos
github:
  token: yaml_token
  poll_interval_ms: 30000
  auto_resolve_on_merge: true
linear:
  api_key: key
"#;
        let file = create_temp_yaml(yaml);

        with_env(
            &[
                ("GITHUB_TOKEN", "env_token"),
                ("GITHUB_POLL_INTERVAL_MS", "60000"),
                ("GITHUB_AUTO_RESOLVE_ON_MERGE", "false"),
            ],
            || {
                let config = Config::load(file.path()).unwrap();
                assert_eq!(config.github.token, Some("env_token".to_string()));
                assert_eq!(config.github.poll_interval_ms, 60000);
                assert!(!config.github.auto_resolve_on_merge);
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
        config.linear = Some(LinearConfig {
            enabled: true,
            api_key: "test_key".into(),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_with_sentry() {
        let mut config = Config::default();
        config.sentry = Some(SentryConfig {
            enabled: true,
            auth_token: "test_token".into(),
            org_slug: "test_org".into(),
            ..Default::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_sentry_missing_org_slug() {
        let mut config = Config::default();
        config.sentry = Some(SentryConfig {
            enabled: true,
            auth_token: "test_token".into(),
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
        config.linear = Some(LinearConfig {
            enabled: false,
            api_key: "test_key".into(),
            ..Default::default()
        });
        assert!(config.validate().is_err());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_validation_empty_api_key_fails() {
        let mut config = Config::default();
        config.linear = Some(LinearConfig {
            enabled: true,
            api_key: String::new(),
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
        assert!(config.linear.is_none());
        assert!(config.sentry.is_none());
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
        };
        assert_eq!(config.max_issues_per_cycle_for("linear"), 7);
        assert_eq!(config.max_issues_per_cycle_for("sentry"), 7);
        assert_eq!(config.max_issues_per_cycle_for("unknown"), 7);
    }

    #[test]
    fn test_per_source_max_issues_overrides_global() {
        let config = Config {
            max_issues_per_cycle: 5,
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
        };
        assert_eq!(config.max_issues_per_cycle_for("linear"), 3);
        assert_eq!(config.max_issues_per_cycle_for("sentry"), 2);
        assert_eq!(config.max_issues_per_cycle_for("unknown"), 5);
    }

    #[test]
    fn test_per_source_max_concurrent_falls_back_to_global() {
        let config = Config {
            max_concurrent: 4,
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
        };
        assert_eq!(config.max_concurrent_for("linear"), 4);
        assert_eq!(config.max_concurrent_for("sentry"), 4);
        assert_eq!(config.max_concurrent_for("unknown"), 4);
    }

    #[test]
    fn test_per_source_max_concurrent_overrides_global() {
        let config = Config {
            max_concurrent: 8,
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
        };
        assert_eq!(config.max_concurrent_for("linear"), 2);
        assert_eq!(config.max_concurrent_for("sentry"), 6);
        assert_eq!(config.max_concurrent_for("unknown"), 8);
    }

    #[test]
    fn test_per_source_config_from_yaml() {
        let yaml = r#"
work_dir: /tmp/repos
max_issues_per_cycle: 5
max_concurrent: 8
linear:
  api_key: lin_key
  max_issues_per_cycle: 3
  max_concurrent: 2
sentry:
  auth_token: sentry_tok
  org_slug: org
  max_issues_per_cycle: 2
  max_concurrent: 6
"#;
        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.max_issues_per_cycle_for("linear"), 3);
        assert_eq!(config.max_issues_per_cycle_for("sentry"), 2);
        assert_eq!(config.max_concurrent_for("linear"), 2);
        assert_eq!(config.max_concurrent_for("sentry"), 6);
    }

    #[test]
    fn test_per_source_config_partial_override() {
        let yaml = r#"
work_dir: /tmp/repos
max_issues_per_cycle: 5
max_concurrent: 8
linear:
  api_key: lin_key
  max_issues_per_cycle: 3
sentry:
  auth_token: sentry_tok
  org_slug: org
  max_concurrent: 6
"#;
        let config = Config::from_yaml(yaml).unwrap();
        // Linear overrides issues but not concurrent
        assert_eq!(config.max_issues_per_cycle_for("linear"), 3);
        assert_eq!(config.max_concurrent_for("linear"), 8);
        // Sentry overrides concurrent but not issues
        assert_eq!(config.max_issues_per_cycle_for("sentry"), 5);
        assert_eq!(config.max_concurrent_for("sentry"), 6);
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

        config.linear = Some(LinearConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(config.is_linear_enabled());

        config.linear.as_mut().unwrap().enabled = false;
        assert!(!config.is_linear_enabled());
    }

    #[test]
    fn test_is_sentry_enabled() {
        let mut config = Config::default();
        assert!(!config.is_sentry_enabled());

        config.sentry = Some(SentryConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(config.is_sentry_enabled());

        config.sentry.as_mut().unwrap().enabled = false;
        assert!(!config.is_sentry_enabled());
    }

    #[test]
    fn test_is_github_enabled() {
        let mut config = Config::default();
        assert!(!config.is_github_enabled());

        config.github.token = Some("ghp_test".to_string());
        assert!(config.is_github_enabled());
    }

    #[test]
    fn test_config_yaml_roundtrip() {
        let yaml = r#"
work_dir: /tmp/repos
known_orgs:
  - appwrite
  - utopia-php
auto_discover_paths:
  - ~/Local
poll_interval_ms: 500000
linear:
  enabled: true
  api_key: test_key
  trigger_labels:
    - label1
    - label2
"#;
        let config = Config::from_yaml(yaml).unwrap();
        let serialized = serde_yaml::to_string(&config).unwrap();
        let deserialized: Config = serde_yaml::from_str(&serialized).unwrap();

        assert_eq!(config.work_dir, deserialized.work_dir);
        assert_eq!(config.known_orgs, deserialized.known_orgs);
        assert_eq!(config.auto_discover_paths, deserialized.auto_discover_paths);
        assert_eq!(config.poll_interval_ms, deserialized.poll_interval_ms);
        assert_eq!(
            config.linear.as_ref().unwrap().api_key,
            deserialized.linear.as_ref().unwrap().api_key
        );
    }

    #[test]
    fn test_retry_config_serialization() {
        let config = RetryConfig::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(yaml.contains("max_retries"));
        assert!(yaml.contains("base_delay_ms"));
        assert!(yaml.contains("max_delay_ms"));
    }

    #[test]
    fn test_regression_config_default() {
        let config = RegressionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.check_interval_hours, 1);
        assert_eq!(config.monitoring_duration_hours, 24);
        assert_eq!(config.sentry_event_threshold, 1);
        assert!((config.similarity_threshold - 0.75).abs() < 0.01);
        // target_repos and github_search_repos should be empty by default
        // (configured in YAML, not hardcoded)
        assert!(config.target_repos.is_empty());
        assert!(config.github_token.is_none());
        assert!(config.github_search_repos.is_empty());
        assert!(config.package_names.is_empty());
    }

    #[test]
    fn test_regression_config_serialization() {
        let config = RegressionConfig::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(yaml.contains("enabled"));
        assert!(yaml.contains("check_interval_hours"));
        assert!(yaml.contains("monitoring_duration_hours"));
        assert!(yaml.contains("sentry_event_threshold"));
        assert!(yaml.contains("similarity_threshold"));
        assert!(yaml.contains("target_repos"));
    }

    #[test]
    fn test_regression_config_deserialization() {
        let yaml = r#"
enabled: true
check_interval_hours: 2
monitoring_duration_hours: 48
sentry_event_threshold: 5
similarity_threshold: 0.8
target_repos:
  - custom/repo
github_search_repos:
  - org/repo1
  - org/repo2
"#;
        let config: RegressionConfig = serde_yaml::from_str(yaml).unwrap();
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
    fn test_config_regression_from_yaml() {
        let yaml = r#"
work_dir: /tmp/test
regression:
  enabled: false
  check_interval_hours: 4
  monitoring_duration_hours: 12
"#;
        let config = Config::from_yaml(yaml).unwrap();
        assert!(!config.regression.enabled);
        assert_eq!(config.regression.check_interval_hours, 4);
        assert_eq!(config.regression.monitoring_duration_hours, 12);
        // Defaults should apply for unspecified fields
        assert_eq!(config.regression.sentry_event_threshold, 1);
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
        config.private_key = Some("-----BEGIN RSA PRIVATE KEY-----".to_string());
        assert!(config.is_configured());
    }

    #[test]
    fn test_github_app_config_load_private_key_inline() {
        let config = GitHubAppConfig {
            private_key: Some("test-key-content".to_string()),
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
        config.github_app.app_id = Some(12345);
        config.github_app.private_key = Some("test-key".to_string());
        assert_eq!(config.github_auth_mode(), GitHubAuthMode::App);
    }

    #[test]
    fn test_is_github_enabled_with_token() {
        let mut config = Config::default();
        assert!(!config.is_github_enabled());

        config.github.token = Some("ghp_test".to_string());
        assert!(config.is_github_enabled());
    }

    #[test]
    fn test_is_github_enabled_with_app() {
        let mut config = Config::default();
        assert!(!config.is_github_enabled());

        config.github_app.app_id = Some(12345);
        config.github_app.private_key = Some("test-key".to_string());
        assert!(config.is_github_enabled());
    }

    #[test]
    fn test_github_app_config_from_yaml() {
        let yaml = r#"
work_dir: /tmp/test
github_app:
  app_id: 12345
  private_key_path: /path/to/key.pem
  webhook_secret: secret123
  installation_id: 67890
  client_id: Iv1.abc123
  client_secret: secret456
  base_url: https://example.com
"#;
        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.github_app.app_id, Some(12345));
        assert_eq!(
            config.github_app.private_key_path,
            Some(PathBuf::from("/path/to/key.pem"))
        );
        assert_eq!(
            config.github_app.webhook_secret,
            Some("secret123".to_string())
        );
        assert_eq!(config.github_app.installation_id, Some(67890));
        assert_eq!(config.github_app.client_id, Some("Iv1.abc123".to_string()));
        assert_eq!(
            config.github_app.client_secret,
            Some("secret456".to_string())
        );
        assert_eq!(
            config.github_app.base_url,
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn test_env_override_github_app() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

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
                assert_eq!(config.github_app.app_id, Some(12345));
                assert_eq!(config.github_app.private_key, Some("test-key".to_string()));
                assert_eq!(
                    config.github_app.webhook_secret,
                    Some("webhook-secret".to_string())
                );
                assert_eq!(config.github_app.installation_id, Some(67890));
                assert_eq!(config.github_app.client_id, Some("client-id".to_string()));
                assert_eq!(
                    config.github_app.client_secret,
                    Some("client-secret".to_string())
                );
                assert_eq!(
                    config.github_app.base_url,
                    Some("https://example.com".to_string())
                );
            },
        );
    }

    #[test]
    fn test_claude_config_default() {
        let config = ClaudeConfig::default();
        assert!(config.model.is_none());
        assert!(config.instructions.is_none());
        assert!(config.permissions.is_empty());
        assert!(config.skip_permissions);
    }

    #[test]
    fn test_config_default_includes_claude() {
        let config = Config::default();
        assert!(config.claude.model.is_none());
        assert!(config.claude.instructions.is_none());
        assert!(config.claude.permissions.is_empty());
        assert!(config.claude.skip_permissions);
    }

    #[test]
    fn test_claude_config_from_yaml() {
        let yaml = r#"
work_dir: /tmp/test
claude:
  model: sonnet
  instructions: "Always write tests."
  permissions:
    - "Bash(git *)"
    - "Read"
  skip_permissions: false
"#;
        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.claude.model, Some("sonnet".to_string()));
        assert_eq!(
            config.claude.instructions,
            Some("Always write tests.".to_string())
        );
        assert_eq!(config.claude.permissions, vec!["Bash(git *)", "Read"]);
        assert!(!config.claude.skip_permissions);
    }

    #[test]
    fn test_claude_config_yaml_defaults() {
        let yaml = r#"
work_dir: /tmp/test
"#;
        let config = Config::from_yaml(yaml).unwrap();
        assert!(config.claude.model.is_none());
        assert!(config.claude.instructions.is_none());
        assert!(config.claude.permissions.is_empty());
        assert!(config.claude.skip_permissions);
    }

    #[test]
    fn test_env_override_claude_model() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("CLAUDE_MODEL", "opus")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.claude.model, Some("opus".to_string()));
        });
    }

    #[test]
    fn test_env_override_claude_instructions() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("CLAUDE_INSTRUCTIONS", "Be concise.")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(config.claude.instructions, Some("Be concise.".to_string()));
        });
    }

    #[test]
    fn test_env_override_claude_instructions_file() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("CLAUDE_INSTRUCTIONS_FILE", "./my-instructions.md")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.claude.instructions_file,
                Some("./my-instructions.md".to_string())
            );
        });
    }

    #[test]
    fn test_env_override_claude_permissions() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("CLAUDE_PERMISSIONS", "Bash(git *), Read, Edit")], || {
            let config = Config::load(file.path()).unwrap();
            assert_eq!(
                config.claude.permissions,
                vec!["Bash(git *)", "Read", "Edit"]
            );
        });
    }

    #[test]
    fn test_env_override_claude_skip_permissions() {
        let yaml = r#"
work_dir: /tmp/repos
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("CLAUDE_SKIP_PERMISSIONS", "false")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(!config.claude.skip_permissions);
        });
    }

    #[test]
    fn test_env_override_claude_skip_permissions_true() {
        let yaml = r#"
work_dir: /tmp/repos
claude:
  skip_permissions: false
"#;
        let file = create_temp_yaml(yaml);

        with_env(&[("CLAUDE_SKIP_PERMISSIONS", "1")], || {
            let config = Config::load(file.path()).unwrap();
            assert!(config.claude.skip_permissions);
        });
    }

    #[test]
    fn test_resolve_instructions_file_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let instructions_path = dir.path().join("instructions.md");
        fs::write(&instructions_path, "Be helpful and concise.").unwrap();

        let yaml = format!(
            "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"{}\"",
            instructions_path.display()
        );
        let config = Config::from_yaml(&yaml).unwrap();
        let resolved = config.resolve_instructions_file(dir.path()).unwrap();
        assert_eq!(resolved, Some("Be helpful and concise.".to_string()));
    }

    #[test]
    fn test_resolve_instructions_file_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let instructions_path = dir.path().join("my-instructions.md");
        fs::write(&instructions_path, "Write tests first.").unwrap();

        let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"my-instructions.md\"";
        let config = Config::from_yaml(yaml).unwrap();
        let resolved = config.resolve_instructions_file(dir.path()).unwrap();
        assert_eq!(resolved, Some("Write tests first.".to_string()));
    }

    #[test]
    fn test_resolve_instructions_file_combines_with_inline() {
        let dir = tempfile::tempdir().unwrap();
        let instructions_path = dir.path().join("base.md");
        fs::write(&instructions_path, "Base instructions from file.").unwrap();

        let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"base.md\"\n  instructions: \"Plus inline.\"";
        let config = Config::from_yaml(yaml).unwrap();
        let resolved = config.resolve_instructions_file(dir.path()).unwrap();
        assert_eq!(
            resolved,
            Some("Base instructions from file.\nPlus inline.".to_string())
        );
    }

    #[test]
    fn test_resolve_instructions_file_inline_only() {
        let dir = tempfile::tempdir().unwrap();

        let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions: \"Just inline.\"";
        let config = Config::from_yaml(yaml).unwrap();
        let resolved = config.resolve_instructions_file(dir.path()).unwrap();
        assert_eq!(resolved, Some("Just inline.".to_string()));
    }

    #[test]
    fn test_resolve_instructions_file_neither_set() {
        let dir = tempfile::tempdir().unwrap();

        let yaml = "work_dir: /tmp/repos";
        let config = Config::from_yaml(yaml).unwrap();
        let resolved = config.resolve_instructions_file(dir.path()).unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn test_resolve_instructions_file_not_found() {
        let dir = tempfile::tempdir().unwrap();

        let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"nonexistent.md\"";
        let config = Config::from_yaml(yaml).unwrap();
        let result = config.resolve_instructions_file(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nonexistent.md"));
    }

    #[test]
    fn test_resolve_instructions_file_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let instructions_path = dir.path().join("empty.md");
        fs::write(&instructions_path, "").unwrap();

        let yaml = "work_dir: /tmp/repos\nclaude:\n  instructions_file: \"empty.md\"";
        let config = Config::from_yaml(yaml).unwrap();
        let resolved = config.resolve_instructions_file(dir.path()).unwrap();
        assert_eq!(resolved, None);
    }
}
