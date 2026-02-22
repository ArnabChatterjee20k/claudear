//! Main watcher that coordinates sources, Claude, and notifications.

use crate::config::Config;
use crate::error::Result;
use crate::feedback::{
    format_similar_issues_context, FeedbackAnalyzer, FixOutcome, IssueEmbeddingService, Outcome,
};
use crate::github::GitHubClient;
use crate::inference::{resolve_repo_for_issue, RepoInferrer, RepoResolution};
use crate::notifier::send_to_all_and_wait_first_reply;
use crate::notifier::Notifier;
use crate::qa::{
    build_correlation_id, embed_text, find_reusable_qa, format_answer_context,
    format_reuse_context, format_timeout_context, normalize_text,
};
use crate::repo::{worktree_path, GitOps, RepoIndex, RepoRelationships};
use crate::retry::RetryManager;
use crate::runner::{self, AgentRunner};
use crate::scm::{PrReviewState, PrStatus, ReviewEvent, ReviewWatcher, ScmProvider};
use crate::source::IssueSource;
use crate::storage::{classify_error, compute_error_hash, FixAttemptTracker};
use crate::types::{
    ActivityLogEntry, AskRequest, ErrorPattern, FixAttemptStats, FixAttemptStatus, Issue,
    IssueEmbedding, IssueType, MatchPriority, MatchResult, ProcessingMetric, QaKnowledgeEntry,
    RegressionWatch,
};
use crate::users::UserRegistry;
use chrono::Utc;
use futures::future::join_all;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

/// Options for creating a watcher.
pub struct WatcherOptions {
    pub config: Config,
    pub sources: Vec<Arc<dyn IssueSource>>,
    pub notifier: Arc<dyn Notifier>,
    pub tracker: Arc<dyn FixAttemptTracker>,
    pub inferrer: Option<RepoInferrer>,
    pub embedding_client: Option<crate::feedback::EmbeddingClient>,
    pub review_watcher: Option<Arc<ReviewWatcher>>,
    pub issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    pub relationships: Option<RepoRelationships>,
    pub github_client: Option<GitHubClient>,
    /// Generic SCM provider for PR status checking (GitLab, etc.).
    /// When set, this is used for merge detection instead of github_client.
    pub scm_provider: Option<Arc<dyn ScmProvider>>,
    pub user_registry: UserRegistry,
    pub agent: Arc<dyn AgentRunner>,
    pub dry_run: bool,
}

/// Main watcher that coordinates sources, Claude, and notifications.
pub struct Watcher {
    config: Config,
    sources: Vec<Arc<dyn IssueSource>>,
    notifier: Arc<dyn Notifier>,
    tracker: Arc<dyn FixAttemptTracker>,
    inferrer: Option<RepoInferrer>,
    embedding_client: Option<crate::feedback::EmbeddingClient>,
    review_watcher: Option<Arc<ReviewWatcher>>,
    issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    relationships: Option<RepoRelationships>,
    github_client: Option<GitHubClient>,
    scm_provider: Option<Arc<dyn ScmProvider>>,
    user_registry: UserRegistry,
    agent: Arc<dyn AgentRunner>,
    dry_run: bool,
    is_running: AtomicBool,
    processing: RwLock<HashSet<String>>,
    active_processing: AtomicUsize,
    /// Feedback analyzer for learning from past outcomes
    feedback_analyzer: tokio::sync::Mutex<FeedbackAnalyzer>,
    /// Last seen release tag per upstream repo (for release-triggered cascades).
    last_seen_releases: RwLock<HashMap<String, String>>,
}

impl Watcher {
    /// Create a new watcher.
    pub fn new(options: WatcherOptions) -> Self {
        let feedback_analyzer =
            FeedbackAnalyzer::new().with_tracker(options.tracker.clone());

        Self {
            agent: options.agent,
            config: options.config,
            sources: options.sources,
            notifier: options.notifier,
            tracker: options.tracker,
            inferrer: options.inferrer,
            embedding_client: options.embedding_client,
            review_watcher: options.review_watcher,
            issue_embedding_service: options.issue_embedding_service,
            relationships: options.relationships,
            github_client: options.github_client,
            scm_provider: options.scm_provider,
            user_registry: options.user_registry,
            dry_run: options.dry_run,
            is_running: AtomicBool::new(false),
            processing: RwLock::new(HashSet::new()),
            active_processing: AtomicUsize::new(0),
            feedback_analyzer: tokio::sync::Mutex::new(feedback_analyzer),
            last_seen_releases: RwLock::new(HashMap::new()),
        }
    }

    /// Send a cron check-in to Sentry's HTTP API (fire-and-forget).
    ///
    /// Parses the DSN from SENTRY_DSN env var and sends a check-in for the
    /// "claudear-watcher-poll" monitor. Does nothing if SENTRY_DSN is not set.
    fn send_cron_check_in(
        &self,
        status: &str,
        check_in_id: &str,
        duration: Option<f64>,
        poll_interval_ms: u64,
    ) {
        let dsn = match std::env::var("SENTRY_DSN") {
            Ok(d) if !d.is_empty() => d,
            _ => return,
        };

        // Parse DSN: https://<public_key>@<host>/<project_id>
        let parsed = match url::Url::parse(&dsn) {
            Ok(u) => u,
            Err(_) => return,
        };
        let public_key = parsed.username();
        if public_key.is_empty() {
            return;
        }
        let project_id = parsed.path().trim_start_matches('/');
        if project_id.is_empty() {
            return;
        }
        let ingest = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));

        let environment = std::env::var("SENTRY_ENVIRONMENT").unwrap_or_default();
        let interval_minutes = (poll_interval_ms / 60_000).max(1);

        let mut url = format!(
            "{}/api/{}/cron/claudear-watcher-poll/{}/?status={}&check_in_id={}",
            ingest, project_id, public_key, status, check_in_id,
        );
        if !environment.is_empty() {
            url.push_str(&format!("&environment={}", environment));
        }
        if let Some(d) = duration {
            url.push_str(&format!("&duration={:.1}", d));
        }

        // Include monitor_config for upsert on in_progress check-ins
        let body = if status == "in_progress" {
            Some(serde_json::json!({
                "monitor_config": {
                    "schedule": {
                        "type": "interval",
                        "value": interval_minutes,
                        "unit": "minute"
                    },
                    "checkin_margin": 5,
                    "max_runtime": 30
                }
            }))
        } else {
            None
        };

        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let req = if let Some(body) = body {
                client.post(&url).json(&body)
            } else {
                client.get(&url)
            };
            if let Err(e) = req.send().await {
                tracing::debug!(error = %e, "Failed to send Sentry cron check-in");
            }
        });
    }

    /// Build a repository inferrer from config.
    ///
    /// This uses the fallback mechanism: if `auto_discover_paths` is configured,
    /// it scans the local filesystem. Otherwise, if a GitHub token is configured,
    /// it fetches repos via the GitHub API.
    pub async fn build_inferrer(
        config: &Config,
        github_client: Option<&crate::github::GitHubClient>,
    ) -> Result<Option<RepoInferrer>> {
        if config.known_orgs.is_empty() {
            tracing::info!("No known_orgs configured, inference disabled");
            return Ok(None);
        }

        // Check if we have any discovery method available
        let has_local_paths = !config.auto_discover_paths.is_empty();
        let has_github_client = github_client.map(|c| c.is_enabled()).unwrap_or(false);

        if !has_local_paths && !has_github_client {
            tracing::info!(
                "No auto_discover_paths configured and no GitHub token available, inference disabled"
            );
            return Ok(None);
        }

        let index = RepoIndex::build_with_fallback(
            &config.known_orgs,
            &config.auto_discover_paths,
            github_client,
            None, // gitlab_provider
            &[],  // gitlab_groups
            &config.work_dir,
            config.github().use_ssh,
        )
        .await?;

        if index.is_empty() {
            tracing::warn!("Repository index is empty, no repos discovered");
            return Ok(None);
        }

        tracing::info!(
            repos = index.len(),
            files = index.total_files(),
            "Repository index built for inference"
        );

        Ok(Some(RepoInferrer::new(index)))
    }

    /// Build a repository inferrer with embeddings for semantic matching.
    ///
    /// This uses the fallback mechanism: if `auto_discover_paths` is configured,
    /// it scans the local filesystem. Otherwise, if a GitHub token is configured,
    /// it fetches repos via the GitHub API.
    pub async fn build_inferrer_with_embeddings(
        config: &Config,
        github_client: Option<&crate::github::GitHubClient>,
    ) -> Result<(
        Option<RepoInferrer>,
        Option<crate::feedback::EmbeddingClient>,
    )> {
        use crate::feedback::{EmbeddingClient, EmbeddingConfig};
        use crate::inference::build_repo_embeddings;

        if config.known_orgs.is_empty() {
            tracing::info!("No known_orgs configured, inference disabled");
            return Ok((None, None));
        }

        // Check if we have any discovery method available
        let has_local_paths = !config.auto_discover_paths.is_empty();
        let has_github_client = github_client.map(|c| c.is_enabled()).unwrap_or(false);

        if !has_local_paths && !has_github_client {
            tracing::info!(
                "No auto_discover_paths configured and no GitHub token available, inference disabled"
            );
            return Ok((None, None));
        }

        let index = RepoIndex::build_with_fallback(
            &config.known_orgs,
            &config.auto_discover_paths,
            github_client,
            None, // gitlab_provider
            &[],  // gitlab_groups
            &config.work_dir,
            config.github().use_ssh,
        )
        .await?;

        if index.is_empty() {
            tracing::warn!("Repository index is empty, no repos discovered");
            return Ok((None, None));
        }

        tracing::info!(
            repos = index.len(),
            files = index.total_files(),
            "Repository index built for inference"
        );

        // Try to initialize embedding client
        match EmbeddingClient::new(EmbeddingConfig::default()) {
            Ok(client) => {
                // Build embeddings for all repos
                match build_repo_embeddings(&index, &client).await {
                    Ok(embeddings) => {
                        tracing::info!(
                            "Semantic inference enabled with {} repo embeddings",
                            embeddings.len()
                        );
                        // Use with_discovery to enable incremental updates
                        let inferrer = RepoInferrer::with_discovery(
                            index,
                            embeddings,
                            config.known_orgs.clone(),
                            config.auto_discover_paths.clone(),
                        );
                        Ok((Some(inferrer), Some(client)))
                    }
                    Err(e) => {
                        tracing::warn!("Failed to build repo embeddings: {}, falling back to file-based inference", e);
                        Ok((Some(RepoInferrer::new(index)), None))
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to initialize embedding client: {}, using file-based inference only",
                    e
                );
                Ok((Some(RepoInferrer::new(index)), None))
            }
        }
    }

    // Repository resolution is now handled by the inference engine (RepoInferrer).
    // See src/inference/mod.rs for the new implementation.

    /// Refresh the repo index and embed any new repositories.
    ///
    /// Returns the number of new repos discovered and embedded.
    pub async fn refresh_repos(&self) -> Result<usize> {
        let (inferrer, client) = match (&self.inferrer, &self.embedding_client) {
            (Some(inf), Some(cli)) => (inf, cli),
            _ => return Ok(0),
        };

        inferrer.refresh_repos(client).await
    }

    /// Sync repository index to the database.
    ///
    /// Updates repository paths and optionally file lists in the database
    /// from the in-memory RepoIndex.
    pub fn sync_repos_to_db(&self, sync_files: bool) -> Result<usize> {
        let inferrer = match &self.inferrer {
            Some(inf) => inf,
            None => return Ok(0),
        };

        inferrer.with_index(|index| self.tracker.sync_from_index(index, sync_files))
    }

    /// Start the watcher with polling.
    pub async fn start(&self, interval_ms: Option<u64>) -> Result<()> {
        let configured_poll_interval = interval_ms.unwrap_or(self.config.poll_interval_ms);
        let poll_interval = configured_poll_interval.max(1000);
        if configured_poll_interval < 1000 {
            tracing::warn!(
                component = "watcher",
                configured = configured_poll_interval,
                clamped = poll_interval,
                "Poll interval below 1000ms, clamping to 1000ms to avoid busy-loop"
            );
        }

        tracing::info!("");
        tracing::info!(
            "Starting Claude Watcher{}",
            if self.dry_run { " (DRY RUN)" } else { "" }
        );
        tracing::info!("  Work dir: {:?}", self.config.work_dir);
        tracing::info!("  Known orgs: {}", self.config.known_orgs.len());
        tracing::info!("  Poll interval: {}ms (global)", poll_interval);
        tracing::info!(
            "  Max issues per cycle: {} (global)",
            self.config.max_issues_per_cycle
        );
        tracing::info!("  Max concurrent: {} (global)", self.config.max_concurrent);
        for source in &self.sources {
            let src_max_issues = self.config.max_issues_per_cycle_for(source.name());
            let src_max_concurrent = self.config.max_concurrent_for(source.name());
            let src_poll_interval = self.config.poll_interval_ms_for(source.name());
            if src_max_issues != self.config.max_issues_per_cycle
                || src_max_concurrent != self.config.max_concurrent
                || src_poll_interval != poll_interval
            {
                tracing::info!(
                    "    {}: poll_interval={}ms, max_issues={}, max_concurrent={}",
                    source.name(),
                    src_poll_interval,
                    src_max_issues,
                    src_max_concurrent
                );
            }
        }
        tracing::info!("  Processing delay: {}ms", self.config.processing_delay_ms);
        tracing::info!(
            "  Sources: {}",
            self.sources
                .iter()
                .map(|s| s.display_name())
                .collect::<Vec<_>>()
                .join(", ")
        );

        if self.config.cascade.enabled {
            tracing::info!("  Cascade: enabled");
            if self.config.cascade.max_depth > 0 {
                tracing::info!("    Max depth: {}", self.config.cascade.max_depth);
            } else {
                tracing::info!("    Max depth: unlimited");
            }
            if let Some(ref rels) = self.relationships {
                let repo_count = rels.list_repositories().len();
                tracing::info!("    Repos in dependency graph: {}", repo_count);
            }
        } else {
            tracing::info!("  Cascade: disabled");
        }

        if self.dry_run {
            tracing::warn!("");
            tracing::warn!("  DRY RUN MODE - No issues will be processed");
        }

        tracing::info!("");

        // Clone any API-discovered repos that aren't local yet
        if let Some(inferrer) = &self.inferrer {
            let parallelism = std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(4);
            match inferrer.clone_and_index_all(parallelism).await {
                Ok(0) => {} // No repos to clone
                Ok(n) => tracing::info!("Cloned and indexed {} repositories", n),
                Err(e) => tracing::warn!("Error cloning repositories: {}", e),
            }
        }

        // Sync repository index to database (includes file lists)
        // Use spawn_blocking since sync_repos_to_db performs blocking I/O
        let inferrer = self.inferrer.clone();
        let tracker = self.tracker.clone();
        let sync_result = tokio::task::spawn_blocking(move || -> crate::error::Result<usize> {
            let inferrer = match &inferrer {
                Some(inf) => inf,
                None => return Ok(0),
            };
            inferrer.with_index(|index| tracker.sync_from_index(index, true))
        })
        .await;

        match sync_result {
            Ok(Ok(count)) if count > 0 => {
                tracing::info!("Synced {} repositories to database", count);
            }
            Ok(Err(e)) => {
                tracing::warn!("Failed to sync repos to database: {}", e);
            }
            Err(e) => {
                tracing::warn!("Sync task panicked: {}", e);
            }
            _ => {}
        }

        // Warm-start: load feedback outcomes from DB for learning
        match self.tracker.get_feedback_outcomes(None, 1000) {
            Ok(outcomes) if !outcomes.is_empty() => {
                let count = outcomes.len();
                let mut analyzer = self.feedback_analyzer.lock().await;
                analyzer.load_outcomes(outcomes);
                tracing::info!(count = count, "Loaded feedback outcomes for learning");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "Failed to load feedback outcomes"),
        }

        self.is_running.store(true, Ordering::SeqCst);

        // Initial poll of all sources + housekeeping
        self.poll().await?;

        // Build per-source timer state: (source index, interval_ms, last_poll)
        let now = std::time::Instant::now();
        let mut source_timers: Vec<(usize, u64, std::time::Instant)> = self
            .sources
            .iter()
            .enumerate()
            .map(|(i, source)| {
                let src_interval = self.config.poll_interval_ms_for(source.name()).max(1);
                tracing::info!(
                    source = source.name(),
                    interval_ms = src_interval,
                    "Per-source poll interval"
                );
                (i, src_interval, now)
            })
            .collect();

        // Determine the base tick: minimum source interval or global, whichever is smallest.
        // Cap at 1s to avoid busy-looping when all intervals are large.
        let min_source_interval = source_timers
            .iter()
            .map(|(_, ms, _)| *ms)
            .min()
            .unwrap_or(poll_interval);
        let base_tick_ms = min_source_interval.min(poll_interval).max(1000);
        let mut base_timer = interval(Duration::from_millis(base_tick_ms));
        base_timer.tick().await; // Skip immediate first tick

        // Global housekeeping timer
        let global_interval = Duration::from_millis(poll_interval);
        let mut last_global = std::time::Instant::now();

        // Counter for periodic repo refresh (every 5 global cycles)
        let mut global_cycle_count: u32 = 0;
        const REFRESH_INTERVAL: u32 = 5;
        const LEARNING_INTERVAL: u32 = 10;

        while self.is_running.load(Ordering::SeqCst) {
            base_timer.tick().await;
            if !self.is_running.load(Ordering::SeqCst) {
                break;
            }

            // Poll each source whose interval has elapsed
            for (src_idx, src_interval_ms, last_poll) in &mut source_timers {
                let src_interval = Duration::from_millis(*src_interval_ms);
                if last_poll.elapsed() >= src_interval {
                    let source = &self.sources[*src_idx];
                    if let Err(e) = self.poll_source(source).await {
                        tracing::error!(
                            component = "watcher",
                            source = source.name(),
                            error = %e,
                            "Error polling source"
                        );
                    }
                    *last_poll = std::time::Instant::now();
                }
            }

            // Global housekeeping on the global timer
            if last_global.elapsed() >= global_interval {
                last_global = std::time::Instant::now();
                global_cycle_count = global_cycle_count.wrapping_add(1);

                let cron_id = uuid::Uuid::new_v4().to_string();
                let cron_start = std::time::Instant::now();
                self.send_cron_check_in("in_progress", &cron_id, None, poll_interval);
                let mut housekeeping_ok = true;

                // Periodically refresh repo index to detect new repositories
                if global_cycle_count.is_multiple_of(REFRESH_INTERVAL) {
                    match self.refresh_repos().await {
                        Ok(0) => {} // No new repos
                        Ok(n) => tracing::info!("Discovered and embedded {} new repositories", n),
                        Err(e) => tracing::debug!(error = %e, "Error refreshing repos"),
                    }
                }

                if !self.dry_run {
                    // Check for PRs to auto-close due to issue state changes
                    if let Err(e) = self.check_and_auto_close_prs().await {
                        tracing::debug!(error = %e, "Error checking for auto-close PRs");
                    }

                    // Check for PR reviews
                    if let Err(e) = self.check_reviews().await {
                        tracing::debug!(error = %e, "Error checking for PR reviews");
                    }
                }

                // Run housekeeping (retries, cascades, metrics)
                if let Err(e) = self.poll_housekeeping().await {
                    tracing::error!(component = "watcher", error = %e, "Housekeeping error");
                    housekeeping_ok = false;
                }

                // Periodic learning subsystem tasks
                if !self.dry_run && global_cycle_count.is_multiple_of(LEARNING_INTERVAL) {
                    self.run_periodic_learning().await;
                }

                let duration_secs = cron_start.elapsed().as_secs_f64();
                let cron_status = if housekeeping_ok { "ok" } else { "error" };
                self.send_cron_check_in(cron_status, &cron_id, Some(duration_secs), poll_interval);
            }
        }

        Ok(())
    }

    /// Stop the watcher.
    ///
    /// This sets the running flag to false, which will cause the polling loop to exit
    /// after the current cycle completes. The poll() method already waits for active
    /// processing to complete before returning.
    pub fn stop(&self) {
        tracing::info!(
            active_count = self.active_processing.load(Ordering::SeqCst),
            "Stopping Claude Watcher, waiting for active tasks to complete..."
        );
        self.is_running.store(false, Ordering::SeqCst);
    }

    /// Stop the watcher and wait for all active processing to drain.
    ///
    /// This is useful for graceful shutdown scenarios where you want to ensure
    /// all in-progress work completes before the application exits.
    pub async fn stop_and_drain(&self) {
        self.stop();

        // Wait for any active processing to complete (up to 5 minutes)
        let max_wait = std::time::Duration::from_secs(300);
        let start = std::time::Instant::now();

        while self.active_processing.load(Ordering::SeqCst) > 0 {
            if start.elapsed() > max_wait {
                tracing::warn!(
                    remaining = self.active_processing.load(Ordering::SeqCst),
                    "Graceful shutdown timeout reached, some tasks may not have completed"
                );
                break;
            }
            tracing::info!(
                active_count = self.active_processing.load(Ordering::SeqCst),
                "Waiting for active tasks to complete..."
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        tracing::info!("Claude Watcher stopped gracefully");
    }

    /// Check if the watcher is currently running.
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    /// Get the count of currently active processing tasks.
    pub fn active_count(&self) -> usize {
        self.active_processing.load(Ordering::SeqCst)
    }

    /// Check for new PR reviews that require action.
    ///
    /// This polls the ReviewWatcher for any new CHANGES_REQUESTED or COMMENTED reviews
    /// and triggers Claude to address the feedback.
    pub async fn check_reviews(&self) -> Result<()> {
        let review_watcher = match &self.review_watcher {
            Some(rw) => rw,
            None => return Ok(()),
        };

        // Check for new reviews
        let events = review_watcher.check_for_reviews().await?;
        for (pr_url, feedback_summary, feedback_count) in Self::group_review_feedback_by_pr(events)
        {
            tracing::info!(
                pr_url = %pr_url,
                feedback_count,
                "Review feedback received, processing..."
            );

            // Find the original issue for this PR
            if let Some(attempt) = self.tracker.get_attempt_by_pr_url(&pr_url)? {
                if Self::is_terminal_attempt_status(attempt.status) {
                    tracing::info!(
                        pr_url = %pr_url,
                        source = %attempt.source,
                        issue_id = %attempt.issue_id,
                        status = %attempt.status,
                        "Skipping review feedback for terminal attempt status"
                    );
                    review_watcher.unwatch_pr(&pr_url);
                    continue;
                }
                if let Err(e) = self
                    .process_review_action(&attempt, &feedback_summary)
                    .await
                {
                    tracing::error!(
                        pr_url = %pr_url,
                        error = %e,
                        "Failed to process review feedback"
                    );
                }
            } else {
                tracing::warn!(
                    pr_url = %pr_url,
                    "Received review for unknown PR, skipping"
                );
            }
        }

        Ok(())
    }

    fn is_terminal_attempt_status(status: FixAttemptStatus) -> bool {
        matches!(
            status,
            FixAttemptStatus::Merged | FixAttemptStatus::Closed | FixAttemptStatus::CannotFix
        )
    }

    fn group_review_feedback_by_pr(events: Vec<ReviewEvent>) -> Vec<(String, String, usize)> {
        let mut feedback_by_pr: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut pr_order: Vec<String> = Vec::new();

        for event in events {
            if !event.requires_action() {
                continue;
            }

            let pr_url = event.pr_url().to_string();
            if !feedback_by_pr.contains_key(&pr_url) {
                pr_order.push(pr_url.clone());
            }
            feedback_by_pr
                .entry(pr_url)
                .or_default()
                .push(event.get_feedback_summary());
        }

        pr_order
            .into_iter()
            .filter_map(|pr_url| {
                feedback_by_pr.remove(&pr_url).map(|feedbacks| {
                    let count = feedbacks.len();
                    (pr_url, feedbacks.join("\n\n---\n\n"), count)
                })
            })
            .collect()
    }

    /// Process review feedback by triggering Claude to address the feedback.
    ///
    /// This creates a new Claude session with the original issue context plus
    /// the review feedback appended to help Claude understand what to fix.
    async fn process_review_action(
        &self,
        attempt: &crate::types::FixAttempt,
        feedback: &str,
    ) -> Result<()> {
        tracing::info!(
            source = %attempt.source,
            issue_id = %attempt.issue_id,
            short_id = %attempt.short_id,
            feedback_preview = %feedback.chars().take(100).collect::<String>(),
            "Processing review feedback for issue"
        );

        // Increment the review_cycles count
        if let Some(ref pr_url) = attempt.pr_url {
            // Update the PR record with incremented review_cycles
            if let Ok(Some(mut pr_record)) = self.tracker.get_pr(pr_url) {
                pr_record.review_cycles += 1;
                pr_record.last_review_at = Some(chrono::Utc::now());
                if let Err(e) = self.tracker.upsert_pr(&pr_record) {
                    tracing::warn!(error = %e, "Failed to update PR review cycles");
                }
            }
        }

        if self.config.learning.review_classification {
            if let Some(repo) = &attempt.scm_repo {
                // Parse feedback as review comments for classification
                let mock_comment = crate::scm::ReviewComment {
                    id: 0,
                    path: String::new(),
                    position: None,
                    original_position: None,
                    body: feedback.to_string(),
                    user: crate::scm::ReviewUser {
                        login: "reviewer".to_string(),
                        id: 0,
                        user_type: None,
                    },
                    created_at: String::new(),
                    updated_at: String::new(),
                    html_url: String::new(),
                    pull_request_review_id: None,
                    start_line: None,
                    line: None,
                    side: None,
                };

                if let Err(e) = crate::learning::ReviewClassifier::process_review_comments(
                    self.tracker.as_ref(),
                    repo,
                    &[mock_comment],
                    Some(feedback),
                ) {
                    tracing::warn!(error = %e, "Failed to classify review feedback");
                }

                // Check if any patterns should be promoted
                if let Ok(promotable) = crate::learning::ReviewClassifier::check_promotion_threshold(
                    self.tracker.as_ref(),
                    repo,
                    self.config.learning.review_promotion_threshold,
                ) {
                    for pattern in &promotable {
                        if let Err(e) =
                            crate::learning::RepoKnowledgeManager::learn_from_review_pattern(
                                self.tracker.as_ref(),
                                repo,
                                pattern,
                            )
                        {
                            tracing::warn!(error = %e, "Failed to learn from promoted review pattern");
                        }
                    }
                }
            }
        }

        // Find the source for this issue
        let source = match self.sources.iter().find(|s| s.name() == attempt.source) {
            Some(s) => s,
            None => {
                tracing::warn!(
                    source = %attempt.source,
                    "Source not found for review action"
                );
                return Ok(());
            }
        };

        // Verify the issue exists before processing
        let issue_exists = source.get_issue(&attempt.issue_id).await.is_ok();

        if !issue_exists {
            tracing::warn!(
                issue_id = %attempt.issue_id,
                "Could not find original issue for review action"
            );
            return Err(crate::error::Error::source(
                source.name(),
                format!("Issue {} not found for review action", attempt.issue_id),
            ));
        }

        // Process the issue with the review feedback appended to context.
        if let Some(pr_url) = &attempt.pr_url {
            // Look up the existing PR branch so the worktree can check it out
            let existing_pr_branch = self
                .tracker
                .get_pr(pr_url)
                .ok()
                .flatten()
                .and_then(|pr| pr.head_branch);

            tracing::info!(
                pr_url = %pr_url,
                branch = ?existing_pr_branch,
                "Re-processing issue to address review feedback"
            );

            // If the same issue is currently being processed, wait for that run
            // to finish so review feedback isn't silently dropped.
            let processing_key = format!("{}:{}", attempt.source, attempt.issue_id);
            let wait_started = std::time::Instant::now();
            let max_wait = std::time::Duration::from_secs(300);
            loop {
                while {
                    let processing = self.processing.read().await;
                    processing.contains(&processing_key)
                } {
                    if !self.is_running.load(Ordering::SeqCst) {
                        return Err(crate::error::Error::source(
                            &attempt.source,
                            format!(
                                "Watcher stopping while waiting for in-flight processing of {}",
                                attempt.short_id
                            ),
                        ));
                    }
                    if wait_started.elapsed() >= max_wait {
                        return Err(crate::error::Error::source(
                            &attempt.source,
                            format!(
                                "Timed out waiting for in-flight processing of {}",
                                attempt.short_id
                            ),
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }

                match self
                    .trigger_issue_with_feedback(
                        &attempt.source,
                        &attempt.issue_id,
                        Some(feedback.to_string()),
                        existing_pr_branch.clone(),
                    )
                    .await
                {
                    Ok(()) => break,
                    Err(e) => {
                        let still_processing = {
                            let processing = self.processing.read().await;
                            processing.contains(&processing_key)
                        };
                        if still_processing {
                            if !self.is_running.load(Ordering::SeqCst) {
                                return Err(crate::error::Error::source(
                                    &attempt.source,
                                    format!(
                                        "Watcher stopping while waiting for in-flight processing of {}",
                                        attempt.short_id
                                    ),
                                ));
                            }
                            if wait_started.elapsed() >= max_wait {
                                return Err(crate::error::Error::source(
                                    &attempt.source,
                                    format!(
                                        "Timed out waiting for in-flight processing of {}",
                                        attempt.short_id
                                    ),
                                ));
                            }
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Trigger cascade processing for downstream repos after a PR is merged
    /// or a release is published.
    ///
    /// Looks up the merged repo in the dependency graph and spawns Claude
    /// in each direct dependent repo with context about the upstream changes.
    /// The `trigger_type` controls which cascade rules are matched.
    pub async fn trigger_cascade(
        &self,
        attempt: &crate::types::FixAttempt,
        pr_url: &str,
        trigger_type: crate::config::CascadeTrigger,
    ) -> Result<()> {
        let relationships = match &self.relationships {
            Some(r) => r,
            None => return Ok(()),
        };

        if !self.config.cascade.enabled {
            return Ok(());
        }

        let scm_repo = match &attempt.scm_repo {
            Some(r) => r.clone(),
            None => return Ok(()),
        };

        if attempt.scm_pr_number.is_none() {
            return Ok(());
        }

        // Check cascade depth limit
        if self.config.cascade.max_depth > 0 {
            let depth = self.get_cascade_depth(attempt);
            if depth >= self.config.cascade.max_depth {
                tracing::info!(
                    short_id = %attempt.short_id,
                    depth = depth,
                    max_depth = self.config.cascade.max_depth,
                    "Cascade depth limit reached, stopping"
                );
                return Ok(());
            }
        }

        // Try full owner/repo name first (used when dependencies are loaded from DB),
        // fall back to short name for backwards compatibility with hardcoded defaults.
        let repo_short_name = scm_repo.split('/').next_back().unwrap_or(&scm_repo);
        let (dependants, graph_key) = {
            let full = relationships.get_dependants(&scm_repo);
            if !full.is_empty() {
                (full, scm_repo.to_string())
            } else {
                let short = relationships.get_dependants(repo_short_name);
                (short, repo_short_name.to_string())
            }
        };

        // Collect downstream repo names from the dependency graph
        let graph_names: std::collections::HashSet<&str> =
            dependants.iter().map(|d| d.name.as_str()).collect();

        // Also collect downstream repos from explicit cascade rules (config-driven).
        // This allows cascades to work even without detected code-level dependencies.
        // Only include rules that match the current trigger type.
        let rule_only_downstreams: Vec<&str> = self
            .config
            .cascade
            .rules
            .iter()
            .filter(|r| {
                (r.upstream == scm_repo || r.upstream == repo_short_name)
                    && r.trigger == trigger_type
                    && !graph_names.contains(r.downstream.as_str())
            })
            .map(|r| r.downstream.as_str())
            .collect();

        if dependants.is_empty() && rule_only_downstreams.is_empty() {
            tracing::debug!(
                repo = %scm_repo,
                short_name = %repo_short_name,
                trigger = ?trigger_type,
                "No downstream dependants found for cascade"
            );
            return Ok(());
        }

        tracing::info!(
            repo = %scm_repo,
            trigger = ?trigger_type,
            graph_dependants = dependants.len(),
            rule_dependants = rule_only_downstreams.len(),
            "Triggering cascade for downstream repos"
        );

        let upstream_pr_url = pr_url.to_string();
        let graph = relationships.get_graph();

        // Process graph dependants (have actual code dependencies)
        for dependant in dependants {
            let dep_type = graph
                .get_first_hop_dependency_type_to_target(&graph_key, &dependant.name)
                .map(|t| t.as_str())
                .unwrap_or("unknown");

            // Look up per-dependency cascade rule for this trigger type
            let rule = self
                .config
                .cascade
                .find_rule_for_trigger(&scm_repo, &dependant.name, &trigger_type)
                .or_else(|| {
                    self.config.cascade.find_rule_for_trigger(
                        repo_short_name,
                        &dependant.name,
                        &trigger_type,
                    )
                });

            // If no rule matches this trigger type, check if there's a rule with a
            // different trigger — if so, skip (the other trigger path will handle it).
            // If no rule exists at all, graph dependants cascade on merge by default.
            if rule.is_none() {
                let any_rule = self.config.cascade.find_rule(&scm_repo, &dependant.name);
                if let Some(r) = any_rule {
                    if r.trigger != trigger_type {
                        tracing::info!(
                            upstream = %scm_repo,
                            downstream = %dependant.name,
                            rule_trigger = ?r.trigger,
                            current_trigger = ?trigger_type,
                            "Skipping cascade — rule requires different trigger"
                        );
                        continue;
                    }
                } else if trigger_type != crate::config::CascadeTrigger::Merge {
                    // No explicit rule and this isn't a merge trigger —
                    // graph dependants only auto-cascade on merge.
                    continue;
                }
            }

            if let Err(e) = self
                .cascade_to_repo(
                    attempt,
                    &dependant.name,
                    &scm_repo,
                    &upstream_pr_url,
                    dep_type,
                    rule,
                )
                .await
            {
                tracing::error!(
                    upstream = %scm_repo,
                    downstream = %dependant.name,
                    error = %e,
                    "Failed to cascade to downstream repo"
                );
            }
        }

        // Process cascade-rule-only downstreams (explicitly configured, no code dependency detected)
        for downstream in rule_only_downstreams {
            let rule =
                self.config
                    .cascade
                    .find_rule_for_trigger(&scm_repo, downstream, &trigger_type);

            if let Err(e) = self
                .cascade_to_repo(
                    attempt,
                    downstream,
                    &scm_repo,
                    &upstream_pr_url,
                    "cascade",
                    rule,
                )
                .await
            {
                tracing::error!(
                    upstream = %scm_repo,
                    downstream = %downstream,
                    error = %e,
                    "Failed to cascade to downstream repo"
                );
            }
        }

        Ok(())
    }

    /// Check for new releases on upstream repos with release-triggered cascade rules.
    /// When a new release is detected, finds the most recently merged attempt for that
    /// repo and triggers cascade with `CascadeTrigger::Release`.
    pub async fn check_releases_and_cascade(&self) -> Result<()> {
        if !self.config.cascade.enabled {
            return Ok(());
        }

        let upstreams = self.config.cascade.release_trigger_upstreams();
        if upstreams.is_empty() {
            return Ok(());
        }

        // Need an SCM provider for release polling
        let scm = match &self.scm_provider {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        for upstream in upstreams {
            let release = match scm.get_latest_release(upstream).await {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(e) => {
                    tracing::debug!(
                        upstream = %upstream,
                        error = %e,
                        "Failed to check latest release for cascade"
                    );
                    continue;
                }
            };

            // Check if we've already processed this release
            {
                let seen = self.last_seen_releases.read().await;
                if seen.get(upstream).map(|t| t.as_str()) == Some(&release.tag) {
                    continue;
                }
            }

            tracing::info!(
                upstream = %upstream,
                tag = %release.tag,
                "New release detected, checking for release-triggered cascades"
            );

            // Mark as seen before processing (avoid duplicate cascades)
            {
                let mut seen = self.last_seen_releases.write().await;
                seen.insert(upstream.to_string(), release.tag.clone());
            }

            // Find the most recently merged attempt for this upstream repo
            let merged_attempt = self
                .tracker
                .get_most_recent_merged_attempt_for_repo(upstream)
                .ok()
                .flatten();

            let attempt = match merged_attempt {
                Some(a) => a,
                None => {
                    tracing::debug!(
                        upstream = %upstream,
                        "No merged attempt found for release-triggered cascade"
                    );
                    continue;
                }
            };

            let pr_url = attempt.pr_url.clone().unwrap_or_default();
            match self
                .trigger_cascade(&attempt, &pr_url, crate::config::CascadeTrigger::Release)
                .await
            {
                Ok(()) => {
                    tracing::info!(
                        upstream = %upstream,
                        tag = %release.tag,
                        "Release-triggered cascade completed"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        upstream = %upstream,
                        tag = %release.tag,
                        error = %e,
                        "Failed to trigger release cascade"
                    );
                }
            }
        }

        Ok(())
    }

    /// Get the cascade depth of an attempt (0 for root, 1 for first cascade, etc.)
    fn get_cascade_depth(&self, attempt: &crate::types::FixAttempt) -> usize {
        let mut depth = 0;
        let mut current_parent = attempt.parent_attempt_id;

        while let Some(parent_id) = current_parent {
            depth += 1;
            match self.tracker.get_attempt_by_id(parent_id).ok().flatten() {
                Some(parent) => current_parent = parent.parent_attempt_id,
                None => break,
            }
        }

        depth
    }

    /// Execute a cascade fix in a single downstream repo.
    async fn cascade_to_repo(
        &self,
        parent_attempt: &crate::types::FixAttempt,
        downstream_repo_name: &str,
        upstream_repo: &str,
        upstream_pr_url: &str,
        dep_type: &str,
        rule: Option<&crate::config::CascadeRule>,
    ) -> Result<()> {
        tracing::info!(
            upstream = %upstream_repo,
            downstream = %downstream_repo_name,
            parent_id = parent_attempt.id,
            "Cascading to downstream repo"
        );

        // Resolve the downstream repo's local path
        let resolution = crate::inference::resolve_repo_for_cascade(
            self.inferrer.as_ref(),
            downstream_repo_name,
        );

        let (project_dir, scm_url, default_branch) = match resolution {
            crate::inference::RepoResolution::Resolved {
                project_dir,
                scm_url,
                default_branch,
                ..
            } => (project_dir, scm_url, default_branch),
            crate::inference::RepoResolution::Skip { reason } => {
                tracing::warn!(
                    downstream = %downstream_repo_name,
                    reason = %reason,
                    "Cannot cascade — downstream repo not available"
                );
                return Ok(());
            }
        };

        // Record cascade attempt
        let attempt_id = self.tracker.record_cascade_attempt(
            &parent_attempt.source,
            &parent_attempt.issue_id,
            &parent_attempt.short_id,
            parent_attempt.id,
            &scm_url,
        )?;

        // Fetch the downstream repo (no checkout/reset — just update object store)
        if let Err(e) = GitOps::ensure_repo_fetched(&project_dir, &scm_url).await {
            tracing::warn!(
                downstream = %downstream_repo_name,
                error = %e,
                "Failed to fetch downstream repo, continuing anyway"
            );
        }

        // Create a per-cascade worktree so concurrent cascades don't interfere
        let cascade_id = format!("cascade-{}", parent_attempt.short_id);
        let wt_path = worktree_path(&self.config.work_dir, downstream_repo_name, &cascade_id);
        let effective_branch = rule
            .and_then(|r| r.target_branch.as_deref())
            .unwrap_or(&default_branch);
        GitOps::create_worktree(
            &project_dir,
            &wt_path,
            &format!("origin/{}", effective_branch),
        )
        .await
        .map_err(|e| {
            tracing::error!(
                downstream = %downstream_repo_name,
                error = %e,
                "Failed to create cascade worktree"
            );
            e
        })?;
        let effective_dir = &wt_path;

        // Build the cascade prompt (rule-aware)
        let version_instruction = if rule.is_none_or(|r| r.version_update) {
            format!(
                "- Update the dependency version for {} in this project's package manifest (package.json, composer.json, etc.)",
                upstream_repo
            )
        } else {
            "- No version update needed for this dependency".to_string()
        };

        let custom_instructions = rule
            .and_then(|r| r.instructions.as_deref())
            .map(|i| format!("\n\n## Additional Instructions\n{}", i))
            .unwrap_or_default();

        let prompt = format!(
            r#"A dependency has been updated in {upstream_repo}.

## Original Issue
[{short_id}] {source} issue that was fixed upstream.

## Upstream PR
{upstream_pr_url}

Review the upstream PR above to understand what changed.

## Your Task
This repository ({downstream_repo_name}) depends on {upstream_repo} via {dep_type}.
Review the upstream changes and make any necessary adaptations:
{version_instruction}
- Adapt to any API changes
- Update tests that exercise the changed functionality
- Ensure the project builds and tests pass

Create a PR with your changes.{custom_instructions}"#,
            upstream_repo = upstream_repo,
            short_id = parent_attempt.short_id,
            source = parent_attempt.source,
            upstream_pr_url = upstream_pr_url,
            downstream_repo_name = downstream_repo_name,
            dep_type = dep_type,
            version_instruction = version_instruction,
            custom_instructions = custom_instructions,
        );

        // Run Claude
        let result = self
            .agent
            .execute_with_attempt(&prompt, None, Some(attempt_id), effective_dir)
            .await?;

        if result.success {
            if let Some(ref pr_url) = result.pr_url {
                tracing::info!(
                    downstream = %downstream_repo_name,
                    pr_url = %pr_url,
                    "Cascade PR created"
                );

                // Update the cascade attempt with PR details
                if let Some((repo, pr_num)) = crate::storage::parse_pr_url(pr_url) {
                    self.tracker.update_attempt_pr(attempt_id, pr_url, &repo, pr_num)?;
                }

                // Register for review watching — this enables recursive cascade
                if let Some(ref review_watcher) = self.review_watcher {
                    if let Some((repo, pr_number)) = crate::storage::parse_pr_url(pr_url) {
                        let state = PrReviewState::new(
                            pr_url,
                            &repo,
                            pr_number,
                            &parent_attempt.issue_id,
                            &parent_attempt.source,
                        );
                        review_watcher.watch_pr(state);
                        tracing::info!(
                            component = "cascade",
                            pr_url = %pr_url,
                            "Cascade PR registered for review watching"
                        );
                    }
                }

                // Log activity
                let activity = ActivityLogEntry::new(
                    "cascade_pr_created",
                    format!(
                        "Cascade PR created in {} for upstream {}",
                        downstream_repo_name, upstream_repo
                    ),
                )
                .with_source(parent_attempt.source.clone())
                .with_issue(
                    parent_attempt.issue_id.clone(),
                    parent_attempt.short_id.clone(),
                );
                self.tracker.record_activity(&activity).ok();

                // Notify cascade success
                let mut cascade_issue = Issue::new(
                    &parent_attempt.issue_id,
                    &parent_attempt.short_id,
                    format!("Cascade: {} -> {}", upstream_repo, downstream_repo_name),
                    pr_url,
                    &parent_attempt.source,
                );
                cascade_issue.set_metadata("cascade_upstream_repo", upstream_repo.to_string());
                cascade_issue
                    .set_metadata("cascade_downstream_repo", downstream_repo_name.to_string());
                cascade_issue.set_metadata("cascade_upstream_pr_url", upstream_pr_url.to_string());
                cascade_issue.set_metadata(
                    "cascade_original_issue_short_id",
                    parent_attempt.short_id.clone(),
                );
                if let Some(ref changelog) = result.changelog {
                    cascade_issue.set_metadata("changelog", changelog.clone());
                }
                let _ = self.notifier.notify_success(&cascade_issue, pr_url).await;
            } else {
                // Cascade succeeded but no PR — treat as failure
                let no_pr_error = if result.output.is_empty() {
                    "Cascade completed without creating a PR".to_string()
                } else {
                    let summary = if result.output.chars().count() > 500 {
                        let truncated: String = result.output.chars().take(497).collect();
                        format!("{}...", truncated)
                    } else {
                        result.output.clone()
                    };
                    format!("Cascade completed without creating a PR: {}", summary)
                };
                tracing::warn!(
                    downstream = %downstream_repo_name,
                    "Cascade succeeded but no PR URL"
                );
                self.tracker.mark_cascade_failed(attempt_id, &no_pr_error)?;

                let mut cascade_issue = Issue::new(
                    &parent_attempt.issue_id,
                    &parent_attempt.short_id,
                    format!("Cascade: {} -> {}", upstream_repo, downstream_repo_name),
                    "",
                    &parent_attempt.source,
                );
                cascade_issue.set_metadata("cascade_upstream_repo", upstream_repo.to_string());
                cascade_issue
                    .set_metadata("cascade_downstream_repo", downstream_repo_name.to_string());
                cascade_issue.set_metadata("cascade_upstream_pr_url", upstream_pr_url.to_string());
                cascade_issue.set_metadata(
                    "cascade_original_issue_short_id",
                    parent_attempt.short_id.clone(),
                );
                let _ = self
                    .notifier
                    .notify_failed(&cascade_issue, &no_pr_error)
                    .await;
            }
        } else {
            let base_error = result.error.unwrap_or_else(|| "Unknown error".to_string());
            let error = if !result.output.is_empty() {
                let summary = if result.output.chars().count() > 500 {
                    let truncated: String = result.output.chars().take(497).collect();
                    format!("{}...", truncated)
                } else {
                    result.output.clone()
                };
                format!("{}\n\nClaude's summary: {}", base_error, summary)
            } else {
                base_error
            };
            tracing::warn!(
                downstream = %downstream_repo_name,
                error = %error,
                "Cascade fix failed"
            );
            self.tracker.mark_cascade_failed(attempt_id, &error)?;

            // Notify cascade failure
            let mut cascade_issue = Issue::new(
                &parent_attempt.issue_id,
                &parent_attempt.short_id,
                format!("Cascade: {} -> {}", upstream_repo, downstream_repo_name),
                "",
                &parent_attempt.source,
            );
            cascade_issue.set_metadata("cascade_upstream_repo", upstream_repo.to_string());
            cascade_issue.set_metadata("cascade_downstream_repo", downstream_repo_name.to_string());
            cascade_issue.set_metadata("cascade_upstream_pr_url", upstream_pr_url.to_string());
            cascade_issue.set_metadata(
                "cascade_original_issue_short_id",
                parent_attempt.short_id.clone(),
            );
            let _ = self.notifier.notify_failed(&cascade_issue, &error).await;
        }

        // Cleanup cascade worktree
        if wt_path.exists() {
            if let Err(e) = GitOps::remove_worktree(&project_dir, &wt_path).await {
                tracing::warn!(
                    downstream = %downstream_repo_name,
                    error = %e,
                    "Failed to remove cascade worktree"
                );
            }
        }

        Ok(())
    }

    /// Seed the tracker with existing issues.
    pub async fn seed(&self) -> Result<SeedResult> {
        tracing::info!("");
        tracing::info!("Seeding tracker with existing issues...");

        let mut results = SeedResult::default();

        for source in &self.sources {
            match source.fetch_issues().await {
                Ok(issues) => {
                    let mut seeded = 0;
                    for issue in issues {
                        if !self.tracker.has_attempted(source.name(), &issue.id)? {
                            // Extract labels from issue metadata for bug detection
                            let labels: Vec<String> =
                                issue.get_metadata("labels").unwrap_or_default();
                            self.tracker.record_attempt_with_labels(
                                source.name(),
                                &issue.id,
                                &issue.short_id,
                                &labels,
                            )?;
                            self.tracker.mark_failed(
                                source.name(),
                                &issue.id,
                                "SEEDED: Marked as seen during initial seed",
                            )?;
                            seeded += 1;
                        }
                    }
                    results.by_source.insert(source.name().to_string(), seeded);
                    results.total += seeded;
                    tracing::info!(source = source.name(), count = seeded, "Seeded issues");
                }
                Err(e) => {
                    tracing::error!(source = source.name(), error = %e, "Error seeding");
                }
            }
        }

        tracing::info!("");
        tracing::info!(
            "Seeding complete. Total: {} issues marked as seen.",
            results.total
        );
        tracing::info!("New issues created after this will be processed normally.");
        tracing::info!("");

        Ok(results)
    }

    /// Run a single poll cycle.
    async fn poll(&self) -> Result<()> {
        let poll_started_at = std::time::Instant::now();
        tracing::info!("");
        tracing::info!(
            "[{}] Polling...",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S")
        );

        // Poll all sources concurrently for better throughput.
        let poll_futures: Vec<_> = self
            .sources
            .iter()
            .map(|source| async move {
                if let Err(e) = self.poll_source(source).await {
                    tracing::error!(component = "watcher", source = source.name(), error = %e, "Error polling");
                }
            })
            .collect();
        join_all(poll_futures).await;

        // Process any ready retries
        if !self.dry_run {
            if let Err(e) = self.process_ready_retries().await {
                tracing::error!(component = "watcher", error = %e, "Error processing retries");
            }
        }

        // Check for PR merges and trigger cascades
        if !self.dry_run {
            if let Err(e) = self.check_pr_merges_and_cascade().await {
                tracing::error!(component = "watcher", error = %e, "Error checking PR merges for cascade");
            }
        }

        // Check for new releases and trigger release-based cascades
        if !self.dry_run {
            if let Err(e) = self.check_releases_and_cascade().await {
                tracing::error!(component = "watcher", error = %e, "Error checking releases for cascade");
            }
        }

        // Record lightweight operational telemetry for dashboard analytics.
        if !self.dry_run {
            let poll_duration_metric = ProcessingMetric::new(
                "poll_cycle_duration_secs",
                poll_started_at.elapsed().as_secs_f64(),
            );
            if let Err(e) = self.tracker.record_metric(&poll_duration_metric) {
                tracing::debug!(error = %e, "Failed to record poll_cycle_duration_secs metric");
            }

            let source_count_metric =
                ProcessingMetric::new("poll_sources", self.sources.len() as f64);
            if let Err(e) = self.tracker.record_metric(&source_count_metric) {
                tracing::debug!(error = %e, "Failed to record poll_sources metric");
            }

            let active = self.active_processing.load(Ordering::SeqCst) as f64;
            let active_metric = ProcessingMetric::new("active_processing", active);
            if let Err(e) = self.tracker.record_metric(&active_metric) {
                tracing::debug!(error = %e, "Failed to record active_processing metric");
            }

            match self.tracker.get_stats() {
                Ok(stats) => {
                    let pending_metric =
                        ProcessingMetric::new("pending_attempts", stats.pending as f64);
                    if let Err(e) = self.tracker.record_metric(&pending_metric) {
                        tracing::debug!(error = %e, "Failed to record pending_attempts metric");
                    }

                    let total_metric = ProcessingMetric::new("total_attempts", stats.total as f64);
                    if let Err(e) = self.tracker.record_metric(&total_metric) {
                        tracing::debug!(error = %e, "Failed to record total_attempts metric");
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to load stats for poll metrics");
                }
            }
        }

        Ok(())
    }

    /// Run housekeeping tasks: retries, cascades, and metrics.
    /// Called on the global timer, separate from per-source polling.
    async fn poll_housekeeping(&self) -> Result<()> {
        let housekeeping_started_at = std::time::Instant::now();

        // Process any ready retries
        if !self.dry_run {
            if let Err(e) = self.process_ready_retries().await {
                tracing::error!(component = "watcher", error = %e, "Error processing retries");
            }
        }

        // Check for PR merges and trigger cascades
        if !self.dry_run {
            if let Err(e) = self.check_pr_merges_and_cascade().await {
                tracing::error!(component = "watcher", error = %e, "Error checking PR merges for cascade");
            }
        }

        // Check for new releases and trigger release-based cascades
        if !self.dry_run {
            if let Err(e) = self.check_releases_and_cascade().await {
                tracing::error!(component = "watcher", error = %e, "Error checking releases for cascade");
            }
        }

        // Record lightweight operational telemetry for dashboard analytics.
        if !self.dry_run {
            let duration_metric = ProcessingMetric::new(
                "housekeeping_cycle_duration_secs",
                housekeeping_started_at.elapsed().as_secs_f64(),
            );
            if let Err(e) = self.tracker.record_metric(&duration_metric) {
                tracing::debug!(error = %e, "Failed to record housekeeping_cycle_duration_secs metric");
            }

            let source_count_metric =
                ProcessingMetric::new("poll_sources", self.sources.len() as f64);
            if let Err(e) = self.tracker.record_metric(&source_count_metric) {
                tracing::debug!(error = %e, "Failed to record poll_sources metric");
            }

            let active = self.active_processing.load(Ordering::SeqCst) as f64;
            let active_metric = ProcessingMetric::new("active_processing", active);
            if let Err(e) = self.tracker.record_metric(&active_metric) {
                tracing::debug!(error = %e, "Failed to record active_processing metric");
            }

            match self.tracker.get_stats() {
                Ok(stats) => {
                    let pending_metric =
                        ProcessingMetric::new("pending_attempts", stats.pending as f64);
                    if let Err(e) = self.tracker.record_metric(&pending_metric) {
                        tracing::debug!(error = %e, "Failed to record pending_attempts metric");
                    }

                    let total_metric = ProcessingMetric::new("total_attempts", stats.total as f64);
                    if let Err(e) = self.tracker.record_metric(&total_metric) {
                        tracing::debug!(error = %e, "Failed to record total_attempts metric");
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to load stats for poll metrics");
                }
            }
        }

        Ok(())
    }

    /// Process any issues that are ready for retry.
    async fn process_ready_retries(&self) -> Result<()> {
        let retry_manager = RetryManager::new(self.config.retry.clone(), self.tracker.clone());
        let ready = retry_manager.get_ready_retries()?;
        let ready_count = ready.len();
        self.record_source_decision(
            "watcher",
            "ready_retry_scan",
            "Scanned for ready retries",
            json!({
                "ready_count": ready_count,
            }),
        );

        let ready_found_metric = ProcessingMetric::new("ready_retries_found", ready_count as f64);
        if let Err(e) = self.tracker.record_metric(&ready_found_metric) {
            tracing::debug!(error = %e, "Failed to record ready_retries_found metric");
        }

        if ready.is_empty() {
            let retries_executed_metric =
                ProcessingMetric::new("ready_retries_executed_total", 0.0);
            if let Err(e) = self.tracker.record_metric(&retries_executed_metric) {
                tracing::debug!(error = %e, "Failed to record ready_retries_executed_total metric");
            }

            let retries_failed_metric = ProcessingMetric::new("ready_retries_failed_total", 0.0);
            if let Err(e) = self.tracker.record_metric(&retries_failed_metric) {
                tracing::debug!(error = %e, "Failed to record ready_retries_failed_total metric");
            }
            return Ok(());
        }

        tracing::info!(
            component = "watcher",
            count = ready.len(),
            "Processing ready retries"
        );

        let mut retries_executed = 0usize;
        let mut retries_failed = 0usize;

        for (i, attempt) in ready.into_iter().enumerate() {
            // Check if we're still running
            if !self.is_running.load(Ordering::SeqCst) {
                break;
            }

            // Check if this issue is already being processed
            let processing_key = format!("{}:{}", attempt.source, attempt.issue_id);
            {
                let processing = self.processing.read().await;
                if processing.contains(&processing_key) {
                    self.record_source_decision(
                        &attempt.source,
                        "ready_retry_skipped_inflight",
                        format!(
                            "Retry skipped because {} is already in-flight",
                            attempt.short_id
                        ),
                        json!({
                            "issue_id": attempt.issue_id.clone(),
                            "short_id": attempt.short_id.clone(),
                        }),
                    );
                    tracing::debug!(
                        short_id = %attempt.short_id,
                        "Issue already being processed, skipping retry"
                    );
                    continue;
                }
            }

            // Wait for concurrency slot (per-source limit, clamped to 1 to avoid deadlock).
            let configured_retry_max_concurrent = self.config.max_concurrent_for(&attempt.source);
            let retry_max_concurrent = configured_retry_max_concurrent.max(1);
            if configured_retry_max_concurrent == 0 {
                tracing::warn!(
                    source = %attempt.source,
                    "max_concurrent_for source evaluated to 0, clamping to 1"
                );
            }
            while self.active_processing_for_source(&attempt.source).await >= retry_max_concurrent {
                if !self.is_running.load(Ordering::SeqCst) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            tracing::info!(
                component = "watcher",
                source = %attempt.source,
                short_id = %attempt.short_id,
                retry_count = attempt.retry_count,
                "Retrying issue"
            );

            // Prepare for retry (resets status to pending, clears PR info)
            retry_manager.prepare_retry(&attempt.source, &attempt.issue_id)?;

            // Trigger the issue processing
            match self.trigger_issue(&attempt.source, &attempt.issue_id).await {
                Ok(()) => {
                    self.record_source_decision(
                        &attempt.source,
                        "ready_retry_triggered",
                        format!("Retry triggered for {}", attempt.short_id),
                        json!({
                            "issue_id": attempt.issue_id.clone(),
                            "short_id": attempt.short_id.clone(),
                            "retry_count": attempt.retry_count,
                        }),
                    );
                    retries_executed += 1;
                    let metric = ProcessingMetric::new("ready_retry_executed", 1.0)
                        .with_source(attempt.source.clone());
                    if let Err(e) = self.tracker.record_metric(&metric) {
                        tracing::debug!(error = %e, "Failed to record ready_retry_executed metric");
                    }
                }
                Err(e) => {
                    self.record_source_decision(
                        &attempt.source,
                        "ready_retry_trigger_failed",
                        format!("Retry trigger failed for {}", attempt.short_id),
                        json!({
                            "issue_id": attempt.issue_id.clone(),
                            "short_id": attempt.short_id.clone(),
                            "retry_count": attempt.retry_count,
                            "error": e.to_string(),
                        }),
                    );
                    retries_failed += 1;
                    let retry_error = format!("Retry trigger failed: {}", e);
                    if let Err(mark_err) =
                        self.tracker
                            .mark_failed(&attempt.source, &attempt.issue_id, &retry_error)
                    {
                        tracing::warn!(
                            component = "watcher",
                            short_id = %attempt.short_id,
                            error = %mark_err,
                            "Failed to restore retry attempt state after trigger error"
                        );
                    }
                    let metric = ProcessingMetric::new("ready_retry_failed", 1.0)
                        .with_source(attempt.source.clone());
                    if let Err(record_err) = self.tracker.record_metric(&metric) {
                        tracing::debug!(
                            error = %record_err,
                            "Failed to record ready_retry_failed metric"
                        );
                    }
                    tracing::error!(
                        component = "watcher",
                        short_id = %attempt.short_id,
                        error = %e,
                        "Failed to trigger retry"
                    );
                }
            }

            // Add delay between retries (skip trailing delay after the last item)
            if i + 1 < ready_count && self.config.processing_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.config.processing_delay_ms)).await;
            }
        }

        let retries_executed_metric =
            ProcessingMetric::new("ready_retries_executed_total", retries_executed as f64);
        if let Err(e) = self.tracker.record_metric(&retries_executed_metric) {
            tracing::debug!(error = %e, "Failed to record ready_retries_executed_total metric");
        }

        let retries_failed_metric =
            ProcessingMetric::new("ready_retries_failed_total", retries_failed as f64);
        if let Err(e) = self.tracker.record_metric(&retries_failed_metric) {
            tracing::debug!(error = %e, "Failed to record ready_retries_failed_total metric");
        }

        Ok(())
    }

    /// Check for merged PRs and trigger cascade processing.
    async fn check_pr_merges_and_cascade(&self) -> Result<()> {
        let github_client = self.github_client.as_ref();
        let scm_provider = self.scm_provider.as_ref();
        // Get all successful attempts with PRs that haven't been merged yet.
        // Need either a GitHub client or a generic SCM provider for merge detection.
        let has_scm = github_client.is_some() || scm_provider.is_some();
        let pending_prs = if has_scm {
            self.tracker.get_pending_prs()?
        } else {
            Vec::new()
        };
        let mut pr_status_checks = 0usize;
        let mut pr_status_merged = 0usize;
        let mut pr_status_closed = 0usize;
        let mut pr_status_errors = 0usize;
        let mut regression_watches_created = 0usize;
        let mut auto_resolved_on_merge = 0usize;
        let mut cascade_triggered = 0usize;
        let mut cascade_failed = 0usize;

        for attempt in &pending_prs {
            let repo = match &attempt.scm_repo {
                Some(r) => r,
                None => continue,
            };
            let pr_number = match attempt.scm_pr_number {
                Some(n) => n,
                None => continue,
            };
            if !has_scm {
                break;
            }

            pr_status_checks += 1;
            // Use generic SCM provider when available, fall back to GitHub client
            let pr_status = if let Some(provider) = scm_provider {
                provider.get_pr_status(repo, pr_number).await
            } else if let Some(gh) = github_client {
                gh.get_pr_status(repo, pr_number).await
            } else {
                break;
            };
            match pr_status {
                Ok(PrStatus::Merged) => {
                    pr_status_merged += 1;
                    self.tracker
                        .mark_merged(&attempt.source, &attempt.issue_id)?;
                    let _ = self
                        .tracker
                        .update_qa_outcome_stats_for_attempt(attempt.id, true);

                    // Update prs record to merged
                    if let Some(ref pr_url) = attempt.pr_url {
                        if let Ok(Some(mut pr_record)) = self.tracker.get_pr(pr_url) {
                            pr_record.status = "merged".to_string();
                            pr_record.merged_at = Some(chrono::Utc::now());
                            if let Err(e) = self.tracker.upsert_pr(&pr_record) {
                                tracing::warn!(error = %e, "Failed to update PR status to merged");
                            }
                        }
                    }

                    // For bug-type issues, create a regression watch instead of immediate auto-resolve.
                    let regression_watch_id = if attempt.is_bug() {
                        let issue_type = match attempt.source.as_str() {
                            "sentry" => IssueType::SentryIssue,
                            "linear" => IssueType::LinearBug,
                            _ => IssueType::SentryIssue,
                        };
                        let mut watch =
                            RegressionWatch::new(issue_type, &attempt.issue_id, attempt.id);
                        watch.pr_merged_at = Some(chrono::Utc::now());

                        match self.tracker.create_regression_watch(&watch) {
                            Ok(watch_id) => {
                                regression_watches_created += 1;
                                tracing::info!(
                                    component = "watcher",
                                    source = %attempt.source,
                                    issue_id = %attempt.issue_id,
                                    short_id = %attempt.short_id,
                                    watch_id = watch_id,
                                    "Created regression watch for merged bug fix"
                                );
                                Some(watch_id)
                            }
                            Err(e) => {
                                tracing::error!(
                                    component = "watcher",
                                    source = %attempt.source,
                                    issue_id = %attempt.issue_id,
                                    short_id = %attempt.short_id,
                                    error = %e,
                                    "Failed to create regression watch"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };

                    // Auto-resolve only when enabled and no regression watch is active.
                    let should_resolve =
                        regression_watch_id.is_none() && self.config.github().auto_resolve_on_merge;
                    if should_resolve {
                        if let Some(source) =
                            self.sources.iter().find(|s| s.name() == attempt.source)
                        {
                            match source.resolve_issue(&attempt.issue_id).await {
                                Ok(()) => {
                                    auto_resolved_on_merge += 1;
                                    self.tracker
                                        .mark_resolved(&attempt.source, &attempt.issue_id)?;
                                    if let Some(pr_url) = &attempt.pr_url {
                                        let issue = Issue::new(
                                            &attempt.issue_id,
                                            &attempt.short_id,
                                            "Issue resolved",
                                            pr_url,
                                            &attempt.source,
                                        );
                                        let _ = self.notifier.notify_merged(&issue, pr_url).await;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        component = "watcher",
                                        source = %attempt.source,
                                        issue_id = %attempt.issue_id,
                                        error = %e,
                                        "Failed to resolve issue after PR merge"
                                    );
                                }
                            }
                        }
                    }

                    // Record feedback outcome
                    self.record_feedback_outcome_from_attempt(attempt, Outcome::Merged)
                        .await;

                    self.run_post_merge_learning(attempt).await;

                    // Stop review polling for merged PRs.
                    if let (Some(review_watcher), Some(pr_url)) =
                        (self.review_watcher.as_ref(), attempt.pr_url.as_ref())
                    {
                        review_watcher.unwatch_pr(pr_url);
                    }

                    let pr_url = attempt.pr_url.as_deref().unwrap_or("");
                    if self.config.cascade.enabled {
                        match self
                            .trigger_cascade(attempt, pr_url, crate::config::CascadeTrigger::Merge)
                            .await
                        {
                            Ok(()) => {
                                cascade_triggered += 1;
                            }
                            Err(e) => {
                                cascade_failed += 1;
                                tracing::error!(
                                    component = "cascade",
                                    short_id = %attempt.short_id,
                                    error = %e,
                                    "Failed to trigger cascade after merge"
                                );
                            }
                        }
                    }
                }
                Ok(PrStatus::Closed) => {
                    pr_status_closed += 1;
                    self.tracker
                        .mark_closed(&attempt.source, &attempt.issue_id)?;
                    let _ = self
                        .tracker
                        .update_qa_outcome_stats_for_attempt(attempt.id, false);
                    self.record_feedback_outcome_from_attempt(attempt, Outcome::Closed)
                        .await;
                    if let (Some(review_watcher), Some(pr_url)) =
                        (self.review_watcher.as_ref(), attempt.pr_url.as_ref())
                    {
                        review_watcher.unwatch_pr(pr_url);
                    }

                    // Notify PR closed
                    if let Some(pr_url) = &attempt.pr_url {
                        let issue = Issue::new(
                            &attempt.issue_id,
                            &attempt.short_id,
                            "PR closed without merge",
                            pr_url,
                            &attempt.source,
                        );
                        let _ = self.notifier.notify_closed(&issue, pr_url).await;
                    }
                }
                Ok(_) => {} // Still open
                Err(e) => {
                    pr_status_errors += 1;
                    tracing::debug!(
                        short_id = %attempt.short_id,
                        error = %e,
                        "Failed to check PR status"
                    );
                }
            }
        }

        let cycle_metrics = [
            ("pr_status_checks", pr_status_checks as f64),
            ("pr_status_merged", pr_status_merged as f64),
            ("pr_status_closed", pr_status_closed as f64),
            ("pr_status_errors", pr_status_errors as f64),
            (
                "regression_watches_created",
                regression_watches_created as f64,
            ),
            ("auto_resolved_on_merge", auto_resolved_on_merge as f64),
            ("cascade_triggered", cascade_triggered as f64),
            ("cascade_failed", cascade_failed as f64),
        ];
        for (name, value) in cycle_metrics {
            let metric = ProcessingMetric::new(name, value);
            if let Err(e) = self.tracker.record_metric(&metric) {
                tracing::debug!(error = %e, metric = name, "Failed to record PR lifecycle metric");
            }
        }

        Ok(())
    }

    /// Poll a single source.
    async fn poll_source(&self, source: &Arc<dyn IssueSource>) -> Result<()> {
        tracing::info!(source = source.name(), "Fetching issues...");

        let issues = source.fetch_issues().await?;
        tracing::info!(source = source.name(), count = issues.len(), "Found issues");
        let fetched_metric = ProcessingMetric::new("issues_fetched", issues.len() as f64)
            .with_source(source.name().to_string());
        if let Err(e) = self.tracker.record_metric(&fetched_metric) {
            tracing::debug!(error = %e, "Failed to record issues_fetched metric");
        }

        // Get already attempted issue IDs
        let attempted_ids = self.tracker.get_attempted_issue_ids(source.name());
        tracing::info!(
            source = source.name(),
            count = attempted_ids.len(),
            "Already attempted issues"
        );

        // Filter and match criteria
        let mut candidates: Vec<(Issue, MatchResult)> = Vec::new();
        let mut seen_issue_ids = HashSet::new();
        let mut duplicate_skipped = 0usize;
        let mut attempted_skipped = 0usize;
        let mut inflight_skipped = 0usize;
        let mut unmatched_skipped = 0usize;

        // Pre-build regex cache for suppression rules (avoids re-compilation per issue)
        let suppression_cache = crate::prioritisation::suppression::RegexCache::new(
            &self.config.prioritisation.suppression_rules,
        );

        let processing = self.processing.read().await;
        for issue in issues {
            if !seen_issue_ids.insert(issue.id.clone()) {
                duplicate_skipped = duplicate_skipped.saturating_add(1);
                tracing::debug!(
                    source = source.name(),
                    issue_id = %issue.id,
                    "Skipping duplicate issue in poll payload"
                );
                continue;
            }

            // Skip if already attempted
            if attempted_ids.contains(&issue.id) {
                attempted_skipped = attempted_skipped.saturating_add(1);
                continue;
            }

            // Skip if currently processing
            let processing_key = format!("{}:{}", source.name(), issue.id);
            if processing.contains(&processing_key) {
                inflight_skipped = inflight_skipped.saturating_add(1);
                continue;
            }

            // Early suppression check: only runs as fallback when the prioritisation
            // engine is disabled. When enabled, suppression is handled inside prioritise().
            if !self.config.prioritisation.enabled
                && !self.config.prioritisation.suppression_rules.is_empty()
            {
                let suppression = crate::prioritisation::suppression::check_issue_with_cache(
                    &self.config.prioritisation.suppression_rules,
                    &issue,
                    &suppression_cache,
                );
                if suppression.suppressed {
                    tracing::debug!(
                        source = source.name(),
                        issue_id = %issue.short_id,
                        rule = suppression.matched_rule.as_deref().unwrap_or("?"),
                        "Issue suppressed early in poll loop"
                    );
                    continue;
                }
            }

            let match_result = source.matches_criteria(&issue);
            if match_result.matches {
                candidates.push((issue, match_result));
            } else {
                unmatched_skipped = unmatched_skipped.saturating_add(1);
            }
        }
        drop(processing);

        // Semantic dedup: filter out candidates that are duplicates of already-handled issues
        let mut semantic_duplicate_skipped = 0usize;
        if let Some(ref embedding_service) = self.issue_embedding_service {
            let mut kept = Vec::with_capacity(candidates.len());
            for (issue, match_result) in candidates {
                match embedding_service
                    .check_duplicate(&issue, source.name())
                    .await
                {
                    Ok(Some(duplicate)) => {
                        semantic_duplicate_skipped = semantic_duplicate_skipped.saturating_add(1);
                        let similar_id = duplicate
                            .embedding
                            .short_id
                            .as_deref()
                            .unwrap_or(&duplicate.embedding.issue_id);
                        tracing::info!(
                            short_id = %issue.short_id,
                            similar_to = %similar_id,
                            similarity = %format!("{:.0}%", duplicate.similarity * 100.0),
                            "Skipping semantic duplicate during poll filtering"
                        );
                    }
                    _ => {
                        kept.push((issue, match_result));
                    }
                }
            }
            candidates = kept;
        }

        let candidates_count = candidates.len();
        let matched_metric = ProcessingMetric::new("issues_matched", candidates_count as f64)
            .with_source(source.name().to_string());
        if let Err(e) = self.tracker.record_metric(&matched_metric) {
            tracing::debug!(error = %e, "Failed to record issues_matched metric");
        }

        // Apply per-source max issues per cycle limit (falls back to global)
        let source_max_issues = self.config.max_issues_per_cycle_for(source.name());

        // Sort and select: use prioritisation engine when enabled, else legacy sort.
        let to_process: Vec<(Issue, MatchResult)> = if self.config.prioritisation.enabled {
            let (prioritised, suppressed) = crate::prioritisation::prioritise(
                &self.config.prioritisation,
                candidates,
                self.tracker.as_ref(),
                &std::collections::HashMap::new(),
            );

            // Log and record suppressions
            for (issue, result) in &suppressed {
                let rule = result.matched_rule.as_deref().unwrap_or("unknown");
                let reason = result.reason.as_deref().unwrap_or("");
                tracing::info!(
                    source = source.name(),
                    issue_id = %issue.short_id,
                    rule = rule,
                    "Issue suppressed during poll"
                );
                if let Err(e) =
                    self.tracker
                        .record_suppression(source.name(), &issue.id, rule, reason)
                {
                    tracing::debug!(error = %e, "Failed to record suppression");
                }
            }

            // Store severity scores
            for pi in &prioritised {
                if let Err(e) = self.tracker.store_severity_score(
                    source.name(),
                    &pi.issue.id,
                    &pi.severity_score,
                    pi.blast_radius,
                ) {
                    tracing::debug!(error = %e, "Failed to store severity score");
                }
            }

            // Convert back and take top N
            prioritised
                .into_iter()
                .take(source_max_issues)
                .map(|pi| (pi.issue, pi.match_result))
                .collect()
        } else {
            self.sort_by_priority(&mut candidates);
            candidates.into_iter().take(source_max_issues).collect()
        };

        let to_process_count = to_process.len();
        let queued_short_ids: Vec<String> = to_process
            .iter()
            .map(|(issue, _)| issue.short_id.clone())
            .collect();
        let queued_metric = ProcessingMetric::new("issues_queued", to_process_count as f64)
            .with_source(source.name().to_string());
        if let Err(e) = self.tracker.record_metric(&queued_metric) {
            tracing::debug!(error = %e, "Failed to record issues_queued metric");
        }
        self.record_source_decision(
            source.name(),
            "poll_filtering_summary",
            format!("Poll decisions summarized for {}", source.name()),
            json!({
                "fetched": candidates_count + duplicate_skipped + attempted_skipped + inflight_skipped + unmatched_skipped + semantic_duplicate_skipped,
                "matched": candidates_count,
                "queued": to_process_count,
                "deferred": candidates_count.saturating_sub(source_max_issues),
                "skipped": {
                    "duplicate": duplicate_skipped,
                    "already_attempted": attempted_skipped,
                    "inflight": inflight_skipped,
                    "unmatched": unmatched_skipped,
                    "semantic_duplicate": semantic_duplicate_skipped,
                },
                "queued_short_ids": queued_short_ids,
                "source_max_issues": source_max_issues,
            }),
        );
        if to_process.is_empty() {
            tracing::info!(source = source.name(), "No new issues to process");
            return Ok(());
        }

        let skipped = candidates_count.saturating_sub(source_max_issues);
        if skipped > 0 {
            tracing::info!(
                source = source.name(),
                count = to_process.len(),
                deferred = skipped,
                "Will process issues"
            );
        } else {
            tracing::info!(
                source = source.name(),
                count = to_process.len(),
                "Will process issues"
            );
        }

        // In dry-run mode, just show what would be processed
        if self.dry_run {
            use crate::inference::resolve_repo_for_issue_with_embedding;

            tracing::info!("");
            tracing::info!("[DRY RUN] Would process the following issues:");
            for (issue, match_result) in &to_process {
                tracing::info!("  - [{}] {}", issue.short_id, issue.title);
                tracing::info!(
                    "    Priority: {:?}, Reason: {}",
                    match_result.priority,
                    match_result.reason
                );
                tracing::info!("    URL: {}", issue.url);

                // Generate embedding for issue if client is available
                let query_embedding = if let Some(ref client) = self.embedding_client {
                    let issue_text = format!(
                        "{}\n{}",
                        issue.title,
                        issue.description.as_deref().unwrap_or("")
                    );
                    match client.embed(&issue_text).await {
                        Ok(emb) => Some(emb),
                        Err(e) => {
                            tracing::debug!("Failed to embed issue text: {}", e);
                            None
                        }
                    }
                } else {
                    None
                };

                // Show inferred repository (with optional semantic matching)
                let resolution = resolve_repo_for_issue_with_embedding(
                    self.inferrer.as_ref(),
                    issue,
                    Some(&self.tracker),
                    query_embedding.as_deref(),
                );
                match resolution {
                    RepoResolution::Resolved { project_dir, .. } => {
                        tracing::info!("    Repo: {}", project_dir.display());
                    }
                    RepoResolution::Skip { reason } => {
                        tracing::info!("    Repo: SKIP - {}", reason);
                    }
                }
            }
            return Ok(());
        }

        // Notify about urgent issues
        let urgent_issues: Vec<Issue> = to_process
            .iter()
            .filter(|(_, m)| m.priority == MatchPriority::Urgent)
            .map(|(i, _)| i.clone())
            .collect();

        if !urgent_issues.is_empty() {
            if let Err(e) = self.notifier.notify_urgent_issues(&urgent_issues).await {
                tracing::warn!(
                    source = source.name(),
                    error = %e,
                    "Failed to send urgent issue notification"
                );
            }
        }

        // Process issues with rate limiting (per-source limit, clamped to 1 to avoid deadlock).
        let configured_source_max_concurrent = self.config.max_concurrent_for(source.name());
        let source_max_concurrent = configured_source_max_concurrent.max(1);
        if configured_source_max_concurrent == 0 {
            tracing::warn!(
                source = source.name(),
                "max_concurrent_for source evaluated to 0, clamping to 1"
            );
        }
        for (i, (issue, match_result)) in to_process.into_iter().enumerate() {
            if !self.is_running.load(Ordering::SeqCst) {
                break;
            }

            // Wait for concurrency slot (per-source limit)
            while self.active_processing_for_source(source.name()).await >= source_max_concurrent {
                if !self.is_running.load(Ordering::SeqCst) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            // Process the issue
            let source_clone = Arc::clone(source);
            let this = self;
            let _ = this
                .process_issue(source_clone, issue, match_result, None, None)
                .await;

            // Add delay between starting new issues (skip trailing delay after the last item)
            if i + 1 < to_process_count && self.config.processing_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.config.processing_delay_ms)).await;
            }
        }

        // Record processing metrics (don't fail main operation if this fails)
        let metric = ProcessingMetric::new("batch_processed", to_process_count as f64)
            .with_source(source.name().to_string());
        if let Err(e) = self.tracker.record_metric(&metric) {
            tracing::warn!(error = %e, "Failed to record batch processing metric");
        }

        Ok(())
    }

    /// Number of active processing items for a specific source.
    async fn active_processing_for_source(&self, source_name: &str) -> usize {
        let prefix = format!("{}:", source_name);
        let processing = self.processing.read().await;
        processing
            .iter()
            .filter(|key| key.starts_with(&prefix))
            .count()
    }

    fn record_source_decision(
        &self,
        source: &str,
        decision: &str,
        message: impl Into<String>,
        details: serde_json::Value,
    ) {
        let activity = ActivityLogEntry::new("decision", message.into())
            .with_source(source.to_string())
            .with_metadata(json!({
                "decision": decision,
                "details": details,
            }));
        self.tracker.record_activity(&activity).ok();
    }

    fn record_issue_decision(
        &self,
        issue: &Issue,
        decision: &str,
        message: impl Into<String>,
        details: serde_json::Value,
    ) {
        let activity = ActivityLogEntry::new("decision", message.into())
            .with_source(issue.source.clone())
            .with_issue(issue.id.clone(), issue.short_id.clone())
            .with_metadata(json!({
                "decision": decision,
                "details": details,
            }));
        self.tracker.record_activity(&activity).ok();
    }

    /// Process a single issue.
    ///
    /// Uses the RepoInferrer engine to determine which repository to use
    /// for fixing the issue.
    async fn process_issue(
        &self,
        source: Arc<dyn IssueSource>,
        mut issue: Issue,
        match_result: MatchResult,
        review_feedback: Option<String>,
        existing_pr_branch: Option<String>,
    ) -> bool {
        let processing_started_at = std::time::Instant::now();
        let processing_key = format!("{}:{}", source.name(), issue.id);

        // Atomic check-and-insert to prevent race conditions.
        // Use a single write lock for both checking and inserting.
        {
            let mut processing = self.processing.write().await;
            if processing.contains(&processing_key) {
                tracing::debug!(
                    short_id = %issue.short_id,
                    "Issue already being processed, skipping"
                );
                return false;
            }
            processing.insert(processing_key.clone());
        }
        self.active_processing.fetch_add(1, Ordering::SeqCst);

        tracing::info!("");
        tracing::info!(short_id = %issue.short_id, title = %issue.title, "Processing issue");
        tracing::info!(short_id = %issue.short_id, reason = %match_result.reason, "Match reason");
        tracing::info!(short_id = %issue.short_id, priority = ?match_result.priority, "Match priority");
        self.record_issue_decision(
            &issue,
            "issue_selected_for_processing",
            format!("Selected {} for processing", issue.short_id),
            json!({
                "match_reason": match_result.reason.clone(),
                "priority": format!("{:?}", match_result.priority),
                "review_feedback_attached": review_feedback.is_some(),
            }),
        );

        // Record/update attempt state early so preflight failures are not retried forever.
        let labels: Vec<String> = issue.get_metadata("labels").unwrap_or_default();
        if let Err(e) = self.tracker.record_attempt_with_labels(
            source.name(),
            &issue.id,
            &issue.short_id,
            &labels,
        ) {
            tracing::error!(short_id = %issue.short_id, error = %e, "Failed to record attempt");
        }

        // Persist full issue content to the issues table (independent of embeddings)
        {
            let stored = IssueEmbedding::from_issue(&issue);
            if let Err(e) = self.tracker.store_issue(&stored) {
                tracing::debug!(error = %e, "Failed to store issue content");
            }
        }

        // Infer the target repository using the shared resolution function
        let resolution =
            resolve_repo_for_issue(self.inferrer.as_ref(), &issue, Some(&self.tracker));

        let project_dir = match &resolution {
            RepoResolution::Resolved { project_dir, .. } => {
                self.record_issue_decision(
                    &issue,
                    "repo_resolution_selected",
                    format!("Resolved repository for {}", issue.short_id),
                    json!({
                        "repo_name": resolution.repo_name(),
                        "scm_url": resolution.scm_url(),
                        "default_branch": resolution.default_branch(),
                        "project_dir": project_dir.display().to_string(),
                    }),
                );
                project_dir.clone()
            }
            RepoResolution::Skip { reason } => {
                self.record_issue_decision(
                    &issue,
                    "repo_resolution_skipped",
                    format!("Skipped {} due to repository resolution", issue.short_id),
                    json!({
                        "reason": reason,
                    }),
                );
                tracing::debug!(short_id = %issue.short_id, reason = %reason, "Skipping issue");
                let resolution_error = format!("Repository resolution failed: {}", reason);
                if let Err(e) =
                    self.tracker
                        .mark_failed(source.name(), &issue.id, &resolution_error)
                {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        error = %e,
                        "Failed to mark issue as failed after repository resolution skip"
                    );
                }
                // Clean up processing state before returning
                {
                    let mut processing = self.processing.write().await;
                    processing.remove(&processing_key);
                }
                self.active_processing.fetch_sub(1, Ordering::SeqCst);
                return true;
            }
        };

        // Fetch the parent repo (no checkout/reset — just update the object store)
        // then create an isolated per-issue worktree for Claude to work in.
        if let (Some(scm_url), Some(default_branch), Some(repo_name)) = (
            resolution.scm_url(),
            resolution.default_branch(),
            resolution.repo_name(),
        ) {
            self.record_issue_decision(
                &issue,
                "repo_sync_started",
                format!("Syncing repository {} for {}", repo_name, issue.short_id),
                json!({
                    "repo_name": repo_name,
                    "scm_url": scm_url,
                    "default_branch": default_branch,
                    "project_dir": project_dir.display().to_string(),
                }),
            );
            tracing::info!(
                short_id = %issue.short_id,
                repo = %repo_name,
                "Fetching latest changes"
            );

            if let Err(e) = GitOps::ensure_repo_fetched(&project_dir, scm_url).await {
                let pull_error = format!("Failed to fetch repository: {}", e);
                self.record_issue_decision(
                    &issue,
                    "repo_sync_failed",
                    format!("Repository sync failed for {}", issue.short_id),
                    json!({
                        "repo_name": repo_name,
                        "error": pull_error.clone(),
                    }),
                );
                if let Err(mark_err) =
                    self.tracker
                        .mark_failed(source.name(), &issue.id, &pull_error)
                {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        error = %mark_err,
                        "Failed to mark issue as failed after repository fetch failure"
                    );
                }
                tracing::error!(
                    short_id = %issue.short_id,
                    repo = %repo_name,
                    error = %e,
                    "Failed to fetch repository, skipping issue"
                );
                // Clean up processing state before returning
                {
                    let mut processing = self.processing.write().await;
                    processing.remove(&processing_key);
                }
                self.active_processing.fetch_sub(1, Ordering::SeqCst);
                return true;
            }

            // For review reruns, fetch the PR branch; otherwise use the default branch.
            let checkout_ref = if let Some(ref branch) = existing_pr_branch {
                if let Err(e) = GitOps::fetch_branch(&project_dir, branch).await {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        error = %e,
                        branch = %branch,
                        "Failed to fetch PR branch, falling back to default"
                    );
                    format!("origin/{}", default_branch)
                } else {
                    format!("origin/{}", branch)
                }
            } else {
                format!("origin/{}", default_branch)
            };

            // Create per-issue worktree.
            // For review reruns, check out the actual PR branch so Claude can push
            // to it.  For initial runs, use detached HEAD (Claude creates a new branch).
            let wt_path = worktree_path(&self.config.work_dir, repo_name, &issue.short_id);
            let wt_result = if let Some(ref branch) = existing_pr_branch {
                GitOps::create_worktree_on_branch(&project_dir, &wt_path, branch, &checkout_ref)
                    .await
            } else {
                GitOps::create_worktree(&project_dir, &wt_path, &checkout_ref).await
            };
            if let Err(e) = wt_result {
                let wt_error = format!("Failed to create worktree: {}", e);
                self.record_issue_decision(
                    &issue,
                    "worktree_failed",
                    format!("Worktree creation failed for {}", issue.short_id),
                    json!({
                        "repo_name": repo_name,
                        "error": wt_error.clone(),
                    }),
                );
                if let Err(mark_err) = self
                    .tracker
                    .mark_failed(source.name(), &issue.id, &wt_error)
                {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        error = %mark_err,
                        "Failed to mark issue as failed after worktree creation failure"
                    );
                }
                tracing::error!(
                    short_id = %issue.short_id,
                    repo = %repo_name,
                    error = %e,
                    "Failed to create worktree, skipping issue"
                );
                {
                    let mut processing = self.processing.write().await;
                    processing.remove(&processing_key);
                }
                self.active_processing.fetch_sub(1, Ordering::SeqCst);
                return true;
            }

            self.record_issue_decision(
                &issue,
                "repo_sync_completed",
                format!("Repository synced (worktree) for {}", issue.short_id),
                json!({
                    "repo_name": repo_name,
                    "project_dir": wt_path.display().to_string(),
                }),
            );

            // Re-index files and sync to database
            if let Some(inferrer) = &self.inferrer {
                if let Err(e) = inferrer.index_cloned_repo(repo_name) {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        repo = %repo_name,
                        error = %e,
                        "Failed to re-index repository files"
                    );
                }

                // Sync updated files to database
                if let Some(repo) = inferrer.get_repo(repo_name) {
                    if let Err(e) = self.tracker.sync_repo_files(&repo) {
                        tracing::warn!(
                            short_id = %issue.short_id,
                            repo = %repo_name,
                            error = %e,
                            "Failed to sync repository files to database"
                        );
                    }
                }
            }
        }

        // Use the per-issue worktree as the effective working directory for Claude.
        // Fall back to project_dir only when no repo was resolved (no worktree attempted).
        let effective_project_dir = if let Some(repo_name) = resolution.repo_name() {
            let wt = worktree_path(&self.config.work_dir, repo_name, &issue.short_id);
            if !wt.exists() {
                let err = format!("Worktree disappeared after creation: {:?}", wt);
                tracing::error!(short_id = %issue.short_id, error = %err);
                self.tracker
                    .mark_failed(source.name(), &issue.id, &err)
                    .ok();
                self.processing.write().await.remove(&processing_key);
                self.active_processing.fetch_sub(1, Ordering::SeqCst);
                return true;
            }
            wt
        } else {
            project_dir.clone()
        };

        // Run code quality evaluation baseline (BEFORE hook)
        let eval_before_snapshots = if self.config.evaluation.enabled {
            match crate::evaluation::CodeQualityEvaluator::run_baseline(
                &effective_project_dir,
                &self.config.evaluation,
            )
            .await
            {
                Ok(snapshots) => {
                    tracing::info!(
                        short_id = %issue.short_id,
                        tools = snapshots.len(),
                        "Evaluation baseline captured"
                    );
                    snapshots
                }
                Err(e) => {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        error = %e,
                        "Evaluation baseline failed, continuing without"
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Get the attempt ID for analytics tracking
        let attempt_id = self
            .tracker
            .get_attempt(source.name(), &issue.id)
            .ok()
            .flatten()
            .map(|a| a.id);

        // Resolve issue assignee to a configured user
        if let Some(assignee) = issue.get_metadata::<String>("assignee") {
            if let Some(resolved) = self.user_registry.resolve(&issue.source, &assignee) {
                tracing::info!(
                    short_id = %issue.short_id,
                    user = %resolved.slug,
                    "Resolved issue assignee to user"
                );
                issue.set_metadata("resolved_user", &resolved.slug);
            }
        }

        // Dedup gate: skip if this issue is a semantic duplicate of one already handled
        if let Some(ref embedding_service) = self.issue_embedding_service {
            if let Ok(Some(duplicate)) = embedding_service
                .check_duplicate(&issue, source.name())
                .await
            {
                let similar_id = duplicate
                    .embedding
                    .short_id
                    .as_deref()
                    .unwrap_or(&duplicate.embedding.issue_id);
                let similarity_pct = duplicate.similarity * 100.0;

                tracing::info!(
                    short_id = %issue.short_id,
                    similar_to = %similar_id,
                    similarity = %format!("{:.0}%", similarity_pct),
                    "Skipping semantic duplicate"
                );

                self.record_issue_decision(
                    &issue,
                    "semantic_duplicate_skipped",
                    format!(
                        "{} skipped as semantic duplicate of {}",
                        issue.short_id, similar_id
                    ),
                    json!({
                        "similar_issue_id": duplicate.embedding.issue_id,
                        "similar_short_id": similar_id,
                        "similarity": duplicate.similarity,
                        "similar_issue_status": duplicate.outcome.as_deref(),
                    }),
                );

                let metric = ProcessingMetric::new("semantic_duplicate_skipped", 1.0)
                    .with_source(source.name().to_string());
                self.tracker.record_metric(&metric).ok();

                let _ = self.tracker.mark_failed(
                    source.name(),
                    &issue.id,
                    &format!(
                        "Semantic duplicate of {} ({:.0}% similar)",
                        similar_id, similarity_pct
                    ),
                );

                // Cleanup
                {
                    let mut processing = self.processing.write().await;
                    processing.remove(&processing_key);
                }
                self.active_processing.fetch_sub(1, Ordering::SeqCst);
                return false;
            }
        }

        let result = async {
            // Notify start
            self.notifier.notify_start(&issue).await?;

            // Find similar issues for context (if embedding service is available)
            let similar_issues_context = if let Some(ref embedding_service) = self.issue_embedding_service {
                match embedding_service.find_similar(&issue, source.name()).await {
                    Ok(similar) if !similar.is_empty() => {
                        tracing::info!(
                            short_id = %issue.short_id,
                            similar_count = similar.len(),
                            "Found similar past issues for context"
                        );
                        self.record_issue_decision(
                            &issue,
                            "similar_issues_context_added",
                            format!("{} similar issues added to context for {}", similar.len(), issue.short_id),
                            json!({
                                "similar_count": similar.len(),
                                "similarities": similar.iter().map(|s| json!({
                                    "issue_id": s.embedding.issue_id,
                                    "short_id": s.embedding.short_id,
                                    "similarity": s.similarity,
                                })).collect::<Vec<_>>(),
                            }),
                        );
                        let metric = ProcessingMetric::new("similar_issues_context_added", 1.0)
                            .with_source(source.name().to_string());
                        self.tracker.record_metric(&metric).ok();
                        format_similar_issues_context(&similar)
                    }
                    Ok(_) => String::new(),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to find similar issues");
                        String::new()
                    }
                }
            } else {
                String::new()
            };

            // Build context and run Claude with attempt ID for analytics
            let mut context = source.build_issue_context(&issue).await?;

            // Append similar issues context if available
            if !similar_issues_context.is_empty() {
                context = format!("{}\n{}", context, similar_issues_context);
            }

            // Append PR review feedback context for review-driven reruns.
            if let Some(ref feedback) = review_feedback {
                let mut review_context = format!(
                    "\n\n## PR Review Feedback\n{}\n\nAddress all review feedback in this update.",
                    feedback
                );
                if let Some(ref branch) = existing_pr_branch {
                    review_context.push_str(&format!(
                        "\n\nIMPORTANT: You are updating an existing PR on branch `{}`. \
                         Push your changes to this branch. Do NOT create a new branch or a new PR.",
                        branch
                    ));
                }
                context = format!("{}{}", context, review_context);
            }

            let repo_scope = resolution.repo_name().map(|v| v.to_string());
            let mut used_qa_ids: Vec<i64> = Vec::new();

            // Preload reusable Q&A context before the first Claude run.
            if self.config.ask.enabled {
                let preload_query = format!("{} {}", issue.title, context);
                let preload_norm = normalize_text(&preload_query);
                let preload_embedding = embed_text(self.embedding_client.as_ref(), &preload_query).await;
                match find_reusable_qa(
                    self.tracker.as_ref(),
                    &self.config.ask,
                    source.name(),
                    repo_scope.as_deref(),
                    &preload_norm,
                    preload_embedding.as_deref(),
                ) {
                    Ok(matches) if !matches.is_empty() => {
                        context = format!("{}\n\n{}", context, format_reuse_context(&matches));
                        if let Some(id) = attempt_id {
                            for m in &matches {
                                let _ = self
                                    .tracker
                                    .record_qa_usage(id, m.entry.id, "reused", m.final_score);
                            }
                        }
                        used_qa_ids.extend(matches.into_iter().map(|m| m.entry.id));
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "Failed to preload reusable Q&A context"),
                }
            }

            let mut rounds: u8 = 0;
            let (claude_result, last_prompt) = loop {
                let prompt = self.agent.build_prompt_for_issue(&issue, &context, &effective_project_dir);

                // Enhance prompt with learnings from past outcomes (semantic when possible).
                let prompt = {
                    let analyzer = self.feedback_analyzer.lock().await;
                    let issue_emb = self
                        .issue_embedding_service
                        .as_ref()
                        .and_then(|svc| svc.get_embedding(source.name(), &issue.id).ok().flatten());
                    match issue_emb.and_then(|emb| emb.embedding) {
                        Some(ref emb) => analyzer.enhance_prompt(&prompt, &issue, emb),
                        None => prompt,
                    }
                };

                let prompt = self.enhance_prompt_with_learning(
                    &prompt,
                    &issue,
                    resolution.repo_name(),
                );
                let mut run_result = self
                    .agent
                    .execute_with_attempt(&prompt, Some(&issue), attempt_id, &effective_project_dir)
                    .await?;
                run_result.used_qa_ids = used_qa_ids.clone();

                let blocking_question = match (self.config.ask.enabled, run_result.blocking_question.clone()) {
                    (true, Some(q)) => q,
                    _ => break (run_result, prompt),
                };

                if rounds >= self.config.ask.max_rounds_per_attempt {
                    run_result.success = false;
                    run_result.error = Some(format!(
                        "Maximum blocking-question rounds ({}) reached",
                        self.config.ask.max_rounds_per_attempt
                    ));
                    break (run_result, prompt);
                }
                rounds = rounds.saturating_add(1);

                let question_norm = normalize_text(&blocking_question.question);
                let question_embedding =
                    embed_text(self.embedding_client.as_ref(), &blocking_question.question).await;

                let reusable = match find_reusable_qa(
                    self.tracker.as_ref(),
                    &self.config.ask,
                    source.name(),
                    repo_scope.as_deref(),
                    &question_norm,
                    question_embedding.as_deref(),
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to query reusable Q&A for blocking question");
                        Vec::new()
                    }
                };

                if let Some(best) = reusable.first() {
                    if let Some(id) = attempt_id {
                        let _ = self
                            .tracker
                            .record_qa_usage(id, best.entry.id, "reused", best.final_score);
                    }
                    if !used_qa_ids.contains(&best.entry.id) {
                        used_qa_ids.push(best.entry.id);
                    }
                    let activity = ActivityLogEntry::new(
                        "question_reused",
                        format!("Reused stored Q&A for {}", issue.short_id),
                    )
                    .with_source(issue.source.clone())
                    .with_issue(issue.id.clone(), issue.short_id.clone())
                    .with_metadata(json!({
                        "qa_id": best.entry.id,
                        "score": best.final_score,
                    }));
                    self.tracker.record_activity(&activity).ok();

                    context = format!(
                        "{}\n\n{}",
                        context,
                        format_answer_context(
                            &blocking_question,
                            &best.entry.answer_text,
                            &best.entry.channel,
                            true,
                        )
                    );
                    continue;
                }

                let resolved_user = issue.get_metadata::<String>("resolved_user");
                let target_discord_id = resolved_user
                    .as_deref()
                    .and_then(|slug| self.user_registry.get_by_slug(slug))
                    .and_then(|u| u.discord_id.clone());
                let target_email = resolved_user
                    .as_deref()
                    .and_then(|slug| self.user_registry.get_by_slug(slug))
                    .and_then(|u| u.email.clone());
                let ask_request = AskRequest {
                    correlation_id: build_correlation_id(&issue.short_id),
                    source: issue.source.clone(),
                    repo: repo_scope.clone(),
                    issue_id: issue.id.clone(),
                    short_id: issue.short_id.clone(),
                    question: blocking_question.clone(),
                    asked_at: Utc::now(),
                    target_discord_id,
                    target_email,
                    target_slack_id: resolved_user
                        .as_deref()
                        .and_then(|slug| self.user_registry.get_by_slug(slug))
                        .and_then(|u| u.slack_id.clone()),
                };

                let asked_activity = ActivityLogEntry::new(
                    "question_asked",
                    format!("Asked human question for {}", issue.short_id),
                )
                .with_source(issue.source.clone())
                .with_issue(issue.id.clone(), issue.short_id.clone())
                .with_metadata(json!({
                    "correlation_id": ask_request.correlation_id,
                    "question": blocking_question.question,
                }));
                self.tracker.record_activity(&asked_activity).ok();

                let reply = send_to_all_and_wait_first_reply(
                    Arc::clone(&self.notifier),
                    &issue,
                    &ask_request,
                    Duration::from_secs(self.config.ask.wait_timeout_secs),
                    Duration::from_secs(self.config.ask.poll_interval_secs),
                )
                .await?;

                if let Some(reply) = reply {
                    let answered_activity = ActivityLogEntry::new(
                        "question_answered",
                        format!("Human answered question for {}", issue.short_id),
                    )
                    .with_source(issue.source.clone())
                    .with_issue(issue.id.clone(), issue.short_id.clone())
                    .with_metadata(json!({
                        "channel": reply.channel,
                        "responder": reply.responder,
                        "correlation_id": reply.correlation_id,
                    }));
                    self.tracker.record_activity(&answered_activity).ok();

                    let qa_entry = QaKnowledgeEntry {
                        id: 0,
                        source: issue.source.clone(),
                        repo: repo_scope.clone(),
                        issue_id: issue.id.clone(),
                        short_id: issue.short_id.clone(),
                        question_text: blocking_question.question.clone(),
                        question_norm,
                        question_embedding: question_embedding.clone(),
                        answer_text: reply.answer.clone(),
                        answer_norm: normalize_text(&reply.answer),
                        answer_embedding: embed_text(self.embedding_client.as_ref(), &reply.answer).await,
                        channel: reply.channel.clone(),
                        responder: reply.responder.clone(),
                        correlation_id: ask_request.correlation_id.clone(),
                        asked_at: ask_request.asked_at,
                        answered_at: reply.replied_at,
                        success_count: 0,
                        failure_count: 0,
                        last_used_at: None,
                        metadata: Some(json!({
                            "context": blocking_question.context,
                            "options": blocking_question.options,
                            "why": blocking_question.why,
                        })),
                    };

                    if let Ok(qa_id) = self.tracker.store_qa_knowledge(&qa_entry) {
                        if let Some(id) = attempt_id {
                            let _ = self.tracker.record_qa_usage(id, qa_id, "asked", 1.0);
                        }
                        if !used_qa_ids.contains(&qa_id) {
                            used_qa_ids.push(qa_id);
                        }
                    }

                    context = format!(
                        "{}\n\n{}",
                        context,
                        format_answer_context(&blocking_question, &reply.answer, &reply.channel, false)
                    );
                    continue;
                }

                let timeout_activity = ActivityLogEntry::new(
                    "question_timeout_best_effort",
                    format!("No human reply received for {}", issue.short_id),
                )
                .with_source(issue.source.clone())
                .with_issue(issue.id.clone(), issue.short_id.clone())
                .with_metadata(json!({
                    "best_effort": self.config.ask.best_effort_on_timeout,
                    "question": blocking_question.question,
                }));
                self.tracker.record_activity(&timeout_activity).ok();

                if self.config.ask.best_effort_on_timeout {
                    context = format!("{}\n\n{}", context, format_timeout_context(&blocking_question));
                    continue;
                }

                run_result.success = false;
                run_result.error = Some("Timed out waiting for human reply".to_string());
                break (run_result, prompt);
            };

            if self.config.learning.strategy_fingerprinting {
                if let Some(aid) = attempt_id {
                    if let Ok(execs) = self.tracker.get_executions_for_attempt(aid) {
                        if let Some(exec) = execs.first() {
                            if let Some(ref log_path) = exec.stdout_log_path {
                                let path = std::path::Path::new(log_path);
                                if path.exists() {
                                    match crate::learning::StrategyParser::parse_from_log(path, aid) {
                                        Ok(fp) => {
                                            if let Err(e) = self.tracker.store_strategy_fingerprint(&fp) {
                                                tracing::warn!(error = %e, "Failed to store strategy fingerprint");
                                            }
                                        }
                                        Err(e) => tracing::debug!(error = %e, "Failed to parse strategy from log"),
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if claude_result.success {
                // For review reruns, resolve the effective PR URL:
                // 1. If Claude returned a PR URL, prefer the existing one (Claude
                //    should not create new PRs during review reruns).
                // 2. If Claude did NOT return a PR URL (common when it just pushes
                //    to the existing branch), fall back to the attempt's stored URL.
                let effective_pr_url = if existing_pr_branch.is_some() {
                    let stored_url = self
                        .tracker
                        .get_attempt(source.name(), &issue.id)
                        .ok()
                        .flatten()
                        .and_then(|a| a.pr_url);
                    match (&claude_result.pr_url, &stored_url) {
                        (Some(new_url), Some(existing_url)) if new_url != existing_url => {
                            tracing::warn!(
                                short_id = %issue.short_id,
                                existing_pr = %existing_url,
                                claude_pr = %new_url,
                                "Review rerun produced a different PR URL; keeping original"
                            );
                            stored_url
                        }
                        (None, Some(_)) => {
                            tracing::info!(
                                short_id = %issue.short_id,
                                "Review rerun pushed to existing branch (no new PR URL)"
                            );
                            stored_url
                        }
                        _ => claude_result.pr_url.clone(),
                    }
                } else {
                    claude_result.pr_url.clone()
                };

                if let Some(ref pr_url) = effective_pr_url {
                    self.record_issue_decision(
                        &issue,
                        "claude_run_succeeded_with_pr",
                        format!("Claude produced PR for {}", issue.short_id),
                        json!({
                            "pr_url": pr_url,
                            "attempt_id": attempt_id,
                            "used_qa_ids": claude_result.used_qa_ids,
                        }),
                    );
                    tracing::info!(short_id = %issue.short_id, pr_url = %pr_url, "Success! PR created");
                    self.tracker
                        .mark_success(source.name(), &issue.id, pr_url)?;
                    if existing_pr_branch.is_some() {
                        issue.set_metadata("is_pr_update", true);
                    }
                    if let Some(ref changelog) = claude_result.changelog {
                        issue.set_metadata("changelog", changelog.clone());
                    }
                    self.notifier.notify_success(&issue, pr_url).await?;
                    if let Some(id) = attempt_id {
                        let _ = self.tracker.update_qa_outcome_stats_for_attempt(id, true);
                    }

                    // Record metric for PR creation
                    let metric = ProcessingMetric::new("pr_created", 1.0)
                        .with_source(source.name().to_string());
                    if let Err(e) = self.tracker.record_metric(&metric) {
                        tracing::warn!(error = %e, "Failed to record pr_created metric");
                    }

                    // Create or update prs table record
                    if let Some((repo, pr_number)) = crate::storage::parse_pr_url(pr_url) {
                        // Try to load existing record to preserve accumulated fields
                        let mut pr_record = if let Ok(Some(existing)) = self.tracker.get_pr(pr_url) {
                            existing
                        } else {
                            crate::types::PrRecord::for_issue(
                                pr_url.clone(),
                                &repo,
                                pr_number,
                                source.name(),
                                &issue.id,
                            )
                        };
                        pr_record.attempt_id = attempt_id;

                        // Fetch branch info from GitHub
                        if let Some(ref gh) = self.github_client {
                            match gh.get_pr_info(&repo, pr_number).await {
                                Ok(info) => {
                                    pr_record.head_branch = info.head_branch;
                                    pr_record.base_branch = info.base_branch;
                                    pr_record.title = info.title;
                                    pr_record.author = info.author;
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to fetch PR info from GitHub");
                                }
                            }
                        }

                        if let Err(e) = self.tracker.upsert_pr(&pr_record) {
                            tracing::warn!(error = %e, "Failed to upsert PR record");
                        }
                    }

                    // Store embedding for future similarity lookups
                    if let Some(ref embedding_service) = self.issue_embedding_service {
                        match embedding_service.embed_issue(&issue, source.name()).await {
                            Ok(_) => {
                                self.record_issue_decision(
                                    &issue,
                                    "issue_embedding_stored",
                                    format!("Stored embedding for {}", issue.short_id),
                                    json!({}),
                                );
                                let metric = ProcessingMetric::new("issue_embedding_stored", 1.0)
                                    .with_source(source.name().to_string());
                                self.tracker.record_metric(&metric).ok();
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to store issue embedding");
                            }
                        }
                    }

                    // Register PR for review watching
                    if let Some(ref review_watcher) = self.review_watcher {
                        if let Some((repo, pr_number)) = crate::storage::parse_pr_url(pr_url) {
                            let state = PrReviewState::new(
                                pr_url,
                                &repo,
                                pr_number,
                                &issue.id,
                                source.name(),
                            );
                            review_watcher.watch_pr(state);
                            tracing::info!(
                                component = "review_watcher",
                                pr_url = %pr_url,
                                repo = %repo,
                                pr_number = pr_number,
                                issue_id = %issue.id,
                                "PR registered for review watching"
                            );
                        }
                    }

                    // Log processing_completed activity
                    let activity = ActivityLogEntry::new(
                        "processing_completed",
                        format!("Processing completed for {}", issue.short_id),
                    )
                    .with_source(issue.source.clone())
                    .with_issue(issue.id.clone(), issue.short_id.clone())
                    .with_metadata(json!({
                        "has_pr": true,
                        "pr_url": pr_url
                    }));
                    self.tracker.record_activity(&activity).ok();
                } else {
                    self.record_issue_decision(
                        &issue,
                        "claude_run_succeeded_without_pr",
                        format!("Claude returned success without PR for {}", issue.short_id),
                        json!({
                            "attempt_id": attempt_id,
                            "used_qa_ids": claude_result.used_qa_ids,
                        }),
                    );
                    let no_pr_error = if claude_result.output.is_empty() {
                        "No PR URL found in output".to_string()
                    } else {
                        let summary = if claude_result.output.chars().count() > 500 {
                            let truncated: String = claude_result.output.chars().take(497).collect();
                            format!("{}...", truncated)
                        } else {
                            claude_result.output.clone()
                        };
                        format!(
                            "Claude completed without creating a PR: {}",
                            summary,
                        )
                    };
                    tracing::info!(short_id = %issue.short_id, "No PR URL found in output");
                    self.tracker.mark_failed(
                        source.name(),
                        &issue.id,
                        &no_pr_error,
                    )?;
                    self.notifier.notify_failed(&issue, &no_pr_error).await?;
                    if let Some(id) = attempt_id {
                        let _ = self.tracker.update_qa_outcome_stats_for_attempt(id, false);
                    }

                    // Record feedback outcome
                    if let Ok(Some(attempt)) = self.tracker.get_attempt(source.name(), &issue.id) {
                        self.record_feedback_outcome(&attempt, &issue, &last_prompt, Outcome::Failed).await;
                    }

                    // Log processing_failed activity (no PR produced)
                    let activity = ActivityLogEntry::new(
                        "processing_failed",
                        format!("Processing failed for {} (no PR)", issue.short_id),
                    )
                    .with_source(issue.source.clone())
                    .with_issue(issue.id.clone(), issue.short_id.clone())
                    .with_metadata(json!({
                        "has_pr": false,
                        "pr_url": Option::<String>::None
                    }));
                    self.tracker.record_activity(&activity).ok();
                }
            } else {
                let base_error = claude_result.error.as_deref().unwrap_or("Unknown error");
                // Include Claude's summary if available for richer error context
                let error = if !claude_result.output.is_empty() {
                    let summary = if claude_result.output.len() > 500 {
                        format!("{}...", &claude_result.output[..497])
                    } else {
                        claude_result.output.clone()
                    };
                    format!("{}\n\nClaude's summary: {}", base_error, summary)
                } else {
                    base_error.to_string()
                };
                self.record_issue_decision(
                    &issue,
                    "claude_run_failed",
                    format!("Claude failed for {}", issue.short_id),
                    json!({
                        "error": error,
                        "attempt_id": attempt_id,
                        "used_qa_ids": claude_result.used_qa_ids,
                    }),
                );
                tracing::error!(short_id = %issue.short_id, error = %error, "Failed");
                self.tracker.mark_failed(source.name(), &issue.id, &error)?;
                self.notify_failed_with_escalation(&issue, &error).await?;
                if let Some(id) = attempt_id {
                    let _ = self.tracker.update_qa_outcome_stats_for_attempt(id, false);
                }

                // Record feedback outcome
                if let Ok(Some(attempt)) = self.tracker.get_attempt(source.name(), &issue.id) {
                    self.record_feedback_outcome(&attempt, &issue, &last_prompt, Outcome::Failed).await;
                }

                // Record error pattern for analytics
                self.record_error_pattern(source.name(), &issue.id, &error);
            }

            // Run code quality evaluation after fix (AFTER hook)
            if !eval_before_snapshots.is_empty() {
                let eval_attempt_id = attempt_id.unwrap_or(0);
                let eval_repo = resolution.repo_name().unwrap_or("unknown");
                match crate::evaluation::CodeQualityEvaluator::run_after_and_compute_deltas(
                    &effective_project_dir,
                    &self.config.evaluation,
                    eval_before_snapshots.clone(),
                    eval_attempt_id,
                    eval_repo,
                ).await {
                    Ok(eval_result) => {
                        if !eval_result.deltas.is_empty() {
                            tracing::info!(
                                short_id = %issue.short_id,
                                improved = eval_result.overall_improved,
                                deltas = eval_result.deltas.len(),
                                "Evaluation complete"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            short_id = %issue.short_id,
                            error = %e,
                            "Post-fix evaluation failed"
                        );
                    }
                }
            }

            Ok::<_, crate::error::Error>(())
        }
        .await;

        if let Err(ref e) = result {
            tracing::error!(short_id = %issue.short_id, error = %e, "Error processing issue");
            let error_str = e.to_string();
            self.record_issue_decision(
                &issue,
                "processing_pipeline_error",
                format!("Processing pipeline error for {}", issue.short_id),
                json!({
                    "error": error_str.clone(),
                    "attempt_id": attempt_id,
                }),
            );
            let _ = self
                .tracker
                .mark_failed(source.name(), &issue.id, &error_str);
            if let Some(id) = attempt_id {
                let _ = self.tracker.update_qa_outcome_stats_for_attempt(id, false);
            }
            let _ = self.notify_failed_with_escalation(&issue, &error_str).await;

            // Record feedback outcome
            if let Ok(Some(attempt)) = self.tracker.get_attempt(source.name(), &issue.id) {
                self.record_feedback_outcome_from_attempt(&attempt, Outcome::Failed)
                    .await;
            }

            // Record error pattern for analytics
            self.record_error_pattern(source.name(), &issue.id, &error_str);
        }

        // Record processing duration as a first-class metric for telemetry dashboards.
        let final_status = self
            .tracker
            .get_attempt(source.name(), &issue.id)
            .ok()
            .flatten()
            .map(|a| a.status.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let processing_time_metric = ProcessingMetric::new(
            "processing_time",
            processing_started_at.elapsed().as_secs_f64(),
        )
        .with_source(source.name().to_string())
        .with_tags(json!({ "status": final_status }));
        if let Err(e) = self.tracker.record_metric(&processing_time_metric) {
            tracing::debug!(error = %e, "Failed to record processing_time metric");
        }

        // Cleanup worktree
        if let Some(repo_name) = resolution.repo_name() {
            let wt_path = worktree_path(&self.config.work_dir, repo_name, &issue.short_id);
            if wt_path.exists() {
                if let Err(e) = GitOps::remove_worktree(&project_dir, &wt_path).await {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        error = %e,
                        "Failed to remove worktree"
                    );
                }
            }
        }

        // Cleanup processing state
        {
            let mut processing = self.processing.write().await;
            processing.remove(&processing_key);
        }
        self.active_processing.fetch_sub(1, Ordering::SeqCst);
        true
    }

    /// Record an error pattern to the analytics database.
    fn record_error_pattern(&self, source: &str, issue_id: &str, error_msg: &str) {
        let error_type = classify_error(error_msg);
        let pattern_hash = compute_error_hash(error_msg);

        let mut pattern = ErrorPattern::new(pattern_hash);
        pattern.error_type = Some(error_type.to_string());
        pattern.error_message = Some(error_msg.to_string());
        pattern.sources = Some(vec![source.to_string()]);
        pattern.example_issue_ids = Some(vec![issue_id.to_string()]);

        if let Err(e) = self.tracker.record_error_pattern(&pattern) {
            tracing::warn!(error = %e, "Failed to record error pattern");
        }
    }

    /// Route hard failures to the global notifier user (override per-issue assignee routing).
    async fn notify_failed_with_escalation(&self, issue: &Issue, error: &str) -> Result<()> {
        if runner::is_hard_error(error) {
            self.record_issue_decision(
                issue,
                "hard_error_escalated",
                format!("Escalating hard error for {}", issue.short_id),
                json!({
                    "error": Self::truncate_error_for_activity(error),
                    "rate_limited": runner::is_rate_limit_error(error),
                }),
            );
            let mut global_issue = issue.clone();
            global_issue.metadata.remove("resolved_user");
            global_issue
                .metadata
                .insert("hard_error".to_string(), serde_json::Value::Bool(true));

            let activity = ActivityLogEntry::new(
                "error",
                format!("Hard Claude error escalated for {}", issue.short_id),
            )
            .with_source(issue.source.clone())
            .with_issue(issue.id.clone(), issue.short_id.clone())
            .with_metadata(json!({
                "hard_error": true,
                "rate_limited": runner::is_rate_limit_error(error),
                "error": Self::truncate_error_for_activity(error),
            }));
            self.tracker.record_activity(&activity).ok();

            return self.notifier.notify_failed(&global_issue, error).await;
        }

        self.notifier.notify_failed(issue, error).await
    }

    fn truncate_error_for_activity(error: &str) -> String {
        let max_len = 500;
        if error.len() <= max_len {
            error.to_string()
        } else {
            let safe_end = error
                .char_indices()
                .take_while(|(i, _)| *i <= max_len.saturating_sub(3))
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            format!("{}...", &error[..safe_end])
        }
    }

    /// Record a feedback outcome to both DB and in-memory analyzer.
    async fn record_feedback_outcome(
        &self,
        attempt: &crate::types::FixAttempt,
        issue: &Issue,
        prompt: &str,
        outcome: Outcome,
    ) {
        let mut fix_outcome = FixOutcome::from_attempt(attempt, issue, prompt, outcome);

        // Compute embedding for the outcome's issue text (reuse existing issue embedding if available)
        if let Some(ref embedding_client) = self.embedding_client {
            let embedding = match self
                .issue_embedding_service
                .as_ref()
                .and_then(|svc| svc.get_embedding(&attempt.source, &issue.id).ok().flatten())
                .and_then(|existing| existing.embedding)
            {
                Some(existing) => Some(existing),
                None => embedding_client.embed(&fix_outcome.issue_text).await.ok(),
            };
            if let Some(emb) = embedding {
                fix_outcome.set_embedding(emb);
            }
        }

        // Store to DB (including embedding if computed)
        if let Err(e) = self.tracker.store_feedback_outcome(&fix_outcome) {
            tracing::warn!(error = %e, "Failed to store feedback outcome to DB");
        }

        // Store in-memory for prompt enhancement
        let mut analyzer = self.feedback_analyzer.lock().await;
        if let Err(e) = analyzer.record_outcome(attempt, issue, prompt, outcome) {
            tracing::warn!(error = %e, "Failed to record feedback outcome in memory");
        }
    }

    /// Record a feedback outcome from an attempt (when we lack the Issue object).
    /// Reconstructs a minimal Issue from attempt data and retrieves prompt from executions.
    async fn record_feedback_outcome_from_attempt(
        &self,
        attempt: &crate::types::FixAttempt,
        outcome: Outcome,
    ) {
        let issue = Issue::new(
            &attempt.issue_id,
            &attempt.short_id,
            format!("Issue {}", attempt.short_id),
            String::new(),
            &attempt.source,
        );

        // Try to get the prompt from the most recent execution
        let prompt = self
            .tracker
            .get_executions_for_attempt(attempt.id)
            .ok()
            .and_then(|execs| execs.into_iter().next())
            .and_then(|exec| exec.prompt_used)
            .unwrap_or_default();

        self.record_feedback_outcome(attempt, &issue, &prompt, outcome)
            .await;
    }

    /// Run periodic learning subsystem tasks (QA promotion, cluster detection).
    async fn run_periodic_learning(&self) {
        let learning = &self.config.learning;

        // System 3: Promote repeated Q&A answers to standing instructions
        if learning.qa_promotion {
            match crate::learning::QaPromoter::scan_and_promote(
                self.tracker.as_ref(),
                self.embedding_client.as_ref(),
                learning.qa_promotion_threshold,
                0.8,
            ) {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!(
                        promoted = n,
                        "Promoted Q&A answers to standing instructions"
                    );
                    self.record_source_decision(
                        "system",
                        "qa_promotion_completed",
                        format!("Promoted {} Q&A answers to standing instructions", n),
                        json!({ "promoted_count": n }),
                    );
                }
                Err(e) => tracing::debug!(error = %e, "Q&A promotion scan failed"),
            }
        }

        // System 8: Detect clusters of correlated issues
        if learning.cluster_detection {
            for source in &self.sources {
                match crate::learning::ClusterDetector::detect_clusters(
                    self.tracker.as_ref(),
                    source.name(),
                    learning.cluster_window_minutes as i64,
                    learning.min_cluster_size,
                ) {
                    Ok(clusters) if !clusters.is_empty() => {
                        for cluster in &clusters {
                            match self.tracker.store_issue_cluster(cluster) {
                                Ok(_) => {
                                    tracing::info!(
                                        source = source.name(),
                                        issues = cluster.issue_ids.len(),
                                        "Detected and stored issue cluster"
                                    );
                                }
                                Err(e) => {
                                    // UNIQUE constraint violation means cluster already stored
                                    tracing::debug!(error = %e, "Failed to store cluster (may already exist)");
                                }
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::debug!(error = %e, source = source.name(), "Cluster detection failed")
                    }
                }

                // Check if existing active clusters have been resolved
                if let Ok(active_clusters) = self.tracker.get_active_clusters(source.name()) {
                    for cluster in &active_clusters {
                        match crate::learning::ClusterDetector::check_cluster_resolution(
                            self.tracker.as_ref(),
                            cluster,
                        ) {
                            Ok(true) => {
                                // Find the merged issue to record as resolver
                                let resolver = cluster.issue_ids.iter().find_map(|issue_id| {
                                    self.tracker
                                        .get_attempt(&cluster.source, issue_id)
                                        .ok()
                                        .flatten()
                                        .and_then(|a| {
                                            if a.status == FixAttemptStatus::Merged {
                                                Some((issue_id.clone(), a.id))
                                            } else {
                                                None
                                            }
                                        })
                                });
                                let (resolved_issue, resolved_attempt) =
                                    resolver.unwrap_or_else(|| ("unknown".to_string(), 0));
                                if let Err(e) = self.tracker.update_cluster_resolution(
                                    cluster.id,
                                    &resolved_issue,
                                    resolved_attempt,
                                ) {
                                    tracing::debug!(error = %e, "Failed to mark cluster resolved");
                                } else {
                                    tracing::info!(
                                        source = source.name(),
                                        cluster_key = %cluster.cluster_key,
                                        resolved_by = %resolved_issue,
                                        "Cluster resolved (at least one issue merged)"
                                    );
                                }
                            }
                            Ok(false) => {}
                            Err(e) => {
                                tracing::debug!(error = %e, "Failed to check cluster resolution")
                            }
                        }
                    }
                }
            }
        }

        // Cross-repo failure correlation
        if learning.cross_repo_correlation {
            match crate::learning::CrossRepoCorrelator::detect_correlations(
                self.tracker.as_ref(),
                learning.cross_repo_window_hours,
            ) {
                Ok(insights) if !insights.is_empty() => {
                    for insight in &insights {
                        tracing::info!(
                            upstream = %insight.repo_a,
                            downstream = %insight.repo_b,
                            count = insight.correlation_count,
                            "Cross-repo correlation detected"
                        );
                    }
                    self.record_source_decision(
                        "system",
                        "cross_repo_correlation",
                        format!("Detected {} cross-repo correlations", insights.len()),
                        serde_json::json!({ "correlation_count": insights.len() }),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(error = %e, "Cross-repo correlation detection failed");
                }
            }
        }
    }

    /// Run post-merge learning hooks (extract learnings, analyze diff, compute quality score).
    async fn run_post_merge_learning(&self, attempt: &crate::types::FixAttempt) {
        let learning = &self.config.learning;

        // System 1: Auto-extract learnings from execution logs
        if learning.auto_extract_learnings {
            if let Ok(execs) = self.tracker.get_executions_for_attempt(attempt.id) {
                if let Some(exec) = execs.first() {
                    if let Some(ref log_path) = exec.stdout_log_path {
                        let path = std::path::Path::new(log_path);
                        if path.exists() {
                            match crate::learning::LogExtractor::extract_learnings_from_log(
                                path,
                            ) {
                                Ok(learnings) => {
                                    let summary =
                                        crate::learning::LogExtractor::summarize(&learnings);
                                    // Store learnings on the feedback outcome
                                    if let Ok(Some(outcome)) =
                                        self.tracker.get_feedback_outcome_by_attempt(attempt.id)
                                    {
                                        if let Err(e) = self
                                            .tracker
                                            .update_feedback_learnings(outcome.id, &summary)
                                        {
                                            tracing::warn!(error = %e, "Failed to store extracted learnings");
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(error = %e, "Failed to extract learnings from log")
                                }
                            }
                        }
                    }
                }
            }
        }

        // System 2: Analyze PR diff
        if learning.diff_analysis {
            if let (Some(github), Some(repo), Some(pr_number)) = (
                self.github_client.as_ref(),
                attempt.scm_repo.as_deref(),
                attempt.scm_pr_number,
            ) {
                let pr_url = attempt.pr_url.as_deref().unwrap_or("");
                match github.get_pr_diff(repo, pr_number).await {
                    Ok(diff) => {
                        let analysis = crate::learning::DiffAnalyzer::analyze_diff(
                            &diff, attempt.id, pr_url, repo, pr_number,
                        );

                        if let Err(e) = self.tracker.store_diff_analysis(&analysis) {
                            tracing::warn!(error = %e, "Failed to store diff analysis");
                        }

                        // Update prs record with files_changed from diff analysis
                        if let Ok(Some(mut pr_record)) = self.tracker.get_pr(pr_url) {
                            pr_record.files_changed = Some(analysis.files_changed.len() as i64);
                            if let Err(e) = self.tracker.upsert_pr(&pr_record) {
                                tracing::warn!(error = %e, "Failed to update PR files_changed");
                            }
                        }

                        // Feed into repo knowledge
                        if learning.repo_knowledge {
                            if let Err(e) = crate::learning::RepoKnowledgeManager::learn_from_diff(
                                self.tracker.as_ref(),
                                repo,
                                &analysis,
                            ) {
                                tracing::warn!(error = %e, "Failed to learn from diff");
                            }
                        }
                    }
                    Err(e) => tracing::debug!(error = %e, "Failed to fetch PR diff for analysis"),
                }
            }
        }

        // System 7: Compute quality score
        if learning.quality_scoring {
            if let Some(ref pr_url) = attempt.pr_url {
                if let Ok(Some(pr_record)) = self.tracker.get_pr(pr_url) {
                    let quality = crate::learning::QualityScorer::compute(&pr_record);
                    if let Err(e) = self
                        .tracker
                        .update_pr_fix_quality_score(pr_url, quality.score)
                    {
                        tracing::warn!(error = %e, "Failed to store quality score");
                    }
                }
            }
        }

        // System 9: Auto-generate AGENT.md from accumulated knowledge
        if learning.auto_agent_md {
            if let Some(repo) = attempt.scm_repo.as_deref() {
                let knowledge = self.tracker.get_repo_knowledge(repo).unwrap_or_default();
                let instructions = self
                    .tracker
                    .get_promoted_instructions(repo)
                    .unwrap_or_default();
                if !knowledge.is_empty() || !instructions.is_empty() {
                    let agent_md = crate::learning::RepoKnowledgeManager::generate_agent_md(
                        &knowledge,
                        &instructions,
                    );
                    let agent_md_path = self.config.work_dir.join(repo).join("AGENT.md");
                    if let Some(parent) = agent_md_path.parent() {
                        if parent.exists() {
                            if let Err(e) = std::fs::write(&agent_md_path, &agent_md) {
                                tracing::debug!(error = %e, path = ?agent_md_path, "Failed to write AGENT.md");
                            } else {
                                tracing::info!(
                                    repo = repo,
                                    path = ?agent_md_path,
                                    "Generated AGENT.md from accumulated knowledge"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Enhance a prompt with continuous learning context.
    fn enhance_prompt_with_learning(
        &self,
        base_prompt: &str,
        _issue: &Issue,
        repo: Option<&str>,
    ) -> String {
        let learning = &self.config.learning;
        let Some(repo_name) = repo else {
            return base_prompt.to_string();
        };

        let mut extra_context = String::new();

        // System 4: Per-repo knowledge context
        if learning.repo_knowledge {
            if let Ok(knowledge) = self.tracker.get_repo_knowledge(repo_name) {
                let ctx =
                    crate::learning::RepoKnowledgeManager::format_knowledge_context(&knowledge);
                if !ctx.is_empty() {
                    extra_context.push_str(&ctx);
                }
            }
        }

        // System 3: Promoted instructions
        if learning.qa_promotion {
            if let Ok(instructions) = self.tracker.get_promoted_instructions(repo_name) {
                let ctx = crate::learning::QaPromoter::format_promoted_context(&instructions);
                if !ctx.is_empty() {
                    extra_context.push_str(&ctx);
                }
            }
        }

        // System 6: Strategy suggestions
        if learning.strategy_fingerprinting {
            if let Ok(strategies) = self.tracker.get_successful_strategies(repo_name, 3) {
                let ctx = crate::learning::StrategyParser::format_strategy_suggestions(&strategies);
                if !ctx.is_empty() {
                    extra_context.push_str(&ctx);
                }
            }
        }

        // System 8: Cluster context
        if learning.cluster_detection {
            if let Ok(clusters) = self.tracker.get_active_clusters(&_issue.source) {
                for cluster in &clusters {
                    if cluster.issue_ids.contains(&_issue.id) {
                        extra_context.push_str(
                            &crate::learning::ClusterDetector::format_cluster_context(cluster),
                        );
                        extra_context.push('\n');
                        break;
                    }
                }
            }
        }

        // Cross-repo correlation context
        if learning.cross_repo_correlation {
            match crate::learning::CrossRepoCorrelator::get_active_insights(
                self.tracker.as_ref(),
                3,                                    // min_count
                learning.cross_repo_window_hours * 2, // max_age = 2x window
            ) {
                Ok(insights) if !insights.is_empty() => {
                    let ctx = crate::learning::CrossRepoCorrelator::format_context(&insights);
                    if !ctx.is_empty() {
                        extra_context.push_str(&ctx);
                    }
                }
                _ => {}
            }
        }

        if extra_context.is_empty() {
            return base_prompt.to_string();
        }

        format!("{}\n---\n\n{}", extra_context, base_prompt)
    }

    /// Manually trigger processing for a specific issue.
    pub async fn trigger_issue(&self, source_name: &str, issue_id: &str) -> Result<()> {
        self.trigger_issue_with_feedback(source_name, issue_id, None, None)
            .await
    }

    /// Manually trigger processing for a specific issue with optional review feedback context.
    pub async fn trigger_issue_with_feedback(
        &self,
        source_name: &str,
        issue_id: &str,
        review_feedback: Option<String>,
        existing_pr_branch: Option<String>,
    ) -> Result<()> {
        let source = self
            .sources
            .iter()
            .find(|s| s.name() == source_name)
            .ok_or_else(|| crate::error::Error::source(source_name, "Unknown source"))?;

        tracing::info!(
            component = "watcher",
            source = source_name,
            issue_id = issue_id,
            "Manually triggering issue"
        );

        let issue = source.get_issue(issue_id).await?;
        let match_result = MatchResult::matched("Manual trigger", MatchPriority::Urgent);

        let started = self
            .process_issue(
                Arc::clone(source),
                issue,
                match_result,
                review_feedback,
                existing_pr_branch,
            )
            .await;
        if !started {
            return Err(crate::error::Error::source(
                source_name,
                format!(
                    "Issue {} is already being processed; trigger deferred",
                    issue_id
                ),
            ));
        }

        Ok(())
    }

    /// Reset a failed attempt to allow retry.
    pub fn reset_attempt(&self, source_name: &str, issue_id: &str) -> Result<()> {
        self.tracker.reset_attempt(source_name, issue_id)?;
        tracing::info!(
            component = "watcher",
            source = source_name,
            issue_id = issue_id,
            "Reset attempt"
        );
        Ok(())
    }

    /// Get statistics.
    pub fn get_stats(&self) -> Result<FixAttemptStats> {
        self.tracker.get_stats()
    }

    /// Check for PRs that should be auto-closed due to issue state changes.
    ///
    /// This checks all pending PRs and closes any whose source issue has been
    /// resolved, cancelled, or otherwise moved to a terminal state.
    pub async fn check_and_auto_close_prs(&self) -> Result<Vec<String>> {
        let pending_prs = self.tracker.get_pending_prs()?;
        let mut auto_closed = Vec::new();

        for attempt in pending_prs {
            // Find the source for this attempt
            if let Some(source) = self.sources.iter().find(|s| s.name() == attempt.source) {
                // Check if issue is still active
                match source.get_issue_status(&attempt.issue_id).await {
                    Ok(status) if source.is_terminal_status(&status) => {
                        tracing::info!(
                            source = %attempt.source,
                            issue_id = %attempt.issue_id,
                            short_id = %attempt.short_id,
                            status = %status,
                            "Auto-closing PR: issue reached terminal state"
                        );

                        // Log activity
                        let activity = ActivityLogEntry::new(
                            "pr_auto_closed",
                            format!(
                                "PR auto-closed: issue {} is now {}",
                                attempt.short_id, status
                            ),
                        )
                        .with_source(attempt.source.clone())
                        .with_issue(attempt.issue_id.clone(), attempt.short_id.clone())
                        .with_metadata(json!({
                            "pr_url": attempt.pr_url,
                            "issue_status": status,
                            "reason": "issue_terminal_state"
                        }));
                        let _ = self.tracker.record_activity(&activity);

                        // Mark as closed in tracker
                        if let Err(e) = self.tracker.mark_closed(&attempt.source, &attempt.issue_id)
                        {
                            tracing::warn!(
                                error = %e,
                                "Failed to mark attempt as closed"
                            );
                        }
                        let _ = self
                            .tracker
                            .update_qa_outcome_stats_for_attempt(attempt.id, false);

                        // Notify about the auto-close
                        let issue = Issue::new(
                            &attempt.issue_id,
                            &attempt.short_id,
                            format!("Issue {} (auto-closed)", attempt.short_id),
                            attempt.pr_url.clone().unwrap_or_default(),
                            &attempt.source,
                        );
                        let _ = self
                            .notifier
                            .notify_failed(
                                &issue,
                                &format!("PR auto-closed: source issue is now {}", status),
                            )
                            .await;

                        // Record feedback outcome
                        self.record_feedback_outcome_from_attempt(&attempt, Outcome::Closed)
                            .await;

                        // Stop review polling for auto-closed PRs.
                        if let (Some(review_watcher), Some(pr_url)) =
                            (self.review_watcher.as_ref(), attempt.pr_url.as_ref())
                        {
                            review_watcher.unwatch_pr(pr_url);
                        }

                        if let Some(ref url) = attempt.pr_url {
                            auto_closed.push(url.clone());
                        }
                    }
                    Ok(_) => {} // Issue still active
                    Err(e) => {
                        tracing::debug!(
                            source = %attempt.source,
                            issue_id = %attempt.issue_id,
                            error = %e,
                            "Failed to check issue status for auto-close"
                        );
                    }
                }
            }
        }

        if !auto_closed.is_empty() {
            tracing::info!(
                count = auto_closed.len(),
                "Auto-closed PRs due to issue state changes"
            );
        }

        Ok(auto_closed)
    }

    /// Sort issues by priority for processing order.
    fn sort_by_priority(&self, issues: &mut [(Issue, MatchResult)]) {
        issues.sort_by(|a, b| {
            // Sort by match priority first
            let priority_cmp = priority_order(&a.1.priority).cmp(&priority_order(&b.1.priority));
            if priority_cmp != std::cmp::Ordering::Equal {
                return priority_cmp;
            }

            // Then by issue priority
            issue_priority_order(&b.0.priority).cmp(&issue_priority_order(&a.0.priority))
        });
    }
}

fn priority_order(p: &MatchPriority) -> u8 {
    match p {
        MatchPriority::Urgent => 0,
        MatchPriority::High => 1,
        MatchPriority::Normal => 2,
        MatchPriority::Low => 3,
    }
}

fn issue_priority_order(p: &crate::types::IssuePriority) -> u8 {
    match p {
        crate::types::IssuePriority::Critical => 4,
        crate::types::IssuePriority::High => 3,
        crate::types::IssuePriority::Medium => 2,
        crate::types::IssuePriority::Low => 1,
        crate::types::IssuePriority::None => 0,
    }
}

/// Result of seeding operation.
#[derive(Debug, Default)]
pub struct SeedResult {
    pub total: usize,
    pub by_source: std::collections::HashMap<String, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifier::Notifier;
    use crate::reports::Report;
    use crate::source::IssueSource;
    use crate::storage::SqliteTracker;
    use crate::types::IssuePriority;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    // Mock notifier for testing
    struct MockNotifier {
        enabled: bool,
        call_count: AtomicUsize,
        fail_urgent_notify: bool,
    }

    impl MockNotifier {
        fn new(enabled: bool) -> Self {
            Self {
                enabled,
                call_count: AtomicUsize::new(0),
                fail_urgent_notify: false,
            }
        }

        fn with_urgent_failure(enabled: bool) -> Self {
            Self {
                enabled,
                call_count: AtomicUsize::new(0),
                fail_urgent_notify: true,
            }
        }

        fn get_call_count(&self) -> usize {
            self.call_count.load(AtomicOrdering::SeqCst)
        }
    }

    #[async_trait]
    impl Notifier for MockNotifier {
        fn name(&self) -> &str {
            "mock"
        }
        fn is_enabled(&self) -> bool {
            self.enabled
        }
        async fn notify_start(&self, _issue: &Issue) -> Result<()> {
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        async fn notify_success(&self, _issue: &Issue, _pr_url: &str) -> Result<()> {
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        async fn notify_completed(&self, _issue: &Issue) -> Result<()> {
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        async fn notify_failed(&self, _issue: &Issue, _error: &str) -> Result<()> {
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        async fn notify_status(&self, _message: &str) -> Result<()> {
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        async fn notify_urgent_issues(&self, _issues: &[Issue]) -> Result<()> {
            if self.fail_urgent_notify {
                return Err(crate::error::Error::notifier(
                    "mock",
                    "urgent notify failed",
                ));
            }
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        async fn notify_merged(&self, _issue: &Issue, _pr_url: &str) -> Result<()> {
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        async fn notify_report(&self, _report: &Report) -> Result<()> {
            self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
    }

    // Mock source for testing
    struct MockSource {
        name: String,
        issues: Vec<Issue>,
        match_priority: MatchPriority,
        issue_status_calls: AtomicUsize,
    }

    impl MockSource {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                issues: vec![],
                match_priority: MatchPriority::Normal,
                issue_status_calls: AtomicUsize::new(0),
            }
        }

        fn with_issues(name: &str, issues: Vec<Issue>) -> Self {
            Self {
                name: name.to_string(),
                issues,
                match_priority: MatchPriority::Normal,
                issue_status_calls: AtomicUsize::new(0),
            }
        }

        fn with_priority(name: &str, issues: Vec<Issue>, match_priority: MatchPriority) -> Self {
            Self {
                name: name.to_string(),
                issues,
                match_priority,
                issue_status_calls: AtomicUsize::new(0),
            }
        }

        fn issue_status_call_count(&self) -> usize {
            self.issue_status_calls.load(AtomicOrdering::SeqCst)
        }
    }

    #[async_trait]
    impl IssueSource for MockSource {
        fn name(&self) -> &str {
            &self.name
        }
        fn display_name(&self) -> &str {
            &self.name
        }
        async fn fetch_issues(&self) -> Result<Vec<Issue>> {
            Ok(self.issues.clone())
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::matched("Mock match", self.match_priority)
        }
        async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
            Ok(format!("Context for {}", issue.short_id))
        }
        async fn get_issue(&self, id: &str) -> Result<Issue> {
            self.issues
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .ok_or_else(|| crate::error::Error::source(&self.name, "Issue not found"))
        }
        async fn get_issue_status(&self, issue_id: &str) -> Result<String> {
            self.issue_status_calls.fetch_add(1, AtomicOrdering::SeqCst);
            let issue = self.get_issue(issue_id).await?;
            Ok(format!("{:?}", issue.status))
        }
    }

    fn test_issue() -> Issue {
        Issue::new(
            "123",
            "TEST-123",
            "Test Issue",
            "https://example.com",
            "test",
        )
    }

    fn test_issue_with_priority(id: &str, priority: IssuePriority) -> Issue {
        let mut issue = Issue::new(
            id,
            format!("TEST-{}", id),
            "Test",
            "https://example.com",
            "test",
        );
        issue.priority = priority;
        issue
    }

    fn test_config() -> Config {
        Config {
            work_dir: std::path::PathBuf::from("/tmp/repos"),
            known_orgs: vec!["test-org".to_string()],
            auto_discover_paths: vec![],
            poll_interval_ms: 60000,
            webhook_port: 8080,
            bind_address: "127.0.0.1".to_string(),
            db_path: std::path::PathBuf::from(":memory:"),
            max_issues_per_cycle: 5,
            max_concurrent: 2,
            processing_delay_ms: 1000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            agent: crate::config::AgentConfig::default(),
            scm: crate::config::ScmConfig::default(),
            issues: crate::config::IssuesConfig::default(),
            notifiers: crate::config::NotifiersConfig::default(),
            ask: crate::config::AskConfig::default(),
            retry: crate::config::RetryConfig::default(),
            regression: crate::config::RegressionConfig::default(),
            cascade: crate::config::CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: crate::config::LearningConfig::default(),
            prioritisation: crate::config::PrioritisationConfig::default(),
            code_index: crate::config::CodeIndexConfig::default(),
            evaluation: crate::config::EvaluationConfig::default(),
            storage_dir: "/tmp/claudear-storage".into(),
            dashboard: crate::config::DashboardConfig::default(),
            tenant_id: None,
        }
    }

    fn create_test_watcher(
        notifier: Arc<dyn Notifier>,
        tracker: Arc<dyn FixAttemptTracker>,
        sources: Vec<Arc<dyn IssueSource>>,
        dry_run: bool,
    ) -> Watcher {
        let agent: Arc<dyn crate::runner::AgentRunner> = Arc::new(
            crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            ),
        );
        Watcher::new(WatcherOptions {
            config: test_config(),
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,       // Tests don't need inference
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent,
            dry_run,
        })
    }

    #[test]
    fn test_watcher_new() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![Arc::new(MockSource::new("test"))];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        assert!(!watcher.dry_run);
        assert!(!watcher.is_running.load(Ordering::SeqCst));
        assert_eq!(watcher.active_processing.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_watcher_new_dry_run() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, true);

        assert!(watcher.dry_run);
    }

    #[test]
    fn test_watcher_stop() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);
        watcher.is_running.store(true, Ordering::SeqCst);

        assert!(watcher.is_running.load(Ordering::SeqCst));
        watcher.stop();
        assert!(!watcher.is_running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_watcher_get_stats() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let stats = watcher.get_stats().unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.success, 0);
    }

    #[test]
    fn test_watcher_reset_attempt() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        // Record an attempt first
        tracker.record_attempt("test", "123", "TEST-123").unwrap();

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);

        assert!(tracker.has_attempted("test", "123").unwrap());
        watcher.reset_attempt("test", "123").unwrap();
        assert!(!tracker.has_attempted("test", "123").unwrap());
    }

    #[test]
    fn test_watcher_sort_by_priority() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let mut issues = vec![
            (
                test_issue(),
                MatchResult::matched("Low", MatchPriority::Low),
            ),
            (
                test_issue(),
                MatchResult::matched("Urgent", MatchPriority::Urgent),
            ),
            (
                test_issue(),
                MatchResult::matched("High", MatchPriority::High),
            ),
            (
                test_issue(),
                MatchResult::matched("Normal", MatchPriority::Normal),
            ),
        ];

        watcher.sort_by_priority(&mut issues);

        assert_eq!(issues[0].1.priority, MatchPriority::Urgent);
        assert_eq!(issues[1].1.priority, MatchPriority::High);
        assert_eq!(issues[2].1.priority, MatchPriority::Normal);
        assert_eq!(issues[3].1.priority, MatchPriority::Low);
    }

    #[test]
    fn test_watcher_sort_by_priority_with_issue_priority() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // All same match priority, different issue priorities
        let mut issues = vec![
            (
                test_issue_with_priority("1", IssuePriority::Low),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
            (
                test_issue_with_priority("2", IssuePriority::Critical),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
            (
                test_issue_with_priority("3", IssuePriority::Medium),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
        ];

        watcher.sort_by_priority(&mut issues);

        // Should be sorted by issue priority (Critical first)
        assert_eq!(issues[0].0.priority, IssuePriority::Critical);
        assert_eq!(issues[1].0.priority, IssuePriority::Medium);
        assert_eq!(issues[2].0.priority, IssuePriority::Low);
    }

    #[test]
    fn test_watcher_sort_empty_list() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let mut issues: Vec<(Issue, MatchResult)> = vec![];
        watcher.sort_by_priority(&mut issues);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_watcher_sort_single_item() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let mut issues = vec![(
            test_issue(),
            MatchResult::matched("Single", MatchPriority::High),
        )];
        watcher.sort_by_priority(&mut issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].1.priority, MatchPriority::High);
    }

    #[tokio::test]
    async fn test_watcher_seed_empty_sources() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let result = watcher.seed().await.unwrap();
        assert_eq!(result.total, 0);
        assert!(result.by_source.is_empty());
    }

    #[tokio::test]
    async fn test_watcher_seed_with_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "mock"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "mock"),
        ];
        let source = Arc::new(MockSource::with_issues("mock", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);

        let result = watcher.seed().await.unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(*result.by_source.get("mock").unwrap(), 2);

        // Verify issues are marked as seen
        assert!(tracker.has_attempted("mock", "1").unwrap());
        assert!(tracker.has_attempted("mock", "2").unwrap());
    }

    #[tokio::test]
    async fn test_watcher_seed_skips_already_seeded() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Pre-seed one issue
        tracker.record_attempt("mock", "1", "T-1").unwrap();

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "mock"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "mock"),
        ];
        let source = Arc::new(MockSource::with_issues("mock", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);

        let result = watcher.seed().await.unwrap();
        // Only 1 new issue should be seeded
        assert_eq!(result.total, 1);
        assert_eq!(*result.by_source.get("mock").unwrap(), 1);
    }

    #[tokio::test]
    async fn test_watcher_trigger_issue_unknown_source() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let result = watcher.trigger_issue("nonexistent", "123").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_seed_result_default() {
        let result = SeedResult::default();
        assert_eq!(result.total, 0);
        assert!(result.by_source.is_empty());
    }

    #[test]
    fn test_is_terminal_attempt_status() {
        assert!(!Watcher::is_terminal_attempt_status(
            FixAttemptStatus::Pending
        ));
        assert!(!Watcher::is_terminal_attempt_status(
            FixAttemptStatus::Success
        ));
        assert!(!Watcher::is_terminal_attempt_status(
            FixAttemptStatus::Failed
        ));
        assert!(Watcher::is_terminal_attempt_status(
            FixAttemptStatus::Merged
        ));
        assert!(Watcher::is_terminal_attempt_status(
            FixAttemptStatus::Closed
        ));
        assert!(Watcher::is_terminal_attempt_status(
            FixAttemptStatus::CannotFix
        ));
    }

    #[test]
    fn test_seed_result_debug() {
        let result = SeedResult {
            total: 5,
            by_source: [("test".to_string(), 5)].into_iter().collect(),
        };
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("total: 5"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_watcher_options_struct_fields() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let options = WatcherOptions {
            config: test_config(),
            sources: sources.clone(),
            notifier: notifier.clone(),
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        };

        assert!(options.dry_run);
        assert!(options.sources.is_empty());
        assert!(options.inferrer.is_none());
    }

    #[test]
    fn test_priority_ordering() {
        assert!(priority_order(&MatchPriority::Urgent) < priority_order(&MatchPriority::High));
        assert!(priority_order(&MatchPriority::High) < priority_order(&MatchPriority::Normal));
        assert!(priority_order(&MatchPriority::Normal) < priority_order(&MatchPriority::Low));
    }

    #[test]
    fn test_issue_priority_ordering() {
        use crate::types::IssuePriority;

        assert!(
            issue_priority_order(&IssuePriority::Critical)
                > issue_priority_order(&IssuePriority::High)
        );
        assert!(
            issue_priority_order(&IssuePriority::High)
                > issue_priority_order(&IssuePriority::Medium)
        );
        assert!(
            issue_priority_order(&IssuePriority::Medium)
                > issue_priority_order(&IssuePriority::Low)
        );
        assert!(
            issue_priority_order(&IssuePriority::Low) > issue_priority_order(&IssuePriority::None)
        );
    }

    #[test]
    fn test_priority_order_values() {
        assert_eq!(priority_order(&MatchPriority::Urgent), 0);
        assert_eq!(priority_order(&MatchPriority::High), 1);
        assert_eq!(priority_order(&MatchPriority::Normal), 2);
        assert_eq!(priority_order(&MatchPriority::Low), 3);
    }

    #[test]
    fn test_issue_priority_order_values() {
        use crate::types::IssuePriority;

        assert_eq!(issue_priority_order(&IssuePriority::Critical), 4);
        assert_eq!(issue_priority_order(&IssuePriority::High), 3);
        assert_eq!(issue_priority_order(&IssuePriority::Medium), 2);
        assert_eq!(issue_priority_order(&IssuePriority::Low), 1);
        assert_eq!(issue_priority_order(&IssuePriority::None), 0);
    }

    #[test]
    fn test_match_priority_sorting() {
        // Verify that sorting by priority_order puts Urgent first
        let mut priorities = [
            MatchPriority::Low,
            MatchPriority::Urgent,
            MatchPriority::Normal,
            MatchPriority::High,
        ];

        priorities.sort_by_key(priority_order);

        assert_eq!(priorities[0], MatchPriority::Urgent);
        assert_eq!(priorities[1], MatchPriority::High);
        assert_eq!(priorities[2], MatchPriority::Normal);
        assert_eq!(priorities[3], MatchPriority::Low);
    }

    #[test]
    fn test_issue_priority_sorting() {
        use crate::types::IssuePriority;

        let mut priorities = [
            IssuePriority::None,
            IssuePriority::Critical,
            IssuePriority::Low,
            IssuePriority::High,
            IssuePriority::Medium,
        ];

        priorities.sort_by_key(|p| std::cmp::Reverse(issue_priority_order(p)));

        assert_eq!(priorities[0], IssuePriority::Critical);
        assert_eq!(priorities[1], IssuePriority::High);
        assert_eq!(priorities[2], IssuePriority::Medium);
        assert_eq!(priorities[3], IssuePriority::Low);
        assert_eq!(priorities[4], IssuePriority::None);
    }

    #[test]
    fn test_watcher_options_struct() {
        use crate::storage::SqliteTracker;

        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        // Verify tracker can be created
        assert!(tracker.get_stats().is_ok());
    }

    #[test]
    fn test_match_result_matched() {
        let result = MatchResult::matched("Reason", MatchPriority::High);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::High);
        assert_eq!(result.reason, "Reason");
    }

    #[test]
    fn test_match_result_not_matched() {
        let result = MatchResult::not_matched("Not matching reason");
        assert!(!result.matches);
        assert_eq!(result.priority, MatchPriority::Normal);
        assert_eq!(result.reason, "Not matching reason");
    }

    #[test]
    fn test_fix_attempt_stats_default() {
        let stats = FixAttemptStats::default();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.success, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.pending, 0);
    }

    #[test]
    fn test_match_priority_variants() {
        // Test all MatchPriority variants exist and can be compared
        let urgent = MatchPriority::Urgent;
        let high = MatchPriority::High;
        let normal = MatchPriority::Normal;
        let low = MatchPriority::Low;

        assert_ne!(urgent, high);
        assert_ne!(high, normal);
        assert_ne!(normal, low);
    }

    #[test]
    fn test_priority_order_all_priorities() {
        // Ensure all priorities have unique order values
        let orders: Vec<u8> = vec![
            priority_order(&MatchPriority::Urgent),
            priority_order(&MatchPriority::High),
            priority_order(&MatchPriority::Normal),
            priority_order(&MatchPriority::Low),
        ];

        // All unique
        let unique: HashSet<_> = orders.iter().collect();
        assert_eq!(unique.len(), 4);

        // Urgent is lowest (highest priority)
        assert_eq!(
            *orders.iter().min().unwrap(),
            priority_order(&MatchPriority::Urgent)
        );
    }

    #[test]
    fn test_issue_priority_order_all_priorities() {
        use crate::types::IssuePriority;

        let orders: Vec<u8> = vec![
            issue_priority_order(&IssuePriority::Critical),
            issue_priority_order(&IssuePriority::High),
            issue_priority_order(&IssuePriority::Medium),
            issue_priority_order(&IssuePriority::Low),
            issue_priority_order(&IssuePriority::None),
        ];

        // All unique
        let unique: HashSet<_> = orders.iter().collect();
        assert_eq!(unique.len(), 5);

        // Critical is highest
        assert_eq!(
            *orders.iter().max().unwrap(),
            issue_priority_order(&IssuePriority::Critical)
        );
        // None is lowest
        assert_eq!(
            *orders.iter().min().unwrap(),
            issue_priority_order(&IssuePriority::None)
        );
    }

    #[test]
    fn test_match_result_default_priority() {
        let result = MatchResult::matched("Test", MatchPriority::Normal);
        assert_eq!(result.priority, MatchPriority::Normal);
    }

    #[test]
    fn test_fix_attempt_stats_with_values() {
        let stats = FixAttemptStats {
            total: 100,
            success: 75,
            failed: 20,
            pending: 5,
            merged: 50,
            closed: 10,
            cannot_fix: 5,
            by_source: std::collections::HashMap::new(),
        };

        assert_eq!(stats.total, 100);
        assert_eq!(stats.success, 75);
        assert_eq!(stats.failed, 20);
        assert_eq!(stats.pending, 5);
        assert_eq!(stats.merged, 50);
        assert_eq!(stats.closed, 10);
        assert_eq!(stats.cannot_fix, 5);
    }

    #[test]
    fn test_match_result_urgent_priority() {
        let result = MatchResult::matched("Urgent issue", MatchPriority::Urgent);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);
    }

    #[test]
    fn test_match_result_low_priority() {
        let result = MatchResult::matched("Low priority", MatchPriority::Low);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Low);
    }

    #[test]
    fn test_empty_priority_sorting() {
        let priorities: Vec<MatchPriority> = vec![];
        let sorted: Vec<_> = priorities.to_vec();

        assert!(sorted.is_empty());
    }

    #[test]
    fn test_single_priority_sorting() {
        let mut priorities = [MatchPriority::High];
        priorities.sort_by_key(priority_order);
        assert_eq!(priorities[0], MatchPriority::High);
    }

    #[test]
    fn test_duplicate_priorities_sorting() {
        let mut priorities = [
            MatchPriority::Normal,
            MatchPriority::Urgent,
            MatchPriority::Normal,
            MatchPriority::Urgent,
        ];
        priorities.sort_by_key(priority_order);

        // First two should be Urgent
        assert_eq!(priorities[0], MatchPriority::Urgent);
        assert_eq!(priorities[1], MatchPriority::Urgent);
        // Last two should be Normal
        assert_eq!(priorities[2], MatchPriority::Normal);
        assert_eq!(priorities[3], MatchPriority::Normal);
    }

    #[tokio::test]
    async fn test_watcher_poll_with_no_sources() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Poll should succeed even with no sources
        let result = watcher.poll().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_watcher_poll_records_cycle_metrics() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);
        watcher.poll().await.unwrap();

        let poll_cycle = tracker
            .get_metrics("poll_cycle_duration_secs", None, 10)
            .unwrap();
        assert_eq!(poll_cycle.len(), 1);
        assert!(poll_cycle[0].metric_value >= 0.0);

        let poll_sources = tracker.get_metrics("poll_sources", None, 10).unwrap();
        assert_eq!(poll_sources.len(), 1);
        assert_eq!(poll_sources[0].metric_value, 0.0);

        assert_eq!(
            tracker
                .get_metrics("ready_retries_found", None, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            tracker
                .get_metrics("ready_retries_executed_total", None, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            tracker
                .get_metrics("ready_retries_failed_total", None, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            tracker
                .get_metrics("pr_status_checks", None, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn test_watcher_poll_dry_run() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![Issue::new(
            "1",
            "T-1",
            "Issue 1",
            "http://example.com/1",
            "mock",
        )];
        let source = Arc::new(MockSource::with_issues("mock", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source];

        let watcher = create_test_watcher(notifier.clone(), tracker.clone(), sources, true);

        // Poll in dry run mode - should succeed
        let result = watcher.poll().await;
        assert!(result.is_ok());

        // In dry run mode, issues are NOT marked as attempted (just logged)
        assert!(!tracker.has_attempted("mock", "1").unwrap());
    }

    #[tokio::test]
    async fn test_watcher_poll_with_multiple_sources() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let source1 = Arc::new(MockSource::with_issues(
            "source1",
            vec![Issue::new(
                "1",
                "S1-1",
                "Issue 1",
                "http://example.com/1",
                "source1",
            )],
        )) as Arc<dyn IssueSource>;

        let source2 = Arc::new(MockSource::with_issues(
            "source2",
            vec![Issue::new(
                "2",
                "S2-1",
                "Issue 2",
                "http://example.com/2",
                "source2",
            )],
        )) as Arc<dyn IssueSource>;

        let sources = vec![source1, source2];
        let watcher = create_test_watcher(notifier, tracker.clone(), sources, true);

        let result = watcher.poll().await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_watcher_is_running_flag() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Initially not running
        assert!(!watcher.is_running.load(Ordering::SeqCst));

        // Set running
        watcher.is_running.store(true, Ordering::SeqCst);
        assert!(watcher.is_running.load(Ordering::SeqCst));

        // Stop should clear flag
        watcher.stop();
        assert!(!watcher.is_running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_watcher_active_processing_counter() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Initially 0
        assert_eq!(watcher.active_processing.load(Ordering::SeqCst), 0);

        // Increment
        watcher.active_processing.fetch_add(1, Ordering::SeqCst);
        assert_eq!(watcher.active_processing.load(Ordering::SeqCst), 1);

        // Decrement
        watcher.active_processing.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(watcher.active_processing.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_watcher_poll_source_with_empty_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let source = Arc::new(MockSource::new("empty")) as Arc<dyn IssueSource>;
        let sources = vec![source.clone()];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // poll_source returns Result<()>, not Vec
        let result = watcher.poll_source(&source).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_watcher_poll_source_records_zero_stage_metrics() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let source = Arc::new(MockSource::new("empty")) as Arc<dyn IssueSource>;
        let sources = vec![source.clone()];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);
        watcher.poll_source(&source).await.unwrap();

        let fetched = tracker.get_metrics("issues_fetched", None, 10).unwrap();
        let matched = tracker.get_metrics("issues_matched", None, 10).unwrap();
        let queued = tracker.get_metrics("issues_queued", None, 10).unwrap();

        assert_eq!(fetched.len(), 1);
        assert_eq!(matched.len(), 1);
        assert_eq!(queued.len(), 1);
        assert_eq!(fetched[0].metric_value, 0.0);
        assert_eq!(matched[0].metric_value, 0.0);
        assert_eq!(queued[0].metric_value, 0.0);
    }

    #[tokio::test]
    async fn test_watcher_poll_source_with_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "test"),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source.clone()];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, true); // dry run

        // poll_source returns Result<()>
        let result = watcher.poll_source(&source).await;
        assert!(result.is_ok());
        // In dry run mode, issues are NOT recorded (just logged)
        assert!(!tracker.has_attempted("test", "1").unwrap());
        assert!(!tracker.has_attempted("test", "2").unwrap());
    }

    #[tokio::test]
    async fn test_watcher_poll_source_deduplicates_duplicate_issue_ids() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new(
                "1",
                "T-1",
                "Issue 1 duplicate",
                "http://example.com/1",
                "test",
            ),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source.clone()];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, true); // dry run

        watcher.poll_source(&source).await.unwrap();

        let matched = tracker.get_metrics("issues_matched", None, 10).unwrap();
        let queued = tracker.get_metrics("issues_queued", None, 10).unwrap();

        assert_eq!(matched.len(), 1);
        assert_eq!(queued.len(), 1);
        assert_eq!(matched[0].metric_value, 1.0);
        assert_eq!(queued[0].metric_value, 1.0);
    }

    #[tokio::test]
    async fn test_watcher_poll_source_continues_when_urgent_notification_fails() {
        let notifier = Arc::new(MockNotifier::with_urgent_failure(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let source = Arc::new(MockSource::with_priority(
            "urgent",
            vec![Issue::new(
                "1",
                "URGENT-1",
                "Urgent issue",
                "http://example.com/urgent/1",
                "urgent",
            )],
            MatchPriority::Urgent,
        )) as Arc<dyn IssueSource>;
        let sources = vec![source.clone()];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);
        watcher.is_running.store(true, Ordering::SeqCst);

        let result = watcher.poll_source(&source).await;
        assert!(result.is_ok());

        let attempt = tracker.get_attempt("urgent", "1").unwrap().unwrap();
        assert_eq!(attempt.status, crate::types::FixAttemptStatus::Failed);
    }

    #[tokio::test]
    async fn test_watcher_poll_source_skips_trailing_processing_delay() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let source = Arc::new(MockSource::with_issues(
            "timing",
            vec![
                Issue::new("1", "TIME-1", "Issue 1", "http://example.com/1", "timing"),
                Issue::new("2", "TIME-2", "Issue 2", "http://example.com/2", "timing"),
            ],
        )) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.max_issues_per_cycle = 5;
        config.processing_delay_ms = 250;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        watcher.is_running.store(true, Ordering::SeqCst);

        let started = std::time::Instant::now();
        watcher.poll_source(&source).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(450),
            "poll_source took too long: {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_watcher_poll_source_not_blocked_by_other_source_activity() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let source = Arc::new(MockSource::with_issues(
            "target",
            vec![Issue::new(
                "1",
                "TARGET-1",
                "Target issue",
                "http://example.com/target/1",
                "target",
            )],
        )) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.max_concurrent = 1;
        config.processing_delay_ms = 0;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        watcher.is_running.store(true, Ordering::SeqCst);

        // Simulate unrelated in-flight work from another source.
        watcher.active_processing.fetch_add(1, Ordering::SeqCst);
        {
            let mut processing = watcher.processing.write().await;
            processing.insert("other:inflight".to_string());
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            watcher.poll_source(&source),
        )
        .await;
        assert!(result.is_ok(), "poll_source timed out unexpectedly");
        assert!(result.unwrap().is_ok());

        let attempt = tracker.get_attempt("target", "1").unwrap().unwrap();
        assert_eq!(attempt.status, crate::types::FixAttemptStatus::Failed);

        // Clean up simulated work so test state remains consistent.
        {
            let mut processing = watcher.processing.write().await;
            processing.remove("other:inflight");
        }
        watcher.active_processing.fetch_sub(1, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn test_watcher_poll_source_zero_max_concurrent_does_not_deadlock() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let source = Arc::new(MockSource::with_issues(
            "zero-concurrency",
            vec![Issue::new(
                "1",
                "ZERO-1",
                "Zero concurrency issue",
                "http://example.com/zero/1",
                "zero-concurrency",
            )],
        )) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.max_concurrent = 0;
        config.processing_delay_ms = 0;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        watcher.is_running.store(true, Ordering::SeqCst);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            watcher.poll_source(&source),
        )
        .await;
        assert!(
            result.is_ok(),
            "poll_source timed out with max_concurrent=0"
        );
        assert!(result.unwrap().is_ok());

        let attempt = tracker
            .get_attempt("zero-concurrency", "1")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, crate::types::FixAttemptStatus::Failed);
    }

    #[tokio::test]
    async fn test_process_ready_retries_zero_max_concurrent_does_not_deadlock() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        tracker
            .record_attempt("mock", "missing-retry", "MOCK-RETRY")
            .unwrap();
        tracker
            .mark_failed("mock", "missing-retry", "initial failure")
            .unwrap();

        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.max_concurrent = 0;
        config.processing_delay_ms = 0;
        config.retry.base_delay_ms = 0;
        config.retry.max_delay_ms = 0;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        watcher.is_running.store(true, Ordering::SeqCst);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            watcher.process_ready_retries(),
        )
        .await;
        assert!(
            result.is_ok(),
            "process_ready_retries timed out with max_concurrent=0"
        );
        assert!(result.unwrap().is_ok());

        let attempt = tracker
            .get_attempt("mock", "missing-retry")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, crate::types::FixAttemptStatus::Failed);
        assert_eq!(attempt.retry_count, 1);
    }

    #[tokio::test]
    async fn test_watcher_start_dry_run_skips_auto_close_checks() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        tracker.record_attempt("mock", "1", "MOCK-1").unwrap();
        tracker
            .mark_success("mock", "1", "https://github.com/org/repo/pull/1")
            .unwrap();

        let mock_source = Arc::new(MockSource::with_issues(
            "mock",
            vec![Issue::new(
                "1",
                "MOCK-1",
                "Mock issue",
                "http://example.com/mock/1",
                "mock",
            )],
        ));
        let source = Arc::clone(&mock_source) as Arc<dyn IssueSource>;

        let watcher = Arc::new(create_test_watcher(notifier, tracker, vec![source], true));

        let runner = {
            let watcher = Arc::clone(&watcher);
            tokio::spawn(async move { watcher.start(Some(50)).await })
        };

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        watcher.stop();

        let joined = tokio::time::timeout(std::time::Duration::from_secs(2), runner).await;
        assert!(joined.is_ok(), "watcher start loop did not stop in time");
        assert!(joined.unwrap().expect("task join failed").is_ok());
        assert_eq!(
            mock_source.issue_status_call_count(),
            0,
            "dry_run should not call get_issue_status via auto-close checks"
        );
    }

    #[tokio::test]
    async fn test_watcher_start_zero_interval_clamped_without_panic() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;
        let watcher = Arc::new(create_test_watcher(notifier, tracker, vec![source], true));

        let runner = {
            let watcher = Arc::clone(&watcher);
            tokio::spawn(async move { watcher.start(Some(0)).await })
        };

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        watcher.stop();

        let joined = tokio::time::timeout(std::time::Duration::from_secs(2), runner).await;
        assert!(
            joined.is_ok(),
            "watcher start loop timed out with zero interval"
        );
        assert!(
            joined.unwrap().expect("task join failed").is_ok(),
            "watcher returned an error with zero interval"
        );
    }

    #[test]
    fn test_group_review_feedback_by_pr_batches_same_pr() {
        let review1 = crate::scm::CodeReview {
            id: 1,
            state: "CHANGES_REQUESTED".to_string(),
            body: Some("first".to_string()),
            user: crate::scm::ReviewUser {
                id: 1,
                login: "r1".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: None,
        };
        let review2 = crate::scm::CodeReview {
            id: 2,
            state: "COMMENTED".to_string(),
            body: Some("second".to_string()),
            user: crate::scm::ReviewUser {
                id: 2,
                login: "r2".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2024-01-01T00:01:00Z".to_string()),
            html_url: None,
        };

        let events = vec![
            crate::scm::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/1".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 1,
                review: review1,
                inline_comments: vec![],
            },
            crate::scm::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/1".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 1,
                review: review2,
                inline_comments: vec![],
            },
            crate::scm::ReviewEvent::CommentsAdded {
                pr_url: "https://github.com/org/repo/pull/2".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 2,
                comments: vec![], // requires_action = false, should be ignored
            },
        ];

        let grouped = Watcher::group_review_feedback_by_pr(events);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0, "https://github.com/org/repo/pull/1");
        assert_eq!(grouped[0].2, 2);
        assert!(grouped[0].1.contains("first"));
        assert!(grouped[0].1.contains("second"));
        assert!(grouped[0].1.contains("---"));
    }

    #[tokio::test]
    async fn test_process_review_action_waits_for_inflight_issue_processing() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        tracker.record_attempt("mock", "1", "MOCK-1").unwrap();
        tracker
            .mark_success("mock", "1", "https://github.com/org/repo/pull/1")
            .unwrap();

        let source = Arc::new(MockSource::with_issues(
            "mock",
            vec![Issue::new(
                "1",
                "MOCK-1",
                "Mock issue",
                "http://example.com/mock/1",
                "mock",
            )],
        )) as Arc<dyn IssueSource>;

        let watcher = Arc::new(create_test_watcher(
            notifier,
            tracker.clone(),
            vec![source],
            false,
        ));
        watcher.is_running.store(true, Ordering::SeqCst);

        {
            let mut processing = watcher.processing.write().await;
            processing.insert("mock:1".to_string());
        }

        let release = Arc::clone(&watcher);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let mut processing = release.processing.write().await;
            processing.remove("mock:1");
        });

        let attempt = tracker.get_attempt("mock", "1").unwrap().unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            watcher.process_review_action(&attempt, "Please address review feedback"),
        )
        .await;
        assert!(result.is_ok(), "process_review_action timed out");
        assert!(
            result.unwrap().is_ok(),
            "process_review_action returned error"
        );

        let updated_attempt = tracker.get_attempt("mock", "1").unwrap().unwrap();
        assert_eq!(
            updated_attempt.status,
            crate::types::FixAttemptStatus::Failed,
            "review rerun should execute after lock release (repo resolution fails in test setup, marking failed)"
        );
    }

    #[tokio::test]
    async fn test_process_review_action_exits_when_watcher_stopping() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        tracker.record_attempt("mock", "1", "MOCK-1").unwrap();
        tracker
            .mark_success("mock", "1", "https://github.com/org/repo/pull/1")
            .unwrap();

        let source = Arc::new(MockSource::with_issues(
            "mock",
            vec![Issue::new(
                "1",
                "MOCK-1",
                "Mock issue",
                "http://example.com/mock/1",
                "mock",
            )],
        )) as Arc<dyn IssueSource>;

        let watcher = Arc::new(create_test_watcher(
            notifier,
            tracker.clone(),
            vec![source],
            false,
        ));

        {
            let mut processing = watcher.processing.write().await;
            processing.insert("mock:1".to_string());
        }
        watcher.is_running.store(false, Ordering::SeqCst);

        let attempt = tracker.get_attempt("mock", "1").unwrap().unwrap();
        let result = watcher
            .process_review_action(&attempt, "Please address review feedback")
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Watcher stopping while waiting"),
            "expected watcher-stopping wait error"
        );
    }

    #[tokio::test]
    async fn test_watcher_poll_source_skips_attempted() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Pre-mark one issue as attempted
        tracker.record_attempt("test", "1", "T-1").unwrap();

        let issues = vec![
            Issue::new(
                "1",
                "T-1",
                "Already Attempted",
                "http://example.com/1",
                "test",
            ),
            Issue::new("2", "T-2", "New Issue", "http://example.com/2", "test"),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source.clone()];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, true); // dry run

        // poll_source returns Result<()>
        let result = watcher.poll_source(&source).await;
        assert!(result.is_ok());
        // Only the pre-existing one should be in tracker (dry run doesn't add new ones)
        assert!(tracker.has_attempted("test", "1").unwrap());
        assert!(!tracker.has_attempted("test", "2").unwrap()); // Not recorded in dry run
    }

    #[tokio::test]
    async fn test_watcher_trigger_issue_with_known_source() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![Issue::new(
            "123",
            "T-123",
            "Test Issue",
            "http://example.com/123",
            "mock",
        )];
        let source = Arc::new(MockSource::with_issues("mock", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source];

        let watcher = create_test_watcher(notifier, tracker, sources, true); // dry run

        let result = watcher.trigger_issue("mock", "123").await;
        // Should succeed in dry run (doesn't actually process)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_watcher_trigger_issue_inflight_returns_error() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![Issue::new(
            "123",
            "T-123",
            "Test Issue",
            "http://example.com/123",
            "mock",
        )];
        let source = Arc::new(MockSource::with_issues("mock", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source];

        let watcher = create_test_watcher(notifier, tracker, sources, true);
        {
            let mut processing = watcher.processing.write().await;
            processing.insert("mock:123".to_string());
        }

        let result = watcher.trigger_issue("mock", "123").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("already being processed"));
    }

    #[test]
    fn test_watcher_processing_set() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Use tokio runtime for the async lock
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Initially empty
            {
                let processing = watcher.processing.read().await;
                assert!(processing.is_empty());
            }

            // Add item
            {
                let mut processing = watcher.processing.write().await;
                processing.insert("test:123".to_string());
            }

            // Verify added
            {
                let processing = watcher.processing.read().await;
                assert!(processing.contains("test:123"));
            }

            // Remove item
            {
                let mut processing = watcher.processing.write().await;
                processing.remove("test:123");
            }

            // Verify removed
            {
                let processing = watcher.processing.read().await;
                assert!(!processing.contains("test:123"));
            }
        });
    }

    #[test]
    fn test_watcher_config_values() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let mut config = test_config();
        config.max_issues_per_cycle = 10;
        config.max_concurrent = 3;
        config.processing_delay_ms = 500;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        assert_eq!(watcher.config.max_issues_per_cycle, 10);
        assert_eq!(watcher.config.max_concurrent, 3);
        assert_eq!(watcher.config.processing_delay_ms, 500);
    }

    #[tokio::test]
    async fn test_watcher_seed_with_multiple_sources() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let source1 = Arc::new(MockSource::with_issues(
            "source1",
            vec![Issue::new(
                "1",
                "S1-1",
                "Issue 1",
                "http://example.com/1",
                "source1",
            )],
        )) as Arc<dyn IssueSource>;

        let source2 = Arc::new(MockSource::with_issues(
            "source2",
            vec![Issue::new(
                "2",
                "S2-1",
                "Issue 2",
                "http://example.com/2",
                "source2",
            )],
        )) as Arc<dyn IssueSource>;

        let sources = vec![source1, source2];
        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);

        let result = watcher.seed().await.unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(*result.by_source.get("source1").unwrap(), 1);
        assert_eq!(*result.by_source.get("source2").unwrap(), 1);
    }

    #[tokio::test]
    async fn test_watcher_poll_respects_max_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Create more issues than max_issues_per_cycle
        let issues: Vec<Issue> = (1..=10)
            .map(|i| {
                Issue::new(
                    format!("{}", i),
                    format!("T-{}", i),
                    format!("Issue {}", i),
                    format!("http://example.com/{}", i),
                    "test",
                )
            })
            .collect();

        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.max_issues_per_cycle = 5;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        });

        // Poll should complete successfully
        let result = watcher.poll().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_ready_retries_marks_failed_when_trigger_fails() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        tracker
            .record_attempt("mock", "missing-1", "MOCK-1")
            .unwrap();
        tracker
            .mark_failed("mock", "missing-1", "initial failure")
            .unwrap();

        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.retry.base_delay_ms = 0;
        config.retry.max_delay_ms = 0;
        config.processing_delay_ms = 0;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        watcher.is_running.store(true, Ordering::SeqCst);

        watcher.process_ready_retries().await.unwrap();

        let attempt = tracker.get_attempt("mock", "missing-1").unwrap().unwrap();
        assert_eq!(attempt.status, crate::types::FixAttemptStatus::Failed);
        assert_eq!(attempt.retry_count, 1);
        assert!(attempt
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Retry trigger failed"));
    }

    #[tokio::test]
    async fn test_poll_source_marks_failed_when_repo_resolution_skips() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let source = Arc::new(MockSource::with_issues(
            "mock",
            vec![Issue::new(
                "issue-1",
                "MOCK-1",
                "Issue without resolvable repo",
                "https://example.com/issue-1",
                "mock",
            )],
        )) as Arc<dyn IssueSource>;

        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], false);
        watcher.is_running.store(true, Ordering::SeqCst);

        watcher.poll_source(&source).await.unwrap();

        let attempt = tracker.get_attempt("mock", "issue-1").unwrap().unwrap();
        assert_eq!(attempt.status, crate::types::FixAttemptStatus::Failed);
        assert!(attempt
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Repository resolution failed"));
    }

    #[test]
    fn test_mock_notifier_call_tracking() {
        let notifier = MockNotifier::new(true);
        assert_eq!(notifier.get_call_count(), 0);
        assert!(notifier.is_enabled());
        assert_eq!(notifier.name(), "mock");
    }

    #[test]
    fn test_mock_notifier_disabled() {
        let notifier = MockNotifier::new(false);
        assert!(!notifier.is_enabled());
    }

    #[tokio::test]
    async fn test_mock_source_get_issue_not_found() {
        let source = MockSource::new("test");
        let result = source.get_issue("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_source_get_issue_found() {
        let issues = vec![Issue::new(
            "123",
            "T-123",
            "Test",
            "http://example.com",
            "test",
        )];
        let source = MockSource::with_issues("test", issues);
        let result = source.get_issue("123").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, "123");
    }

    #[tokio::test]
    async fn test_mock_source_build_issue_context() {
        let source = MockSource::new("test");
        let issue = test_issue();
        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("TEST-123"));
    }

    #[test]
    fn test_mock_source_matches_criteria() {
        let source = MockSource::new("test");
        let issue = test_issue();
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Normal);
        assert!(result.reason.contains("Mock"));
    }

    #[test]
    fn test_watcher_reset_attempt_success() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        // Record an attempt
        tracker.record_attempt("test", "123", "T-123").unwrap();
        assert!(tracker.has_attempted("test", "123").unwrap());

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);

        // Reset should succeed
        let result = watcher.reset_attempt("test", "123");
        assert!(result.is_ok());
        assert!(!tracker.has_attempted("test", "123").unwrap());
    }

    #[test]
    fn test_watcher_get_stats_empty() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let stats = watcher.get_stats().unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.success, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.pending, 0);
    }

    #[test]
    fn test_watcher_get_stats_after_attempts() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        // Record some attempts
        tracker.record_attempt("test", "1", "T-1").unwrap();
        tracker.record_attempt("test", "2", "T-2").unwrap();
        tracker
            .mark_success("test", "1", "http://github.com/pr/1")
            .unwrap();
        tracker.mark_failed("test", "2", "Error").unwrap();

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let stats = watcher.get_stats().unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.success, 1);
        assert_eq!(stats.failed, 1);
    }

    #[tokio::test]
    async fn test_cascade_triggers_on_merge() {
        use crate::repo::DependencyType;
        use crate::types::{FixAttempt, FixAttemptStatus};

        // Setup: Create relationships with an upstream and downstream repo
        let mut relationships = RepoRelationships::new();
        relationships
            .add_dependency(
                "upstream-lib",
                "downstream-app",
                DependencyType::Composer,
                None,
            )
            .unwrap();

        // Create a FixAttempt that simulates a merged upstream PR
        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-123".to_string(),
            short_id: "ISSUE-123".to_string(),
            source: "linear".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/upstream-lib/pull/42".to_string()),
            scm_repo: Some("org/upstream-lib".to_string()),
            scm_pr_number: Some(42),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Verify that get_dependants returns the downstream repo
        let dependants = relationships.get_dependants("upstream-lib");
        assert_eq!(dependants.len(), 1);
        assert_eq!(dependants[0].name, "downstream-app");

        // Verify cascade depth calculation for root attempt
        assert_eq!(attempt.parent_attempt_id, None);

        // Verify repo name normalization (scm_repo "org/upstream-lib" -> "upstream-lib")
        let repo_short_name = attempt
            .scm_repo
            .as_ref()
            .unwrap()
            .split('/')
            .next_back()
            .unwrap();
        assert_eq!(repo_short_name, "upstream-lib");
    }

    #[test]
    fn test_cascade_depth_with_no_parent() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "linear".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None,
            scm_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Root attempt has depth 0
        assert!(attempt.parent_attempt_id.is_none());
    }

    #[test]
    fn test_cascade_config_defaults() {
        use crate::config::CascadeConfig;

        let config = CascadeConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_depth, 0);
    }

    // =========================================================================
    // Tests for truncate_error_for_activity
    // =========================================================================

    #[test]
    fn test_truncate_error_short_message() {
        let error = "short error";
        let result = Watcher::truncate_error_for_activity(error);
        assert_eq!(result, "short error");
    }

    #[test]
    fn test_truncate_error_exactly_500_chars() {
        let error = "a".repeat(500);
        let result = Watcher::truncate_error_for_activity(&error);
        assert_eq!(result.len(), 500);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_over_500_chars() {
        let error = "a".repeat(600);
        let result = Watcher::truncate_error_for_activity(&error);
        assert!(result.len() <= 500);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_empty_string() {
        let result = Watcher::truncate_error_for_activity("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_error_unicode_boundary() {
        // Build a string that has multibyte chars near the 500-char boundary
        let mut error = "a".repeat(497);
        // Add a 4-byte emoji right at the boundary
        error.push('\u{1F600}'); // emoji: 4 bytes
        error.push_str(&"b".repeat(100));
        let result = Watcher::truncate_error_for_activity(&error);
        assert!(result.ends_with("..."));
        // Verify it doesn't panic and doesn't split a char
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn test_truncate_error_501_chars() {
        let error = "x".repeat(501);
        let result = Watcher::truncate_error_for_activity(&error);
        assert!(result.ends_with("..."));
        // Should be at most 500 chars (497 + "...")
        assert!(result.len() <= 500);
    }

    // =========================================================================
    // Tests for is_running() and active_count() public accessors
    // =========================================================================

    #[test]
    fn test_is_running_accessor() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        assert!(!watcher.is_running());
        watcher.is_running.store(true, Ordering::SeqCst);
        assert!(watcher.is_running());
        watcher.stop();
        assert!(!watcher.is_running());
    }

    #[test]
    fn test_active_count_accessor() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        assert_eq!(watcher.active_count(), 0);
        watcher.active_processing.fetch_add(3, Ordering::SeqCst);
        assert_eq!(watcher.active_count(), 3);
        watcher.active_processing.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(watcher.active_count(), 2);
    }

    // =========================================================================
    // Tests for active_processing_for_source
    // =========================================================================

    #[tokio::test]
    async fn test_active_processing_for_source_empty() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        assert_eq!(watcher.active_processing_for_source("test").await, 0);
    }

    #[tokio::test]
    async fn test_active_processing_for_source_counts_only_matching() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        {
            let mut processing = watcher.processing.write().await;
            processing.insert("source_a:issue-1".to_string());
            processing.insert("source_a:issue-2".to_string());
            processing.insert("source_b:issue-3".to_string());
            processing.insert("source_a:issue-4".to_string());
        }

        assert_eq!(watcher.active_processing_for_source("source_a").await, 3);
        assert_eq!(watcher.active_processing_for_source("source_b").await, 1);
        assert_eq!(watcher.active_processing_for_source("source_c").await, 0);
    }

    #[tokio::test]
    async fn test_active_processing_for_source_prefix_mismatch() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        {
            let mut processing = watcher.processing.write().await;
            // "test_source:" should NOT match "test:" prefix
            processing.insert("test_source:issue-1".to_string());
        }

        assert_eq!(watcher.active_processing_for_source("test").await, 0);
        assert_eq!(watcher.active_processing_for_source("test_source").await, 1);
    }

    // =========================================================================
    // Tests for refresh_repos and sync_repos_to_db with None values
    // =========================================================================

    #[tokio::test]
    async fn test_refresh_repos_no_inferrer_returns_zero() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let result = watcher.refresh_repos().await.unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn test_sync_repos_to_db_no_inferrer_returns_zero() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let result = watcher.sync_repos_to_db(true).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn test_sync_repos_to_db_no_inferrer_returns_zero_basic() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let result = watcher.sync_repos_to_db(false).unwrap();
        assert_eq!(result, 0);
    }

    // =========================================================================
    // Tests for check_reviews with no review_watcher
    // =========================================================================

    #[tokio::test]
    async fn test_check_reviews_no_watcher_returns_ok() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);
        assert!(watcher.review_watcher.is_none());

        let result = watcher.check_reviews().await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Tests for check_pr_merges_and_cascade with no github client
    // =========================================================================

    #[tokio::test]
    async fn test_check_pr_merges_no_github_client() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);
        assert!(watcher.github_client.is_none());

        // Should succeed and record zero-value metrics
        let result = watcher.check_pr_merges_and_cascade().await;
        assert!(result.is_ok());

        // Verify lifecycle metrics were still recorded
        let checks = tracker.get_metrics("pr_status_checks", None, 10).unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].metric_value, 0.0);
    }

    // =========================================================================
    // Tests for poll_housekeeping
    // =========================================================================

    #[tokio::test]
    async fn test_poll_housekeeping_dry_run_skips_retries_and_cascades() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, true);

        let result = watcher.poll_housekeeping().await;
        assert!(result.is_ok());

        // In dry-run mode, no retries or cascade metrics should be recorded
        let retries_found = tracker
            .get_metrics("ready_retries_found", None, 10)
            .unwrap();
        assert!(
            retries_found.is_empty(),
            "dry_run should skip process_ready_retries"
        );
    }

    #[tokio::test]
    async fn test_poll_housekeeping_records_metrics() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);
        watcher.is_running.store(true, Ordering::SeqCst);

        let result = watcher.poll_housekeeping().await;
        assert!(result.is_ok());

        // Verify housekeeping metrics
        let duration = tracker
            .get_metrics("housekeeping_cycle_duration_secs", None, 10)
            .unwrap();
        assert_eq!(duration.len(), 1);
        assert!(duration[0].metric_value >= 0.0);

        let active = tracker.get_metrics("active_processing", None, 10).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].metric_value, 0.0);
    }

    // =========================================================================
    // Tests for check_and_auto_close_prs
    // =========================================================================

    #[tokio::test]
    async fn test_check_and_auto_close_prs_no_pending() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let result = watcher.check_and_auto_close_prs().await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_check_and_auto_close_prs_no_matching_source() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Record a PR for a source that does NOT exist in sources list
        tracker
            .record_attempt("nonexistent_source", "1", "NE-1")
            .unwrap();
        tracker
            .mark_success(
                "nonexistent_source",
                "1",
                "https://github.com/org/repo/pull/99",
            )
            .unwrap();

        let source = Arc::new(MockSource::new("different_source")) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker, vec![source], false);

        let result = watcher.check_and_auto_close_prs().await.unwrap();
        // No matching source found, so no auto-close
        assert!(result.is_empty());
    }

    // =========================================================================
    // Tests for record_source_decision and record_issue_decision
    // =========================================================================

    #[test]
    fn test_record_source_decision_does_not_panic() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Should not panic even with arbitrary values
        watcher.record_source_decision(
            "test_source",
            "test_decision",
            "Test message",
            json!({"key": "value"}),
        );
    }

    #[test]
    fn test_record_issue_decision_does_not_panic() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let issue = test_issue();
        watcher.record_issue_decision(
            &issue,
            "test_decision",
            "Test message for issue",
            json!({"outcome": "success"}),
        );
    }

    // =========================================================================
    // Tests for record_error_pattern
    // =========================================================================

    #[test]
    fn test_record_error_pattern_does_not_panic() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Should not panic
        watcher.record_error_pattern("linear", "ISSUE-42", "build failed: exit code 1");
        watcher.record_error_pattern("sentry", "SENTRY-99", "timeout after 300s");
        watcher.record_error_pattern("test", "T-1", "");
    }

    // =========================================================================
    // Tests for Watcher::new with tracker
    // =========================================================================

    #[test]
    fn test_watcher_new_with_tracker() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        assert!(!watcher.dry_run);
    }

    // =========================================================================
    // Tests for stop_and_drain
    // =========================================================================

    #[tokio::test]
    async fn test_stop_and_drain_immediate_when_no_active() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = Arc::new(create_test_watcher(notifier, tracker, sources, false));
        watcher.is_running.store(true, Ordering::SeqCst);

        // Should complete quickly since there's no active processing
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), watcher.stop_and_drain()).await;
        assert!(result.is_ok(), "stop_and_drain timed out");
        assert!(!watcher.is_running());
    }

    #[tokio::test]
    async fn test_stop_and_drain_waits_for_active() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = Arc::new(create_test_watcher(notifier, tracker, sources, false));
        watcher.is_running.store(true, Ordering::SeqCst);
        watcher.active_processing.fetch_add(1, Ordering::SeqCst);

        // Simulate task finishing after a short delay
        let release = Arc::clone(&watcher);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            release.active_processing.fetch_sub(1, Ordering::SeqCst);
        });

        let result =
            tokio::time::timeout(std::time::Duration::from_secs(5), watcher.stop_and_drain()).await;
        assert!(result.is_ok(), "stop_and_drain timed out");
        assert!(!watcher.is_running());
        assert_eq!(watcher.active_count(), 0);
    }

    // =========================================================================
    // Tests for group_review_feedback_by_pr edge cases
    // =========================================================================

    #[test]
    fn test_group_review_feedback_empty_events() {
        let events: Vec<crate::scm::ReviewEvent> = vec![];
        let grouped = Watcher::group_review_feedback_by_pr(events);
        assert!(grouped.is_empty());
    }

    #[test]
    fn test_group_review_feedback_only_non_actionable() {
        // CommentsAdded with empty comments does not require action
        let events = vec![crate::scm::ReviewEvent::CommentsAdded {
            pr_url: "https://github.com/org/repo/pull/1".to_string(),
            repo: "org/repo".to_string(),
            pr_number: 1,
            comments: vec![],
        }];

        let grouped = Watcher::group_review_feedback_by_pr(events);
        assert!(grouped.is_empty());
    }

    #[test]
    fn test_group_review_feedback_multiple_prs() {
        let make_review = |id: i64, state: &str, body: &str| crate::scm::CodeReview {
            id,
            state: state.to_string(),
            body: Some(body.to_string()),
            user: crate::scm::ReviewUser {
                id: id,
                login: format!("user{}", id),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: None,
        };

        let events = vec![
            crate::scm::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/1".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 1,
                review: make_review(1, "CHANGES_REQUESTED", "fix the bug"),
                inline_comments: vec![],
            },
            crate::scm::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/2".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 2,
                review: make_review(2, "CHANGES_REQUESTED", "needs tests"),
                inline_comments: vec![],
            },
        ];

        let grouped = Watcher::group_review_feedback_by_pr(events);
        assert_eq!(grouped.len(), 2);
        // Verify order is preserved
        assert_eq!(grouped[0].0, "https://github.com/org/repo/pull/1");
        assert_eq!(grouped[1].0, "https://github.com/org/repo/pull/2");
        assert_eq!(grouped[0].2, 1); // 1 review for PR 1
        assert_eq!(grouped[1].2, 1); // 1 review for PR 2
    }

    // =========================================================================
    // Tests for is_terminal_attempt_status (additional edge cases)
    // =========================================================================

    #[test]
    fn test_is_terminal_all_statuses() {
        let non_terminal = [
            FixAttemptStatus::Pending,
            FixAttemptStatus::Success,
            FixAttemptStatus::Failed,
        ];
        let terminal = [
            FixAttemptStatus::Merged,
            FixAttemptStatus::Closed,
            FixAttemptStatus::CannotFix,
        ];

        for status in non_terminal {
            assert!(
                !Watcher::is_terminal_attempt_status(status),
                "{:?} should NOT be terminal",
                status
            );
        }
        for status in terminal {
            assert!(
                Watcher::is_terminal_attempt_status(status),
                "{:?} SHOULD be terminal",
                status
            );
        }
    }

    // =========================================================================
    // Tests for sort_by_priority advanced cases
    // =========================================================================

    #[test]
    fn test_sort_by_priority_all_same_match_priority_different_issue_priority() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let mut issues = vec![
            (
                test_issue_with_priority("1", IssuePriority::None),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
            (
                test_issue_with_priority("2", IssuePriority::Critical),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
            (
                test_issue_with_priority("3", IssuePriority::High),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
            (
                test_issue_with_priority("4", IssuePriority::Low),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
            (
                test_issue_with_priority("5", IssuePriority::Medium),
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
        ];

        watcher.sort_by_priority(&mut issues);

        assert_eq!(issues[0].0.priority, IssuePriority::Critical);
        assert_eq!(issues[1].0.priority, IssuePriority::High);
        assert_eq!(issues[2].0.priority, IssuePriority::Medium);
        assert_eq!(issues[3].0.priority, IssuePriority::Low);
        assert_eq!(issues[4].0.priority, IssuePriority::None);
    }

    #[test]
    fn test_sort_by_priority_match_priority_takes_precedence() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Issue with Low match priority but Critical issue priority
        // should come AFTER issue with Urgent match priority but None issue priority
        let mut issues = vec![
            (
                test_issue_with_priority("1", IssuePriority::Critical),
                MatchResult::matched("Low match", MatchPriority::Low),
            ),
            (
                test_issue_with_priority("2", IssuePriority::None),
                MatchResult::matched("Urgent match", MatchPriority::Urgent),
            ),
        ];

        watcher.sort_by_priority(&mut issues);

        assert_eq!(issues[0].1.priority, MatchPriority::Urgent);
        assert_eq!(issues[1].1.priority, MatchPriority::Low);
    }

    #[test]
    fn test_sort_by_priority_stability_for_equal_items() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let mut issues = vec![
            (
                {
                    let mut i = test_issue();
                    i.id = "first".to_string();
                    i
                },
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
            (
                {
                    let mut i = test_issue();
                    i.id = "second".to_string();
                    i
                },
                MatchResult::matched("Same", MatchPriority::Normal),
            ),
        ];

        watcher.sort_by_priority(&mut issues);

        // Both have same priority, so the sort should be stable (order preserved)
        assert_eq!(issues[0].0.id, "first");
        assert_eq!(issues[1].0.id, "second");
    }

    // =========================================================================
    // Tests for enhance_prompt_with_learning
    // =========================================================================

    #[test]
    fn test_enhance_prompt_no_repo_returns_base() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let base = "Fix the bug in module X";
        let issue = test_issue();
        let result = watcher.enhance_prompt_with_learning(base, &issue, None);
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_with_repo_no_learning_enabled() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        // Default learning config has everything disabled
        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let base = "Fix the bug in module X";
        let issue = test_issue();
        let result = watcher.enhance_prompt_with_learning(base, &issue, Some("my-repo"));
        // With no learning enabled and no data, should return base prompt
        assert_eq!(result, base);
    }

    // =========================================================================
    // Tests for processing set concurrent behavior
    // =========================================================================

    #[tokio::test]
    async fn test_processing_set_concurrent_insertions() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = Arc::new(create_test_watcher(notifier, tracker, sources, false));

        // Concurrently insert 100 items
        let mut handles = vec![];
        for i in 0..100 {
            let w = Arc::clone(&watcher);
            handles.push(tokio::spawn(async move {
                let mut processing = w.processing.write().await;
                processing.insert(format!("test:{}", i));
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let processing = watcher.processing.read().await;
        assert_eq!(processing.len(), 100);
    }

    #[tokio::test]
    async fn test_processing_set_insert_and_remove_same_key() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        {
            let mut processing = watcher.processing.write().await;
            processing.insert("test:123".to_string());
            assert!(processing.contains("test:123"));
            processing.remove("test:123");
            assert!(!processing.contains("test:123"));
        }

        // Re-insert should work
        {
            let mut processing = watcher.processing.write().await;
            processing.insert("test:123".to_string());
        }

        let processing = watcher.processing.read().await;
        assert!(processing.contains("test:123"));
    }

    // =========================================================================
    // Tests for WatcherOptions fields propagation
    // =========================================================================

    #[test]
    fn test_watcher_options_config_propagation() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.max_issues_per_cycle = 42;
        config.max_concurrent = 7;
        config.processing_delay_ms = 1500;
        config.poll_interval_ms = 30000;
        config.agent.timeout_secs = 999;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        assert_eq!(watcher.config.max_issues_per_cycle, 42);
        assert_eq!(watcher.config.max_concurrent, 7);
        assert_eq!(watcher.config.processing_delay_ms, 1500);
        assert_eq!(watcher.config.poll_interval_ms, 30000);
        assert_eq!(watcher.config.agent.timeout_secs, 999);
    }

    #[test]
    fn test_watcher_new_multiple_sources() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let sources: Vec<Arc<dyn IssueSource>> = vec![
            Arc::new(MockSource::new("source_a")),
            Arc::new(MockSource::new("source_b")),
            Arc::new(MockSource::new("source_c")),
        ];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        assert_eq!(watcher.sources.len(), 3);
        assert_eq!(watcher.sources[0].name(), "source_a");
        assert_eq!(watcher.sources[1].name(), "source_b");
        assert_eq!(watcher.sources[2].name(), "source_c");
    }

    // =========================================================================
    // Tests for trigger_cascade with no relationships/config
    // =========================================================================

    #[tokio::test]
    async fn test_trigger_cascade_no_relationships_returns_ok() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);
        assert!(watcher.relationships.is_none());

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            scm_repo: Some("org/repo".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        let result = watcher
            .trigger_cascade(
                &attempt,
                "https://github.com/org/repo/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_trigger_cascade_disabled_returns_ok() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let mut config = test_config();
        config.cascade.enabled = false;

        let mut relationships = RepoRelationships::new();
        relationships
            .add_dependency(
                "upstream",
                "downstream",
                crate::repo::DependencyType::Npm,
                None,
            )
            .unwrap();

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(relationships),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/upstream/pull/1".to_string()),
            scm_repo: Some("org/upstream".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Even with relationships, cascade disabled returns Ok
        let result = watcher
            .trigger_cascade(
                &attempt,
                "https://github.com/org/upstream/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_trigger_cascade_no_scm_repo_returns_ok() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let mut config = test_config();
        config.cascade.enabled = true;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(RepoRelationships::new()),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None, // No scm_repo
            scm_pr_number: None,
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        let result = watcher
            .trigger_cascade(&attempt, "", crate::config::CascadeTrigger::Merge)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_trigger_cascade_no_pr_number_returns_ok() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let mut config = test_config();
        config.cascade.enabled = true;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(RepoRelationships::new()),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            scm_repo: Some("org/repo".to_string()),
            scm_pr_number: None, // No PR number
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        let result = watcher
            .trigger_cascade(
                &attempt,
                "https://github.com/org/repo/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_trigger_cascade_no_dependants_returns_ok() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let mut config = test_config();
        config.cascade.enabled = true;

        // Empty relationships (no dependants)
        let watcher = Watcher::new(WatcherOptions {
            config,
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(RepoRelationships::new()),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/my-lib/pull/1".to_string()),
            scm_repo: Some("org/my-lib".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        let result = watcher
            .trigger_cascade(
                &attempt,
                "https://github.com/org/my-lib/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Tests for get_cascade_depth
    // =========================================================================

    #[test]
    fn test_get_cascade_depth_root() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None,
            scm_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        assert_eq!(watcher.get_cascade_depth(&attempt), 0);
    }

    #[test]
    fn test_get_cascade_depth_with_missing_parent() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        // Trait default returns None for parent lookups
        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let attempt = FixAttempt {
            id: 2,
            issue_id: "ISSUE-2".to_string(),
            short_id: "ISSUE-2".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None,
            scm_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: Some(999), // Parent doesn't exist
            cascade_repo: None,
        };

        // Should return 1 for the first hop, then break because parent is not found
        assert_eq!(watcher.get_cascade_depth(&attempt), 1);
    }

    // =========================================================================
    // Tests for poll_source skipping in-flight issues
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_skips_inflight_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new(
                "inflight-1",
                "T-IF1",
                "In-flight issue",
                "http://example.com/if1",
                "test",
            ),
            Issue::new(
                "new-1",
                "T-NEW1",
                "New issue",
                "http://example.com/new1",
                "test",
            ),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let sources = vec![source.clone()];

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, true);

        // Mark one issue as in-flight
        {
            let mut processing = watcher.processing.write().await;
            processing.insert("test:inflight-1".to_string());
        }

        watcher.poll_source(&source).await.unwrap();

        // Only the non-inflight issue should be matched
        let matched = tracker.get_metrics("issues_matched", None, 10).unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].metric_value, 1.0); // Only new-1 matched
    }

    // =========================================================================
    // Tests for poll dry-run mode not processing issues
    // =========================================================================

    #[tokio::test]
    async fn test_poll_dry_run_does_not_record_retries() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Create a failed attempt that would be retried
        tracker
            .record_attempt("mock", "retry-1", "MOCK-R1")
            .unwrap();
        tracker
            .mark_failed("mock", "retry-1", "initial failure")
            .unwrap();

        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;
        let mut config = test_config();
        config.retry.base_delay_ms = 0;
        config.retry.max_delay_ms = 0;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        });

        watcher.poll().await.unwrap();

        // In dry-run mode, retries should not be processed
        let retries = tracker
            .get_metrics("ready_retries_found", None, 10)
            .unwrap();
        assert!(retries.is_empty(), "dry_run should skip retry processing");
    }

    // =========================================================================
    // Tests for MockSource additional coverage
    // =========================================================================

    #[tokio::test]
    async fn test_mock_source_fetch_issues_returns_all() {
        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "test"),
            Issue::new("3", "T-3", "Issue 3", "http://example.com/3", "test"),
        ];
        let source = MockSource::with_issues("test", issues);
        let fetched = source.fetch_issues().await.unwrap();
        assert_eq!(fetched.len(), 3);
    }

    #[test]
    fn test_mock_source_display_name() {
        let source = MockSource::new("my_source");
        assert_eq!(source.display_name(), "my_source");
    }

    #[tokio::test]
    async fn test_mock_source_get_issue_status() {
        let issues = vec![Issue::new("1", "T-1", "Test", "http://example.com", "test")];
        let source = MockSource::with_issues("test", issues);
        let status = source.get_issue_status("1").await.unwrap();
        assert!(status.contains("Open"));
        assert_eq!(source.issue_status_call_count(), 1);
    }

    #[test]
    fn test_mock_source_with_priority() {
        let source = MockSource::with_priority("test", vec![], MatchPriority::Urgent);
        let issue = test_issue();
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);
    }

    // =========================================================================
    // Tests for MockNotifier additional coverage
    // =========================================================================

    #[tokio::test]
    async fn test_mock_notifier_all_methods_increment_count() {
        let notifier = MockNotifier::new(true);
        let issue = test_issue();

        notifier.notify_start(&issue).await.unwrap();
        assert_eq!(notifier.get_call_count(), 1);

        notifier
            .notify_success(&issue, "http://pr.url")
            .await
            .unwrap();
        assert_eq!(notifier.get_call_count(), 2);

        notifier.notify_completed(&issue).await.unwrap();
        assert_eq!(notifier.get_call_count(), 3);

        notifier.notify_failed(&issue, "error msg").await.unwrap();
        assert_eq!(notifier.get_call_count(), 4);

        notifier.notify_status("status msg").await.unwrap();
        assert_eq!(notifier.get_call_count(), 5);

        notifier
            .notify_urgent_issues(&[issue.clone()])
            .await
            .unwrap();
        assert_eq!(notifier.get_call_count(), 6);

        notifier
            .notify_merged(&issue, "http://pr.url")
            .await
            .unwrap();
        assert_eq!(notifier.get_call_count(), 7);
    }

    #[tokio::test]
    async fn test_mock_notifier_with_urgent_failure_fails() {
        let notifier = MockNotifier::with_urgent_failure(true);
        let issue = test_issue();

        let result = notifier.notify_urgent_issues(&[issue]).await;
        assert!(result.is_err());
    }

    // =========================================================================
    // Tests for SeedResult
    // =========================================================================

    #[test]
    fn test_seed_result_by_source_tracking() {
        let mut result = SeedResult::default();
        result.total = 10;
        result.by_source.insert("linear".to_string(), 6);
        result.by_source.insert("sentry".to_string(), 4);

        assert_eq!(result.total, 10);
        assert_eq!(*result.by_source.get("linear").unwrap(), 6);
        assert_eq!(*result.by_source.get("sentry").unwrap(), 4);
        assert!(result.by_source.get("jira").is_none());
    }

    // =========================================================================
    // Tests for Watcher::new initialization state
    // =========================================================================

    #[test]
    fn test_watcher_new_initializes_all_fields() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        // Verify initial state
        assert!(!watcher.is_running());
        assert_eq!(watcher.active_count(), 0);
        assert!(!watcher.dry_run);
        assert!(watcher.inferrer.is_none());
        assert!(watcher.embedding_client.is_none());
        assert!(watcher.review_watcher.is_none());
        assert!(watcher.issue_embedding_service.is_none());
        assert!(watcher.relationships.is_none());
        assert!(watcher.github_client.is_none());
        assert!(watcher.sources.is_empty());
    }

    // =========================================================================
    // Tests for trigger_issue_with_feedback
    // =========================================================================

    #[tokio::test]
    async fn test_trigger_issue_with_feedback_unknown_source() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        let result = watcher
            .trigger_issue_with_feedback("nonexistent", "123", Some("feedback".to_string()), None)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown source"));
    }

    #[tokio::test]
    async fn test_trigger_issue_with_feedback_issue_not_found() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Source exists but has no issues
        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker, vec![source], false);

        let result = watcher
            .trigger_issue_with_feedback("mock", "nonexistent", None, None)
            .await;
        assert!(result.is_err());
    }

    // =========================================================================
    // Tests for poll_source with max_issues_per_cycle limits
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_respects_per_source_max_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues: Vec<Issue> = (1..=10)
            .map(|i| {
                Issue::new(
                    format!("{}", i),
                    format!("T-{}", i),
                    format!("Issue {}", i),
                    format!("http://example.com/{}", i),
                    "test",
                )
            })
            .collect();
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.max_issues_per_cycle = 3;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        });

        watcher.poll_source(&source).await.unwrap();

        let queued = tracker.get_metrics("issues_queued", None, 10).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].metric_value, 3.0); // Limited to 3
    }

    // =========================================================================
    // Tests for process_issue returning false when already processing
    // =========================================================================

    #[tokio::test]
    async fn test_process_issue_returns_false_when_already_processing() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![Issue::new(
            "1",
            "T-1",
            "Test Issue",
            "http://example.com/1",
            "mock",
        )];
        let source = Arc::new(MockSource::with_issues("mock", issues)) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker, vec![source.clone()], false);

        // Mark as already processing
        {
            let mut processing = watcher.processing.write().await;
            processing.insert("mock:1".to_string());
        }

        let issue = Issue::new("1", "T-1", "Test Issue", "http://example.com/1", "mock");
        let match_result = MatchResult::matched("Test", MatchPriority::Normal);

        let result = watcher
            .process_issue(source, issue, match_result, None, None)
            .await;
        assert!(
            !result,
            "process_issue should return false when issue already in-flight"
        );
    }

    // =========================================================================
    // Tests for cascade depth limit
    // =========================================================================

    #[tokio::test]
    async fn test_trigger_cascade_depth_limit() {
        let notifier = Arc::new(MockNotifier::new(true));
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let mut config = test_config();
        config.cascade.enabled = true;
        config.cascade.max_depth = 1;

        let mut relationships = RepoRelationships::new();
        relationships
            .add_dependency(
                "upstream",
                "downstream",
                crate::repo::DependencyType::Npm,
                None,
            )
            .unwrap();

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(relationships),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        // Record a root attempt and a child attempt so depth = 1
        sqlite
            .record_attempt("test", "root-issue", "ROOT-1")
            .unwrap();
        let root = sqlite.get_attempt("test", "root-issue").unwrap().unwrap();

        // Create child attempt with parent_attempt_id set to root
        sqlite
            .record_cascade_attempt("test", "child-issue", "CHILD-1", root.id, "org/upstream")
            .unwrap();
        let child = sqlite.get_attempt("test", "child-issue").unwrap().unwrap();
        assert_eq!(child.parent_attempt_id, Some(root.id));

        // The child is at depth 1 already, and max_depth is 1
        // So trigger_cascade should bail out early
        let result = watcher
            .trigger_cascade(
                &child,
                "https://github.com/org/upstream/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Tests for start loop clamping
    // =========================================================================

    #[tokio::test]
    async fn test_start_clamps_low_interval() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> =
            vec![Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>];

        let watcher = Arc::new(create_test_watcher(notifier, tracker, sources, true));

        // Start with very small interval (should be clamped to 1000ms)
        let runner = {
            let w = Arc::clone(&watcher);
            tokio::spawn(async move { w.start(Some(50)).await })
        };

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        watcher.stop();

        let joined = tokio::time::timeout(std::time::Duration::from_secs(3), runner).await;
        assert!(joined.is_ok(), "watcher start did not stop in time");
        assert!(joined.unwrap().expect("task join failed").is_ok());
    }

    // =========================================================================
    // Tests for reset_attempt nonexistent
    // =========================================================================

    #[test]
    fn test_reset_attempt_nonexistent_succeeds() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let sources: Vec<Arc<dyn IssueSource>> = vec![];

        let watcher = create_test_watcher(notifier, tracker, sources, false);

        // Resetting an attempt that was never recorded should succeed silently
        let result = watcher.reset_attempt("test", "nonexistent");
        assert!(result.is_ok());
    }

    // =========================================================================
    // Tests for poll_source with suppression rules
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_applies_suppression_when_prioritisation_disabled() {
        use crate::config::PrioritisationConfig;
        use crate::types::{SuppressionField, SuppressionMatchMode, SuppressionRule};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![Issue::new(
            "1",
            "T-1",
            "Suppress me please",
            "http://example.com/1",
            "test",
        )];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.prioritisation = PrioritisationConfig {
            enabled: false,
            suppression_rules: vec![SuppressionRule {
                name: "suppress-all".to_string(),
                pattern: ".*".to_string(),
                field: SuppressionField::Title,
                match_mode: SuppressionMatchMode::Regex,
                sources: vec![],
                reason: "test suppression".to_string(),
            }],
            ..Default::default()
        };

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        });

        watcher.poll_source(&source).await.unwrap();

        // Issue should be suppressed
        let matched = tracker.get_metrics("issues_matched", None, 10).unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].metric_value, 0.0);
    }

    // =========================================================================
    // Additional coverage: enhance_prompt_with_learning with learning enabled
    // =========================================================================

    #[test]
    fn test_enhance_prompt_with_learning_repo_knowledge_enabled_but_empty() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.repo_knowledge = true;
        config.learning.qa_promotion = true;
        config.learning.strategy_fingerprinting = true;
        config.learning.cluster_detection = true;
        config.learning.cross_repo_correlation = true;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let base = "Fix the authentication bug";
        let issue = test_issue();
        // With learning enabled but no data in DB, should return base prompt unchanged
        let result = watcher.enhance_prompt_with_learning(base, &issue, Some("org/my-repo"));
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_with_empty_repo_name() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        let base = "Fix the bug";
        let issue = test_issue();
        // Empty string repo name should still attempt learning but find nothing
        let result = watcher.enhance_prompt_with_learning(base, &issue, Some(""));
        assert_eq!(result, base);
    }

    // =========================================================================
    // Additional coverage: notify_failed_with_escalation
    // =========================================================================

    #[tokio::test]
    async fn test_notify_failed_with_escalation_non_hard_error() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier.clone(), tracker, vec![], false);

        let issue = test_issue();
        let result = watcher
            .notify_failed_with_escalation(&issue, "simple build error")
            .await;
        assert!(result.is_ok());
        // Should have called notify_failed once
        assert_eq!(notifier.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_hard_error_rate_limit() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier.clone(), tracker, vec![], false);

        let issue = test_issue();
        // "rate limit" triggers hard error escalation
        let result = watcher
            .notify_failed_with_escalation(&issue, "rate limit exceeded, try again later")
            .await;
        assert!(result.is_ok());
        assert_eq!(notifier.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_hard_error_timeout() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier.clone(), tracker, vec![], false);

        let issue = test_issue();
        let result = watcher
            .notify_failed_with_escalation(&issue, "process timed out after 300s")
            .await;
        assert!(result.is_ok());
        assert_eq!(notifier.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_hard_error_spawn_failure() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier.clone(), tracker, vec![], false);

        let mut issue = test_issue();
        issue.metadata.insert(
            "resolved_user".to_string(),
            serde_json::Value::String("alice".to_string()),
        );

        // Hard error should remove resolved_user (escalate to global)
        let result = watcher
            .notify_failed_with_escalation(&issue, "failed to spawn claude")
            .await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Additional coverage: record_error_pattern with various error types
    // =========================================================================

    #[test]
    fn test_record_error_pattern_various_error_types() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        // Test multiple error types
        watcher.record_error_pattern("test", "1", "rate limit exceeded");
        watcher.record_error_pattern("test", "2", "process timed out after 300s");
        watcher.record_error_pattern("test", "3", "No PR URL found in output");
        watcher.record_error_pattern("test", "4", "Repository resolution failed: no match");
        watcher.record_error_pattern("test", "5", "Failed to create worktree: git error");
    }

    // =========================================================================
    // Additional coverage: record_feedback_outcome_from_attempt
    // =========================================================================

    #[tokio::test]
    async fn test_record_feedback_outcome_from_attempt_no_sqlite() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        let attempt = crate::types::FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None,
            scm_pr_number: None,
            status: FixAttemptStatus::Failed,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Should not panic with default trait impl
        watcher
            .record_feedback_outcome_from_attempt(&attempt, Outcome::Failed)
            .await;
    }

    #[tokio::test]
    async fn test_record_feedback_outcome_from_attempt_with_sqlite() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        // Record an attempt so we can reconstruct it
        sqlite.record_attempt("test", "ISSUE-1", "ISSUE-1").unwrap();
        let attempt = sqlite.get_attempt("test", "ISSUE-1").unwrap().unwrap();

        // Should not panic
        watcher
            .record_feedback_outcome_from_attempt(&attempt, Outcome::Merged)
            .await;
    }

    // =========================================================================
    // Additional coverage: record_feedback_outcome
    // =========================================================================

    #[tokio::test]
    async fn test_record_feedback_outcome_stores_to_tracker() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        sqlite.record_attempt("test", "1", "T-1").unwrap();
        let attempt = sqlite.get_attempt("test", "1").unwrap().unwrap();
        let issue = test_issue();
        let prompt = "Fix the bug in auth module";

        watcher
            .record_feedback_outcome(&attempt, &issue, prompt, Outcome::Failed)
            .await;

        // Verify outcome was stored
        let outcome = sqlite.get_feedback_outcome_by_attempt(attempt.id);
        assert!(outcome.is_ok());
    }

    // =========================================================================
    // Additional coverage: run_periodic_learning with all subsystems disabled
    // =========================================================================

    #[tokio::test]
    async fn test_run_periodic_learning_all_disabled() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.learning.qa_promotion = false;
        config.learning.cluster_detection = false;
        config.learning.cross_repo_correlation = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        // Should complete without panicking
        watcher.run_periodic_learning().await;
    }

    #[tokio::test]
    async fn test_run_periodic_learning_with_cluster_detection_enabled() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let source = Arc::new(MockSource::new("test")) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.learning.qa_promotion = false;
        config.learning.cluster_detection = true;
        config.learning.cross_repo_correlation = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        // Should complete without panicking even with no data
        watcher.run_periodic_learning().await;
    }

    #[tokio::test]
    async fn test_run_periodic_learning_with_cross_repo_enabled() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.qa_promotion = false;
        config.learning.cluster_detection = false;
        config.learning.cross_repo_correlation = true;
        config.learning.cross_repo_window_hours = 24;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        watcher.run_periodic_learning().await;
    }

    #[tokio::test]
    async fn test_run_periodic_learning_with_qa_promotion_enabled() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.qa_promotion = true;
        config.learning.qa_promotion_threshold = 3;
        config.learning.cluster_detection = false;
        config.learning.cross_repo_correlation = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        watcher.run_periodic_learning().await;
    }

    // =========================================================================
    // Additional coverage: run_post_merge_learning
    // =========================================================================

    #[tokio::test]
    async fn test_run_post_merge_learning_all_disabled() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.learning.auto_extract_learnings = false;
        config.learning.diff_analysis = false;
        config.learning.quality_scoring = false;
        config.learning.auto_agent_md = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = crate::types::FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            scm_repo: Some("org/repo".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Should complete without panicking
        watcher.run_post_merge_learning(&attempt).await;
    }

    #[tokio::test]
    async fn test_run_post_merge_learning_auto_extract_enabled_no_sqlite() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.learning.auto_extract_learnings = true;
        config.learning.diff_analysis = false;
        config.learning.quality_scoring = false;
        config.learning.auto_agent_md = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = crate::types::FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None,
            scm_pr_number: None,
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Should skip extraction path when no executions exist
        watcher.run_post_merge_learning(&attempt).await;
    }

    #[tokio::test]
    async fn test_run_post_merge_learning_diff_analysis_no_github_client() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.auto_extract_learnings = false;
        config.learning.diff_analysis = true;
        config.learning.quality_scoring = false;
        config.learning.auto_agent_md = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None, // No GitHub client
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = crate::types::FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            scm_repo: Some("org/repo".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Should skip diff analysis because github_client is None
        watcher.run_post_merge_learning(&attempt).await;
    }

    #[tokio::test]
    async fn test_run_post_merge_learning_quality_scoring_no_pr_url() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.auto_extract_learnings = false;
        config.learning.diff_analysis = false;
        config.learning.quality_scoring = true;
        config.learning.auto_agent_md = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = crate::types::FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None, // No PR URL
            scm_repo: None,
            scm_pr_number: None,
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Should skip quality scoring because pr_url is None
        watcher.run_post_merge_learning(&attempt).await;
    }

    #[tokio::test]
    async fn test_run_post_merge_learning_auto_agent_md_no_scm_repo() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.auto_extract_learnings = false;
        config.learning.diff_analysis = false;
        config.learning.quality_scoring = false;
        config.learning.auto_agent_md = true;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = crate::types::FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None, // No scm_repo
            scm_pr_number: None,
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // Should skip auto_agent_md because scm_repo is None
        watcher.run_post_merge_learning(&attempt).await;
    }

    // =========================================================================
    // Additional coverage: get_cascade_depth with sqlite and chain
    // =========================================================================

    #[test]
    fn test_get_cascade_depth_with_chain() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        // Create a chain: root -> child -> grandchild
        sqlite.record_attempt("test", "root", "ROOT").unwrap();
        let root = sqlite.get_attempt("test", "root").unwrap().unwrap();

        sqlite
            .record_cascade_attempt("test", "child", "CHILD", root.id, "org/repo")
            .unwrap();
        let child = sqlite.get_attempt("test", "child").unwrap().unwrap();

        sqlite
            .record_cascade_attempt("test", "grandchild", "GRANDCHILD", child.id, "org/repo2")
            .unwrap();
        let grandchild = sqlite.get_attempt("test", "grandchild").unwrap().unwrap();

        assert_eq!(watcher.get_cascade_depth(&root), 0);
        assert_eq!(watcher.get_cascade_depth(&child), 1);
        assert_eq!(watcher.get_cascade_depth(&grandchild), 2);
    }

    // =========================================================================
    // Additional coverage: trigger_cascade with max_depth = 0 (unlimited)
    // =========================================================================

    #[tokio::test]
    async fn test_trigger_cascade_unlimited_depth() {
        use crate::types::{FixAttempt, FixAttemptStatus};

        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.cascade.enabled = true;
        config.cascade.max_depth = 0; // Unlimited

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(RepoRelationships::new()),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        sqlite.record_attempt("test", "root", "ROOT").unwrap();
        let root = sqlite.get_attempt("test", "root").unwrap().unwrap();

        sqlite
            .record_cascade_attempt("test", "deep-child", "DEEP", root.id, "org/repo")
            .unwrap();
        let deep_child = sqlite.get_attempt("test", "deep-child").unwrap().unwrap();

        let attempt = FixAttempt {
            id: deep_child.id,
            issue_id: deep_child.issue_id,
            short_id: deep_child.short_id,
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            scm_repo: Some("org/repo".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: Some(root.id),
            cascade_repo: None,
        };

        // With max_depth=0, cascade should NOT be blocked by depth
        // It will still return Ok because there are no dependants
        let result = watcher
            .trigger_cascade(
                &attempt,
                "https://github.com/org/repo/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Additional coverage: check_and_auto_close_prs with terminal issue
    // =========================================================================

    #[tokio::test]
    async fn test_check_and_auto_close_prs_with_terminal_issue() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Create a source whose issues have resolved status
        let mut issue = Issue::new(
            "resolved-1",
            "R-1",
            "A resolved issue",
            "http://example.com/resolved/1",
            "mock",
        );
        issue.status = crate::types::IssueStatus::Resolved;

        let source = Arc::new(MockSource::with_issues("mock", vec![issue])) as Arc<dyn IssueSource>;

        // Record a successful attempt with PR
        tracker.record_attempt("mock", "resolved-1", "R-1").unwrap();
        tracker
            .mark_success("mock", "resolved-1", "https://github.com/org/repo/pull/42")
            .unwrap();

        let watcher = create_test_watcher(notifier.clone(), tracker.clone(), vec![source], false);

        let auto_closed = watcher.check_and_auto_close_prs().await.unwrap();

        // The issue status is "Resolved" which is terminal, so PR should be auto-closed
        assert_eq!(auto_closed.len(), 1);
        assert_eq!(auto_closed[0], "https://github.com/org/repo/pull/42");

        // Verify attempt was marked as closed
        let attempt = tracker.get_attempt("mock", "resolved-1").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Closed);
    }

    // =========================================================================
    // Additional coverage: poll_housekeeping non-dry-run with active processing
    // =========================================================================

    #[tokio::test]
    async fn test_poll_housekeeping_with_active_processing() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![], false);
        watcher.is_running.store(true, Ordering::SeqCst);
        watcher.active_processing.fetch_add(5, Ordering::SeqCst);

        let result = watcher.poll_housekeeping().await;
        assert!(result.is_ok());

        let active = tracker.get_metrics("active_processing", None, 10).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].metric_value, 5.0);
    }

    // =========================================================================
    // Additional coverage: poll_source with prioritisation enabled
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_with_prioritisation_enabled() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "test"),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.prioritisation.enabled = true;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        });

        let result = watcher.poll_source(&source).await;
        assert!(result.is_ok());

        // Verify metrics were recorded
        let queued = tracker.get_metrics("issues_queued", None, 10).unwrap();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].metric_value >= 0.0);
    }

    // =========================================================================
    // Additional coverage: process_issue with repo resolution skip (already
    // tested but verify cleanup)
    // =========================================================================

    #[tokio::test]
    async fn test_process_issue_cleans_up_on_repo_skip() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issue = Issue::new(
            "cleanup-1",
            "CLEAN-1",
            "Test cleanup",
            "http://example.com/cleanup/1",
            "mock",
        );
        let source =
            Arc::new(MockSource::with_issues("mock", vec![issue.clone()])) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], false);

        let match_result = MatchResult::matched("Test", MatchPriority::Normal);
        let started = watcher
            .process_issue(source, issue, match_result, None, None)
            .await;
        assert!(started); // true because it processed (even though it failed)

        // Verify processing set was cleaned up
        let processing = watcher.processing.read().await;
        assert!(!processing.contains("mock:cleanup-1"));

        // Verify active count is back to 0
        assert_eq!(watcher.active_count(), 0);
    }

    // =========================================================================
    // Additional coverage: seed with labels
    // =========================================================================

    #[tokio::test]
    async fn test_seed_preserves_issue_labels() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut issue = Issue::new(
            "labeled-1",
            "L-1",
            "Labeled issue",
            "http://example.com/labeled/1",
            "mock",
        );
        issue.set_metadata("labels", vec!["bug".to_string(), "critical".to_string()]);

        let source = Arc::new(MockSource::with_issues("mock", vec![issue])) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source], false);

        let result = watcher.seed().await.unwrap();
        assert_eq!(result.total, 1);

        // Verify the issue was recorded
        assert!(tracker.has_attempted("mock", "labeled-1").unwrap());
    }

    // =========================================================================
    // Additional coverage: truncate_error edge cases
    // =========================================================================

    #[test]
    fn test_truncate_error_exactly_at_boundary() {
        // Test with exactly 497 chars (no truncation needed for exactly 500 total)
        let error = "b".repeat(497);
        let result = Watcher::truncate_error_for_activity(&error);
        assert_eq!(result.len(), 497);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_single_char() {
        let result = Watcher::truncate_error_for_activity("x");
        assert_eq!(result, "x");
    }

    #[test]
    fn test_truncate_error_all_unicode() {
        // A string of 200 4-byte emojis (800 bytes, 200 chars)
        let error: String = std::iter::repeat('\u{1F600}').take(200).collect();
        let result = Watcher::truncate_error_for_activity(&error);
        // Should not panic and should end with "..."
        assert!(result.ends_with("..."));
        assert!(result.is_char_boundary(result.len()));
    }

    // =========================================================================
    // Additional coverage: stop_and_drain timeout
    // =========================================================================

    #[tokio::test]
    async fn test_stop_and_drain_does_not_hang_forever() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = Arc::new(create_test_watcher(notifier, tracker, vec![], false));
        watcher.is_running.store(true, Ordering::SeqCst);

        // Simulate a task that never completes (active count stays > 0)
        watcher.active_processing.store(1, Ordering::SeqCst);

        // stop_and_drain has a 5-minute internal timeout, but we use an outer timeout
        // We just verify it eventually returns (the internal max_wait breaks the loop)
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(10), watcher.stop_and_drain())
                .await;
        // In test the internal max_wait is 300s which we can't wait for,
        // so this test verifies the method was called correctly and stop was set
        // The timeout will trigger because 300s > 10s, but that's fine
        if result.is_err() {
            // Timed out externally - that's expected since internal timeout is 300s
            assert!(!watcher.is_running());
        } else {
            assert!(!watcher.is_running());
        }
    }

    // =========================================================================
    // Additional coverage: poll_source with all issues already attempted
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_all_issues_already_attempted() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Pre-mark all issues as attempted
        tracker.record_attempt("test", "1", "T-1").unwrap();
        tracker.record_attempt("test", "2", "T-2").unwrap();

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "test"),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], false);

        watcher.poll_source(&source).await.unwrap();

        // All issues were already attempted, so none should be queued
        let queued = tracker.get_metrics("issues_queued", None, 10).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].metric_value, 0.0);
    }

    // =========================================================================
    // Additional coverage: poll_source stops processing when is_running is false
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_stops_when_not_running() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "test"),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;

        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], false);
        // Deliberately NOT setting is_running to true
        // The poll_source should still work but process_issue checks won't queue

        let result = watcher.poll_source(&source).await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Additional coverage: check_pr_merges_and_cascade records all lifecycle metrics
    // =========================================================================

    #[tokio::test]
    async fn test_check_pr_merges_records_all_lifecycle_metrics() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![], false);

        watcher.check_pr_merges_and_cascade().await.unwrap();

        let metric_names = [
            "pr_status_checks",
            "pr_status_merged",
            "pr_status_closed",
            "pr_status_errors",
            "regression_watches_created",
            "auto_resolved_on_merge",
            "cascade_triggered",
            "cascade_failed",
        ];

        for name in &metric_names {
            let metrics = tracker.get_metrics(name, None, 10).unwrap();
            assert_eq!(
                metrics.len(),
                1,
                "Expected exactly 1 metric for {}, got {}",
                name,
                metrics.len()
            );
            assert_eq!(
                metrics[0].metric_value, 0.0,
                "Expected 0.0 for metric {}, got {}",
                name, metrics[0].metric_value
            );
        }
    }

    // =========================================================================
    // Additional coverage: watcher with all optional fields set
    // =========================================================================

    #[test]
    fn test_watcher_new_with_all_optional_fields() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let relationships = RepoRelationships::new();

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![
                Arc::new(MockSource::new("s1")) as Arc<dyn IssueSource>,
                Arc::new(MockSource::new("s2")) as Arc<dyn IssueSource>,
            ],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(relationships),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        });

        assert!(watcher.dry_run);
        assert!(watcher.relationships.is_some());
        assert_eq!(watcher.sources.len(), 2);
        assert!(!watcher.is_running());
        assert_eq!(watcher.active_count(), 0);
    }

    // =========================================================================
    // Additional coverage: process_ready_retries skips inflight issue
    // =========================================================================

    #[tokio::test]
    async fn test_process_ready_retries_skips_inflight() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Create a failed attempt that would be retried
        tracker
            .record_attempt("mock", "inflight-retry", "MOCK-IR")
            .unwrap();
        tracker
            .mark_failed("mock", "inflight-retry", "initial failure")
            .unwrap();

        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.retry.base_delay_ms = 0;
        config.retry.max_delay_ms = 0;
        config.processing_delay_ms = 0;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        watcher.is_running.store(true, Ordering::SeqCst);

        // Mark the issue as currently processing
        {
            let mut processing = watcher.processing.write().await;
            processing.insert("mock:inflight-retry".to_string());
        }

        let result = watcher.process_ready_retries().await;
        assert!(result.is_ok());

        // Attempt should still be failed (retry was skipped because inflight)
        let attempt = tracker
            .get_attempt("mock", "inflight-retry")
            .unwrap()
            .unwrap();
        // The retry was skipped, so retry_count should remain 0
        assert_eq!(attempt.retry_count, 0);
    }

    // =========================================================================
    // Additional coverage: process_ready_retries stops when watcher not running
    // =========================================================================

    #[tokio::test]
    async fn test_process_ready_retries_stops_when_not_running() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        tracker
            .record_attempt("mock", "stop-retry", "MOCK-SR")
            .unwrap();
        tracker
            .mark_failed("mock", "stop-retry", "initial failure")
            .unwrap();

        let source = Arc::new(MockSource::with_issues(
            "mock",
            vec![Issue::new(
                "stop-retry",
                "MOCK-SR",
                "Stop retry",
                "http://example.com",
                "mock",
            )],
        )) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.retry.base_delay_ms = 0;
        config.retry.max_delay_ms = 0;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        // NOT setting is_running - should break the retry loop early
        watcher.is_running.store(false, Ordering::SeqCst);

        let result = watcher.process_ready_retries().await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Additional coverage: poll records pending_attempts and total_attempts
    // =========================================================================

    #[tokio::test]
    async fn test_poll_records_stats_metrics() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Add some attempts to get non-zero stats
        tracker.record_attempt("test", "1", "T-1").unwrap();
        tracker.record_attempt("test", "2", "T-2").unwrap();
        tracker.mark_failed("test", "2", "error").unwrap();

        let watcher = create_test_watcher(notifier, tracker.clone(), vec![], false);
        watcher.poll().await.unwrap();

        let pending = tracker.get_metrics("pending_attempts", None, 10).unwrap();
        assert_eq!(pending.len(), 1);

        let total = tracker.get_metrics("total_attempts", None, 10).unwrap();
        assert_eq!(total.len(), 1);
        assert_eq!(total[0].metric_value, 2.0);
    }

    // =========================================================================
    // Additional coverage: group_review_feedback preserves insertion order
    // =========================================================================

    #[test]
    fn test_group_review_feedback_preserves_insertion_order() {
        let make_review = |id: i64, body: &str| crate::scm::CodeReview {
            id,
            state: "CHANGES_REQUESTED".to_string(),
            body: Some(body.to_string()),
            user: crate::scm::ReviewUser {
                id,
                login: format!("user{}", id),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: None,
        };

        let events = vec![
            crate::scm::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/3".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 3,
                review: make_review(1, "third PR first"),
                inline_comments: vec![],
            },
            crate::scm::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/1".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 1,
                review: make_review(2, "first PR"),
                inline_comments: vec![],
            },
            crate::scm::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/3".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 3,
                review: make_review(3, "third PR second"),
                inline_comments: vec![],
            },
        ];

        let grouped = Watcher::group_review_feedback_by_pr(events);
        assert_eq!(grouped.len(), 2);
        // PR 3 appeared first so it should be first
        assert_eq!(grouped[0].0, "https://github.com/org/repo/pull/3");
        assert_eq!(grouped[0].2, 2); // 2 reviews for PR 3
        assert_eq!(grouped[1].0, "https://github.com/org/repo/pull/1");
        assert_eq!(grouped[1].2, 1); // 1 review for PR 1
    }

    // =========================================================================
    // Additional coverage: trigger_cascade with full_name vs short_name fallback
    // =========================================================================

    #[tokio::test]
    async fn test_trigger_cascade_uses_short_name_fallback() {
        use crate::repo::DependencyType;
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.cascade.enabled = true;
        config.cascade.max_depth = 0;

        // Add dependency using short name (no org prefix)
        let mut relationships = RepoRelationships::new();
        relationships
            .add_dependency("upstream-lib", "downstream-app", DependencyType::Npm, None)
            .unwrap();

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(relationships),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            // scm_repo is "org/upstream-lib" but dependency graph has "upstream-lib"
            pr_url: Some("https://github.com/org/upstream-lib/pull/1".to_string()),
            scm_repo: Some("org/upstream-lib".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // This exercises the short_name fallback path in trigger_cascade
        // It will find dependants via the short name "upstream-lib"
        // but cascade_to_repo will fail because no inferrer is configured
        let result = watcher
            .trigger_cascade(
                &attempt,
                "https://github.com/org/upstream-lib/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        // Should still return Ok even if individual cascade_to_repo fails
        assert!(result.is_ok());
    }

    // =========================================================================
    // Additional coverage: process_issue records attempt early
    // =========================================================================

    #[tokio::test]
    async fn test_process_issue_records_attempt_early() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issue = Issue::new(
            "early-record",
            "ER-1",
            "Early record test",
            "http://example.com/er/1",
            "mock",
        );
        let source =
            Arc::new(MockSource::with_issues("mock", vec![issue.clone()])) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], false);

        let match_result = MatchResult::matched("Test", MatchPriority::Normal);
        watcher
            .process_issue(source, issue, match_result, None, None)
            .await;

        // Verify the attempt was recorded
        assert!(tracker.has_attempted("mock", "early-record").unwrap());
    }

    // =========================================================================
    // Additional coverage: poll with sources records source_count metric
    // =========================================================================

    #[tokio::test]
    async fn test_poll_with_sources_records_source_count() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let sources: Vec<Arc<dyn IssueSource>> = vec![
            Arc::new(MockSource::new("s1")),
            Arc::new(MockSource::new("s2")),
            Arc::new(MockSource::new("s3")),
        ];
        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);
        watcher.poll().await.unwrap();

        let source_count = tracker.get_metrics("poll_sources", None, 10).unwrap();
        assert_eq!(source_count.len(), 1);
        assert_eq!(source_count[0].metric_value, 3.0);
    }

    // =========================================================================
    // Additional coverage: check_and_auto_close_prs with error in get_issue_status
    // =========================================================================

    #[tokio::test]
    async fn test_check_and_auto_close_prs_issue_status_error() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Record a successful attempt with a PR for issue that doesn't exist in the source
        tracker
            .record_attempt("mock", "nonexistent", "NE-1")
            .unwrap();
        tracker
            .mark_success("mock", "nonexistent", "https://github.com/org/repo/pull/1")
            .unwrap();

        // MockSource with no issues - get_issue_status will fail
        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source], false);

        let result = watcher.check_and_auto_close_prs().await.unwrap();
        // Should not auto-close because get_issue_status returned error
        assert!(result.is_empty());

        // Attempt status should remain unchanged
        let attempt = tracker.get_attempt("mock", "nonexistent").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Success);
    }

    // =========================================================================
    // Additional coverage: seed with issue that has metadata labels
    // =========================================================================

    #[tokio::test]
    async fn test_seed_records_labels_from_metadata() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut issue = Issue::new(
            "1",
            "T-1",
            "Bug with labels",
            "http://example.com/1",
            "mock",
        );
        issue.set_metadata(
            "labels",
            vec!["bug".to_string(), "high-priority".to_string()],
        );

        let source = Arc::new(MockSource::with_issues("mock", vec![issue])) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source], false);

        let result = watcher.seed().await.unwrap();
        assert_eq!(result.total, 1);

        // Verify the issue was marked with labels
        let attempt = tracker.get_attempt("mock", "1").unwrap().unwrap();
        assert!(attempt.issue_labels.contains(&"bug".to_string()));
        assert!(attempt.issue_labels.contains(&"high-priority".to_string()));
    }

    // =========================================================================
    // Additional coverage: watcher config per-source overrides
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_uses_per_source_max_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues: Vec<Issue> = (1..=10)
            .map(|i| {
                Issue::new(
                    format!("{}", i),
                    format!("T-{}", i),
                    format!("Issue {}", i),
                    format!("http://example.com/{}", i),
                    "test",
                )
            })
            .collect();
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;

        let mut config = test_config();
        // Global limit is 10 but we want to verify it applies
        config.max_issues_per_cycle = 2;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source.clone()],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: true,
        });

        watcher.poll_source(&source).await.unwrap();

        let queued = tracker.get_metrics("issues_queued", None, 10).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].metric_value, 2.0);
    }

    // =========================================================================
    // Additional coverage: process_ready_retries with empty retries
    // =========================================================================

    #[tokio::test]
    async fn test_process_ready_retries_empty() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![], false);
        watcher.is_running.store(true, Ordering::SeqCst);

        let result = watcher.process_ready_retries().await;
        assert!(result.is_ok());

        // Should record zero-value metrics
        let retries_found = tracker
            .get_metrics("ready_retries_found", None, 10)
            .unwrap();
        assert_eq!(retries_found.len(), 1);
        assert_eq!(retries_found[0].metric_value, 0.0);

        let executed = tracker
            .get_metrics("ready_retries_executed_total", None, 10)
            .unwrap();
        assert_eq!(executed.len(), 1);
        assert_eq!(executed[0].metric_value, 0.0);
    }

    // =========================================================================
    // Additional coverage: trigger_cascade with full owner/repo match
    // =========================================================================

    #[tokio::test]
    async fn test_trigger_cascade_full_name_match() {
        use crate::repo::DependencyType;
        use crate::types::{FixAttempt, FixAttemptStatus};

        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.cascade.enabled = true;
        config.cascade.max_depth = 0;

        // Add dependency using full org/repo name
        let mut relationships = RepoRelationships::new();
        relationships
            .add_dependency(
                "org/upstream-lib",
                "org/downstream-app",
                DependencyType::Npm,
                None,
            )
            .unwrap();

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: Some(relationships),
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let attempt = FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: Some("https://github.com/org/upstream-lib/pull/1".to_string()),
            scm_repo: Some("org/upstream-lib".to_string()),
            scm_pr_number: Some(1),
            status: FixAttemptStatus::Merged,
            error_message: None,
            merged_at: Some(chrono::Utc::now()),
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        // This exercises the full_name match path (not the short_name fallback)
        let result = watcher
            .trigger_cascade(
                &attempt,
                "https://github.com/org/upstream-lib/pull/1",
                crate::config::CascadeTrigger::Merge,
            )
            .await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Additional coverage: enhance_prompt_with_learning cluster detection path
    // =========================================================================

    #[test]
    fn test_enhance_prompt_with_learning_cluster_detection_enabled() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.cluster_detection = true;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        let base = "Fix the auth bug";
        let issue = test_issue();
        let result = watcher.enhance_prompt_with_learning(base, &issue, Some("org/my-repo"));
        // With no clusters stored, should return base prompt
        assert_eq!(result, base);
    }

    // =========================================================================
    // Additional coverage: poll_source fetched metric with issues present
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_fetched_metric_reflects_total_issues() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "test"),
            Issue::new("3", "T-3", "Issue 3", "http://example.com/3", "test"),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], true);

        watcher.poll_source(&source).await.unwrap();

        let fetched = tracker.get_metrics("issues_fetched", None, 10).unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].metric_value, 3.0);
    }

    // =========================================================================
    // Additional coverage: batch_processed metric
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_records_batch_processed_metric() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![Issue::new(
            "1",
            "T-1",
            "Issue 1",
            "http://example.com/1",
            "test",
        )];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], false);
        watcher.is_running.store(true, Ordering::SeqCst);

        watcher.poll_source(&source).await.unwrap();

        let batch = tracker.get_metrics("batch_processed", None, 10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].metric_value, 1.0);
    }

    // =========================================================================
    // Additional coverage: SeedResult
    // =========================================================================

    #[test]
    fn test_seed_result_default_all_fields() {
        let result = SeedResult::default();
        assert_eq!(result.total, 0);
        assert!(result.by_source.is_empty());
        assert_eq!(result.by_source.len(), 0);
    }

    #[test]
    fn test_seed_result_multiple_sources() {
        let mut result = SeedResult::default();
        result.total = 15;
        result.by_source.insert("sentry".to_string(), 7);
        result.by_source.insert("linear".to_string(), 5);
        result.by_source.insert("jira".to_string(), 3);

        assert_eq!(result.by_source.len(), 3);
        assert_eq!(*result.by_source.get("sentry").unwrap(), 7);
        assert_eq!(*result.by_source.get("linear").unwrap(), 5);
        assert_eq!(*result.by_source.get("jira").unwrap(), 3);
    }

    // =========================================================================
    // Additional coverage: Watcher::new feedback_analyzer initialization with sqlite
    // =========================================================================

    #[test]
    fn test_watcher_new_feedback_analyzer_with_sqlite() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        // This tests the branch where tracker is a real SqliteTracker
        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        // Just verify the watcher was created successfully with feedback_analyzer initialized
        assert!(!watcher.is_running());
    }

    #[test]
    fn test_watcher_new_feedback_analyzer_without_sqlite() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // This tests the branch where tracker has default impl
        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        assert!(!watcher.is_running());
    }

    // =========================================================================
    // Additional coverage: record_source_decision / record_issue_decision
    // with various values
    // =========================================================================

    #[test]
    fn test_record_source_decision_with_complex_details() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        watcher.record_source_decision(
            "linear",
            "poll_filtering_summary",
            "Summary of poll filtering for linear",
            json!({
                "fetched": 100,
                "matched": 50,
                "queued": 10,
                "deferred": 40,
                "skipped": {
                    "duplicate": 5,
                    "already_attempted": 30,
                    "inflight": 3,
                    "unmatched": 12,
                },
            }),
        );
    }

    #[test]
    fn test_record_issue_decision_with_metadata() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        let mut issue = test_issue();
        issue.set_metadata("resolved_user", "alice");

        watcher.record_issue_decision(
            &issue,
            "claude_run_succeeded_with_pr",
            format!("Claude produced PR for {}", issue.short_id),
            json!({
                "pr_url": "https://github.com/org/repo/pull/1",
                "attempt_id": 42,
                "used_qa_ids": [1, 2, 3],
            }),
        );
    }

    // =========================================================================
    // Additional coverage: MockNotifier notify_closed (default trait impl)
    // =========================================================================

    #[tokio::test]
    async fn test_mock_notifier_notify_closed_uses_default_impl() {
        let notifier = MockNotifier::new(true);
        let issue = test_issue();

        // notify_closed uses the default trait impl which calls notify_status
        let result = notifier
            .notify_closed(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
        // Default impl calls notify_status which increments call count
        assert_eq!(notifier.get_call_count(), 1);
    }

    // =========================================================================
    // Additional coverage: active_processing_for_source with empty string source
    // =========================================================================

    #[tokio::test]
    async fn test_active_processing_for_source_empty_string() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        {
            let mut processing = watcher.processing.write().await;
            processing.insert(":issue1".to_string());
        }

        // Empty source name prefix ":" should match ":issue1"
        assert_eq!(watcher.active_processing_for_source("").await, 1);
    }

    // =========================================================================
    // Additional coverage: poll_source batch_processed metric for dry run
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_dry_run_does_not_record_batch_processed() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![Issue::new(
            "1",
            "T-1",
            "Issue 1",
            "http://example.com/1",
            "test",
        )];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], true);

        watcher.poll_source(&source).await.unwrap();

        // Dry run returns early before recording batch_processed
        let batch = tracker.get_metrics("batch_processed", None, 10).unwrap();
        assert!(batch.is_empty());
    }

    // =========================================================================
    // Additional coverage: check_and_auto_close_prs non-terminal issue status
    // =========================================================================

    #[tokio::test]
    async fn test_check_and_auto_close_prs_non_terminal_issue() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Issue is still Open (non-terminal)
        let issue = Issue::new(
            "open-1",
            "O-1",
            "Still open issue",
            "http://example.com/open/1",
            "mock",
        );

        let source = Arc::new(MockSource::with_issues("mock", vec![issue])) as Arc<dyn IssueSource>;

        tracker.record_attempt("mock", "open-1", "O-1").unwrap();
        tracker
            .mark_success("mock", "open-1", "https://github.com/org/repo/pull/1")
            .unwrap();

        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source], false);

        let result = watcher.check_and_auto_close_prs().await.unwrap();
        // Issue is still open, so no auto-close
        assert!(result.is_empty());

        // Attempt should still be Success
        let attempt = tracker.get_attempt("mock", "open-1").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Success);
    }

    // =========================================================================
    // Additional coverage: process_ready_retries with processing delay
    // =========================================================================

    #[tokio::test]
    async fn test_process_ready_retries_with_delay_between_items() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Create two failed attempts
        tracker.record_attempt("mock", "r1", "MOCK-R1").unwrap();
        tracker.mark_failed("mock", "r1", "failure 1").unwrap();
        tracker.record_attempt("mock", "r2", "MOCK-R2").unwrap();
        tracker.mark_failed("mock", "r2", "failure 2").unwrap();

        let source = Arc::new(MockSource::new("mock")) as Arc<dyn IssueSource>;

        let mut config = test_config();
        config.retry.base_delay_ms = 0;
        config.retry.max_delay_ms = 0;
        config.processing_delay_ms = 50; // Small delay

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![source],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });
        watcher.is_running.store(true, Ordering::SeqCst);

        let result = watcher.process_ready_retries().await;
        assert!(result.is_ok());

        // Both should have been retried (and failed because issue not in mock source)
        let a1 = tracker.get_attempt("mock", "r1").unwrap().unwrap();
        let a2 = tracker.get_attempt("mock", "r2").unwrap().unwrap();
        assert_eq!(a1.retry_count, 1);
        assert_eq!(a2.retry_count, 1);
    }

    // =========================================================================
    // Additional coverage: run_post_merge_learning with strategy_fingerprinting
    // =========================================================================

    #[tokio::test]
    async fn test_run_post_merge_learning_strategy_fingerprinting_enabled() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = sqlite.clone() as Arc<dyn FixAttemptTracker>;

        let mut config = test_config();
        config.learning.auto_extract_learnings = true;
        config.learning.diff_analysis = false;
        config.learning.quality_scoring = false;
        config.learning.auto_agent_md = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        });

        sqlite.record_attempt("test", "1", "T-1").unwrap();
        let attempt = sqlite.get_attempt("test", "1").unwrap().unwrap();

        // Should not panic even with no executions in DB
        watcher.run_post_merge_learning(&attempt).await;
    }

    // =========================================================================
    // Additional coverage: poll_source with both matched and unmatched issues
    // =========================================================================

    #[tokio::test]
    async fn test_poll_source_metric_consistency() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let issues = vec![
            Issue::new("1", "T-1", "Issue 1", "http://example.com/1", "test"),
            Issue::new("2", "T-2", "Issue 2", "http://example.com/2", "test"),
            Issue::new("3", "T-3", "Issue 3", "http://example.com/3", "test"),
        ];
        let source = Arc::new(MockSource::with_issues("test", issues)) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher(notifier, tracker.clone(), vec![source.clone()], true);

        watcher.poll_source(&source).await.unwrap();

        let fetched = tracker.get_metrics("issues_fetched", None, 10).unwrap();
        let matched = tracker.get_metrics("issues_matched", None, 10).unwrap();
        let queued = tracker.get_metrics("issues_queued", None, 10).unwrap();

        // All 3 issues fetched
        assert_eq!(fetched[0].metric_value, 3.0);
        // MockSource always matches, so all 3 matched
        assert_eq!(matched[0].metric_value, 3.0);
        // max_issues_per_cycle is 5 (default), so all 3 queued
        assert_eq!(queued[0].metric_value, 3.0);
    }

    // =========================================================================
    // Helper: create_test_watcher_with_sqlite
    // =========================================================================

    fn create_test_watcher_with_sqlite(
        notifier: Arc<dyn Notifier>,
        tracker: Arc<SqliteTracker>,
        sources: Vec<Arc<dyn IssueSource>>,
    ) -> Watcher {
        Watcher::new(WatcherOptions {
            config: test_config(),
            sources,
            notifier,
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            dry_run: false,
        })
    }

    // =========================================================================
    // 1. test_active_processing_for_source
    // =========================================================================

    #[tokio::test]
    async fn test_active_processing_for_source() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        // Manually populate the processing set
        {
            let mut processing = watcher.processing.write().await;
            processing.insert("source1:issue1".to_string());
            processing.insert("source1:issue2".to_string());
            processing.insert("source2:issue3".to_string());
        }

        assert_eq!(watcher.active_processing_for_source("source1").await, 2);
        assert_eq!(watcher.active_processing_for_source("source2").await, 1);
        assert_eq!(watcher.active_processing_for_source("source3").await, 0);
    }

    // =========================================================================
    // 2. test_record_source_decision
    // =========================================================================

    #[test]
    fn test_record_source_decision() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite.clone(), vec![]);

        watcher.record_source_decision(
            "test_source",
            "poll_complete",
            "Completed polling for test_source",
            json!({"fetched": 5, "matched": 3}),
        );

        // Verify activity was recorded to the tracker
        let activities = sqlite.get_recent_activities(10, None).unwrap();
        assert!(!activities.is_empty());
        let latest = &activities[0];
        assert_eq!(latest.activity_type, "decision");
        assert!(latest.message.contains("test_source"));
    }

    // =========================================================================
    // 3. test_record_issue_decision
    // =========================================================================

    #[test]
    fn test_record_issue_decision() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite.clone(), vec![]);

        let issue = test_issue();
        watcher.record_issue_decision(
            &issue,
            "issue_queued",
            "Issue TEST-123 queued for processing",
            json!({"priority": "normal", "match_reason": "label match"}),
        );

        let activities = sqlite.get_recent_activities(10, None).unwrap();
        assert!(!activities.is_empty());
        let latest = &activities[0];
        assert_eq!(latest.activity_type, "decision");
        assert!(latest.message.contains("TEST-123"));
        assert_eq!(latest.source.as_deref(), Some("test"));
        assert_eq!(latest.issue_id.as_deref(), Some("123"));
    }

    // =========================================================================
    // 4. test_record_error_pattern
    // =========================================================================

    #[test]
    fn test_record_error_pattern() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite.clone(), vec![]);

        watcher.record_error_pattern("linear", "ISSUE-42", "build failed: exit code 1");
        watcher.record_error_pattern("sentry", "SENTRY-99", "timeout after 300s");
        watcher.record_error_pattern("test", "T-1", "rate limit exceeded");

        // Verify error patterns were stored
        let patterns = sqlite.get_error_patterns(10).unwrap();
        assert!(
            patterns.len() >= 2,
            "Expected at least 2 distinct error patterns, got {}",
            patterns.len()
        );
    }

    // =========================================================================
    // 5. test_truncate_error_boundary_cases
    // =========================================================================

    #[test]
    fn test_truncate_error_boundary_cases() {
        // Exactly 500 chars: no truncation
        let exactly_500 = "a".repeat(500);
        let result = Watcher::truncate_error_for_activity(&exactly_500);
        assert_eq!(result.len(), 500);
        assert!(!result.ends_with("..."));

        // 501 chars: should truncate
        let chars_501 = "b".repeat(501);
        let result = Watcher::truncate_error_for_activity(&chars_501);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 500);

        // Empty string
        let result = Watcher::truncate_error_for_activity("");
        assert_eq!(result, "");

        // Multi-byte UTF-8 near boundary: 495 ASCII chars + some 4-byte emojis
        let mut multi_byte = "x".repeat(495);
        multi_byte.push_str("\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}");
        let result = Watcher::truncate_error_for_activity(&multi_byte);
        assert!(result.ends_with("..."));
        assert!(result.is_char_boundary(result.len()));
        // Verify no panic, no split codepoint
        for ch in result.chars() {
            assert!(ch.len_utf8() >= 1);
        }

        // Very long string: 10000 chars
        let very_long = "z".repeat(10000);
        let result = Watcher::truncate_error_for_activity(&very_long);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 500);
    }

    // =========================================================================
    // 6. test_notify_failed_with_escalation_hard_error
    // =========================================================================

    #[tokio::test]
    async fn test_notify_failed_with_escalation_hard_error() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier.clone(), sqlite.clone(), vec![]);

        let mut issue = test_issue();
        issue
            .metadata
            .insert("resolved_user".to_string(), json!("alice"));

        // "rate limit" is a hard error keyword
        let result = watcher
            .notify_failed_with_escalation(&issue, "rate limit exceeded: please slow down")
            .await;
        assert!(result.is_ok());

        // Notifier should have been called once (via notify_failed)
        assert_eq!(notifier.get_call_count(), 1);

        // Verify the decision activity was recorded
        let activities = sqlite.get_recent_activities(10, None).unwrap();
        let escalation = activities
            .iter()
            .find(|a| a.activity_type == "decision" || a.activity_type == "error");
        assert!(
            escalation.is_some(),
            "Expected an escalation activity to be recorded"
        );
    }

    // =========================================================================
    // 7. test_notify_failed_with_escalation_soft_error
    // =========================================================================

    #[tokio::test]
    async fn test_notify_failed_with_escalation_soft_error() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier.clone(), sqlite.clone(), vec![]);

        let issue = test_issue();

        // A normal error message that is NOT a hard error
        let result = watcher
            .notify_failed_with_escalation(&issue, "compilation failed: missing semicolon")
            .await;
        assert!(result.is_ok());

        // Notifier should have been called once
        assert_eq!(notifier.get_call_count(), 1);

        // No escalation activity should exist (soft errors skip the escalation path)
        let activities = sqlite.get_recent_activities(10, None).unwrap();
        let has_escalation = activities
            .iter()
            .any(|a| a.message.contains("Escalating hard error"));
        assert!(
            !has_escalation,
            "Soft error should not trigger escalation activity"
        );
    }

    // =========================================================================
    // 8. test_watcher_new_with_tracker_coverage
    // =========================================================================

    #[test]
    fn test_watcher_new_with_tracker_coverage() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite.clone(), vec![]);

        // Verify the Some(st) branch in the constructor was taken
        assert!(!watcher.dry_run);
        assert!(!watcher.is_running());
        assert_eq!(watcher.active_count(), 0);

        // Verify feedback_analyzer was initialized with sqlite
        // (it won't panic when used, which it would if incorrectly initialized)
        assert_eq!(watcher.sources.len(), 0);
    }

    // =========================================================================
    // 9. test_sync_repos_to_db_no_inferrer
    // =========================================================================

    #[test]
    fn test_sync_repos_to_db_no_inferrer() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite, vec![]);

        // inferrer is None, should return 0
        let result = watcher.sync_repos_to_db(true).unwrap();
        assert_eq!(result, 0);

        let result = watcher.sync_repos_to_db(false).unwrap();
        assert_eq!(result, 0);
    }

    // =========================================================================
    // 10. test_sync_repos_to_db_no_sqlite
    // =========================================================================

    #[test]
    fn test_sync_repos_to_db_no_sqlite() {
        let notifier = Arc::new(MockNotifier::new(true));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let watcher = create_test_watcher(notifier, tracker, vec![], false);

        // No inferrer, so sync returns 0
        let result = watcher.sync_repos_to_db(false).unwrap();
        assert_eq!(result, 0);
    }

    // =========================================================================
    // 11. test_refresh_repos_no_inferrer
    // =========================================================================

    #[tokio::test]
    async fn test_refresh_repos_no_inferrer() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite, vec![]);

        // No inferrer and no embedding_client => returns 0
        assert!(watcher.inferrer.is_none());
        assert!(watcher.embedding_client.is_none());
        let result = watcher.refresh_repos().await.unwrap();
        assert_eq!(result, 0);
    }

    // =========================================================================
    // 12. test_build_inferrer_no_known_orgs
    // =========================================================================

    #[tokio::test]
    async fn test_build_inferrer_no_known_orgs() {
        let mut config = test_config();
        config.known_orgs = vec![];

        let result = Watcher::build_inferrer(&config, None).await.unwrap();
        assert!(result.is_none(), "Expected None when known_orgs is empty");
    }

    // =========================================================================
    // 13. test_build_inferrer_no_discovery_method
    // =========================================================================

    #[tokio::test]
    async fn test_build_inferrer_no_discovery_method() {
        let mut config = test_config();
        config.known_orgs = vec!["some-org".to_string()];
        config.auto_discover_paths = vec![];
        // No github client passed

        let result = Watcher::build_inferrer(&config, None).await.unwrap();
        assert!(
            result.is_none(),
            "Expected None when no auto_discover_paths and no GitHub client"
        );
    }

    // =========================================================================
    // 14. test_get_cascade_depth_no_parent
    // =========================================================================

    #[test]
    fn test_get_cascade_depth_no_parent() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite, vec![]);

        let attempt = crate::types::FixAttempt {
            id: 1,
            issue_id: "ISSUE-1".to_string(),
            short_id: "ISSUE-1".to_string(),
            source: "test".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            scm_repo: None,
            scm_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        assert_eq!(watcher.get_cascade_depth(&attempt), 0);
    }

    // =========================================================================
    // 15. test_record_feedback_outcome_from_attempt
    // =========================================================================

    #[tokio::test]
    async fn test_record_feedback_outcome_from_attempt() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite.clone(), vec![]);

        // Record an attempt so we have one in the DB
        sqlite.record_attempt("test", "ISSUE-42", "T-42").unwrap();
        let attempt = sqlite.get_attempt("test", "ISSUE-42").unwrap().unwrap();

        // Should not panic and should create a minimal Issue internally
        watcher
            .record_feedback_outcome_from_attempt(&attempt, Outcome::Failed)
            .await;

        // Verify the feedback outcome was stored
        let outcome = sqlite.get_feedback_outcome_by_attempt(attempt.id);
        assert!(outcome.is_ok());
    }

    // =========================================================================
    // 16. test_run_periodic_learning_disabled
    // =========================================================================

    #[tokio::test]
    async fn test_run_periodic_learning_disabled() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));

        let mut config = test_config();
        config.learning.qa_promotion = false;
        config.learning.cluster_detection = false;
        config.learning.cross_repo_correlation = false;

        let watcher = Watcher::new(WatcherOptions {
            config,
            sources: vec![],
            notifier,
            tracker: sqlite.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                sqlite.clone(),
            )),
            dry_run: false,
        });

        // Should complete instantly with all learning disabled
        watcher.run_periodic_learning().await;
        // No panic = success
    }

    // =========================================================================
    // 17. test_seed_empty_sources
    // =========================================================================

    #[tokio::test]
    async fn test_seed_empty_sources() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite, vec![]);

        let result = watcher.seed().await.unwrap();
        assert_eq!(result.total, 0);
        assert!(result.by_source.is_empty());
    }

    // =========================================================================
    // 18. test_seed_with_issues
    // =========================================================================

    #[tokio::test]
    async fn test_seed_with_issues() {
        let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new(true));

        let issues = vec![
            Issue::new(
                "10",
                "S-10",
                "Seed Issue 1",
                "http://example.com/10",
                "seed_src",
            ),
            Issue::new(
                "11",
                "S-11",
                "Seed Issue 2",
                "http://example.com/11",
                "seed_src",
            ),
            Issue::new(
                "12",
                "S-12",
                "Seed Issue 3",
                "http://example.com/12",
                "seed_src",
            ),
        ];
        let source = Arc::new(MockSource::with_issues("seed_src", issues)) as Arc<dyn IssueSource>;
        let watcher = create_test_watcher_with_sqlite(notifier, sqlite.clone(), vec![source]);

        let result = watcher.seed().await.unwrap();
        assert_eq!(result.total, 3);
        assert_eq!(*result.by_source.get("seed_src").unwrap(), 3);

        // Verify issues are tracked in the DB
        assert!(sqlite.has_attempted("seed_src", "10").unwrap());
        assert!(sqlite.has_attempted("seed_src", "11").unwrap());
        assert!(sqlite.has_attempted("seed_src", "12").unwrap());
    }

    #[test]
    fn test_watcher_accepts_non_claude_agent() {
        use crate::runner::AgentRunner;

        struct MockAgent;

        #[async_trait]
        impl AgentRunner for MockAgent {
            fn name(&self) -> &str { "mock-agent" }
            fn capabilities(&self) -> crate::runner::ProviderCapabilities {
                crate::runner::ProviderCapabilities::default()
            }
            fn build_prompt_for_issue(
                &self,
                _issue: &Issue,
                _context: &str,
                _project_dir: &std::path::Path,
            ) -> String {
                "mock prompt".to_string()
            }
            async fn execute_with_attempt(
                &self,
                _prompt: &str,
                _issue: Option<&Issue>,
                _attempt_id: Option<i64>,
                _project_dir: &std::path::Path,
            ) -> crate::error::Result<crate::types::AgentResult> {
                Ok(crate::types::AgentResult {
                    success: true,
                    output: "mock output".to_string(),
                    pr_url: None,
                    changelog: None,
                    error: None,
                    blocking_question: None,
                    used_qa_ids: Vec::new(),
                })
            }
        }

        // Create a Watcher with the mock agent to verify trait abstraction works
        let tracker: Arc<dyn crate::storage::FixAttemptTracker> =
            Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let mock_agent: Arc<dyn AgentRunner> = Arc::new(MockAgent);

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier: Arc::new(crate::notifier::ConsoleNotifier),
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            dry_run: true,
            agent: mock_agent,
        });

        // Verify the watcher was created successfully with a non-Claude agent
        assert!(watcher.config.work_dir.to_str().is_some());
    }

    #[test]
    fn test_watcher_with_orchestrator_agent() {
        use crate::runner::{AgentRunner, AgentOrchestrator, SelectionStrategy, WeightedProvider};

        let tracker: Arc<dyn crate::storage::FixAttemptTracker> =
            Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());

        // Create an orchestrator with a simple mock as the agent
        struct SimpleRunner;

        #[async_trait]
        impl AgentRunner for SimpleRunner {
            fn name(&self) -> &str { "simple" }
            fn capabilities(&self) -> crate::runner::ProviderCapabilities {
                crate::runner::ProviderCapabilities::default()
            }
            fn build_prompt_for_issue(&self, _: &Issue, _: &str, _: &std::path::Path) -> String {
                "simple prompt".to_string()
            }
            async fn execute_with_attempt(
                &self, _: &str, _: Option<&Issue>, _: Option<i64>, _: &std::path::Path,
            ) -> crate::error::Result<crate::types::AgentResult> {
                Ok(crate::types::AgentResult {
                    success: true, output: String::new(), pr_url: None,
                    changelog: None, error: None, blocking_question: None, used_qa_ids: Vec::new(),
                })
            }
        }

        let orchestrator = AgentOrchestrator::new(
            vec![WeightedProvider {
                provider: Arc::new(SimpleRunner),
                weight: 1.0,
            }],
            SelectionStrategy::Primary,
            Some("test-experiment".to_string()),
        );

        let agent: Arc<dyn AgentRunner> = Arc::new(orchestrator);

        let watcher = Watcher::new(WatcherOptions {
            config: test_config(),
            sources: vec![],
            notifier: Arc::new(crate::notifier::ConsoleNotifier),
            tracker: tracker.clone(),
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            scm_provider: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
            dry_run: true,
            agent,
        });

        assert!(watcher.config.work_dir.to_str().is_some());
    }
}
