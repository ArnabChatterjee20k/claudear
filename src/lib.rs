//! # Claudear
//!
//! A unified watcher service that monitors issue trackers and error monitoring services,
//! automatically spawning Claude Code agents to fix issues and create pull requests.
//!
//! ## Features
//!
//! - **Multi-Source Support**: Monitor Linear issues and Sentry errors from a single service
//! - **Extensible Architecture**: Easy to add new sources (GitHub Issues, Jira, etc.)
//! - **Discord Notifications**: Get notified about fix attempts with PR links
//! - **SQLite Tracking**: Persistent tracking of fix attempts to avoid duplicates
//! - **Priority-Based Processing**: Urgent/escalating issues are processed first
//! - **Graceful Handling**: Proper error handling and retry support
//!
//! ## Usage
//!
//! ```bash
//! # First-time setup - mark existing issues as seen
//! claudear seed
//!
//! # Start polling for new issues
//! claudear poll
//!
//! # Start webhook server for real-time events
//! claudear webhook
//! ```

pub mod api;
pub mod api_events;
pub(crate) mod ask_reply_inbox;
pub mod config;
pub mod discord;
pub mod env_writer;
pub mod error;
pub mod evaluation;
pub mod feedback;
pub mod github;
pub mod github_app;
pub mod gitlab;
pub mod housekeeping;
pub mod http;
pub mod inference;
pub mod ipc;
pub mod learning;
pub mod notifier;
pub mod prioritisation;
pub mod qa;
pub mod regression;
pub mod release;
pub mod repo;
pub mod reports;
pub mod retry;
pub mod runner;
pub mod scm;
pub mod secret;
pub mod source;
pub mod storage;
pub mod telemetry;
pub mod templates;
pub mod types;
pub mod users;
pub mod watcher;
pub mod webhook;

pub use config::{CascadeConfig, CodeIndexConfig, Config, EvaluationConfig, RetryConfig};
pub use discord::{DiscordClient, ThreadManager, ThreadState};
pub use error::{Error, Result};
pub use evaluation::{
    CodeQualityEvaluator, Diagnostic, EvalCategory, EvalDelta, EvalSnapshot, EvaluationResult,
};
pub use feedback::{
    cosine_similarity, euclidean_distance, format_similar_issues_context, normalize,
    EmbeddingClient, EmbeddingConfig, EmbeddingResult, FeedbackAnalyzer, FixOutcome,
    IssueEmbeddingConfig, IssueEmbeddingService, Outcome, PromptSuggestion, SimilarIssue,
    SimilarIssueWithDetails,
};
pub use github::GitHubClient;
pub use gitlab::GitLabClient;
pub use housekeeping::HousekeepingWorker;
pub use scm::{
    CodeReview, OrgRepo, PostReviewAction, PrInfo, PrMonitor, PrReview, PrReviewComment,
    PrReviewState, PrStatus, PrStatusUpdate, PrSummary, RemoteRepo, ReviewComment, ReviewEvent,
    ReviewUser, ReviewWatcher, ScmProvider, ScmRelease,
};
pub use secret::SecretValue;
// Backward-compat alias
pub use github_app::{
    AppManifest, AppPermissions, CachedToken, GitHubAppAuth, GitHubAppClient, HookAttributes,
    SetupState,
};
pub use inference::{
    resolve_repo_for_cascade, resolve_repo_for_issue, Confidence, InferredRepo, IssueContext,
    RepoInferrer, RepoResolution,
};
pub use ipc::{
    default_socket_path, is_daemon_running, print_response, IpcClient, IpcCommand, IpcData,
    IpcResponse, IpcServer, WatcherState,
};
pub use repo::{
    DependencyDiscovery, DependencyGraph, DependencyType, DiscoveredDependency, IndexedRepo,
    RepoIndex, RepoRelationships, Repository,
};
pub use reports::{Report, ReportFrequency, ReportGenerator, ReportSchedule, ReportScheduler};
pub use retry::{RetryDecision, RetryManager};
pub use scm::GitHubUser;
pub use storage::{
    classify_error, compute_error_hash, parse_pr_url, AnalyticsService, FixAttemptTracker,
    StoredDependency, StoredRepository, TimePeriod, TrendAnalysis, TrendDirection,
};
#[cfg(feature = "sqlite")]
pub use storage::{is_vectorlite_available, try_load_vectorlite, SqliteTracker};
pub use types::*;
pub use users::{ResolvedUser, UserRegistry};

// ---------------------------------------------------------------------------
// Composable startup: lets alternative binaries (e.g. SaaS) bring their own
// storage backend while reusing all watcher / webhook / API logic.
// ---------------------------------------------------------------------------

use std::sync::Arc;

/// Shared state assembled during startup, passed to `run_daemon` / `run_webhook_server`.
pub struct AppComponents {
    pub config: Config,
    pub tracker: Arc<dyn storage::FixAttemptTracker>,
    pub sources: Vec<Arc<dyn source::IssueSource>>,
    pub notifier: Arc<dyn notifier::Notifier>,
    pub user_registry: UserRegistry,
    pub inferrer: Option<RepoInferrer>,
    pub embedding_client: Option<EmbeddingClient>,
    pub review_watcher: Option<Arc<ReviewWatcher>>,
    pub issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    pub agent: Arc<dyn runner::AgentRunner>,
}

/// Build all non-storage components.
///
/// The caller provides the config and a tracker (which may be backed by
/// SQLite or anything else that implements [`FixAttemptTracker`]).
/// Everything else is wired up from the config.
pub async fn build_app(
    config: Config,
    tracker: Arc<dyn storage::FixAttemptTracker>,
) -> anyhow::Result<AppComponents> {
    let user_registry = UserRegistry::new(config.users.clone());

    // Notifier
    let notifier = build_notifier(&config, user_registry.clone());

    // Sources
    let sources = build_sources(&config);

    // GitHub client for inferrer
    let github_client = github::GitHubClient::new(config.github().clone());
    let (inferrer, embedding_client) =
        watcher::Watcher::build_inferrer_with_embeddings(&config, Some(&github_client)).await?;

    // Review watcher
    let review_watcher = build_review_watcher(&config, tracker.clone());

    // Issue embedding service
    let issue_embedding_service = build_embedding_service(&tracker);

    // Agent runner
    let agent: Arc<dyn runner::AgentRunner> =
        telemetry::InstrumentedRunner::wrap(Arc::new(runner::ClaudeAgentRunner::new(
            runner::ClaudeRunnerConfig {
                timeout_secs: config.agent.timeout_secs,
                model: config
                    .agent
                    .default_provider_config()
                    .and_then(|p| p.model.clone()),
                instructions: config
                    .agent
                    .default_provider_config()
                    .and_then(|p| p.instructions.clone()),
                permissions: config
                    .agent
                    .default_provider_config()
                    .map(|p| p.permissions.clone())
                    .unwrap_or_default(),
                skip_permissions: config
                    .agent
                    .default_provider_config()
                    .map(|p| p.skip_permissions)
                    .unwrap_or(false),
            },
            tracker.clone(),
        )));

    Ok(AppComponents {
        config,
        tracker,
        sources,
        notifier,
        user_registry,
        inferrer,
        embedding_client,
        review_watcher,
        issue_embedding_service,
        agent,
    })
}

// --- internal helpers used by both build_app and main.rs ---

fn build_notifier(config: &Config, user_registry: UserRegistry) -> Arc<dyn notifier::Notifier> {
    use notifier::*;
    use telemetry::InstrumentedNotifier;

    let mut composite = CompositeNotifier::new();
    composite.add(InstrumentedNotifier::wrap(Arc::new(ConsoleNotifier::new())));

    let discord_notifier = DiscordNotifier::new(config.discord_merged(), user_registry.clone());
    if discord_notifier.is_enabled() {
        composite.add(InstrumentedNotifier::wrap(Arc::new(discord_notifier)));
    }

    let slack_notifier = SlackNotifier::new(config.slack_merged(), user_registry.clone());
    if slack_notifier.is_enabled() {
        composite.add(InstrumentedNotifier::wrap(Arc::new(slack_notifier)));
    }

    if let Ok(email_notifier) = EmailNotifier::new(config.email().clone(), user_registry.clone()) {
        if email_notifier.is_enabled() {
            composite.add(InstrumentedNotifier::wrap(Arc::new(email_notifier)));
        }
    }

    let sms_notifier = SmsNotifier::new(config.sms().clone(), user_registry.clone());
    if sms_notifier.is_enabled() {
        composite.add(InstrumentedNotifier::wrap(Arc::new(sms_notifier)));
    }

    let push_notifier = PushNotifier::new(config.push_config().clone(), user_registry.clone());
    if push_notifier.is_enabled() {
        composite.add(InstrumentedNotifier::wrap(Arc::new(push_notifier)));
    }

    let whatsapp_notifier =
        WhatsAppNotifier::new(config.notifiers.whatsapp.clone(), user_registry.clone());
    if whatsapp_notifier.is_enabled() {
        composite.add(InstrumentedNotifier::wrap(Arc::new(whatsapp_notifier)));
    }

    let telegram_notifier = TelegramNotifier::new(config.notifiers.telegram.clone(), user_registry);
    if telegram_notifier.is_enabled() {
        composite.add(InstrumentedNotifier::wrap(Arc::new(telegram_notifier)));
    }

    Arc::new(composite)
}

fn build_sources(config: &Config) -> Vec<Arc<dyn source::IssueSource>> {
    use source::*;

    let mut sources: Vec<Arc<dyn IssueSource>> = Vec::new();

    if let Some(linear_config) = config.linear() {
        if linear_config.enabled {
            sources.push(Arc::new(LinearSource::new(linear_config.clone())));
        }
    }

    if let Some(sentry_config) = config.sentry_config() {
        if sentry_config.enabled {
            sources.push(Arc::new(SentrySource::new(sentry_config.clone())));
        }
    }

    if let Some(jira_config) = config.jira() {
        if jira_config.enabled {
            sources.push(Arc::new(JiraSource::new(jira_config.clone())));
        }
    }

    let discord = config.discord_merged();
    if discord.source_enabled
        && discord.bot_token.is_some()
        && (discord.listen_channel_id.is_some() || discord.channel_id.is_some())
    {
        sources.push(Arc::new(DiscordSource::new(discord)));
    }

    let slack = config.slack_merged();
    if slack.source_enabled
        && slack.bot_token.is_some()
        && (slack.listen_channel_id.is_some() || slack.channel_id.is_some())
    {
        sources.push(Arc::new(SlackSource::new(slack)));
    }

    if config.notifiers.whatsapp.source_enabled
        && config.notifiers.whatsapp.access_token.is_some()
        && config.notifiers.whatsapp.phone_number_id.is_some()
    {
        sources.push(Arc::new(WhatsAppSource::new(
            config.notifiers.whatsapp.clone(),
        )));
    }

    if config.notifiers.telegram.source_enabled && config.notifiers.telegram.bot_token.is_some() {
        sources.push(Arc::new(TelegramSource::new(
            config.notifiers.telegram.clone(),
        )));
    }

    sources
        .into_iter()
        .map(telemetry::InstrumentedSource::wrap)
        .collect()
}

fn build_review_watcher(
    config: &Config,
    tracker: Arc<dyn storage::FixAttemptTracker>,
) -> Option<Arc<ReviewWatcher>> {
    if !config.is_github_enabled() {
        return None;
    }

    let github_client = github::GitHubClient::new(config.github().clone());
    if !github_client.is_enabled() {
        return None;
    }

    let provider: Arc<dyn ScmProvider> = telemetry::InstrumentedScm::wrap(Arc::new(github_client));
    let review_watcher = ReviewWatcher::with_tracker(provider, tracker.clone());

    if let Ok(states) = tracker.get_active_pr_review_states() {
        if !states.is_empty() {
            review_watcher.load_states(states);
        }
    }

    Some(Arc::new(review_watcher))
}

fn build_embedding_service(
    tracker: &Arc<dyn storage::FixAttemptTracker>,
) -> Option<Arc<IssueEmbeddingService>> {
    match EmbeddingClient::new(EmbeddingConfig::default()) {
        Ok(client) => Some(Arc::new(IssueEmbeddingService::with_defaults(
            Arc::new(client),
            tracker.clone(),
        ))),
        Err(_) => None,
    }
}
