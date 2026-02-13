# Multi-Repo Cascade Chaining Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** When a PR merges in an upstream repo, automatically spawn Claude agents in downstream repos to adapt to the changes, cascading transitively through the dependency graph.

**Architecture:** Hook into `PrMonitor`'s merge detection. On merge, look up dependents in `DependencyGraph`, create child `fix_attempt` records, and spawn Claude in each downstream repo with the upstream PR link (Claude fetches diff details itself). Recursion happens naturally — each downstream PR merge triggers another cascade check.

**Tech Stack:** Rust, SQLite, GitHub REST API, existing Claude runner

---

### Task 1: Add `parent_attempt_id` and `cascade_repo` to FixAttempt type

**Files:**
- Modify: `src/types.rs:282-321`

**Step 1: Add fields to FixAttempt struct**

In `src/types.rs`, add two new fields to the `FixAttempt` struct (after `issue_labels` at line 320):

```rust
    /// Parent attempt ID for cascade chains. NULL for root attempts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_attempt_id: Option<i64>,
    /// Target repository for cascade attempts (e.g., "appwrite/appwrite").
    /// NULL for root attempts (original issue fix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cascade_repo: Option<String>,
```

**Step 2: Verify compilation**

Run: `cargo check 2>&1 | head -30`

This will produce errors wherever `FixAttempt` is constructed without the new fields. Fix each one by adding `parent_attempt_id: None, cascade_repo: None` to every construction site. Key locations to check:
- `src/storage/sqlite.rs` — the `row_to_fix_attempt` or equivalent deserialization
- Any test files that construct `FixAttempt` literals

**Step 3: Commit**

```bash
git add src/types.rs src/storage/sqlite.rs
git commit -m "feat: add parent_attempt_id and cascade_repo to FixAttempt"
```

---

### Task 2: Schema migration — add columns and update UNIQUE constraint

**Files:**
- Modify: `src/storage/sqlite.rs:105-126`

**Step 1: Add migration logic after schema creation**

Find the schema creation block at `src/storage/sqlite.rs:103`. After the existing `CREATE TABLE IF NOT EXISTS fix_attempts` block and its indexes (line ~126), add a migration block:

```rust
            -- Migration: Add cascade columns to fix_attempts
            -- parent_attempt_id links child cascade attempts to their parent
            -- cascade_repo identifies which repo a cascade attempt targets
            ALTER TABLE fix_attempts ADD COLUMN parent_attempt_id INTEGER REFERENCES fix_attempts(id);
            ALTER TABLE fix_attempts ADD COLUMN cascade_repo TEXT;
```

Since SQLite `ALTER TABLE ADD COLUMN` fails if the column already exists, wrap this in a Rust check. Find how the existing code handles migrations — look for patterns like checking `PRAGMA table_info`. If no migration pattern exists, use a simple approach:

```rust
// After the main schema creation, run migrations
// Check if parent_attempt_id column exists
let has_parent_col: bool = conn
    .prepare("SELECT parent_attempt_id FROM fix_attempts LIMIT 0")
    .is_ok();

if !has_parent_col {
    conn.execute_batch(
        r#"
        ALTER TABLE fix_attempts ADD COLUMN parent_attempt_id INTEGER REFERENCES fix_attempts(id);
        ALTER TABLE fix_attempts ADD COLUMN cascade_repo TEXT;
        CREATE INDEX IF NOT EXISTS idx_fix_attempts_parent ON fix_attempts(parent_attempt_id);
        "#,
    )?;
    tracing::info!("Migrated fix_attempts: added cascade columns");
}
```

**Important:** The existing `UNIQUE(source, issue_id)` constraint **cannot be changed** in SQLite without recreating the table. Instead, we'll handle uniqueness in application code: before inserting a cascade attempt, check `SELECT 1 FROM fix_attempts WHERE source = ? AND issue_id = ? AND cascade_repo = ?`. The original UNIQUE constraint still works for root attempts (where cascade_repo is NULL).

**Step 2: Update the row deserialization**

Find where `FixAttempt` is constructed from a database row (search for `source:` and `issue_id:` in sqlite.rs). Add:

```rust
parent_attempt_id: row.get("parent_attempt_id").ok().flatten(),
cascade_repo: row.get("cascade_repo").ok().flatten(),
```

**Step 3: Add `record_cascade_attempt` method to SqliteTracker**

```rust
/// Record a cascade fix attempt linked to a parent attempt.
pub fn record_cascade_attempt(
    &self,
    source: &str,
    issue_id: &str,
    short_id: &str,
    parent_attempt_id: i64,
    cascade_repo: &str,
) -> Result<i64> {
    let conn = self.acquire_lock()?;

    // Check if this cascade already exists
    let exists: bool = conn
        .prepare_cached(
            "SELECT 1 FROM fix_attempts WHERE source = ? AND issue_id = ? AND cascade_repo = ?",
        )?
        .exists(params![source, issue_id, cascade_repo])?;

    if exists {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            cascade_repo = cascade_repo,
            "Cascade attempt already exists, skipping"
        );
        // Return the existing attempt ID
        let id: i64 = conn.query_row(
            "SELECT id FROM fix_attempts WHERE source = ? AND issue_id = ? AND cascade_repo = ?",
            params![source, issue_id, cascade_repo],
            |row| row.get(0),
        )?;
        return Ok(id);
    }

    conn.execute(
        r#"INSERT INTO fix_attempts (source, issue_id, short_id, status, attempted_at, parent_attempt_id, cascade_repo)
           VALUES (?, ?, ?, 'pending', datetime('now'), ?, ?)"#,
        params![source, issue_id, short_id, parent_attempt_id, cascade_repo],
    )?;

    let id = conn.last_insert_rowid();
    tracing::info!(
        source = source,
        issue_id = issue_id,
        cascade_repo = cascade_repo,
        parent_attempt_id = parent_attempt_id,
        attempt_id = id,
        "Recorded cascade fix attempt"
    );
    Ok(id)
}
```

**Step 4: Run tests**

Run: `cargo test --lib storage -- --nocapture 2>&1 | tail -20`
Expected: All existing tests pass

**Step 5: Commit**

```bash
git add src/storage/sqlite.rs
git commit -m "feat: add cascade columns to fix_attempts schema with migration"
```

---

### Task 3: Add CascadeConfig to config

**Files:**
- Modify: `src/config.rs:44-74`

**Step 1: Add CascadeConfig struct**

Add near the other config structs in `src/config.rs`:

```rust
/// Configuration for multi-repo cascade chaining.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeConfig {
    /// Whether cascade chaining is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum cascade depth (0 = unlimited).
    #[serde(default)]
    pub max_depth: usize,
}

impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_depth: 0,
        }
    }
}
```

**Step 2: Add to Config struct**

Add to the `Config` struct:

```rust
    /// Cascade configuration for multi-repo chaining.
    #[serde(default)]
    pub cascade: CascadeConfig,
```

**Step 3: Verify compilation**

Run: `cargo check 2>&1 | head -20`
Expected: Clean compilation

**Step 4: Commit**

```bash
git add src/config.rs
git commit -m "feat: add CascadeConfig for multi-repo cascade settings"
```

---

### Task 4: Add RepoRelationships to Watcher and WatcherOptions

**Files:**
- Modify: `src/watcher.rs:26-90`

**Step 1: Add field to WatcherOptions**

In `src/watcher.rs`, add to `WatcherOptions` struct (line ~37):

```rust
    pub relationships: Option<RepoRelationships>,
```

Add the import at the top of the file:

```rust
use crate::repo::RepoRelationships;
```

**Step 2: Add field to Watcher struct**

Add to the `Watcher` struct (line ~57):

```rust
    relationships: Option<RepoRelationships>,
```

**Step 3: Wire in constructor**

In `Watcher::new` (line ~61), add to the struct initialization:

```rust
    relationships: options.relationships,
```

**Step 4: Fix all test WatcherOptions constructions**

Search for `WatcherOptions {` in watcher.rs tests. Add `relationships: None,` to each construction site. There are several test helper functions that build WatcherOptions — find them all with:

Run: `grep -n "WatcherOptions" src/watcher.rs`

Add `relationships: None,` to each.

**Step 5: Verify compilation and tests**

Run: `cargo check 2>&1 | head -20`
Run: `cargo test --lib watcher -- --nocapture 2>&1 | tail -20`
Expected: Clean compilation and all existing tests pass

**Step 6: Commit**

```bash
git add src/watcher.rs
git commit -m "feat: add RepoRelationships to Watcher for cascade support"
```

---

### Task 5: Implement `trigger_cascade` on Watcher

**Files:**
- Modify: `src/watcher.rs`

**Step 1: Add the trigger_cascade method**

Add to `impl Watcher`, after the existing `check_reviews` method (around line 483):

```rust
    /// Trigger cascade processing for downstream repos after a PR is merged.
    ///
    /// Looks up the merged repo in the dependency graph and spawns Claude
    /// in each direct dependent repo with context about the upstream changes.
    pub async fn trigger_cascade(
        &self,
        attempt: &FixAttempt,
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

        let pr_number = match attempt.github_pr_number {
            Some(n) => n,
            None => return Ok(()),
        };

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
        let repo_short_name = github_repo
            .split('/')
            .last()
            .unwrap_or(&github_repo);

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

        // Build the upstream PR URL for the downstream agent to inspect
        let upstream_pr_url = pr_url.to_string();

        // Get the dependency type for context
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
    fn get_cascade_depth(&self, attempt: &FixAttempt) -> usize {
        let mut depth = 0;
        let mut current_parent = attempt.parent_attempt_id;

        while let Some(parent_id) = current_parent {
            depth += 1;
            // Look up the parent attempt
            match self.sqlite_tracker.as_ref().and_then(|t| {
                t.get_attempt_by_id(parent_id).ok().flatten()
            }) {
                Some(parent) => current_parent = parent.parent_attempt_id,
                None => break,
            }
        }

        depth
    }

    /// Execute a cascade fix in a single downstream repo.
    async fn cascade_to_repo(
        &self,
        parent_attempt: &FixAttempt,
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

        let project_dir = match resolution {
            crate::inference::RepoResolution::Resolved { project_dir, .. } => project_dir,
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

        // Determine the full github_repo name for the downstream
        let downstream_github_repo = self.inferrer.as_ref()
            .and_then(|inf| {
                inf.with_index(|index| {
                    index.get_repo_by_name(downstream_repo_name)
                        .map(|r| r.github_url.clone())
                }).ok().flatten()
            })
            .unwrap_or_else(|| downstream_repo_name.to_string());

        let attempt_id = sqlite.record_cascade_attempt(
            &parent_attempt.source,
            &parent_attempt.issue_id,
            &parent_attempt.short_id,
            parent_attempt.id,
            &downstream_github_repo,
        )?;

        // Build the cascade prompt — pass the PR link so Claude can fetch details itself
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

        // Pull latest before running Claude
        if let Err(e) = crate::repo::GitOps::pull_latest(&project_dir).await {
            tracing::warn!(
                downstream = %downstream_repo_name,
                error = %e,
                "Failed to pull latest, continuing anyway"
            );
        }

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
                self.tracker.mark_success(
                    &parent_attempt.source,
                    &parent_attempt.issue_id,
                    pr_url,
                )?;

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
                let activity = crate::types::ActivityLogEntry::new(
                    "cascade_pr_created",
                    format!(
                        "Cascade PR created in {} for upstream {} PR #{}",
                        downstream_repo_name, upstream_repo, upstream_pr_number
                    ),
                )
                .with_source(parent_attempt.source.clone())
                .with_issue(parent_attempt.issue_id.clone(), parent_attempt.short_id.clone());
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
```

**Step 2: Add helper methods to SqliteTracker**

In `src/storage/sqlite.rs`, add:

```rust
/// Get a fix attempt by its database ID.
pub fn get_attempt_by_id(&self, id: i64) -> Result<Option<FixAttempt>> {
    let conn = self.acquire_lock()?;
    let mut stmt = conn.prepare_cached(
        "SELECT * FROM fix_attempts WHERE id = ?",
    )?;
    let attempt = stmt
        .query_row(params![id], |row| self.row_to_fix_attempt(row))
        .optional()?;
    Ok(attempt)
}

/// Update a cascade attempt's PR info.
pub fn update_attempt_pr(
    &self,
    attempt_id: i64,
    pr_url: &str,
    github_repo: &str,
    pr_number: i64,
) -> Result<()> {
    let conn = self.acquire_lock()?;
    conn.execute(
        "UPDATE fix_attempts SET pr_url = ?, github_repo = ?, github_pr_number = ?, status = 'success' WHERE id = ?",
        params![pr_url, github_repo, pr_number, attempt_id],
    )?;
    Ok(())
}

/// Mark a cascade attempt as failed.
pub fn mark_cascade_failed(&self, attempt_id: i64, error: &str) -> Result<()> {
    let conn = self.acquire_lock()?;
    conn.execute(
        "UPDATE fix_attempts SET status = 'failed', error_message = ? WHERE id = ?",
        params![error, attempt_id],
    )?;
    Ok(())
}
```

**Step 3: Add `resolve_repo_for_cascade` to inference module**

In `src/inference/mod.rs`, add a function that resolves a repo by name (not by issue):

```rust
/// Resolve a repository path for cascade processing.
/// Unlike issue-based resolution, this looks up a repo directly by name.
pub fn resolve_repo_for_cascade(
    inferrer: Option<&RepoInferrer>,
    repo_name: &str,
) -> RepoResolution {
    let inferrer = match inferrer {
        Some(i) => i,
        None => {
            return RepoResolution::Skip {
                reason: "No inferrer available".to_string(),
            }
        }
    };

    inferrer.with_index(|index| {
        match index.get_repo_by_name(repo_name) {
            Some(repo) => RepoResolution::Resolved {
                project_dir: PathBuf::from(&repo.path),
                repo_name: repo.name.clone(),
                repo_id: Some(repo.id),
                github_url: repo.github_url.clone(),
                default_branch: repo.default_branch.clone().unwrap_or_else(|| "main".to_string()),
            },
            None => RepoResolution::Skip {
                reason: format!("Repository '{}' not found in index", repo_name),
            },
        }
    }).unwrap_or_else(|e| RepoResolution::Skip {
        reason: format!("Index error: {}", e),
    })
}
```

Check if `RepoIndex` has a `get_repo_by_name` method. If not, you'll need to add one — it should search the indexed repos by name (try exact match, then partial match on the repo portion of "org/repo").

**Step 4: Verify compilation**

Run: `cargo check 2>&1 | head -30`

Fix any compilation errors. Common issues:
- Missing imports (`use crate::repo::RepoRelationships;`)
- `row_to_fix_attempt` may need updating for new columns
- `GitOps::pull_latest` signature may differ — check exact method name

**Step 5: Commit**

```bash
git add src/watcher.rs src/storage/sqlite.rs src/inference/mod.rs
git commit -m "feat: implement trigger_cascade and cascade_to_repo on Watcher"
```

---

### Task 6: Wire cascade into PrMonitor merge handling

**Files:**
- Modify: `src/main.rs:1686-1700` (WatcherOptions construction)
- Modify: `src/main.rs:1858-1932` (PrMonitor merge handling)

**Step 1: Pass RepoRelationships to WatcherOptions**

In `src/main.rs`, find the WatcherOptions construction around line 1687. Before it, build the relationships:

```rust
        // Build dependency graph for cascade support
        let relationships = if config.cascade.enabled {
            let mut rels = RepoRelationships::with_defaults();
            // Also load any DB-stored dependencies
            let db_deps = sqlite_tracker.list_all_dependencies().unwrap_or_default();
            for dep in db_deps {
                rels.add_dependency(
                    &dep.upstream,
                    &dep.downstream,
                    dep.dep_type,
                    dep.version_pattern.clone(),
                ).ok();
            }
            Some(rels)
        } else {
            None
        };
```

Then add to the WatcherOptions struct:

```rust
            Some(Arc::new(Watcher::new(WatcherOptions {
                // ... existing fields ...
                relationships,
            })))
```

**Step 2: Add cascade call to PrMonitor merge block**

In the `prs monitor` continuous command (around line 1897), after the existing merge handling, add:

```rust
                                PrStatus::Merged => {
                                    tracing::info!(component = "pr_monitor", short_id = %update.short_id, pr_url = %update.pr_url, "PR merged");

                                    // ... existing auto-resolve code ...

                                    // Trigger cascade to downstream repos
                                    if config.cascade.enabled {
                                        if let Some(ref watcher) = watcher {
                                            if let Ok(Some(attempt)) = tracker.get_attempt(&update.source, &update.issue_id) {
                                                if let Err(e) = watcher.trigger_cascade(&attempt, &update.pr_url).await {
                                                    tracing::error!(
                                                        component = "cascade",
                                                        error = %e,
                                                        "Failed to trigger cascade"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
```

Also ensure the `watcher` variable is in scope — it may need to be created similarly to how it's done in the daemon command.

**Step 3: Also wire cascade into the daemon's watcher poll loop**

The daemon uses `Watcher::check_reviews()` for PR review feedback. We need to also add PR merge checking to the daemon. The simplest way: add a `check_pr_merges_and_cascade` method to Watcher that checks for merged PRs and triggers cascade.

Add to `Watcher` struct:

```rust
    github_client: Option<GitHubClient>,
```

Add to `WatcherOptions`:

```rust
    pub github_client: Option<GitHubClient>,
```

Add a `check_pr_merges_and_cascade` method to Watcher:

```rust
    /// Check for merged PRs and trigger cascade processing.
    pub async fn check_pr_merges_and_cascade(&self) -> Result<()> {
        let github_client = match &self.github_client {
            Some(c) => c,
            None => return Ok(()),
        };

        if !self.config.cascade.enabled {
            return Ok(());
        }

        // Get all successful attempts with PRs that haven't been merged yet
        let pending_prs = self.tracker.get_pending_prs()?;

        for attempt in &pending_prs {
            let repo = match &attempt.github_repo {
                Some(r) => r,
                None => continue,
            };
            let pr_number = match attempt.github_pr_number {
                Some(n) => n,
                None => continue,
            };

            match github_client.get_pr_status(repo, pr_number).await {
                Ok(PrStatus::Merged) => {
                    self.tracker.mark_merged(&attempt.source, &attempt.issue_id)?;
                    let pr_url = attempt.pr_url.as_deref().unwrap_or("");
                    if let Err(e) = self.trigger_cascade(attempt, pr_url).await {
                        tracing::error!(
                            component = "cascade",
                            short_id = %attempt.short_id,
                            error = %e,
                            "Failed to trigger cascade after merge"
                        );
                    }
                }
                Ok(_) => {} // Still open or closed
                Err(e) => {
                    tracing::debug!(
                        short_id = %attempt.short_id,
                        error = %e,
                        "Failed to check PR status"
                    );
                }
            }
        }

        Ok(())
    }
```

Add call in `Watcher::poll()` method (after `process_ready_retries` around line 636):

```rust
        // Check for PR merges and trigger cascades
        if !self.dry_run {
            if let Err(e) = self.check_pr_merges_and_cascade().await {
                tracing::error!(component = "watcher", error = %e, "Error checking PR merges for cascade");
            }
        }
```

**Step 4: Update daemon WatcherOptions construction**

In the daemon section of main.rs (around line 1687), add:

```rust
        let github_client_for_watcher = if config.cascade.enabled && config.is_github_enabled() {
            Some(GitHubClient::new(config.github.clone()))
        } else {
            None
        };
```

Add `github_client: github_client_for_watcher,` to the WatcherOptions.

**Step 5: Verify compilation**

Run: `cargo check 2>&1 | head -30`
Expected: Clean compilation

**Step 6: Commit**

```bash
git add src/main.rs src/watcher.rs
git commit -m "feat: wire cascade into PrMonitor merge handling and daemon poll loop"
```

---

### Task 7: Export new types from lib.rs

**Files:**
- Modify: `src/lib.rs`

**Step 1: Ensure new public items are exported**

Check `src/lib.rs` and add exports for:
- `CascadeConfig` from config
- `resolve_repo_for_cascade` from inference
- Any new public methods

**Step 2: Verify compilation**

Run: `cargo check 2>&1 | head -20`

**Step 3: Commit**

```bash
git add src/lib.rs
git commit -m "feat: export cascade types from lib.rs"
```

---

### Task 8: Write integration test for cascade flow

**Files:**
- Modify: `src/watcher.rs` (test module) or create `tests/cascade_test.rs`

**Step 1: Write the test**

Add to the test module in `src/watcher.rs`:

```rust
#[tokio::test]
async fn test_cascade_triggers_on_merge() {
    // Setup: Create a watcher with relationships and a mock tracker
    let mut relationships = RepoRelationships::new();
    relationships.add_repository(Repository::new("upstream-lib"));
    relationships.add_repository(Repository::new("downstream-app"));
    relationships
        .add_dependency("upstream-lib", "downstream-app", DependencyType::Composer, None)
        .unwrap();

    // Create a FixAttempt that simulates a merged upstream PR
    let attempt = FixAttempt {
        id: 1,
        issue_id: "ISSUE-123".to_string(),
        short_id: "ISSUE-123".to_string(),
        source: "linear".to_string(),
        attempted_at: Utc::now(),
        pr_url: Some("https://github.com/org/upstream-lib/pull/42".to_string()),
        github_repo: Some("org/upstream-lib".to_string()),
        github_pr_number: Some(42),
        status: FixAttemptStatus::Merged,
        error_message: None,
        merged_at: Some(Utc::now()),
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
}
```

**Step 2: Run test**

Run: `cargo test --lib watcher::tests::test_cascade_triggers_on_merge -- --nocapture 2>&1 | tail -10`
Expected: PASS

**Step 3: Commit**

```bash
git add src/watcher.rs
git commit -m "test: add integration test for cascade trigger on merge"
```

---

### Task 9: Log cascade startup info and add documentation

**Files:**
- Modify: `src/watcher.rs:258-290` (start method logging)

**Step 1: Add cascade status to startup logging**

In `Watcher::start()`, after the existing startup logs (around line 274), add:

```rust
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
```

**Step 2: Verify compilation and all tests pass**

Run: `cargo check 2>&1 | head -20`
Run: `cargo test 2>&1 | tail -30`
Expected: All pass

**Step 3: Commit**

```bash
git add src/watcher.rs
git commit -m "feat: log cascade configuration on watcher startup"
```

---

## Verification Checklist

After all tasks are complete:

1. `cargo check` — clean compilation
2. `cargo test` — all tests pass
3. `cargo clippy` — no warnings
4. Manual verification: set `cascade.enabled: true` in claudear.yaml and run `claudear daemon` — should show "Cascade: enabled" in startup logs
5. Dry run: merge a test PR in an upstream repo and verify the cascade attempt is recorded in the database
