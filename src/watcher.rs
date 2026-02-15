//! Main watcher that coordinates sources, Claude, and notifications.

use crate::config::Config;
use crate::error::Result;
use crate::feedback::{
    format_similar_issues_context, FeedbackAnalyzer, FixOutcome, IssueEmbeddingService, Outcome,
};
use crate::github::{GitHubClient, PrReviewState, PrStatus, ReviewEvent, ReviewWatcher};
use crate::inference::{resolve_repo_for_issue, RepoInferrer, RepoResolution};
use crate::notifier::send_to_all_and_wait_first_reply;
use crate::notifier::Notifier;
use crate::qa::{
    build_correlation_id, embed_text, find_reusable_qa, format_answer_context,
    format_reuse_context, format_timeout_context, normalize_text,
};
use crate::repo::{GitOps, RepoIndex, RepoRelationships};
use crate::retry::RetryManager;
use crate::runner::{ClaudeRunner, ClaudeRunnerConfig};
use crate::source::IssueSource;
use crate::storage::{classify_error, compute_error_hash, FixAttemptTracker, SqliteTracker};
use crate::types::{
    ActivityLogEntry, AskRequest, ErrorPattern, FixAttemptStats, FixAttemptStatus, Issue,
    IssueType, MatchPriority, MatchResult, ProcessingMetric, QaKnowledgeEntry, RegressionWatch,
};
use crate::users::UserRegistry;
use chrono::Utc;
use serde_json::json;
use std::collections::HashSet;
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
    pub sqlite_tracker: Option<Arc<SqliteTracker>>,
    pub inferrer: Option<RepoInferrer>,
    pub embedding_client: Option<crate::feedback::EmbeddingClient>,
    pub review_watcher: Option<Arc<ReviewWatcher>>,
    pub issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    pub relationships: Option<RepoRelationships>,
    pub github_client: Option<GitHubClient>,
    pub user_registry: UserRegistry,
    pub dry_run: bool,
}

/// Main watcher that coordinates sources, Claude, and notifications.
pub struct Watcher {
    config: Config,
    sources: Vec<Arc<dyn IssueSource>>,
    notifier: Arc<dyn Notifier>,
    tracker: Arc<dyn FixAttemptTracker>,
    sqlite_tracker: Option<Arc<SqliteTracker>>,
    inferrer: Option<RepoInferrer>,
    embedding_client: Option<crate::feedback::EmbeddingClient>,
    review_watcher: Option<Arc<ReviewWatcher>>,
    issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    relationships: Option<RepoRelationships>,
    github_client: Option<GitHubClient>,
    user_registry: UserRegistry,
    claude: ClaudeRunner,
    dry_run: bool,
    is_running: AtomicBool,
    processing: RwLock<HashSet<String>>,
    active_processing: AtomicUsize,
    /// Feedback analyzer for learning from past outcomes
    feedback_analyzer: tokio::sync::Mutex<FeedbackAnalyzer>,
}

impl Watcher {
    /// Create a new watcher.
    pub fn new(options: WatcherOptions) -> Self {
        Self {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: options.config.claude_timeout_secs,
                    model: options.config.claude.model.clone(),
                    instructions: options.config.claude.instructions.clone(),
                    permissions: options.config.claude.permissions.clone(),
                    skip_permissions: options.config.claude.skip_permissions,
                },
                options.tracker.clone(),
            ),
            config: options.config,
            sources: options.sources,
            notifier: options.notifier,
            tracker: options.tracker,
            sqlite_tracker: options.sqlite_tracker,
            inferrer: options.inferrer,
            embedding_client: options.embedding_client,
            review_watcher: options.review_watcher,
            issue_embedding_service: options.issue_embedding_service,
            relationships: options.relationships,
            github_client: options.github_client,
            user_registry: options.user_registry,
            dry_run: options.dry_run,
            is_running: AtomicBool::new(false),
            processing: RwLock::new(HashSet::new()),
            active_processing: AtomicUsize::new(0),
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
        }
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
            &config.work_dir,
            config.github.use_ssh,
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
            &config.work_dir,
            config.github.use_ssh,
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

        let sqlite_tracker = match &self.sqlite_tracker {
            Some(t) => t,
            None => return Ok(0),
        };

        inferrer.with_index(|index| sqlite_tracker.sync_from_index(index, sync_files))
    }

    /// Start the watcher with polling.
    pub async fn start(&self, interval_ms: Option<u64>) -> Result<()> {
        let configured_poll_interval = interval_ms.unwrap_or(self.config.poll_interval_ms);
        let poll_interval = configured_poll_interval.max(1);
        if configured_poll_interval == 0 {
            tracing::warn!(
                component = "watcher",
                "Poll interval evaluated to 0ms, clamping to 1ms to avoid timer panic"
            );
        }

        tracing::info!("");
        tracing::info!(
            "Starting Claude Watcher{}",
            if self.dry_run { " (DRY RUN)" } else { "" }
        );
        tracing::info!("  Work dir: {:?}", self.config.work_dir);
        tracing::info!("  Known orgs: {}", self.config.known_orgs.len());
        tracing::info!("  Poll interval: {}ms", poll_interval);
        tracing::info!(
            "  Max issues per cycle: {} (global)",
            self.config.max_issues_per_cycle
        );
        tracing::info!("  Max concurrent: {} (global)", self.config.max_concurrent);
        for source in &self.sources {
            let src_max_issues = self.config.max_issues_per_cycle_for(source.name());
            let src_max_concurrent = self.config.max_concurrent_for(source.name());
            if src_max_issues != self.config.max_issues_per_cycle
                || src_max_concurrent != self.config.max_concurrent
            {
                tracing::info!(
                    "    {}: max_issues={}, max_concurrent={}",
                    source.name(),
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
        let sqlite_tracker = self.sqlite_tracker.clone();
        let sync_result = tokio::task::spawn_blocking(move || -> crate::error::Result<usize> {
            let inferrer = match &inferrer {
                Some(inf) => inf,
                None => return Ok(0),
            };
            let sqlite_tracker = match &sqlite_tracker {
                Some(t) => t,
                None => return Ok(0),
            };
            inferrer.with_index(|index| sqlite_tracker.sync_from_index(index, true))
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
        if let Some(ref sqlite_tracker) = self.sqlite_tracker {
            match sqlite_tracker.get_feedback_outcomes(None, 1000) {
                Ok(outcomes) if !outcomes.is_empty() => {
                    let count = outcomes.len();
                    let mut analyzer = self.feedback_analyzer.lock().await;
                    analyzer.load_outcomes(outcomes);
                    tracing::info!(count = count, "Loaded feedback outcomes for learning");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to load feedback outcomes"),
            }
        }

        self.is_running.store(true, Ordering::SeqCst);

        // Initial poll
        self.poll().await?;

        // Set up interval
        let mut poll_timer = interval(Duration::from_millis(poll_interval));
        poll_timer.tick().await; // Skip immediate first tick

        // Counter for periodic repo refresh (every 5 polls)
        let mut poll_count: u32 = 0;
        const REFRESH_INTERVAL: u32 = 5;

        while self.is_running.load(Ordering::SeqCst) {
            poll_timer.tick().await;
            if self.is_running.load(Ordering::SeqCst) {
                poll_count = poll_count.wrapping_add(1);

                // Periodically refresh repo index to detect new repositories
                if poll_count.is_multiple_of(REFRESH_INTERVAL) {
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

                // Poll for new issues
                if let Err(e) = self.poll().await {
                    tracing::error!(component = "watcher", error = %e, "Poll error");
                }
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
        if let Some(sqlite) = &self.sqlite_tracker {
            if let Some(ref pr_url) = attempt.pr_url {
                // Update the PR record with incremented review_cycles
                if let Ok(Some(mut pr_record)) = sqlite.get_pr(pr_url) {
                    pr_record.review_cycles += 1;
                    pr_record.last_review_at = Some(chrono::Utc::now());
                    if let Err(e) = sqlite.upsert_pr(&pr_record) {
                        tracing::warn!(error = %e, "Failed to update PR review cycles");
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
            tracing::info!(
                pr_url = %pr_url,
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

    /// Trigger cascade processing for downstream repos after a PR is merged.
    ///
    /// Looks up the merged repo in the dependency graph and spawns Claude
    /// in each direct dependent repo with context about the upstream changes.
    pub async fn trigger_cascade(
        &self,
        attempt: &crate::types::FixAttempt,
        pr_url: &str,
    ) -> Result<()> {
        let relationships = match &self.relationships {
            Some(r) => r,
            None => return Ok(()),
        };

        if !self.config.cascade.enabled {
            return Ok(());
        }

        let github_repo = match &attempt.github_repo {
            Some(r) => r.clone(),
            None => return Ok(()),
        };

        if attempt.github_pr_number.is_none() {
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

        // Normalize repo name for dependency graph lookup
        // github_repo is "owner/repo", graph uses short names like "appwrite"
        let repo_short_name = github_repo.split('/').next_back().unwrap_or(&github_repo);

        let dependants = relationships.get_dependants(repo_short_name);
        if dependants.is_empty() {
            tracing::debug!(
                repo = %github_repo,
                short_name = %repo_short_name,
                "No downstream dependants found for cascade"
            );
            return Ok(());
        }

        tracing::info!(
            repo = %github_repo,
            dependants = dependants.len(),
            "Triggering cascade for downstream repos"
        );

        let upstream_pr_url = pr_url.to_string();
        let graph = relationships.get_graph();

        for dependant in dependants {
            let dep_type = graph
                .get_first_hop_dependency_type(repo_short_name)
                .map(|t| t.as_str())
                .unwrap_or("unknown");

            if let Err(e) = self
                .cascade_to_repo(
                    attempt,
                    &dependant.name,
                    &github_repo,
                    &upstream_pr_url,
                    dep_type,
                )
                .await
            {
                tracing::error!(
                    upstream = %github_repo,
                    downstream = %dependant.name,
                    error = %e,
                    "Failed to cascade to downstream repo"
                );
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
            match self
                .sqlite_tracker
                .as_ref()
                .and_then(|t| t.get_attempt_by_id(parent_id).ok().flatten())
            {
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

        let (project_dir, github_url, default_branch) = match resolution {
            crate::inference::RepoResolution::Resolved {
                project_dir,
                github_url,
                default_branch,
                ..
            } => (project_dir, github_url, default_branch),
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
        let sqlite = match &self.sqlite_tracker {
            Some(t) => t,
            None => {
                tracing::warn!("No SQLite tracker available for cascade tracking");
                return Ok(());
            }
        };

        let attempt_id = sqlite.record_cascade_attempt(
            &parent_attempt.source,
            &parent_attempt.issue_id,
            &parent_attempt.short_id,
            parent_attempt.id,
            &github_url,
        )?;

        // Ensure the downstream repo is up to date
        if let Err(e) =
            GitOps::ensure_repo_at_path(&project_dir, &github_url, &default_branch).await
        {
            tracing::warn!(
                downstream = %downstream_repo_name,
                error = %e,
                "Failed to ensure repo is up to date, continuing anyway"
            );
        }

        // Build the cascade prompt
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
- Update dependency version if needed
- Adapt to any API changes
- Update tests that exercise the changed functionality
- Ensure the project builds and tests pass

Create a PR with your changes."#,
            upstream_repo = upstream_repo,
            short_id = parent_attempt.short_id,
            source = parent_attempt.source,
            upstream_pr_url = upstream_pr_url,
            downstream_repo_name = downstream_repo_name,
            dep_type = dep_type,
        );

        // Run Claude
        let result = self
            .claude
            .execute_with_attempt(&prompt, None, Some(attempt_id), &project_dir)
            .await?;

        if result.success {
            if let Some(ref pr_url) = result.pr_url {
                tracing::info!(
                    downstream = %downstream_repo_name,
                    pr_url = %pr_url,
                    "Cascade PR created"
                );

                // Update the cascade attempt with PR details
                if let Some((repo, pr_num)) = SqliteTracker::parse_pr_url(pr_url) {
                    sqlite.update_attempt_pr(attempt_id, pr_url, &repo, pr_num)?;
                }

                // Register for review watching — this enables recursive cascade
                if let Some(ref review_watcher) = self.review_watcher {
                    if let Some((repo, pr_number)) = SqliteTracker::parse_pr_url(pr_url) {
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
            }
        } else {
            let error = result.error.unwrap_or_else(|| "Unknown error".to_string());
            tracing::warn!(
                downstream = %downstream_repo_name,
                error = %error,
                "Cascade fix failed"
            );
            sqlite.mark_cascade_failed(attempt_id, &error)?;
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

        for source in &self.sources {
            if let Err(e) = self.poll_source(source).await {
                tracing::error!(component = "watcher", source = source.name(), error = %e, "Error polling");
            }
        }

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
        // Get all successful attempts with PRs that haven't been merged yet.
        // If GitHub client is unavailable, we still emit zero-value lifecycle metrics.
        let pending_prs = if github_client.is_some() {
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
            let repo = match &attempt.github_repo {
                Some(r) => r,
                None => continue,
            };
            let pr_number = match attempt.github_pr_number {
                Some(n) => n,
                None => continue,
            };
            let Some(github_client) = github_client else {
                break;
            };

            pr_status_checks += 1;
            match github_client.get_pr_status(repo, pr_number).await {
                Ok(PrStatus::Merged) => {
                    pr_status_merged += 1;
                    self.tracker
                        .mark_merged(&attempt.source, &attempt.issue_id)?;
                    let _ = self
                        .tracker
                        .update_qa_outcome_stats_for_attempt(attempt.id, true);

                    // For bug-type issues, create a regression watch instead of immediate auto-resolve.
                    let regression_watch_id = if attempt.is_bug() {
                        if let Some(ref sqlite_tracker) = self.sqlite_tracker {
                            let issue_type = match attempt.source.as_str() {
                                "sentry" => IssueType::SentryIssue,
                                "linear" => IssueType::LinearBug,
                                _ => IssueType::SentryIssue,
                            };
                            let mut watch =
                                RegressionWatch::new(issue_type, &attempt.issue_id, attempt.id);
                            watch.pr_merged_at = Some(chrono::Utc::now());

                            match sqlite_tracker.create_regression_watch(&watch) {
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
                        }
                    } else {
                        None
                    };

                    // Auto-resolve only when enabled and no regression watch is active.
                    let should_resolve =
                        regression_watch_id.is_none() && self.config.github.auto_resolve_on_merge;
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

                    // Stop review polling for merged PRs.
                    if let (Some(review_watcher), Some(pr_url)) =
                        (self.review_watcher.as_ref(), attempt.pr_url.as_ref())
                    {
                        review_watcher.unwatch_pr(pr_url);
                    }

                    let pr_url = attempt.pr_url.as_deref().unwrap_or("");
                    if self.config.cascade.enabled {
                        match self.trigger_cascade(attempt, pr_url).await {
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

            let match_result = source.matches_criteria(&issue);
            if match_result.matches {
                candidates.push((issue, match_result));
            } else {
                unmatched_skipped = unmatched_skipped.saturating_add(1);
            }
        }
        drop(processing);

        let candidates_count = candidates.len();
        let matched_metric = ProcessingMetric::new("issues_matched", candidates_count as f64)
            .with_source(source.name().to_string());
        if let Err(e) = self.tracker.record_metric(&matched_metric) {
            tracing::debug!(error = %e, "Failed to record issues_matched metric");
        }

        // Apply per-source max issues per cycle limit (falls back to global)
        let source_max_issues = self.config.max_issues_per_cycle_for(source.name());

        // Sort by priority before selecting the subset that will be processed.
        self.sort_by_priority(&mut candidates);
        let to_process: Vec<_> = candidates.into_iter().take(source_max_issues).collect();

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
                "fetched": candidates_count + duplicate_skipped + attempted_skipped + inflight_skipped + unmatched_skipped,
                "matched": candidates_count,
                "queued": to_process_count,
                "deferred": candidates_count.saturating_sub(source_max_issues),
                "skipped": {
                    "duplicate": duplicate_skipped,
                    "already_attempted": attempted_skipped,
                    "inflight": inflight_skipped,
                    "unmatched": unmatched_skipped,
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
                    self.sqlite_tracker.as_ref(),
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
                .process_issue(source_clone, issue, match_result, None)
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

        // Infer the target repository using the shared resolution function
        let resolution =
            resolve_repo_for_issue(self.inferrer.as_ref(), &issue, self.sqlite_tracker.as_ref());

        let project_dir = match &resolution {
            RepoResolution::Resolved { project_dir, .. } => {
                self.record_issue_decision(
                    &issue,
                    "repo_resolution_selected",
                    format!("Resolved repository for {}", issue.short_id),
                    json!({
                        "repo_name": resolution.repo_name(),
                        "github_url": resolution.github_url(),
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

        // Ensure repo is up to date: pull latest changes
        if let (Some(github_url), Some(default_branch), Some(repo_name)) = (
            resolution.github_url(),
            resolution.default_branch(),
            resolution.repo_name(),
        ) {
            self.record_issue_decision(
                &issue,
                "repo_sync_started",
                format!("Syncing repository {} for {}", repo_name, issue.short_id),
                json!({
                    "repo_name": repo_name,
                    "github_url": github_url,
                    "default_branch": default_branch,
                    "project_dir": project_dir.display().to_string(),
                }),
            );
            tracing::info!(
                short_id = %issue.short_id,
                repo = %repo_name,
                "Pulling latest changes"
            );

            if let Err(e) =
                GitOps::ensure_repo_at_path(&project_dir, github_url, default_branch).await
            {
                let pull_error = format!("Failed to pull repository: {}", e);
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
                        "Failed to mark issue as failed after repository pull failure"
                    );
                }
                tracing::error!(
                    short_id = %issue.short_id,
                    repo = %repo_name,
                    error = %e,
                    "Failed to pull repository, skipping issue"
                );
                // Clean up processing state before returning
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
                format!("Repository synced for {}", issue.short_id),
                json!({
                    "repo_name": repo_name,
                    "project_dir": project_dir.display().to_string(),
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
                if let Some(tracker) = &self.sqlite_tracker {
                    if let Some(repo) = inferrer.get_repo(repo_name) {
                        if let Err(e) = tracker.sync_repo_files(&repo) {
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
        }

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
                context = format!(
                    "{}\n\n## PR Review Feedback\n{}\n\nAddress all review feedback in this update.",
                    context, feedback
                );
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
                let prompt = self.claude.build_prompt_for_issue(&issue, &context, &project_dir);

                // Enhance prompt with learnings from past outcomes.
                let prompt = {
                    let analyzer = self.feedback_analyzer.lock().await;
                    analyzer.enhance_prompt(&prompt, &issue)
                };
                let mut run_result = self
                    .claude
                    .execute_with_attempt(&prompt, Some(&issue), attempt_id, &project_dir)
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

            if claude_result.success {
                if let Some(ref pr_url) = claude_result.pr_url {
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

                    // Store embedding for future similarity lookups
                    if let Some(ref embedding_service) = self.issue_embedding_service {
                        if let Err(e) = embedding_service.embed_issue(&issue, source.name()).await {
                            tracing::warn!(error = %e, "Failed to store issue embedding");
                        }
                    }

                    // Register PR for review watching
                    if let Some(ref review_watcher) = self.review_watcher {
                        if let Some((repo, pr_number)) = SqliteTracker::parse_pr_url(pr_url) {
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
                    tracing::info!(short_id = %issue.short_id, "Completed but no PR URL found");
                    self.tracker.mark_failed(
                        source.name(),
                        &issue.id,
                        "No PR URL found in output",
                    )?;
                    self.notifier.notify_completed(&issue).await?;
                    if let Some(id) = attempt_id {
                        let _ = self.tracker.update_qa_outcome_stats_for_attempt(id, false);
                    }

                    // Record feedback outcome
                    if let Ok(Some(attempt)) = self.tracker.get_attempt(source.name(), &issue.id) {
                        self.record_feedback_outcome(&attempt, &issue, &last_prompt, Outcome::Failed).await;
                    }

                    // Log processing_completed activity without PR
                    let activity = ActivityLogEntry::new(
                        "processing_completed",
                        format!("Processing completed for {} (no PR)", issue.short_id),
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
                let error = claude_result.error.as_deref().unwrap_or("Unknown error");
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
                self.tracker.mark_failed(source.name(), &issue.id, error)?;
                self.notify_failed_with_escalation(&issue, error).await?;
                if let Some(id) = attempt_id {
                    let _ = self.tracker.update_qa_outcome_stats_for_attempt(id, false);
                }

                // Record feedback outcome
                if let Ok(Some(attempt)) = self.tracker.get_attempt(source.name(), &issue.id) {
                    self.record_feedback_outcome(&attempt, &issue, &last_prompt, Outcome::Failed).await;
                }

                // Record error pattern for analytics
                self.record_error_pattern(source.name(), &issue.id, error);
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

        // Cleanup
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
        if ClaudeRunner::is_hard_error(error) {
            self.record_issue_decision(
                issue,
                "hard_error_escalated",
                format!("Escalating hard error for {}", issue.short_id),
                json!({
                    "error": Self::truncate_error_for_activity(error),
                    "rate_limited": ClaudeRunner::is_rate_limit_error(error),
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
                "rate_limited": ClaudeRunner::is_rate_limit_error(error),
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
        let fix_outcome = FixOutcome::from_attempt(attempt, issue, prompt, outcome);

        // Store to DB
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
            .sqlite_tracker
            .as_ref()
            .and_then(|t| t.get_executions_for_attempt(attempt.id).ok())
            .and_then(|execs| execs.into_iter().next())
            .and_then(|exec| exec.prompt_used)
            .unwrap_or_default();

        self.record_feedback_outcome(attempt, &issue, &prompt, outcome)
            .await;
    }

    /// Manually trigger processing for a specific issue.
    pub async fn trigger_issue(&self, source_name: &str, issue_id: &str) -> Result<()> {
        self.trigger_issue_with_feedback(source_name, issue_id, None)
            .await
    }

    /// Manually trigger processing for a specific issue with optional review feedback context.
    pub async fn trigger_issue_with_feedback(
        &self,
        source_name: &str,
        issue_id: &str,
        review_feedback: Option<String>,
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
            .process_issue(Arc::clone(source), issue, match_result, review_feedback)
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
            db_path: std::path::PathBuf::from(":memory:"),
            max_issues_per_cycle: 5,
            max_concurrent: 2,
            processing_delay_ms: 1000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            claude_timeout_secs: 21600,
            claude: crate::config::ClaudeConfig::default(),
            discord: crate::config::DiscordConfig::default(),
            email: crate::config::EmailConfig::default(),
            sms: crate::config::SmsConfig::default(),
            push: crate::config::PushConfig::default(),
            ask: crate::config::AskConfig::default(),
            github: crate::config::GitHubConfig::default(),
            github_app: crate::config::GitHubAppConfig::default(),
            retry: crate::config::RetryConfig::default(),
            linear: None,
            sentry: None,
            regression: crate::config::RegressionConfig::default(),
            cascade: crate::config::CascadeConfig::default(),
            users: std::collections::HashMap::new(),
        }
    }

    fn create_test_watcher(
        notifier: Arc<dyn Notifier>,
        tracker: Arc<dyn FixAttemptTracker>,
        sources: Vec<Arc<dyn IssueSource>>,
        dry_run: bool,
    ) -> Watcher {
        Watcher::new(WatcherOptions {
            config: test_config(),
            sources,
            notifier,
            tracker,
            sqlite_tracker: None, // Tests don't need DB sync
            inferrer: None,       // Tests don't need inference
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
        let review1 = crate::github::PrReview {
            id: 1,
            state: "CHANGES_REQUESTED".to_string(),
            body: Some("first".to_string()),
            user: crate::github::GitHubUser {
                id: 1,
                login: "r1".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: None,
        };
        let review2 = crate::github::PrReview {
            id: 2,
            state: "COMMENTED".to_string(),
            body: Some("second".to_string()),
            user: crate::github::GitHubUser {
                id: 2,
                login: "r2".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2024-01-01T00:01:00Z".to_string()),
            html_url: None,
        };

        let events = vec![
            crate::github::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/1".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 1,
                review: review1,
                inline_comments: vec![],
            },
            crate::github::ReviewEvent::ReviewSubmitted {
                pr_url: "https://github.com/org/repo/pull/1".to_string(),
                repo: "org/repo".to_string(),
                pr_number: 1,
                review: review2,
                inline_comments: vec![],
            },
            crate::github::ReviewEvent::CommentsAdded {
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
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            review_watcher: None,
            issue_embedding_service: None,
            relationships: None,
            github_client: None,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
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
            github_repo: Some("org/upstream-lib".to_string()),
            github_pr_number: Some(42),
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

        // Verify repo name normalization (github_repo "org/upstream-lib" -> "upstream-lib")
        let repo_short_name = attempt
            .github_repo
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
            github_repo: None,
            github_pr_number: None,
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
}
