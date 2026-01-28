//! Claudear - Unified watcher for issue trackers and error services.

use clap::{Parser, Subcommand};
use claudear::{
    api::ApiServer,
    config::Config,
    github::{GitHubClient, PrMonitor, PrStatus, ReviewWatcher},
    ipc::{default_socket_path, is_daemon_running, print_response, IpcClient, IpcServer},
    notifier::{
        CompositeNotifier, ConsoleNotifier, DiscordNotifier, EmailNotifier, Notifier, PushNotifier,
        SmsNotifier,
    },
    regression::{
        CompositeChecker, LinearRegressionChecker, LinearRegressionConfig, NoOpChecker,
        RegressionScheduler, RegressionSchedulerConfig, SentryRegressionChecker,
        SentryRegressionConfig,
    },
    release::{ReleaseTracker, ReleaseTrackerConfig},
    repo::{DependencyType, RepoIndex, RepoRelationships},
    reports::{ReportFrequency, ReportGenerator, ReportSchedule, ReportScheduler},
    retry::RetryManager,
    source::{IssueSource, LinearSource, SentrySource},
    storage::{FixAttemptTracker, SqliteTracker},
    types::{ActivityLogEntry, FixAttemptStatus},
    watcher::{Watcher, WatcherOptions},
    webhook::{
        print_setup_result, LinearWebhookHandler, SentryWebhookHandler, WebhookConfigurator,
        WebhookHandlerRegistry, WebhookServer,
    },
};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser)]
#[command(name = "claudear")]
#[command(about = "Unified watcher for issue trackers and error services")]
#[command(version)]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "claudear.yaml")]
    config: String,

    /// Directory for log files (with daily rotation). Set to empty string to disable file logging.
    #[arg(long, env = "CLAUDEAR_LOG_DIR", default_value = "./logs")]
    log_dir: std::path::PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the watcher daemon with all configured services
    Start {
        /// HTTP port for dashboard API and webhooks
        #[arg(long, default_value = "3100")]
        port: u16,

        /// Enable background polling (in addition to webhooks)
        #[arg(long)]
        poll: bool,

        /// Polling interval in milliseconds (requires --poll)
        #[arg(long, default_value = "300000")]
        poll_interval: u64,

        /// Disable webhook server
        #[arg(long)]
        no_webhooks: bool,

        /// Disable dashboard API
        #[arg(long)]
        no_dashboard: bool,
    },

    /// Stop the running watcher daemon
    Stop,

    /// Show the status of the running daemon
    Status,

    /// Pause the running watcher (stops processing new issues)
    Pause,

    /// Resume a paused watcher
    Resume,

    /// Show recent activity from the daemon
    Activity {
        /// Number of entries to show
        #[arg(default_value = "20")]
        limit: usize,
    },

    /// Mark all existing issues as seen (run this first!)
    Seed,

    /// Show what would be processed without running Claude
    DryRun,

    /// Start polling all enabled sources (foreground, no daemon)
    Poll {
        /// Polling interval in milliseconds
        #[arg(default_value = "300000")]
        interval: u64,
    },

    /// Start webhook server for real-time events
    Webhook {
        /// Port to listen on
        #[arg(default_value = "3100")]
        port: u16,

        /// Auto-configure webhooks with Linear/Sentry APIs before starting
        #[arg(long)]
        setup_webhooks: bool,

        /// Public base URL where webhooks will be received (required with --setup-webhooks)
        #[arg(long)]
        base_url: Option<String>,

        /// Path to .env file for saving webhook secrets
        #[arg(long, default_value = ".env")]
        env_file: String,
    },

    /// Manually trigger a fix for an issue
    Trigger {
        /// Source name (linear, sentry)
        source: String,
        /// Issue ID
        issue_id: String,
    },

    /// Reset a failed attempt to allow retry
    Reset {
        /// Source name (linear, sentry)
        source: String,
        /// Issue ID
        issue_id: String,
    },

    /// Show statistics about fix attempts
    Stats,

    /// List configured sources
    Sources,

    /// Start the dashboard API server
    Dashboard {
        /// Port to listen on
        #[arg(default_value = "3100")]
        port: u16,

        /// Path to built dashboard files (optional, serves static files)
        #[arg(long)]
        dashboard_dir: Option<std::path::PathBuf>,
    },

    /// Repository management commands
    #[command(subcommand)]
    Repos(ReposCommands),

    /// Pull request management commands
    #[command(subcommand)]
    Prs(PrsCommands),

    /// Retry management commands
    #[command(subcommand)]
    Retries(RetriesCommands),

    /// Inference analytics and management
    #[command(subcommand)]
    Inference(InferenceCommands),

    /// Report generation and scheduling
    #[command(subcommand)]
    Report(ReportCommands),

    /// Diagnostic commands for debugging
    #[command(subcommand)]
    Diag(DiagCommands),
}

/// Repository management subcommands
#[derive(Subcommand)]
enum ReposCommands {
    /// List all indexed repositories
    List,

    /// Build/refresh the repository file index
    Index {
        /// Force full re-indexing even if already indexed
        #[arg(long, default_value = "false")]
        force: bool,
    },

    /// Search for files across all indexed repositories
    Search {
        /// Search query (file name or partial path)
        query: String,
    },

    /// Show index statistics
    Stats,

    /// Link two repositories (declare a dependency)
    Link {
        /// Upstream repository (the dependency)
        upstream: String,

        /// Downstream repository (depends on upstream)
        downstream: String,

        /// Dependency type (npm, composer, git_submodule, manual)
        #[arg(long, default_value = "manual")]
        dep_type: String,
    },

    /// Show the dependency graph
    Graph {
        /// Start from a specific repository
        #[arg(long)]
        root: Option<String>,
    },

    /// Show what would cascade from a repository change
    Cascade {
        /// Repository that changed
        repo: String,
    },

    /// Auto-discover dependencies by scanning directories
    Discover {
        /// Paths to scan (defaults to config's auto_discover_paths)
        #[arg(long)]
        paths: Vec<String>,

        /// Save discovered dependencies to database
        #[arg(long, default_value = "false")]
        save: bool,

        /// Clear existing dependencies before saving
        #[arg(long, default_value = "false")]
        clear: bool,
    },

    /// [DEPRECATED] Add a repository (repos now auto-discovered via known_orgs)
    #[command(hide = true)]
    Add {
        /// Repository name
        name: String,

        /// Local filesystem path
        #[arg(long)]
        path: Option<String>,

        /// GitHub URL (owner/repo format)
        #[arg(long)]
        github_url: Option<String>,
    },

    /// Sync repository index to database (paths and optionally files)
    Sync {
        /// Also sync file lists (can be slow for large codebases)
        #[arg(long, default_value = "false")]
        files: bool,
    },
}

/// Inference analytics subcommands
#[derive(Subcommand)]
enum InferenceCommands {
    /// Show inference statistics (success rates by confidence level)
    Stats,

    /// List recent inference attempts
    History {
        /// Maximum number of entries to show
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Provide feedback on an inference attempt
    Feedback {
        /// Inference attempt ID
        id: i64,

        /// Whether the inference was correct
        #[arg(long)]
        correct: bool,

        /// Actual repository name if inference was incorrect
        #[arg(long)]
        actual_repo: Option<String>,
    },
}

/// Pull request management subcommands
#[derive(Subcommand)]
enum PrsCommands {
    /// List all PRs that are being tracked
    List,

    /// Monitor PRs for merge status and auto-resolve issues
    Monitor {
        /// Run continuously with polling
        #[arg(long, default_value = "false")]
        continuous: bool,
    },
}

/// Retry management subcommands
#[derive(Subcommand)]
enum RetriesCommands {
    /// List issues that are eligible for retry
    List,

    /// Process ready retries now
    Process,
}

/// Report subcommands
#[derive(Subcommand)]
enum ReportCommands {
    /// Generate and show a report (preview without sending)
    Preview {
        /// Report frequency: daily, weekly, monthly
        #[arg(default_value = "daily")]
        frequency: String,
    },

    /// Generate and send a report immediately
    Send {
        /// Report frequency: daily, weekly, monthly
        #[arg(default_value = "daily")]
        frequency: String,
    },

    /// Start the report scheduler (runs in background)
    Schedule {
        /// Enable daily reports
        #[arg(long, default_value = "true")]
        daily: bool,

        /// Enable weekly reports (Monday)
        #[arg(long, default_value = "false")]
        weekly: bool,

        /// Hour to send reports (0-23 UTC)
        #[arg(long, default_value = "9")]
        hour: u32,
    },
}

/// Diagnostic subcommands
#[derive(Subcommand)]
enum DiagCommands {
    /// Show database table counts and recent operations
    Db,
}

/// Initialize logging with both console and file output.
/// Returns a guard that must be kept alive for the duration of the program.
fn init_logging(
    log_dir: Option<&std::path::Path>,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Console layer - always enabled
    let console_layer = fmt::layer().with_target(false).with_writer(std::io::stdout);

    // File layer - optional, with daily rotation
    if let Some(dir) = log_dir {
        // Create log directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("Warning: Failed to create log directory {:?}: {}", dir, e);
            // Fall back to console-only logging
            tracing_subscriber::registry()
                .with(filter)
                .with(console_layer)
                .init();
            return None;
        }

        // Create file appender with daily rotation
        let file_appender = tracing_appender::rolling::daily(dir, "claudear.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = fmt::layer()
            .with_target(true)
            .with_ansi(false) // No ANSI colors in file
            .with_writer(non_blocking);

        tracing_subscriber::registry()
            .with(filter)
            .with(console_layer)
            .with(file_layer)
            .init();

        tracing::info!("Logging to file: {}/claudear.log", dir.display());
        Some(guard)
    } else {
        // Console-only logging
        tracing_subscriber::registry()
            .with(filter)
            .with(console_layer)
            .init();
        None
    }
}

/// Create a ReviewWatcher if GitHub is configured.
fn create_review_watcher(
    config: &Config,
    tracker: Arc<dyn FixAttemptTracker>,
    sqlite_tracker: Arc<SqliteTracker>,
) -> Option<Arc<ReviewWatcher>> {
    if !config.is_github_enabled() {
        tracing::debug!("GitHub not configured, ReviewWatcher disabled");
        return None;
    }

    let github_client = GitHubClient::new(config.github.clone());
    if !github_client.is_enabled() {
        tracing::debug!("GitHub client not enabled, ReviewWatcher disabled");
        return None;
    }

    let review_watcher =
        ReviewWatcher::with_sqlite_tracker(github_client, tracker, Some(sqlite_tracker.clone()));

    // Restore states from database
    match sqlite_tracker.get_active_pr_review_states() {
        Ok(states) => {
            let count = states.len();
            if count > 0 {
                review_watcher.load_states(states);
                tracing::info!(
                    component = "review_watcher",
                    count = count,
                    "Restored PR review states from database"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                component = "review_watcher",
                error = %e,
                "Failed to restore PR review states from database"
            );
        }
    }

    tracing::info!(
        component = "review_watcher",
        "ReviewWatcher enabled for PR review tracking"
    );

    Some(Arc::new(review_watcher))
}

fn create_sources(config: &Config) -> Vec<Arc<dyn IssueSource>> {
    let mut sources: Vec<Arc<dyn IssueSource>> = Vec::new();

    if let Some(ref linear_config) = config.linear {
        if linear_config.enabled {
            sources.push(Arc::new(LinearSource::new(linear_config.clone())));
            tracing::info!("Linear source initialized");
        }
    }

    if let Some(ref sentry_config) = config.sentry {
        if sentry_config.enabled {
            sources.push(Arc::new(SentrySource::new(sentry_config.clone())));
            tracing::info!("Sentry source initialized");
        }
    }

    sources
}

fn create_webhook_handlers(config: &Config) -> WebhookHandlerRegistry {
    let mut registry = WebhookHandlerRegistry::new();

    if let Some(ref linear_config) = config.linear {
        if linear_config.enabled {
            registry.register(Arc::new(LinearWebhookHandler::new(linear_config.clone())));
            tracing::info!("Linear webhook handler registered");
        }
    }

    if let Some(ref sentry_config) = config.sentry {
        if sentry_config.enabled {
            registry.register(Arc::new(SentryWebhookHandler::new(sentry_config.clone())));
            tracing::info!("Sentry webhook handler registered");
        }
    }

    registry
}

fn create_notifier(config: &Config) -> Arc<dyn Notifier> {
    let mut composite = CompositeNotifier::new();

    // Always add console notifier
    composite.add(Arc::new(ConsoleNotifier::new()));

    // Add Discord if configured
    if config.discord.webhook_url.is_some() {
        composite.add(Arc::new(DiscordNotifier::new(config.discord.clone())));
        tracing::info!("Discord notifier enabled");
    }

    // Add Email if configured
    if let Ok(email_notifier) = EmailNotifier::new(config.email.clone()) {
        if email_notifier.is_enabled() {
            composite.add(Arc::new(email_notifier));
            tracing::info!("Email notifier enabled");
        }
    }

    // Add SMS if configured
    let sms_notifier = SmsNotifier::new(config.sms.clone());
    if sms_notifier.is_enabled() {
        composite.add(Arc::new(sms_notifier));
        tracing::info!("SMS notifier enabled");
    }

    // Add Push if configured
    let push_notifier = PushNotifier::new(config.push.clone());
    if push_notifier.is_enabled() {
        composite.add(Arc::new(push_notifier));
        tracing::info!("Push notifier enabled");
    }

    Arc::new(composite)
}

fn create_tracker(config: &Config) -> (Arc<dyn FixAttemptTracker>, Arc<SqliteTracker>) {
    let tracker =
        Arc::new(SqliteTracker::new(&config.db_path).expect("Failed to initialize SQLite tracker"));
    (tracker.clone(), tracker)
}

/// Start the regression monitoring background tasks.
///
/// This runs two background tasks:
/// 1. ReleaseTracker: Checks if fixes have been included in releases and transitions watches to Monitoring
/// 2. RegressionScheduler: Runs hourly checks on watches in Monitoring state
///
/// Returns a join handle that can be used to stop the monitoring.
fn start_regression_monitoring(
    config: &Config,
    sqlite_tracker: Arc<SqliteTracker>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !config.regression.enabled {
        tracing::info!("Regression monitoring disabled in configuration");
        return None;
    }

    // Get GitHub token (from regression config or fall back to github config)
    let github_token = config
        .regression
        .github_token
        .clone()
        .or_else(|| config.github.token.clone());

    let github_token = match github_token {
        Some(token) if !token.is_empty() => token,
        _ => {
            tracing::warn!("No GitHub token configured, regression monitoring disabled");
            return None;
        }
    };

    // Create release tracker config
    let release_config = ReleaseTrackerConfig {
        target_repos: config.regression.target_repos.clone(),
        poll_interval_ms: config.regression.check_interval_hours as u64 * 60 * 60 * 1000, // Convert hours to ms
    };

    // Create scheduler config
    let scheduler_config = RegressionSchedulerConfig {
        check_interval_hours: config.regression.check_interval_hours,
        monitoring_duration_hours: config.regression.monitoring_duration_hours,
        sentry_event_threshold: config.regression.sentry_event_threshold,
        similarity_threshold: config.regression.similarity_threshold,
    };

    // Create the sentry regression checker if sentry is configured
    let sentry_checker: Box<dyn claudear::regression::RegressionChecker> =
        if let Some(ref sentry_config) = config.sentry {
            if sentry_config.enabled && !sentry_config.auth_token.is_empty() {
                let sentry_regression_config = SentryRegressionConfig {
                    auth_token: sentry_config.auth_token.clone(),
                    org_slug: sentry_config.org_slug.clone(),
                    event_threshold: config.regression.sentry_event_threshold,
                };
                // Use the public HTTP client
                let http_client = claudear::source::sentry::ReqwestSentryClient::new();
                Box::new(SentryRegressionChecker::new(
                    sentry_regression_config,
                    http_client,
                ))
            } else {
                Box::new(NoOpChecker)
            }
        } else {
            Box::new(NoOpChecker)
        };

    // Create the linear regression checker
    // NOTE: LinearRegressionChecker requires per-issue keywords for effective similarity matching.
    // Currently we create it with empty keywords, which means it will only check appwrite.io/threads
    // and won't be able to search GitHub issues effectively. A future improvement would be to
    // store issue keywords in the regression_watches table and load them during each check.
    let linear_checker: Box<dyn claudear::regression::RegressionChecker> = {
        let linear_regression_config = LinearRegressionConfig {
            github_token: github_token.clone(),
            github_repos: config.regression.github_search_repos.clone(),
            similarity_threshold: config.regression.similarity_threshold,
        };
        tracing::warn!(
            "Linear regression checker initialized without issue-specific keywords. \
             GitHub issue search will be limited. Only thread similarity checking will work."
        );
        Box::new(LinearRegressionChecker::new(
            linear_regression_config,
            vec![],
            String::new(),
        ))
    };

    // Create composite checker
    let composite_checker = CompositeChecker::new(sentry_checker, linear_checker);

    // Create release tracker and scheduler
    let release_tracker =
        ReleaseTracker::with_config(github_token, sqlite_tracker.clone(), release_config);

    let scheduler = RegressionScheduler::new(
        composite_checker,
        sqlite_tracker.clone(),
        scheduler_config.clone(),
    );

    let check_interval_hours = scheduler_config.check_interval_hours;

    // Start background task
    let handle = tokio::spawn(async move {
        let mut release_check_interval = interval(Duration::from_secs(300)); // Check for releases every 5 minutes
        let mut regression_check_interval =
            interval(Duration::from_secs(check_interval_hours as u64 * 60 * 60)); // Hourly regression checks

        tracing::info!(
            component = "regression_monitor",
            check_interval_hours = check_interval_hours,
            "Regression monitoring started"
        );

        loop {
            tokio::select! {
                _ = release_check_interval.tick() => {
                    // Check for releases and transition watches to Monitoring
                    match release_tracker.check_pending_watches().await {
                        Ok(transitioned) => {
                            if !transitioned.is_empty() {
                                tracing::info!(
                                    component = "regression_monitor",
                                    count = transitioned.len(),
                                    "Transitioned watches to monitoring state"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                component = "regression_monitor",
                                error = %e,
                                "Error checking for releases"
                            );
                        }
                    }
                }
                _ = regression_check_interval.tick() => {
                    // Run regression checks on watches in Monitoring state
                    match scheduler.check_monitoring_watches().await {
                        Ok(results) => {
                            for result in &results {
                                if result.regression_detected {
                                    tracing::warn!(
                                        component = "regression_monitor",
                                        watch_id = result.watch_id,
                                        check_number = result.check_number,
                                        "Regression detected!"
                                    );
                                } else if result.is_final_check {
                                    tracing::info!(
                                        component = "regression_monitor",
                                        watch_id = result.watch_id,
                                        "Final check complete, no regression"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                component = "regression_monitor",
                                error = %e,
                                "Error running regression checks"
                            );
                        }
                    }
                }
            }
        }
    });

    Some(handle)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging (must keep _guard alive for file logging to work)
    // Empty path disables file logging
    let log_dir = if cli.log_dir.as_os_str().is_empty() {
        None
    } else {
        Some(cli.log_dir.as_path())
    };
    let _log_guard = init_logging(log_dir);

    let config_path = cli.config.clone();

    // Load and validate config from YAML file
    let config = Config::load(&config_path)?;

    // Handle daemon control commands early (don't need full config validation)
    match &cli.command {
        Commands::Stop => {
            if !is_daemon_running() {
                println!("No daemon is running.");
                return Ok(());
            }

            let client = IpcClient::new();
            match client.shutdown().await {
                Ok(response) => {
                    print_response(&response);
                }
                Err(e) => {
                    eprintln!("Failed to stop daemon: {}", e);
                    std::process::exit(1);
                }
            }
            return Ok(());
        }

        Commands::Status => {
            if !is_daemon_running() {
                println!("No daemon is running.");
                println!("Socket path: {:?}", default_socket_path());
                return Ok(());
            }

            let client = IpcClient::new();
            match client.status().await {
                Ok(response) => {
                    print_response(&response);
                }
                Err(e) => {
                    eprintln!("Failed to get status: {}", e);
                    std::process::exit(1);
                }
            }
            return Ok(());
        }

        Commands::Pause => {
            if !is_daemon_running() {
                anyhow::bail!("No daemon is running. Start one with 'claudear start'");
            }

            let client = IpcClient::new();
            match client.pause().await {
                Ok(response) => {
                    print_response(&response);
                }
                Err(e) => {
                    eprintln!("Failed to pause: {}", e);
                    std::process::exit(1);
                }
            }
            return Ok(());
        }

        Commands::Resume => {
            if !is_daemon_running() {
                anyhow::bail!("No daemon is running. Start one with 'claudear start'");
            }

            let client = IpcClient::new();
            match client.resume().await {
                Ok(response) => {
                    print_response(&response);
                }
                Err(e) => {
                    eprintln!("Failed to resume: {}", e);
                    std::process::exit(1);
                }
            }
            return Ok(());
        }

        Commands::Activity { limit } => {
            if !is_daemon_running() {
                anyhow::bail!("No daemon is running. Start one with 'claudear start'");
            }

            let client = IpcClient::new();
            match client.activity(*limit).await {
                Ok(response) => {
                    print_response(&response);
                }
                Err(e) => {
                    eprintln!("Failed to get activity: {}", e);
                    std::process::exit(1);
                }
            }
            return Ok(());
        }

        _ => {}
    }

    // Handle sources command early (doesn't need full validation)
    if matches!(cli.command, Commands::Sources) {
        println!("\nConfigured Sources:");
        if config.is_linear_enabled() {
            if let Some(ref linear) = config.linear {
                println!("  Linear (labels: {})", linear.trigger_labels.join(", "));
                if linear.webhook_secret.is_some() {
                    println!("    Webhook secret: configured");
                }
            }
        } else {
            println!("  Linear (not configured)");
        }

        if config.is_sentry_enabled() {
            if let Some(ref sentry) = config.sentry {
                println!("  Sentry (org: {})", sentry.org_slug);
                if sentry.client_secret.is_some() {
                    println!("    Client secret: configured");
                }
            }
        } else {
            println!("  Sentry (not configured)");
        }

        println!("\nRate Limiting:");
        println!("  Max issues per cycle: {}", config.max_issues_per_cycle);
        println!("  Max concurrent: {}", config.max_concurrent);
        println!("  Processing delay: {}ms", config.processing_delay_ms);

        println!("\nNotifiers:");
        println!("  Console: enabled");
        if config.discord.webhook_url.is_some() {
            println!("  Discord: enabled");
        }
        if config.email.smtp_host.is_some() {
            println!("  Email: enabled");
        }
        if config.sms.account_sid.is_some() {
            println!("  SMS (Twilio): enabled");
        }
        if config.push.api_token.is_some() {
            println!("  Push (Pushover): enabled");
        }

        return Ok(());
    }

    // Handle Repos commands early (don't need sources or repository validation)
    if let Commands::Repos(ref repos_cmd) = cli.command {
        let db_tracker = SqliteTracker::new(&config.db_path)?;

        match repos_cmd {
            ReposCommands::List => {
                // First show indexed repos from the database
                let indexed_repos = db_tracker.list_indexed_repos()?;

                if indexed_repos.is_empty() {
                    println!("\nNo indexed repositories.");
                    println!("Run 'claudear repos index' to build the repository index.");
                } else {
                    println!("\nIndexed Repositories ({}):", indexed_repos.len());
                    for repo in &indexed_repos {
                        println!(
                            "  {} - {} files [{}]",
                            repo.name, repo.file_count, repo.path
                        );
                    }
                }

                // Show known orgs from config
                if !config.known_orgs.is_empty() {
                    println!("\nKnown Organizations: {:?}", config.known_orgs);
                }

                // Show auto-discover paths
                if !config.auto_discover_paths.is_empty() {
                    println!("Auto-Discover Paths: {:?}", config.auto_discover_paths);
                }
            }

            ReposCommands::Index { force } => {
                println!("\nBuilding repository index...");
                println!("  Known orgs: {:?}", config.known_orgs);
                println!("  Scanning: {:?}", config.auto_discover_paths);

                if config.known_orgs.is_empty() || config.auto_discover_paths.is_empty() {
                    anyhow::bail!(
                        "known_orgs and auto_discover_paths must be configured in claudear.yaml"
                    );
                }

                let index = RepoIndex::build(&config.known_orgs, &config.auto_discover_paths)?;

                if index.is_empty() {
                    println!("\nNo repositories found from known orgs in the specified paths.");
                    return Ok(());
                }

                println!("\nDiscovered {} repositories:", index.len());

                // Save to database
                let mut saved_count = 0;
                for repo in index.list() {
                    // Check if already indexed (skip unless force)
                    if !force {
                        if let Ok(Some(_)) = db_tracker.get_indexed_repo(&repo.name) {
                            println!("  {} - skipped (already indexed)", repo.name);
                            continue;
                        }
                    }

                    // Save repo and its files
                    let repo_id = db_tracker.save_indexed_repo(
                        &repo.name,
                        &repo.path.to_string_lossy(),
                        Some(repo.github_url.as_str()),
                        &repo.default_branch,
                        repo.files.len(),
                    )?;

                    // Convert files to (path, file_type) tuples for storage
                    let files_with_types: Vec<(String, Option<String>)> = repo
                        .files
                        .iter()
                        .map(|f| {
                            let file_type = std::path::Path::new(f)
                                .extension()
                                .map(|e| e.to_string_lossy().to_string());
                            (f.clone(), file_type)
                        })
                        .collect();

                    // Save file index
                    db_tracker.save_repo_files(repo_id, &files_with_types)?;

                    println!("  {} - {} files indexed", repo.name, repo.files.len());
                    saved_count += 1;
                }

                println!(
                    "\nIndexed {} repositories to {:?}",
                    saved_count, config.db_path
                );
            }

            ReposCommands::Search { query } => {
                // Build index from config for searching
                if config.known_orgs.is_empty() || config.auto_discover_paths.is_empty() {
                    anyhow::bail!("known_orgs and auto_discover_paths must be configured");
                }

                println!("\nSearching for '{}'...", query);

                let index = RepoIndex::build(&config.known_orgs, &config.auto_discover_paths)?;
                let matches = index.search_files(query);

                if matches.is_empty() {
                    println!("No matches found.");
                } else {
                    println!("\nMatches ({}):", matches.len());
                    for (repo, file) in matches.iter().take(50) {
                        println!("  [{}] {}", repo.name, file);
                    }
                    if matches.len() > 50 {
                        println!("  ... and {} more", matches.len() - 50);
                    }
                }
            }

            ReposCommands::Stats => {
                let stats = db_tracker.get_index_stats()?;

                println!("\nRepository Index Statistics:");
                println!("  Total repositories: {}", stats.repo_count);
                println!("  Total files indexed: {}", stats.file_count);

                if stats.repo_count > 0 {
                    println!(
                        "  Average files per repo: {:.1}",
                        stats.file_count as f64 / stats.repo_count as f64
                    );
                }

                if let Some(ref last_indexed) = stats.last_indexed_at {
                    println!("  Last indexed: {}", last_indexed);
                }

                // Also show inference stats if available
                if let Ok(inference_stats) = db_tracker.get_inference_stats() {
                    println!("\n  Inference Statistics:");
                    println!("    Total attempts: {}", inference_stats.total_attempts);
                    if inference_stats.total_attempts > 0 {
                        println!("    With feedback: {}", inference_stats.with_feedback);
                        if inference_stats.with_feedback > 0 {
                            println!("    Accuracy: {:.1}%", inference_stats.accuracy * 100.0);
                        }
                    }
                }
            }

            ReposCommands::Add {
                name,
                path,
                github_url,
            } => {
                println!("\n=== Deprecated ===\n");
                println!("The 'repos add' command is deprecated.");
                println!("Repositories are now auto-discovered from known_orgs config.");
                println!("\nUse 'claudear repos index' instead.");
                println!(
                    "\nIf you still want to manually track '{}', add the org to known_orgs",
                    name
                );
                let _ = (path, github_url); // Suppress unused warning
            }

            ReposCommands::Link {
                upstream,
                downstream,
                dep_type,
            } => {
                db_tracker.add_dependency(upstream, downstream, dep_type)?;
                println!(
                    "Linked: {} depends on {} ({})",
                    downstream, upstream, dep_type
                );
                println!("  Saved to database at: {:?}", config.db_path);
            }

            ReposCommands::Graph { root } => {
                // Load dependencies from DB
                let mut manager = RepoRelationships::with_defaults();

                // Add DB dependencies to the manager
                let db_deps = db_tracker.list_all_dependencies()?;
                for dep in db_deps {
                    let dtype =
                        DependencyType::parse(&dep.dep_type).unwrap_or(DependencyType::Manual);
                    manager
                        .add_dependency(&dep.upstream, &dep.downstream, dtype, None)
                        .ok();
                }

                println!("\n=== Repository Dependency Graph ===\n");
                let tree = manager.print_tree(root.as_deref());
                print!("{}", tree);

                if root.is_none() {
                    println!("\nLegend: upstream -> downstream (changes in upstream may require updates in downstream)");
                }
            }

            ReposCommands::Cascade { repo } => {
                // Query DB directly for dependants
                let direct = db_tracker.get_direct_dependants(repo)?;
                let all = db_tracker.get_all_dependants(repo)?;

                println!("\n=== Cascading Changes from {} ===\n", repo);

                if direct.is_empty() {
                    println!("  No downstream repositories depend on {}", repo);
                } else {
                    println!("  Changes in {} would trigger updates in:", repo);
                    for dep in &direct {
                        println!("    -> {} (VersionBump, {})", dep.downstream, dep.dep_type);
                    }

                    // Show transitive dependants (depth > 1)
                    let indirect: Vec<_> = all.iter().filter(|(_, depth)| *depth > 1).collect();
                    if !indirect.is_empty() {
                        println!("\n  Transitively affected:");
                        for (name, depth) in indirect {
                            println!("    -> {} (indirect, depth {})", name, depth);
                        }
                    }
                }
            }

            ReposCommands::Discover { paths, save, clear } => {
                use claudear::DependencyDiscovery;

                // Use provided paths or fall back to config
                let scan_paths = if paths.is_empty() {
                    config.auto_discover_paths.clone()
                } else {
                    paths.clone()
                };

                if scan_paths.is_empty() {
                    anyhow::bail!(
                        "No paths to scan. Provide --paths or set auto_discover_paths in config.\n\
                        Example: claudear repos discover --paths ~/Local"
                    );
                }

                println!("\n=== Auto-Discovering Dependencies ===\n");
                println!("Known orgs: {:?}", config.known_orgs);
                println!("Scanning: {:?}\n", scan_paths);

                let discovery = DependencyDiscovery::new(config.known_orgs.clone());
                let discovered = discovery.scan_directories(&scan_paths)?;

                if discovered.is_empty() {
                    println!("No dependencies found from known organizations.");
                    return Ok(());
                }

                // Group by repo for display
                let mut by_repo: std::collections::HashMap<String, Vec<_>> =
                    std::collections::HashMap::new();
                for dep in &discovered {
                    by_repo.entry(dep.repo.clone()).or_default().push(dep);
                }

                println!(
                    "Discovered {} dependencies across {} repositories:\n",
                    discovered.len(),
                    by_repo.len()
                );
                for (repo, deps) in &by_repo {
                    println!("  {}", repo);
                    for dep in deps {
                        println!("    -> {} ({})", dep.depends_on, dep.dep_type);
                    }
                }

                if *save {
                    if *clear {
                        println!("\nClearing existing dependencies...");
                        db_tracker.clear_repositories()?;
                    }

                    println!("\nSaving to database...");
                    for dep in &discovered {
                        // Add repo with its path
                        db_tracker.upsert_repository(&dep.repo, Some(&dep.repo_path), None)?;
                        // Add dependency
                        db_tracker.add_dependency(&dep.depends_on, &dep.repo, &dep.dep_type)?;
                    }
                    println!(
                        "Saved {} dependencies to {:?}",
                        discovered.len(),
                        config.db_path
                    );
                } else {
                    println!("\nRun with --save to persist to database.");
                }
            }

            ReposCommands::Sync { files } => {
                use claudear::repo::RepoIndex;

                println!("\nSyncing repository index to database...");

                if config.known_orgs.is_empty() || config.auto_discover_paths.is_empty() {
                    anyhow::bail!("known_orgs and auto_discover_paths must be configured");
                }

                // Build in-memory index
                let index = RepoIndex::build(&config.known_orgs, &config.auto_discover_paths)?;
                if index.is_empty() {
                    println!("No repositories found.");
                    return Ok(());
                }

                println!(
                    "  Found {} repositories with {} files",
                    index.len(),
                    index.total_files()
                );

                // Sync to database
                let synced = db_tracker.sync_from_index(&index, *files)?;

                println!("\nSynced {} repository paths to database", synced);
                if *files {
                    println!("  Including file lists (this may have taken a while)");
                    // Show stats after file sync
                    let stats = db_tracker.get_index_stats()?;
                    println!("  Total files in database: {}", stats.file_count);
                } else {
                    println!("\nRun with --files to also sync file lists (slower)");
                }
            }
        }

        return Ok(());
    }

    // Handle Inference commands early
    if let Commands::Inference(ref inference_cmd) = cli.command {
        let db_tracker = SqliteTracker::new(&config.db_path)?;

        match inference_cmd {
            InferenceCommands::Stats => {
                let stats = db_tracker.get_inference_stats()?;

                println!("\nInference Statistics:");
                println!("  Total inference attempts: {}", stats.total_attempts);
                println!("  Attempts with feedback: {}", stats.with_feedback);

                if stats.with_feedback > 0 {
                    println!(
                        "  Correct inferences: {} ({:.1}%)",
                        stats.correct,
                        stats.accuracy * 100.0
                    );
                }

                println!("\n  By confidence level:");
                println!("    High:   {} attempts", stats.by_confidence.high);
                println!("    Medium: {} attempts", stats.by_confidence.medium);
                println!("    Low:    {} attempts", stats.by_confidence.low);
                println!("    None:   {} attempts", stats.by_confidence.none);
            }

            InferenceCommands::History { limit } => {
                let history = db_tracker.get_inference_history(*limit)?;

                if history.is_empty() {
                    println!("\nNo inference attempts recorded yet.");
                    println!("  Run 'claudear process' to start processing issues.");
                } else {
                    println!("\nRecent Inference Attempts (last {}):\n", history.len());

                    for entry in &history {
                        let status = match entry.was_correct {
                            Some(true) => "✓ correct",
                            Some(false) => "✗ incorrect",
                            None => "? pending",
                        };

                        let repo_display =
                            entry.inferred_repo_name.as_deref().unwrap_or("(no match)");

                        let confidence_display = entry.confidence.as_deref().unwrap_or("none");

                        let duration_display = entry
                            .duration_ms
                            .map(|ms| format!("{}ms", ms))
                            .unwrap_or_else(|| "-".to_string());

                        println!(
                            "  #{} [{}] {} → {} ({}) [{}]",
                            entry.id,
                            entry.issue_source,
                            entry.issue_id,
                            repo_display,
                            confidence_display,
                            status
                        );

                        if let Some(ref reason) = entry.inference_reason {
                            // Truncate long reasons
                            let truncated = if reason.len() > 60 {
                                format!("{}...", &reason[..57])
                            } else {
                                reason.clone()
                            };
                            println!("      Reason: {}", truncated);
                        }

                        if let Some(ref keywords) = entry.extracted_keywords {
                            // Truncate long keyword lists
                            let truncated = if keywords.len() > 50 {
                                format!("{}...", &keywords[..47])
                            } else {
                                keywords.clone()
                            };
                            println!("      Keywords: {}", truncated);
                        }

                        println!(
                            "      Time: {} | Duration: {}",
                            entry.created_at, duration_display
                        );
                        println!();
                    }

                    println!("  Use 'claudear inference feedback <id> --correct/--incorrect' to provide feedback.");
                }
            }

            InferenceCommands::Feedback {
                id,
                correct,
                actual_repo,
            } => {
                let actual_repo_id = if let Some(ref repo_name) = actual_repo {
                    // Look up repo ID by name
                    if let Some(repo) = db_tracker.get_indexed_repo(repo_name)? {
                        Some(repo.id)
                    } else {
                        anyhow::bail!("Repository '{}' not found in index", repo_name);
                    }
                } else {
                    None
                };

                db_tracker.record_inference_feedback(*id, *correct, actual_repo_id, "manual")?;

                println!("\nFeedback recorded for inference attempt {}:", id);
                println!("  Correct: {}", correct);
                if let Some(ref repo_name) = actual_repo {
                    println!("  Actual repo: {}", repo_name);
                }
            }
        }

        return Ok(());
    }

    // Handle Diag commands early (don't need sources or full validation)
    if let Commands::Diag(ref diag_cmd) = cli.command {
        let db_tracker = SqliteTracker::new(&config.db_path)?;

        match diag_cmd {
            DiagCommands::Db => {
                let counts = db_tracker.get_diagnostic_counts()?;

                println!("\n=== Database Diagnostics ===\n");
                println!("Database path: {:?}", config.db_path);
                println!();

                println!("Table Counts:");
                println!("  fix_attempts:       {}", counts.fix_attempts);
                if !counts.fix_attempts_by_status.is_empty() {
                    println!("    By status:");
                    for (status, count) in &counts.fix_attempts_by_status {
                        println!("      {}: {}", status, count);
                    }
                }
                println!("  activity_log:       {}", counts.activity_log);
                println!("  claude_executions:  {}", counts.claude_executions);
                println!("  pr_reviews:         {}", counts.pr_reviews);
                println!("  pr_review_states:   {}", counts.pr_review_states);
                println!("  issue_embeddings:   {}", counts.issue_embeddings);
                println!("  similar_issues:     {}", counts.similar_issues);
                println!("  repositories:       {}", counts.repositories);
                println!("  repo_files:         {}", counts.repo_files);
                println!("  inference_attempts: {}", counts.inference_attempts);
                println!("  error_patterns:     {}", counts.error_patterns);
                println!("  processing_metrics: {}", counts.processing_metrics);
                println!("  feedback_outcomes:  {}", counts.feedback_outcomes);
                println!("  prs:                {}", counts.prs);

                if !counts.recent_fix_attempts.is_empty() {
                    println!("\nRecent Fix Attempts (last 5):");
                    for (source, issue_id, short_id, status) in &counts.recent_fix_attempts {
                        println!("  [{}] {} ({}) - {}", source, short_id, issue_id, status);
                    }
                } else {
                    println!("\nNo fix attempts recorded yet.");
                    println!("\nTo debug why fix_attempts is empty:");
                    println!(
                        "  1. Run with verbose logging: RUST_LOG=claudear=debug claudear poll"
                    );
                    println!("  2. Check that issues have the trigger labels configured");
                    println!("  3. Verify the watcher is processing issues (check activity_log)");
                }
            }
        }

        return Ok(());
    }

    config.validate()?;

    // Initialize components
    tracing::info!("Initializing...");
    let notifier = create_notifier(&config);
    let (tracker, sqlite_tracker) = create_tracker(&config);

    // Handle Start command (daemon mode with IPC - runs all services concurrently)
    if let Commands::Start {
        port,
        poll,
        poll_interval,
        no_webhooks,
        no_dashboard,
    } = &cli.command
    {
        if is_daemon_running() {
            anyhow::bail!("A daemon is already running. Stop it first with 'claudear stop'");
        }

        let sources = create_sources(&config);
        if sources.is_empty() {
            anyhow::bail!("No sources were initialized");
        }

        // Determine what services to run
        let enable_webhooks = !no_webhooks;
        let enable_dashboard = !no_dashboard;
        let enable_polling = *poll;

        if !enable_webhooks && !enable_dashboard && !enable_polling {
            anyhow::bail!("No services enabled. Remove --no-webhooks/--no-dashboard or add --poll");
        }

        // Build mode string for status
        let mut modes = Vec::new();
        if enable_dashboard {
            modes.push("dashboard");
        }
        if enable_webhooks {
            modes.push("webhooks");
        }
        if enable_polling {
            modes.push("polling");
        }
        let mode_str = modes.join("+");

        // Build repository inferrer for issue-to-repo mapping
        let inferrer = Watcher::build_inferrer(&config)?;
        if inferrer.is_some() {
            tracing::info!("Repository inference enabled");
        }

        // Create ReviewWatcher for PR review tracking
        let review_watcher =
            create_review_watcher(&config, tracker.clone(), sqlite_tracker.clone());

        // Create watcher if polling is enabled
        let watcher = if enable_polling {
            Some(Arc::new(Watcher::new(WatcherOptions {
                config: config.clone(),
                sources: sources.clone(),
                notifier: notifier.clone(),
                tracker: tracker.clone(),
                sqlite_tracker: Some(sqlite_tracker.clone()),
                inferrer: inferrer.clone(),
                embedding_client: None,
                review_watcher,
                issue_embedding_service: None,
                dry_run: false,
            })))
        } else {
            None
        };

        // Create IPC server
        let ipc_server = Arc::new(if let Some(ref w) = watcher {
            IpcServer::new(tracker.clone(), sources.clone(), notifier.clone())
                .with_watcher(w.clone())
        } else {
            IpcServer::new(tracker.clone(), sources.clone(), notifier.clone())
        });

        ipc_server.set_mode(&mode_str).await;
        if enable_polling {
            ipc_server.set_poll_interval(*poll_interval);
        }

        // Log watcher_started activity
        let activity = ActivityLogEntry::new(
            "watcher_started",
            format!("Watcher daemon started in {} mode", mode_str),
        )
        .with_source("system".to_string())
        .with_metadata(json!({
            "mode": mode_str,
            "port": port,
            "sources": sources.iter().map(|s| s.name()).collect::<Vec<_>>()
        }));
        tracker.record_activity(&activity).ok();

        // Log startup info
        tracing::info!("Starting watcher daemon...");
        tracing::info!("  Mode: {}", mode_str);
        tracing::info!("  Port: {}", port);
        tracing::info!("  Socket: {:?}", default_socket_path());
        tracing::info!(
            "  Sources: {}",
            sources
                .iter()
                .map(|s| s.name())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if enable_polling {
            tracing::info!("  Poll interval: {}ms", poll_interval);
        }

        // Start regression monitoring background task
        let regression_handle = start_regression_monitoring(&config, sqlite_tracker.clone());
        if regression_handle.is_some() {
            tracing::info!("  Regression monitoring: enabled");
        }

        // Shutdown signal handler with graceful drain
        let watcher_for_shutdown = watcher.clone();
        let tracker_for_shutdown = tracker.clone();
        let mut shutdown_rx = ipc_server.shutdown_receiver();
        let shutdown = async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("\nReceived shutdown signal, initiating graceful shutdown...");
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("\nReceived IPC shutdown command, initiating graceful shutdown...");
                }
            }
            if let Some(w) = watcher_for_shutdown {
                // Use stop_and_drain for graceful shutdown that waits for active tasks
                w.stop_and_drain().await;
            }

            // Log watcher_stopped activity
            let activity = ActivityLogEntry::new("watcher_stopped", "Watcher daemon stopped")
                .with_source("system".to_string());
            tracker_for_shutdown.record_activity(&activity).ok();
        };

        // Abort regression monitoring on shutdown
        let regression_shutdown = {
            let handle = regression_handle;
            async move {
                if let Some(h) = handle {
                    h.abort();
                }
            }
        };

        // Build the unified HTTP server with dashboard API + webhooks
        let mut config = config.clone();
        config.webhook_port = *port;

        // Start all services concurrently
        let ipc_future = ipc_server.start();

        // Dashboard + Webhooks can share the same axum server
        // For now, we'll run them on the same port with webhooks taking precedence
        let inferrer_clone = inferrer.clone();
        let http_future = async move {
            if enable_webhooks {
                let handlers = create_webhook_handlers(&config);
                if handlers.get_all().is_empty() && !enable_dashboard {
                    return Err(anyhow::anyhow!("No webhook handlers configured"));
                }

                // Webhook server also serves health endpoint which dashboard uses
                let server = WebhookServer::new(
                    config.clone(),
                    handlers,
                    notifier.clone(),
                    tracker.clone(),
                    Some(sqlite_tracker.clone()),
                    inferrer_clone,
                );
                server.start().await?;
            } else if enable_dashboard {
                // Dashboard only (no webhooks)
                let server = ApiServer::with_port(config.clone(), tracker.clone(), *port);
                server.start().await?;
            }
            Ok::<(), anyhow::Error>(())
        };

        let poll_future = async {
            if let Some(w) = watcher {
                w.start(Some(*poll_interval)).await?;
            } else {
                // Just wait forever if no polling
                std::future::pending::<()>().await;
            }
            Ok::<(), anyhow::Error>(())
        };

        // Run everything concurrently
        tokio::select! {
            result = ipc_future => {
                if let Err(e) = result {
                    tracing::error!("IPC server error: {}", e);
                }
            }
            result = http_future => {
                if let Err(e) = result {
                    tracing::error!("HTTP server error: {}", e);
                }
            }
            result = poll_future => {
                if let Err(e) = result {
                    tracing::error!("Polling error: {}", e);
                }
            }
            _ = shutdown => {
                // Stop regression monitoring
                regression_shutdown.await;
            }
        }

        return Ok(());
    }

    // Handle PR commands early since they don't need sources
    if let Commands::Prs(PrsCommands::Monitor { continuous }) = cli.command {
        if !config.is_github_enabled() {
            anyhow::bail!("GitHub token not configured. Set GITHUB_TOKEN environment variable.");
        }

        let github_client = GitHubClient::new(config.github.clone());
        let sources = create_sources(&config);
        let pr_monitor = PrMonitor::new(
            github_client,
            tracker.clone(),
            config.github.auto_resolve_on_merge,
        );

        if continuous {
            tracing::info!("Starting PR monitor (continuous mode)...");
            tracing::info!("  Poll interval: {}ms", config.github.poll_interval_ms);
            tracing::info!(
                "  Auto-resolve on merge: {}",
                config.github.auto_resolve_on_merge
            );

            let mut poll_timer = interval(Duration::from_millis(config.github.poll_interval_ms));

            let shutdown = async {
                tokio::signal::ctrl_c()
                    .await
                    .expect("Failed to install signal handler");
                tracing::info!("\nReceived shutdown signal...");
            };

            tokio::select! {
                _ = async {
                    loop {
                        poll_timer.tick().await;
                        match pr_monitor.check_pending_prs().await {
                            Ok(updates) => {
                                for update in updates {
                                    match update.new_status {
                                        PrStatus::Merged => {
                                            tracing::info!(component = "pr_monitor", short_id = %update.short_id, pr_url = %update.pr_url, "PR merged");

                                            // Auto-resolve on source if enabled
                                            if update.should_resolve {
                                                if let Some(source) = sources.iter().find(|s| s.name() == update.source) {
                                                    if let Err(e) = source.resolve_issue(&update.issue_id).await {
                                                        tracing::warn!(component = "pr_monitor", source = %update.source, error = %e, "Failed to resolve issue");
                                                    } else {
                                                        tracker.mark_resolved(&update.source, &update.issue_id).ok();
                                                        notifier.notify_merged(&claudear::types::Issue::new(
                                                            &update.issue_id,
                                                            &update.short_id,
                                                            "Issue resolved",
                                                            &update.pr_url,
                                                            &update.source,
                                                        ), &update.pr_url).await.ok();
                                                    }
                                                }
                                            }
                                        }
                                        PrStatus::Closed => {
                                            tracing::info!(component = "pr_monitor", short_id = %update.short_id, pr_url = %update.pr_url, "PR closed without merge");
                                        }
                                        PrStatus::Open => {}
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!(component = "pr_monitor", error = %e, "Error checking PRs");
                            }
                        }
                    }
                } => {}
                _ = shutdown => {}
            }
        } else {
            // One-shot check
            tracing::info!("Checking pending PRs...");
            let updates = pr_monitor.check_pending_prs().await?;

            if updates.is_empty() {
                println!("\nNo PR status changes detected.");
            } else {
                println!("\nPR Status Updates:");
                for update in updates {
                    let status = match update.new_status {
                        PrStatus::Merged => "MERGED",
                        PrStatus::Closed => "CLOSED",
                        PrStatus::Open => "OPEN",
                    };
                    println!("  [{}] {} - {}", status, update.short_id, update.pr_url);

                    // Auto-resolve on source if merged
                    if update.should_resolve {
                        if let Some(source) = sources.iter().find(|s| s.name() == update.source) {
                            if let Err(e) = source.resolve_issue(&update.issue_id).await {
                                tracing::warn!(
                                    "Failed to resolve issue on {}: {}",
                                    update.source,
                                    e
                                );
                            } else {
                                tracker.mark_resolved(&update.source, &update.issue_id).ok();
                                println!("    -> Issue resolved on {}", update.source);
                            }
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    if let Commands::Prs(PrsCommands::List) = cli.command {
        let pending_prs = tracker.get_pending_prs()?;
        let merged = tracker.get_attempts_by_status(FixAttemptStatus::Merged)?;
        let closed = tracker.get_attempts_by_status(FixAttemptStatus::Closed)?;

        println!("\n=== Pending PRs (awaiting merge) ===");
        if pending_prs.is_empty() {
            println!("  No pending PRs");
        } else {
            for attempt in &pending_prs {
                println!(
                    "  [{}] {} - {}",
                    attempt.source,
                    attempt.short_id,
                    attempt.pr_url.as_deref().unwrap_or("N/A")
                );
            }
        }

        println!("\n=== Merged PRs ===");
        if merged.is_empty() {
            println!("  No merged PRs");
        } else {
            for attempt in merged.iter().take(10) {
                let resolved = if attempt.resolved_at.is_some() {
                    " (resolved)"
                } else {
                    ""
                };
                println!(
                    "  [{}] {}{} - {}",
                    attempt.source,
                    attempt.short_id,
                    resolved,
                    attempt.pr_url.as_deref().unwrap_or("N/A")
                );
            }
            if merged.len() > 10 {
                println!("  ... and {} more", merged.len() - 10);
            }
        }

        println!("\n=== Closed PRs (not merged) ===");
        if closed.is_empty() {
            println!("  No closed PRs");
        } else {
            for attempt in closed.iter().take(10) {
                println!(
                    "  [{}] {} - {}",
                    attempt.source,
                    attempt.short_id,
                    attempt.pr_url.as_deref().unwrap_or("N/A")
                );
            }
            if closed.len() > 10 {
                println!("  ... and {} more", closed.len() - 10);
            }
        }

        return Ok(());
    }

    if let Commands::Retries(RetriesCommands::List) = cli.command {
        let retry_manager = RetryManager::new(config.retry.clone(), tracker.clone());
        let retryable = tracker.get_retryable_issues(config.retry.max_retries)?;
        let ready = retry_manager.get_ready_retries()?;

        println!(
            "\n=== Retryable Issues (max_retries: {}) ===",
            config.retry.max_retries
        );
        if retryable.is_empty() {
            println!("  No issues eligible for retry");
        } else {
            for attempt in &retryable {
                let next_retry = retry_manager.get_next_retry_time(attempt);
                let ready_str = if ready.iter().any(|r| r.id == attempt.id) {
                    " [READY]"
                } else {
                    ""
                };
                let next_time = next_retry
                    .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| "N/A".to_string());

                println!(
                    "  [{}] {} - retry {}/{} - next: {}{}",
                    attempt.source,
                    attempt.short_id,
                    attempt.retry_count,
                    config.retry.max_retries,
                    next_time,
                    ready_str
                );
                if let Some(ref error) = attempt.error_message {
                    let truncated = if error.len() > 60 {
                        format!("{}...", &error[..60])
                    } else {
                        error.clone()
                    };
                    println!("       Error: {}", truncated);
                }
            }
        }

        println!("\n=== Ready for Retry Now ===");
        if ready.is_empty() {
            println!("  No issues ready for retry");
        } else {
            println!("  {} issues ready", ready.len());
            for attempt in &ready {
                println!("  - [{}] {}", attempt.source, attempt.short_id);
            }
        }

        return Ok(());
    }

    if let Commands::Retries(RetriesCommands::Process) = cli.command {
        let retry_manager = RetryManager::new(config.retry.clone(), tracker.clone());
        let ready = retry_manager.get_ready_retries()?;

        if ready.is_empty() {
            println!("\nNo issues ready for retry.");
            return Ok(());
        }

        println!("\nProcessing {} retries...", ready.len());

        // Need sources and watcher for processing
        let sources = create_sources(&config);
        if sources.is_empty() {
            anyhow::bail!("No sources were initialized");
        }

        // Build inferrer for retry processing
        let inferrer = Watcher::build_inferrer(&config)?;

        // Create ReviewWatcher for PR review tracking
        let review_watcher =
            create_review_watcher(&config, tracker.clone(), sqlite_tracker.clone());

        let watcher = Watcher::new(WatcherOptions {
            config: config.clone(),
            sources,
            notifier,
            tracker: tracker.clone(),
            sqlite_tracker: Some(sqlite_tracker.clone()),
            inferrer,
            embedding_client: None,
            review_watcher,
            issue_embedding_service: None,
            dry_run: false,
        });

        for attempt in ready {
            println!("\n  Retrying [{}] {}...", attempt.source, attempt.short_id);

            // Prepare for retry
            retry_manager.prepare_retry(&attempt.source, &attempt.issue_id)?;

            // Trigger the fix
            if let Err(e) = watcher
                .trigger_issue(&attempt.source, &attempt.issue_id)
                .await
            {
                tracing::error!("Failed to retry {}: {}", attempt.short_id, e);
            }
        }

        println!("\nRetry processing complete.");
        return Ok(());
    }

    if let Commands::Dashboard {
        port,
        dashboard_dir,
    } = cli.command
    {
        let server = if let Some(dir) = dashboard_dir {
            ApiServer::with_dashboard(config, tracker, port, dir)
        } else {
            ApiServer::with_port(config, tracker, port)
        };

        // Handle shutdown signals
        let shutdown = async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to install signal handler");
            tracing::info!("\nReceived shutdown signal...");
        };

        tokio::select! {
            result = server.start() => result?,
            _ = shutdown => {}
        }

        return Ok(());
    }

    if let Commands::Report(ReportCommands::Preview { frequency }) = cli.command {
        let freq = ReportFrequency::parse(&frequency).ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid frequency: {}. Use daily, weekly, or monthly",
                frequency
            )
        })?;

        let generator = ReportGenerator::new(tracker);
        let report = match freq {
            ReportFrequency::Daily => generator.generate_daily()?,
            ReportFrequency::Weekly(_) => generator.generate_weekly()?,
            ReportFrequency::Monthly => generator.generate_monthly()?,
        };

        println!("{}", report.format_text());
        return Ok(());
    }

    if let Commands::Report(ReportCommands::Send { frequency }) = cli.command {
        let freq = ReportFrequency::parse(&frequency).ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid frequency: {}. Use daily, weekly, or monthly",
                frequency
            )
        })?;

        let scheduler = ReportScheduler::new(tracker, notifier);
        let report = scheduler.send_now(freq).await?;

        println!("Report sent successfully!");
        println!("{}", report.format_text());
        return Ok(());
    }

    if let Commands::Report(ReportCommands::Schedule {
        daily,
        weekly,
        hour,
    }) = cli.command
    {
        let mut scheduler = ReportScheduler::new(tracker, notifier);

        if daily {
            scheduler.add_schedule(ReportSchedule::daily("daily-report", hour));
            tracing::info!("Daily reports scheduled at {:02}:00 UTC", hour);
        }

        if weekly {
            scheduler.add_schedule(ReportSchedule::weekly(
                "weekly-report",
                chrono::Weekday::Mon,
                hour,
            ));
            tracing::info!("Weekly reports scheduled for Monday at {:02}:00 UTC", hour);
        }

        if scheduler.schedules().is_empty() {
            anyhow::bail!("No report schedules enabled. Use --daily or --weekly");
        }

        tracing::info!("Report scheduler started...");

        // Run scheduler every hour to check for due reports
        let mut check_timer = interval(Duration::from_secs(3600));

        let shutdown = async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to install signal handler");
            tracing::info!("\nReceived shutdown signal...");
        };

        tokio::select! {
            _ = async {
                loop {
                    check_timer.tick().await;
                    match scheduler.check_and_send().await {
                        Ok(sent) => {
                            for name in sent {
                                tracing::info!("Sent report: {}", name);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error checking schedules: {}", e);
                        }
                    }
                }
            } => {}
            _ = shutdown => {}
        }

        return Ok(());
    }

    match cli.command {
        Commands::Webhook {
            port,
            setup_webhooks,
            base_url,
            env_file,
        } => {
            let mut config = config;
            config.webhook_port = port;

            // Auto-configure webhooks if requested
            if setup_webhooks {
                let base_url = base_url.ok_or_else(|| {
                    anyhow::anyhow!(
                        "--base-url is required with --setup-webhooks. \
                        Example: --base-url https://my-server.example.com:3100"
                    )
                })?;

                let env_path = std::path::PathBuf::from(&env_file);
                let configurator = WebhookConfigurator::new(config.clone(), &env_path);

                match configurator.configure(&base_url).await {
                    Ok(result) => {
                        print_setup_result(&result);

                        // Reload config to get the new secrets from env vars
                        // (webhook secrets are stored in env file, which overrides YAML config)
                        tracing::info!("Reloading configuration with new secrets...");
                        config = Config::load(&config_path)?;
                        config.webhook_port = port;
                    }
                    Err(e) => {
                        anyhow::bail!("Webhook auto-configuration failed: {}", e);
                    }
                }
            }

            let handlers = create_webhook_handlers(&config);

            if handlers.get_all().is_empty() {
                anyhow::bail!("No webhook handlers were registered");
            }

            // Build inferrer for repo inference
            let inferrer = WebhookServer::build_inferrer(&config)?;

            let server = WebhookServer::new(
                config,
                handlers,
                notifier,
                tracker,
                Some(sqlite_tracker),
                inferrer,
            );

            // Handle shutdown signals
            let shutdown = async {
                tokio::signal::ctrl_c()
                    .await
                    .expect("Failed to install signal handler");
                tracing::info!("\nReceived shutdown signal...");
            };

            tokio::select! {
                result = server.start() => result?,
                _ = shutdown => {}
            }
        }

        _ => {
            // Initialize sources for polling/seed/dry-run modes
            tracing::info!("Initializing sources...");
            let sources = create_sources(&config);

            if sources.is_empty() {
                anyhow::bail!("No sources were initialized");
            }

            let dry_run = matches!(cli.command, Commands::DryRun);

            // Build inferrer for repo inference (with embeddings for semantic matching)
            let (inferrer, embedding_client) =
                Watcher::build_inferrer_with_embeddings(&config).await?;

            // Create ReviewWatcher for PR review tracking
            let review_watcher =
                create_review_watcher(&config, tracker.clone(), sqlite_tracker.clone());

            let watcher = Watcher::new(WatcherOptions {
                config: config.clone(),
                sources,
                notifier,
                tracker,
                sqlite_tracker: Some(sqlite_tracker.clone()),
                inferrer,
                embedding_client,
                review_watcher,
                issue_embedding_service: None,
                dry_run,
            });

            // Handle shutdown signals
            let watcher_ref = &watcher;

            match cli.command {
                Commands::Seed => {
                    watcher.seed().await?;
                }

                Commands::DryRun => {
                    let shutdown = async {
                        tokio::signal::ctrl_c()
                            .await
                            .expect("Failed to install signal handler");
                        tracing::info!("\nReceived shutdown signal...");
                        watcher_ref.stop();
                    };

                    tokio::select! {
                        result = watcher.start(None) => result?,
                        _ = shutdown => {}
                    }
                }

                Commands::Poll { interval } => {
                    let shutdown = async {
                        tokio::signal::ctrl_c()
                            .await
                            .expect("Failed to install signal handler");
                        tracing::info!("\nReceived shutdown signal...");
                        watcher_ref.stop();
                    };

                    tokio::select! {
                        result = watcher.start(Some(interval)) => result?,
                        _ = shutdown => {}
                    }
                }

                Commands::Trigger { source, issue_id } => {
                    watcher.trigger_issue(&source, &issue_id).await?;
                }

                Commands::Reset { source, issue_id } => {
                    watcher.reset_attempt(&source, &issue_id)?;
                }

                Commands::Stats => {
                    let stats = watcher.get_stats()?;
                    println!("\nFix Attempt Statistics:");
                    println!("  Total:      {}", stats.total);
                    println!("  Pending:    {}", stats.pending);
                    println!("  Success:    {} (PRs created)", stats.success);
                    println!(
                        "  Merged:     {} (PRs merged, issues resolved)",
                        stats.merged
                    );
                    println!("  Closed:     {} (PRs closed without merge)", stats.closed);
                    println!("  Failed:     {}", stats.failed);
                    println!("  Cannot Fix: {} (max retries reached)", stats.cannot_fix);

                    // Calculate success rate
                    let completed = stats.merged + stats.closed + stats.failed + stats.cannot_fix;
                    if completed > 0 {
                        let merge_rate = (stats.merged as f64 / completed as f64) * 100.0;
                        println!("\n  Merge Rate: {:.1}%", merge_rate);
                    }

                    if !stats.by_source.is_empty() {
                        println!("\nBy Source:");
                        for (source, source_stats) in &stats.by_source {
                            println!("  {}:", source);
                            println!(
                                "    Total: {}, Success: {}, Merged: {}, Closed: {}, Failed: {}, Cannot Fix: {}",
                                source_stats.total, source_stats.success, source_stats.merged,
                                source_stats.closed, source_stats.failed, source_stats.cannot_fix
                            );
                        }
                    }
                }

                Commands::Start { .. }
                | Commands::Stop
                | Commands::Status
                | Commands::Pause
                | Commands::Resume
                | Commands::Activity { .. }
                | Commands::Sources
                | Commands::Webhook { .. }
                | Commands::Prs(_)
                | Commands::Retries(_)
                | Commands::Dashboard { .. }
                | Commands::Report(_)
                | Commands::Repos(_)
                | Commands::Inference(_)
                | Commands::Diag(_) => unreachable!(),
            }
        }
    }

    Ok(())
}
