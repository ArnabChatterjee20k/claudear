# Multi-Repo Cascade Chaining

## Problem

When a fix lands in an upstream repository (e.g., `utopia-database`), downstream repos that depend on it (e.g., `appwrite`, then `cloud`) need corresponding updates. The dependency graph and cascading change structures exist but aren't wired into the processing loop.

## Design Decisions

- **Trigger**: On PR merge (not creation) — avoids wasted work if upstream is rejected
- **Depth**: Full transitive cascade via BFS through dependency graph
- **Task**: Context-aware — downstream Claude agent receives upstream diff + original issue context
- **Schema**: Child attempts linked via `parent_attempt_id` on `fix_attempts`

## Architecture

```
PR merged in repo X (detected by PrMonitor)
  → Look up X in DependencyGraph
  → For each direct dependent Y:
    → Create child fix_attempt (parent_attempt_id = X's attempt)
    → Fetch upstream PR diff via GitHub API
    → Build cascade prompt (issue context + diff + instructions)
    → Spawn Claude runner in repo Y
    → Register new PR for review watching
    → When Y's PR merges → repeat for Y's dependents
```

Recursion is natural: each merged child PR triggers `PrMonitor` again, which calls `trigger_cascade`, which finds the next level.

## Schema Changes

### fix_attempts table

```sql
ALTER TABLE fix_attempts ADD COLUMN parent_attempt_id INTEGER REFERENCES fix_attempts(id);
ALTER TABLE fix_attempts ADD COLUMN cascade_repo TEXT;
```

UNIQUE constraint changes: `UNIQUE(source, issue_id)` → `UNIQUE(source, issue_id, cascade_repo)`

New index: `CREATE INDEX idx_fix_attempts_parent ON fix_attempts(parent_attempt_id)`

Root attempts have `cascade_repo = NULL`. Child attempts set `cascade_repo` to the target repo name (e.g., `appwrite/appwrite`). SQLite treats NULL as distinct in UNIQUE, so the original constraint behavior is preserved for root attempts.

## New: GitHubClient::get_pr_diff

```rust
pub async fn get_pr_diff(&self, repo: &str, pr_number: u64) -> Result<String>
```

- Uses `Accept: application/vnd.github.v3.diff` header
- Truncates to configurable max size (default 50KB)

## Watcher Changes

### New field

```rust
pub struct Watcher {
    // ... existing fields ...
    relationships: Option<RepoRelationships>,
}
```

### New method: trigger_cascade

```rust
async fn trigger_cascade(&self, attempt: &FixAttempt, update: &PrStatusUpdate) -> Result<()>
```

1. Get `github_repo` from attempt
2. Normalize repo name for dependency graph lookup
3. Call `relationships.get_dependants(repo_name)` for direct dependents
4. For each dependent:
   a. Fetch upstream PR diff
   b. Create child `fix_attempt` with `parent_attempt_id`
   c. Build cascade prompt
   d. Run Claude in dependent repo directory
   e. Register resulting PR for review watching

### Integration point

Called from `main.rs` in the PrMonitor merge handling block (line ~1897), right after the existing merge processing.

## Cascade Prompt Template

```
A dependency has been updated in {upstream_repo}.

## Original Issue
{issue_title}: {issue_description}

## Upstream Changes (PR #{pr_number} in {upstream_repo})
{truncated_diff}

## Your Task
This repository ({downstream_repo}) depends on {upstream_repo} via {dep_type}.
Review the upstream changes and make any necessary adaptations:
- Update dependency version if needed
- Adapt to any API changes
- Update tests that exercise the changed functionality
- Ensure the project builds and tests pass
```

## Configuration

```yaml
cascade:
  enabled: true           # Master switch
  max_diff_size: 51200    # Max bytes of upstream diff to include (50KB)
  max_depth: 0            # 0 = unlimited transitive cascade
```

## Files to Modify

1. `src/storage/sqlite.rs` — Schema migration, new query methods
2. `src/types.rs` — Add `parent_attempt_id` and `cascade_repo` to `FixAttempt`
3. `src/github.rs` — Add `get_pr_diff()` to `GitHubClient`
4. `src/watcher.rs` — Add `RepoRelationships` field, `trigger_cascade` method
5. `src/config.rs` — Add `CascadeConfig` struct
6. `src/main.rs` — Wire up relationships to watcher, call `trigger_cascade` on merge
7. `src/storage/mod.rs` — Extend `FixAttemptTracker` trait if needed
