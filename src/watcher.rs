//! Main watcher that coordinates sources, Claude, and notifications.

use crate::config::Config;
use crate::error::Result;
use crate::feedback::{format_similar_issues_context, IssueEmbeddingService};
use crate::github::{PrReviewState, ReviewWatcher};
use crate::inference::{resolve_repo_for_issue, RepoInferrer, RepoResolution};
use crate::notifier::Notifier;
use crate::repo::{GitOps, RepoIndex};
use crate::retry::RetryManager;
use crate::runner::{ClaudeRunner, ClaudeRunnerConfig};
use crate::source::IssueSource;
use crate::storage::{classify_error, compute_error_hash, FixAttemptTracker, SqliteTracker};
use crate::types::{
    ActivityLogEntry, ErrorPattern, FixAttemptStats, Issue, MatchPriority, MatchResult,
    ProcessingMetric,
};
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
    claude: ClaudeRunner,
    dry_run: bool,
    is_running: AtomicBool,
    processing: RwLock<HashSet<String>>,
    active_processing: AtomicUsize,
    /// Counter for review poll intervals (check reviews every N poll cycles)
    review_poll_counter: AtomicUsize,
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
            dry_run: options.dry_run,
            is_running: AtomicBool::new(false),
            processing: RwLock::new(HashSet::new()),
            active_processing: AtomicUsize::new(0),
            review_poll_counter: AtomicUsize::new(0),
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
        let poll_interval = interval_ms.unwrap_or(self.config.poll_interval_ms);

        tracing::info!("");
        tracing::info!(
            "Starting Claude Watcher{}",
            if self.dry_run { " (DRY RUN)" } else { "" }
        );
        tracing::info!("  Work dir: {:?}", self.config.work_dir);
        tracing::info!("  Known orgs: {}", self.config.known_orgs.len());
        tracing::info!("  Poll interval: {}ms", poll_interval);
        tracing::info!(
            "  Max issues per cycle: {}",
            self.config.max_issues_per_cycle
        );
        tracing::info!("  Max concurrent: {}", self.config.max_concurrent);
        tracing::info!("  Processing delay: {}ms", self.config.processing_delay_ms);
        tracing::info!(
            "  Sources: {}",
            self.sources
                .iter()
                .map(|s| s.display_name())
                .collect::<Vec<_>>()
                .join(", ")
        );

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

                // Check for PRs to auto-close due to issue state changes
                if let Err(e) = self.check_and_auto_close_prs().await {
                    tracing::debug!(error = %e, "Error checking for auto-close PRs");
                }

                // Check for PR reviews (every 3rd poll cycle)
                let review_count = self.review_poll_counter.fetch_add(1, Ordering::SeqCst);
                if review_count.is_multiple_of(3) {
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

        for event in events {
            if !event.requires_action() {
                continue;
            }

            // Get the PR URL from the event
            let pr_url = event.pr_url();
            let feedback_summary = event.get_feedback_summary();

            tracing::info!(
                pr_url = %pr_url,
                "Review feedback received, processing..."
            );

            // Find the original issue for this PR
            if let Some(attempt) = self.tracker.get_attempt_by_pr_url(pr_url)? {
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
        let issues = source.fetch_issues().await?;
        let issue_exists = issues.iter().any(|i| i.id == attempt.issue_id)
            || source.get_issue(&attempt.issue_id).await.is_ok();

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

        // Process the issue (this will create a new commit on the existing branch)
        // Note: The issue will be re-fetched by trigger_issue, which will get the
        // latest description. Review feedback context is passed via the tracker's
        // attempt record for reference.
        if let Some(pr_url) = &attempt.pr_url {
            tracing::info!(
                pr_url = %pr_url,
                "Re-processing issue to address review feedback"
            );

            // Use trigger_issue to re-process
            if let Err(e) = self.trigger_issue(&attempt.source, &attempt.issue_id).await {
                tracing::error!(
                    error = %e,
                    "Failed to trigger re-processing for review feedback"
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
                        if !self.tracker.has_attempted(source.name(), &issue.id) {
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

        Ok(())
    }

    /// Process any issues that are ready for retry.
    async fn process_ready_retries(&self) -> Result<()> {
        let retry_manager = RetryManager::new(self.config.retry.clone(), self.tracker.clone());
        let ready = retry_manager.get_ready_retries()?;

        if ready.is_empty() {
            return Ok(());
        }

        tracing::info!(
            component = "watcher",
            count = ready.len(),
            "Processing ready retries"
        );

        for attempt in ready {
            // Check if we're still running
            if !self.is_running.load(Ordering::SeqCst) {
                break;
            }

            // Check if this issue is already being processed
            let processing_key = format!("{}:{}", attempt.source, attempt.issue_id);
            {
                let processing = self.processing.read().await;
                if processing.contains(&processing_key) {
                    tracing::debug!(
                        short_id = %attempt.short_id,
                        "Issue already being processed, skipping retry"
                    );
                    continue;
                }
            }

            // Wait for concurrency slot
            while self.active_processing.load(Ordering::SeqCst) >= self.config.max_concurrent {
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
            if let Err(e) = self.trigger_issue(&attempt.source, &attempt.issue_id).await {
                tracing::error!(
                    component = "watcher",
                    short_id = %attempt.short_id,
                    error = %e,
                    "Failed to trigger retry"
                );
            }

            // Add delay between retries
            if self.config.processing_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.config.processing_delay_ms)).await;
            }
        }

        Ok(())
    }

    /// Poll a single source.
    async fn poll_source(&self, source: &Arc<dyn IssueSource>) -> Result<()> {
        tracing::info!(source = source.name(), "Fetching issues...");

        let issues = source.fetch_issues().await?;
        tracing::info!(source = source.name(), count = issues.len(), "Found issues");

        // Get already attempted issue IDs
        let attempted_ids = self.tracker.get_attempted_issue_ids(source.name());
        tracing::info!(
            source = source.name(),
            count = attempted_ids.len(),
            "Already attempted issues"
        );

        // Filter and match criteria
        let mut candidates: Vec<(Issue, MatchResult)> = Vec::new();

        let processing = self.processing.read().await;
        for issue in issues {
            // Skip if already attempted
            if attempted_ids.contains(&issue.id) {
                continue;
            }

            // Skip if currently processing
            let processing_key = format!("{}:{}", source.name(), issue.id);
            if processing.contains(&processing_key) {
                continue;
            }

            let match_result = source.matches_criteria(&issue);
            if match_result.matches {
                candidates.push((issue, match_result));
            }
        }
        drop(processing);

        if candidates.is_empty() {
            tracing::info!(source = source.name(), "No new issues to process");
            return Ok(());
        }

        // Sort by priority
        self.sort_by_priority(&mut candidates);

        // Apply max issues per cycle limit
        let to_process: Vec<_> = candidates
            .into_iter()
            .take(self.config.max_issues_per_cycle)
            .collect();

        let to_process_count = to_process.len();
        let skipped = to_process
            .len()
            .saturating_sub(self.config.max_issues_per_cycle);
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
            self.notifier.notify_urgent_issues(&urgent_issues).await?;
        }

        // Process issues with rate limiting
        for (i, (issue, match_result)) in to_process.into_iter().enumerate() {
            if !self.is_running.load(Ordering::SeqCst) {
                break;
            }

            // Wait for concurrency slot
            while self.active_processing.load(Ordering::SeqCst) >= self.config.max_concurrent {
                if !self.is_running.load(Ordering::SeqCst) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            // Process the issue
            let source_clone = Arc::clone(source);
            let this = self;
            this.process_issue(source_clone, issue, match_result).await;

            // Add delay between starting new issues
            if i < self.config.max_issues_per_cycle - 1 && self.config.processing_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.config.processing_delay_ms)).await;
            }
        }

        // Wait for all processing to complete
        while self.active_processing.load(Ordering::SeqCst) > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        // Record processing metrics (don't fail main operation if this fails)
        let metric = ProcessingMetric::new("batch_processed", to_process_count as f64)
            .with_source(source.name().to_string());
        if let Err(e) = self.tracker.record_metric(&metric) {
            tracing::warn!(error = %e, "Failed to record batch processing metric");
        }

        Ok(())
    }

    /// Process a single issue.
    ///
    /// Uses the RepoInferrer engine to determine which repository to use
    /// for fixing the issue.
    async fn process_issue(
        &self,
        source: Arc<dyn IssueSource>,
        issue: Issue,
        match_result: MatchResult,
    ) {
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
                return;
            }
            processing.insert(processing_key.clone());
        }
        self.active_processing.fetch_add(1, Ordering::SeqCst);

        tracing::info!("");
        tracing::info!(short_id = %issue.short_id, title = %issue.title, "Processing issue");
        tracing::info!(short_id = %issue.short_id, reason = %match_result.reason, "Match reason");
        tracing::info!(short_id = %issue.short_id, priority = ?match_result.priority, "Match priority");

        // Infer the target repository using the shared resolution function
        let resolution =
            resolve_repo_for_issue(self.inferrer.as_ref(), &issue, self.sqlite_tracker.as_ref());

        let project_dir = match &resolution {
            RepoResolution::Resolved { project_dir, .. } => project_dir.clone(),
            RepoResolution::Skip { reason } => {
                tracing::debug!(short_id = %issue.short_id, reason = %reason, "Skipping issue");
                // Clean up processing state before returning
                {
                    let mut processing = self.processing.write().await;
                    processing.remove(&processing_key);
                }
                self.active_processing.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        };

        // Ensure repo is up to date: pull latest changes
        if let (Some(github_url), Some(default_branch), Some(repo_name)) = (
            resolution.github_url(),
            resolution.default_branch(),
            resolution.repo_name(),
        ) {
            tracing::info!(
                short_id = %issue.short_id,
                repo = %repo_name,
                "Pulling latest changes"
            );

            if let Err(e) =
                GitOps::ensure_repo_at_path(&project_dir, github_url, default_branch).await
            {
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
                return;
            }

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

        // Extract labels from issue metadata for bug detection
        let labels: Vec<String> = issue.get_metadata("labels").unwrap_or_default();
        if let Err(e) = self.tracker.record_attempt_with_labels(
            source.name(),
            &issue.id,
            &issue.short_id,
            &labels,
        ) {
            tracing::error!(short_id = %issue.short_id, error = %e, "Failed to record attempt");
        }

        // Get the attempt ID for analytics tracking
        let attempt_id = self
            .tracker
            .get_attempt(source.name(), &issue.id)
            .ok()
            .flatten()
            .map(|a| a.id);

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

            let prompt = self.claude.build_prompt_for_issue(&issue, &context, &project_dir);
            let claude_result = self.claude.execute_with_attempt(&prompt, Some(&issue), attempt_id, &project_dir).await?;

            if claude_result.success {
                if let Some(ref pr_url) = claude_result.pr_url {
                    tracing::info!(short_id = %issue.short_id, pr_url = %pr_url, "Success! PR created");
                    self.tracker
                        .mark_success(source.name(), &issue.id, pr_url)?;
                    self.notifier.notify_success(&issue, pr_url).await?;

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
                    tracing::info!(short_id = %issue.short_id, "Completed but no PR URL found");
                    self.tracker.mark_failed(
                        source.name(),
                        &issue.id,
                        "No PR URL found in output",
                    )?;
                    self.notifier.notify_completed(&issue).await?;

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
                tracing::error!(short_id = %issue.short_id, error = %error, "Failed");
                self.tracker.mark_failed(source.name(), &issue.id, error)?;
                self.notifier.notify_failed(&issue, error).await?;

                // Record error pattern for analytics
                self.record_error_pattern(source.name(), &issue.id, error);
            }

            Ok::<_, crate::error::Error>(())
        }
        .await;

        if let Err(ref e) = result {
            tracing::error!(short_id = %issue.short_id, error = %e, "Error processing issue");
            let error_str = e.to_string();
            let _ = self
                .tracker
                .mark_failed(source.name(), &issue.id, &error_str);
            let _ = self.notifier.notify_failed(&issue, &error_str).await;

            // Record error pattern for analytics
            self.record_error_pattern(source.name(), &issue.id, &error_str);
        }

        // Cleanup
        {
            let mut processing = self.processing.write().await;
            processing.remove(&processing_key);
        }
        self.active_processing.fetch_sub(1, Ordering::SeqCst);
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

    /// Manually trigger processing for a specific issue.
    pub async fn trigger_issue(&self, source_name: &str, issue_id: &str) -> Result<()> {
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

        self.process_issue(Arc::clone(source), issue, match_result)
            .await;

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
    }

    impl MockNotifier {
        fn new(enabled: bool) -> Self {
            Self {
                enabled,
                call_count: AtomicUsize::new(0),
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
    }

    impl MockSource {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                issues: vec![],
            }
        }

        fn with_issues(name: &str, issues: Vec<Issue>) -> Self {
            Self {
                name: name.to_string(),
                issues,
            }
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
            MatchResult::matched("Mock match", MatchPriority::Normal)
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
            github: crate::config::GitHubConfig::default(),
            github_app: crate::config::GitHubAppConfig::default(),
            retry: crate::config::RetryConfig::default(),
            linear: None,
            sentry: None,
            regression: crate::config::RegressionConfig::default(),
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

        assert!(tracker.has_attempted("test", "123"));
        watcher.reset_attempt("test", "123").unwrap();
        assert!(!tracker.has_attempted("test", "123"));
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
        assert!(tracker.has_attempted("mock", "1"));
        assert!(tracker.has_attempted("mock", "2"));
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
        assert!(!tracker.has_attempted("mock", "1"));
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
        assert!(!tracker.has_attempted("test", "1"));
        assert!(!tracker.has_attempted("test", "2"));
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
        assert!(tracker.has_attempted("test", "1"));
        assert!(!tracker.has_attempted("test", "2")); // Not recorded in dry run
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
            dry_run: true,
        });

        // Poll should complete successfully
        let result = watcher.poll().await;
        assert!(result.is_ok());
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
        assert!(tracker.has_attempted("test", "123"));

        let watcher = create_test_watcher(notifier, tracker.clone(), sources, false);

        // Reset should succeed
        let result = watcher.reset_attempt("test", "123");
        assert!(result.is_ok());
        assert!(!tracker.has_attempted("test", "123"));
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
}
