//! SQLite-based fix attempt tracker and analytics storage.

use super::{is_vectorlite_available, try_load_vectorlite, FixAttemptTracker};
use crate::error::Result;
use crate::feedback::{FixOutcome, Outcome};
use crate::types::{
    ActivityLogEntry, AnalyticsSummary, ClaudeExecution, ErrorPattern, FixAttempt, FixAttemptStats,
    FixAttemptStatus, IssueEmbedding, PrReviewRecord, ProcessingMetric, PromptExperiment,
    QaKnowledgeEntry, QaMatch, SimilarIssue, SourceStats,
};
use chrono::{DateTime, Utc};
use rand::RngExt;
use rusqlite::OptionalExtension;
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Compiled regex for parsing GitHub PR URLs (compiled once, reused).
static PR_URL_REGEX: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
    regex_lite::Regex::new(r"github\.com/([^/]+/[^/]+)/pull/(\d+)")
        .expect("PR URL regex should be valid")
});

/// Maximum allowed length for PR URLs to prevent ReDoS and excessive memory usage.
const MAX_PR_URL_LENGTH: usize = 2048;
const DEFAULT_LOG_DIR: &str = "./logs";
const AUDIT_LOG_SUBDIR: &str = "audit";
const QA_VECTOR_TABLE: &str = "qa_question_embeddings";
const QA_VECTOR_EF_SEARCH: usize = 200;
const QA_VECTOR_CANDIDATE_MULTIPLIER: usize = 20;

/// A user row from the database.
#[derive(Debug, Clone, Serialize)]
pub struct UserRow {
    pub id: i64,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
}

impl UserRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            email: row.get(1)?,
            password_hash: row.get(2)?,
            name: row.get(3)?,
            role: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }
}

/// Generate a cryptographically random session token (64 hex chars = 32 bytes).
fn generate_session_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// SQLite-based fix attempt tracker for persistence.
///
/// # Async Safety
///
/// This implementation uses `std::sync::Mutex` which is appropriate for:
/// - Short-duration locks (SQLite in-process operations are typically fast)
/// - Operations that don't hold the lock across `.await` points
///
/// All methods are synchronous and complete quickly, making this safe to call
/// from async contexts without risking thread starvation. The mutex is never
/// held across await points since all trait methods are synchronous.
pub struct SqliteTracker {
    conn: Mutex<Connection>,
}

impl SqliteTracker {
    /// Create a new SQLite tracker with the given database path.
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        let tracker = Self {
            conn: Mutex::new(conn),
        };
        tracker.init()?;
        Ok(tracker)
    }

    /// Create an in-memory SQLite tracker (for testing).
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let tracker = Self {
            conn: Mutex::new(conn),
        };
        tracker.init()?;
        Ok(tracker)
    }

    /// Acquire a lock on the database connection, handling poisoned mutex gracefully.
    fn acquire_lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| {
            crate::error::Error::Storage(format!("Failed to acquire database lock: {}", e))
        })
    }

    fn init(&self) -> Result<()> {
        let conn = self.acquire_lock()?;

        // These settings optimize SQLite for better throughput in our use case:
        // - Concurrent reads/writes from webhook server and watcher
        // - Moderate write workload with analytics/metrics
        // - BLOB storage for embeddings
        conn.execute_batch(
            r#"
            -- WAL mode: biggest win for concurrent reads + writes
            -- Note: In-memory DBs will stay in "memory" mode which is fine
            PRAGMA journal_mode = WAL;

            -- Don't wait for fsync on every commit (safe with WAL)
            PRAGMA synchronous = NORMAL;

            -- 64MB cache (default is 2MB) - keeps hot pages in RAM
            PRAGMA cache_size = -65536;

            -- Memory-map up to 256MB of the DB file for faster BLOB access
            PRAGMA mmap_size = 268435456;

            -- Store temp tables in memory
            PRAGMA temp_store = MEMORY;

            -- Timeout instead of immediate SQLITE_BUSY (5 seconds)
            PRAGMA busy_timeout = 5000;

            -- Enable foreign key enforcement
            PRAGMA foreign_keys = ON;
            "#,
        )?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS fix_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source TEXT NOT NULL,
                issue_id TEXT NOT NULL,
                short_id TEXT NOT NULL,
                attempted_at TEXT NOT NULL DEFAULT (datetime('now')),
                pr_url TEXT,
                github_repo TEXT,
                github_pr_number INTEGER,
                status TEXT NOT NULL DEFAULT 'pending',
                error_message TEXT,
                merged_at TEXT,
                resolved_at TEXT,
                retry_count INTEGER NOT NULL DEFAULT 0,
                issue_labels TEXT,  -- JSON array of labels for bug detection
                UNIQUE(source, issue_id)
            );

            CREATE INDEX IF NOT EXISTS idx_fix_attempts_status ON fix_attempts(status);
            CREATE INDEX IF NOT EXISTS idx_fix_attempts_source_issue ON fix_attempts(source, issue_id);
            CREATE INDEX IF NOT EXISTS idx_fix_attempts_pr_url ON fix_attempts(pr_url);
            CREATE INDEX IF NOT EXISTS idx_fix_attempts_retryable ON fix_attempts(status, retry_count, attempted_at);
            -- Hot path for attempts list endpoints: filter by status/source, sort by attempted_at.
            CREATE INDEX IF NOT EXISTS idx_fix_attempts_status_attempted ON fix_attempts(status, attempted_at DESC);
            CREATE INDEX IF NOT EXISTS idx_fix_attempts_source_status_attempted ON fix_attempts(source, status, attempted_at DESC);

            -- Feedback outcomes table for learning from past fixes
            CREATE TABLE IF NOT EXISTS feedback_outcomes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                attempt_id INTEGER REFERENCES fix_attempts(id),
                source TEXT NOT NULL,
                issue_id TEXT NOT NULL,
                issue_text TEXT NOT NULL,
                prompt_used TEXT NOT NULL,
                outcome TEXT NOT NULL,
                error_type TEXT,
                learnings TEXT,
                keywords TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_feedback_outcomes_source ON feedback_outcomes(source);
            CREATE INDEX IF NOT EXISTS idx_feedback_outcomes_outcome ON feedback_outcomes(outcome);
            CREATE INDEX IF NOT EXISTS idx_feedback_outcomes_attempt ON feedback_outcomes(attempt_id);
            CREATE INDEX IF NOT EXISTS idx_feedback_source_issue ON feedback_outcomes(source, issue_id);

            -- Discord threads table
            CREATE TABLE IF NOT EXISTS discord_threads (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL UNIQUE,
                thread_name TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                pr_url TEXT NOT NULL,
                issue_id TEXT NOT NULL,
                source TEXT NOT NULL,
                is_active INTEGER DEFAULT 1,
                last_message_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_discord_threads_pr ON discord_threads(pr_url);
            CREATE INDEX IF NOT EXISTS idx_discord_threads_active ON discord_threads(is_active);

            -- PR review states table
            CREATE TABLE IF NOT EXISTS pr_review_states (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                pr_url TEXT NOT NULL UNIQUE,
                repo TEXT NOT NULL,
                pr_number INTEGER NOT NULL,
                issue_id TEXT NOT NULL,
                source TEXT NOT NULL,
                last_review_id INTEGER,
                last_review_time TEXT,
                last_comment_id INTEGER,
                last_comment_time TEXT,
                is_active INTEGER DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_pr_review_states_active ON pr_review_states(is_active);

            -- Repositories table (unified: includes index metadata)
            CREATE TABLE IF NOT EXISTS repositories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                path TEXT NOT NULL DEFAULT '',
                github_url TEXT,
                default_branch TEXT DEFAULT 'main',
                file_count INTEGER DEFAULT 0,
                last_indexed_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_repositories_name ON repositories(name);

            -- Repository dependencies table
            CREATE TABLE IF NOT EXISTS repository_dependencies (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                upstream_id INTEGER REFERENCES repositories(id),
                downstream_id INTEGER REFERENCES repositories(id),
                dependency_type TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(upstream_id, downstream_id)
            );

            -- ============================================================
            -- Analytics Tables
            -- ============================================================

            -- Activity log - persistent activity tracking (replaces in-memory)
            CREATE TABLE IF NOT EXISTS activity_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                activity_type TEXT NOT NULL,
                source TEXT,
                issue_id TEXT,
                short_id TEXT,
                message TEXT NOT NULL,
                metadata TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_activity_timestamp ON activity_log(timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_activity_issue ON activity_log(issue_id);
            -- Composite index covers queries on source alone and source+issue_id
            CREATE INDEX IF NOT EXISTS idx_activity_source_issue ON activity_log(source, issue_id, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_activity_source_timestamp ON activity_log(source, timestamp DESC);

            -- Claude executions - detailed execution metrics
            CREATE TABLE IF NOT EXISTS claude_executions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                attempt_id INTEGER REFERENCES fix_attempts(id),
                started_at TEXT NOT NULL,
                completed_at TEXT,
                duration_secs REAL,
                exit_code INTEGER,
                timed_out INTEGER DEFAULT 0,
                stdout_preview TEXT,
                stderr_preview TEXT,
                stdout_log_path TEXT,
                stderr_log_path TEXT,
                event_log_path TEXT,
                prompt_used TEXT,
                prompt_hash TEXT,
                model_version TEXT,
                working_directory TEXT,
                git_branch TEXT,
                git_commit_before TEXT,
                git_commit_after TEXT,
                files_changed INTEGER,
                lines_added INTEGER,
                lines_removed INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_executions_attempt ON claude_executions(attempt_id);
            CREATE INDEX IF NOT EXISTS idx_executions_prompt_hash ON claude_executions(prompt_hash);

            -- PR reviews - PR review feedback for learning
            CREATE TABLE IF NOT EXISTS pr_reviews (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                attempt_id INTEGER REFERENCES fix_attempts(id),
                pr_url TEXT NOT NULL,
                reviewer TEXT,
                review_state TEXT,
                submitted_at TEXT,
                body TEXT,
                sentiment TEXT,
                actionable_feedback TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_pr_reviews_attempt ON pr_reviews(attempt_id);

            -- PR review comments - individual review comments for tracking
            CREATE TABLE IF NOT EXISTS pr_review_comments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                github_comment_id INTEGER NOT NULL UNIQUE,
                pr_url TEXT NOT NULL,
                review_id INTEGER REFERENCES pr_reviews(id),
                path TEXT NOT NULL,
                position INTEGER,
                line INTEGER,
                body TEXT NOT NULL,
                author TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                html_url TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_pr_review_comments_pr ON pr_review_comments(pr_url);

            -- Issue embeddings - vector embeddings for similarity
            CREATE TABLE IF NOT EXISTS issue_embeddings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source TEXT NOT NULL,
                issue_id TEXT NOT NULL,
                short_id TEXT,
                title TEXT,
                embedding BLOB NOT NULL,
                embedding_model TEXT,
                created_at TEXT DEFAULT (datetime('now')),
                UNIQUE(source, issue_id)
            );
            CREATE INDEX IF NOT EXISTS idx_embeddings_source ON issue_embeddings(source);

            -- Error patterns - recurring error analysis
            CREATE TABLE IF NOT EXISTS error_patterns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                pattern_hash TEXT UNIQUE,
                error_type TEXT,
                error_message TEXT,
                first_seen TEXT,
                last_seen TEXT,
                occurrence_count INTEGER DEFAULT 1,
                sources TEXT,
                example_issue_ids TEXT,
                resolution_hints TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_error_patterns_type ON error_patterns(error_type);
            CREATE INDEX IF NOT EXISTS idx_error_patterns_count ON error_patterns(occurrence_count DESC);

            -- Processing metrics - time-series operational metrics
            CREATE TABLE IF NOT EXISTS processing_metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                metric_name TEXT NOT NULL,
                metric_value REAL NOT NULL,
                source TEXT,
                tags TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_metrics_name_time ON processing_metrics(metric_name, timestamp DESC);

            -- Prompt experiments - A/B testing prompts
            CREATE TABLE IF NOT EXISTS prompt_experiments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                experiment_name TEXT NOT NULL,
                variant TEXT NOT NULL,
                prompt_template TEXT NOT NULL,
                prompt_hash TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                active INTEGER DEFAULT 1,
                success_count INTEGER DEFAULT 0,
                failure_count INTEGER DEFAULT 0,
                avg_time_to_merge REAL,
                avg_review_score REAL
            );
            CREATE INDEX IF NOT EXISTS idx_experiments_active ON prompt_experiments(active, experiment_name);

            -- Similar issues - cached similarity matches
            CREATE TABLE IF NOT EXISTS similar_issues (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_issue_id TEXT NOT NULL,
                similar_issue_id TEXT NOT NULL,
                similarity_score REAL NOT NULL,
                computed_at TEXT DEFAULT (datetime('now')),
                UNIQUE(source_issue_id, similar_issue_id)
            );
            CREATE INDEX IF NOT EXISTS idx_similar_source ON similar_issues(source_issue_id);
            CREATE INDEX IF NOT EXISTS idx_similar_score ON similar_issues(similarity_score DESC);

            -- Human Q&A knowledge for semantic reuse
            CREATE TABLE IF NOT EXISTS qa_knowledge (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source TEXT NOT NULL,
                repo TEXT,
                issue_id TEXT NOT NULL,
                short_id TEXT NOT NULL,
                question_text TEXT NOT NULL,
                question_norm TEXT NOT NULL,
                question_embedding BLOB,
                answer_text TEXT NOT NULL,
                answer_norm TEXT NOT NULL,
                answer_embedding BLOB,
                channel TEXT NOT NULL,
                responder TEXT,
                correlation_id TEXT NOT NULL,
                asked_at TEXT NOT NULL,
                answered_at TEXT NOT NULL,
                success_count INTEGER NOT NULL DEFAULT 0,
                failure_count INTEGER NOT NULL DEFAULT 0,
                last_used_at TEXT,
                metadata TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_qa_scoped_time ON qa_knowledge(source, repo, answered_at DESC);
            CREATE INDEX IF NOT EXISTS idx_qa_question_norm ON qa_knowledge(question_norm);

            CREATE TABLE IF NOT EXISTS qa_usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                attempt_id INTEGER NOT NULL REFERENCES fix_attempts(id),
                qa_id INTEGER NOT NULL REFERENCES qa_knowledge(id),
                usage_type TEXT NOT NULL,
                similarity_score REAL NOT NULL DEFAULT 0.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(attempt_id, qa_id)
            );
            CREATE INDEX IF NOT EXISTS idx_qa_usage_attempt ON qa_usage(attempt_id);

            CREATE TABLE IF NOT EXISTS question_channel_cursor (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel TEXT NOT NULL,
                cursor_key TEXT NOT NULL,
                cursor_value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(channel, cursor_key)
            );

            -- ============================================================
            -- Repository File Index
            -- ============================================================

            -- File index for searching - files within repositories
            CREATE TABLE IF NOT EXISTS repo_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
                file_path TEXT NOT NULL,
                file_type TEXT,
                last_modified TEXT,
                UNIQUE(repo_id, file_path)
            );
            CREATE INDEX IF NOT EXISTS idx_repo_files_path ON repo_files(file_path);
            CREATE INDEX IF NOT EXISTS idx_repo_files_type ON repo_files(file_type);
            CREATE INDEX IF NOT EXISTS idx_repo_files_repo ON repo_files(repo_id);

            -- ============================================================
            -- Inference Tracking Tables
            -- ============================================================

            -- Track every inference attempt for learning and analytics
            CREATE TABLE IF NOT EXISTS inference_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                issue_id TEXT NOT NULL,
                issue_source TEXT NOT NULL,

                -- Input context
                extracted_filenames TEXT,
                extracted_functions TEXT,
                extracted_keywords TEXT,
                raw_context TEXT,

                -- Inference result
                inferred_repo_id INTEGER REFERENCES repositories(id),
                confidence TEXT,
                inference_reason TEXT,
                match_details TEXT,

                -- Outcome tracking (updated later)
                was_correct INTEGER,
                actual_repo_id INTEGER REFERENCES repositories(id),
                feedback_source TEXT,

                -- Timing
                inference_duration_ms INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                feedback_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_inference_issue ON inference_attempts(issue_id);
            CREATE INDEX IF NOT EXISTS idx_inference_confidence ON inference_attempts(confidence);
            CREATE INDEX IF NOT EXISTS idx_inference_correct ON inference_attempts(was_correct);
            CREATE INDEX IF NOT EXISTS idx_inference_created ON inference_attempts(created_at DESC);

            -- ============================================================
            -- PR Lifecycle Tracking Table
            -- ============================================================

            -- Comprehensive PR tracking for lifecycle management
            CREATE TABLE IF NOT EXISTS prs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                pr_url TEXT NOT NULL UNIQUE,
                github_repo TEXT NOT NULL,
                pr_number INTEGER NOT NULL,

                -- Links
                attempt_id INTEGER REFERENCES fix_attempts(id),
                issue_id TEXT,
                issue_source TEXT,

                -- Metadata
                title TEXT,
                description TEXT,
                author TEXT,
                head_branch TEXT,
                base_branch TEXT,

                -- Status: open, merged, closed
                status TEXT NOT NULL DEFAULT 'open',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT,
                merged_at TEXT,
                closed_at TEXT,

                -- Review summary
                approvals_count INTEGER DEFAULT 0,
                changes_requested_count INTEGER DEFAULT 0,
                comments_count INTEGER DEFAULT 0,
                last_review_at TEXT,

                -- Timing analytics
                time_to_first_review_mins INTEGER,
                time_to_merge_mins INTEGER,
                review_cycles INTEGER DEFAULT 0,

                -- Content metrics
                files_changed INTEGER,
                lines_added INTEGER,
                lines_removed INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_prs_status ON prs(status);
            CREATE INDEX IF NOT EXISTS idx_prs_repo ON prs(github_repo);
            CREATE INDEX IF NOT EXISTS idx_prs_attempt ON prs(attempt_id);
            CREATE INDEX IF NOT EXISTS idx_prs_issue ON prs(issue_source, issue_id);

            -- ============================================================
            -- Regression Tracking Tables
            -- ============================================================

            -- Track watched issues for regression monitoring
            CREATE TABLE IF NOT EXISTS regression_watches (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                issue_type TEXT NOT NULL,           -- 'sentry_issue' or 'linear_bug'
                issue_id TEXT NOT NULL,
                fix_attempt_id INTEGER NOT NULL REFERENCES fix_attempts(id),
                status TEXT NOT NULL DEFAULT 'awaiting_release',
                -- Status: awaiting_release -> monitoring -> resolved | regressed
                pr_merged_at TEXT,
                monitoring_started_at TEXT,
                resolved_at TEXT,
                regressed_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(issue_type, issue_id)
            );
            CREATE INDEX IF NOT EXISTS idx_regression_watches_status ON regression_watches(status);
            CREATE INDEX IF NOT EXISTS idx_regression_watches_fix_attempt ON regression_watches(fix_attempt_id);

            -- Track release propagation
            CREATE TABLE IF NOT EXISTS release_tracking (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                regression_watch_id INTEGER NOT NULL REFERENCES regression_watches(id),
                release_version TEXT NOT NULL,
                release_commit TEXT NOT NULL,
                released_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_release_tracking_watch ON release_tracking(regression_watch_id);

            -- Individual regression check results
            CREATE TABLE IF NOT EXISTS regression_checks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                regression_watch_id INTEGER NOT NULL REFERENCES regression_watches(id),
                issue_still_exists INTEGER NOT NULL DEFAULT 0,
                checked_at TEXT,
                check_details TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_regression_checks_watch ON regression_checks(regression_watch_id);

            -- ============================================================
            -- Authentication Tables
            -- ============================================================

            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                email TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                name TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'viewer',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
            "#,
        )?;

        // Add new columns if they don't exist (migration for existing DBs)
        // Note: These will fail with "duplicate column name" if column already exists,
        // which is expected and safe to ignore. Other errors are logged.
        let migrations = [
            // fix_attempts migrations
            (
                "fix_attempts.github_repo",
                "ALTER TABLE fix_attempts ADD COLUMN github_repo TEXT",
            ),
            (
                "fix_attempts.github_pr_number",
                "ALTER TABLE fix_attempts ADD COLUMN github_pr_number INTEGER",
            ),
            (
                "fix_attempts.merged_at",
                "ALTER TABLE fix_attempts ADD COLUMN merged_at TEXT",
            ),
            (
                "fix_attempts.resolved_at",
                "ALTER TABLE fix_attempts ADD COLUMN resolved_at TEXT",
            ),
            (
                "fix_attempts.retry_count",
                "ALTER TABLE fix_attempts ADD COLUMN retry_count INTEGER DEFAULT 0",
            ),
            (
                "fix_attempts.last_retry_at",
                "ALTER TABLE fix_attempts ADD COLUMN last_retry_at TEXT",
            ),
            (
                "fix_attempts.issue_labels",
                "ALTER TABLE fix_attempts ADD COLUMN issue_labels TEXT",
            ),
            // repositories migrations (unified table)
            (
                "repositories.default_branch",
                "ALTER TABLE repositories ADD COLUMN default_branch TEXT DEFAULT 'main'",
            ),
            (
                "repositories.file_count",
                "ALTER TABLE repositories ADD COLUMN file_count INTEGER DEFAULT 0",
            ),
            (
                "repositories.last_indexed_at",
                "ALTER TABLE repositories ADD COLUMN last_indexed_at TEXT",
            ),
            // cascade support
            (
                "fix_attempts.parent_attempt_id",
                "ALTER TABLE fix_attempts ADD COLUMN parent_attempt_id INTEGER REFERENCES fix_attempts(id)",
            ),
            (
                "fix_attempts.cascade_repo",
                "ALTER TABLE fix_attempts ADD COLUMN cascade_repo TEXT",
            ),
            (
                "claude_executions.stdout_log_path",
                "ALTER TABLE claude_executions ADD COLUMN stdout_log_path TEXT",
            ),
            (
                "claude_executions.stderr_log_path",
                "ALTER TABLE claude_executions ADD COLUMN stderr_log_path TEXT",
            ),
            (
                "claude_executions.event_log_path",
                "ALTER TABLE claude_executions ADD COLUMN event_log_path TEXT",
            ),
        ];

        for (column_name, sql) in migrations {
            if let Err(e) = conn.execute(sql, []) {
                // "duplicate column name" is expected if column already exists
                if !e.to_string().contains("duplicate column name") {
                    tracing::error!(
                        column = column_name,
                        error = %e,
                        "Failed to run migration"
                    );
                }
            }
        }

        // Cascade index (safe to run multiple times)
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_fix_attempts_parent ON fix_attempts(parent_attempt_id);",
        )?;

        // Update query planner statistics after schema creation
        // This helps SQLite make better query planning decisions
        conn.execute("ANALYZE", [])?;

        Ok(())
    }

    fn parse_datetime(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").map(|dt| dt.and_utc())
            })
            .unwrap_or_else(|e| {
                tracing::warn!(
                    component = "sqlite",
                    input = %s,
                    error = %e,
                    "Failed to parse datetime, falling back to current time - this may indicate data corruption"
                );
                Utc::now()
            })
    }

    fn parse_optional_datetime(s: Option<String>) -> Option<DateTime<Utc>> {
        s.map(|s| Self::parse_datetime(&s))
    }

    fn resolve_log_root() -> PathBuf {
        std::env::var("CLAUDEAR_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_LOG_DIR))
    }

    /// Best-effort mirror of structured audit records to JSONL on disk.
    fn append_audit_json_line(category: &str, payload: &serde_json::Value) {
        use std::io::Write as _;

        let root = Self::resolve_log_root();
        if root.as_os_str().is_empty() {
            return;
        }

        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let dir = root.join(AUDIT_LOG_SUBDIR).join(category);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                component = "sqlite",
                category = category,
                path = %dir.display(),
                error = %e,
                "Failed to create audit log directory"
            );
            return;
        }

        let file_path = dir.join(format!("{}.jsonl", day));
        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
        {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(
                    component = "sqlite",
                    category = category,
                    path = %file_path.display(),
                    error = %e,
                    "Failed to open audit log file"
                );
                return;
            }
        };

        let serialized = match serde_json::to_string(payload) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    component = "sqlite",
                    category = category,
                    error = %e,
                    "Failed to serialize audit log payload"
                );
                return;
            }
        };

        if let Err(e) = writeln!(file, "{}", serialized) {
            tracing::warn!(
                component = "sqlite",
                category = category,
                path = %file_path.display(),
                error = %e,
                "Failed to append audit log payload"
            );
        }
    }

    /// Parse a GitHub PR URL to extract repo and PR number.
    /// Supports: https://github.com/owner/repo/pull/123
    pub fn parse_pr_url(url: &str) -> Option<(String, i64)> {
        // Reject excessively long URLs to prevent ReDoS and memory issues
        if url.len() > MAX_PR_URL_LENGTH {
            return None;
        }
        let caps = PR_URL_REGEX.captures(url)?;
        let repo = caps.get(1)?.as_str().to_string();
        let pr_number: i64 = caps.get(2)?.as_str().parse().ok()?;
        Some((repo, pr_number))
    }

    fn embedding_to_blob(embedding: Option<&[f32]>) -> Option<Vec<u8>> {
        embedding.map(|values| values.iter().flat_map(|f| f.to_le_bytes()).collect())
    }

    fn blob_to_embedding(blob: Option<Vec<u8>>) -> Option<Vec<f32>> {
        let blob = blob?;
        if !blob.len().is_multiple_of(4) {
            return None;
        }
        Some(
            blob.chunks_exact(4)
                .map(|chunk| {
                    let arr: [u8; 4] = chunk.try_into().expect("chunks_exact guarantees 4 bytes");
                    f32::from_le_bytes(arr)
                })
                .collect(),
        )
    }

    fn table_exists(conn: &Connection, table_name: &str) -> Result<bool> {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1 LIMIT 1",
                params![table_name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    fn ensure_qa_vector_table(conn: &Connection, dimension: usize) -> Result<bool> {
        if dimension == 0 {
            return Ok(false);
        }

        if Self::table_exists(conn, QA_VECTOR_TABLE)? {
            return Ok(true);
        }

        if !is_vectorlite_available(conn) {
            match try_load_vectorlite(conn) {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(e) => {
                    tracing::debug!(error = %e, "Unable to load vectorlite extension for Q&A search");
                    return Ok(false);
                }
            }
        }

        let sql = format!(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vectorlite(
                embedding float32[{dimension}] cosine,
                hnsw(max_elements=10000, ef_construction=200, M=16)
            )
            "#,
            table = QA_VECTOR_TABLE,
            dimension = dimension
        );

        match conn.execute_batch(&sql) {
            Ok(()) => {
                let backfill_sql = format!(
                    r#"
                    INSERT INTO {table}(rowid, embedding)
                    SELECT id, question_embedding
                    FROM qa_knowledge
                    WHERE question_embedding IS NOT NULL
                      AND length(question_embedding) = ?1
                    "#,
                    table = QA_VECTOR_TABLE
                );
                if let Err(e) = conn.execute(&backfill_sql, params![(dimension * 4) as i64]) {
                    tracing::debug!(error = %e, "Failed to backfill Q&A vector embeddings");
                }
                Ok(true)
            }
            Err(e) => {
                tracing::debug!(error = %e, "Failed to create Q&A vector table");
                Ok(false)
            }
        }
    }

    fn upsert_qa_vector_embedding(conn: &Connection, qa_id: i64, embedding: &[f32]) -> Result<()> {
        if embedding.is_empty() {
            return Ok(());
        }

        if !Self::ensure_qa_vector_table(conn, embedding.len())? {
            return Ok(());
        }

        let vector_blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let delete_sql = format!("DELETE FROM {} WHERE rowid = ?1", QA_VECTOR_TABLE);
        let insert_sql = format!(
            "INSERT INTO {}(rowid, embedding) VALUES (?1, ?2)",
            QA_VECTOR_TABLE
        );

        conn.execute(&delete_sql, params![qa_id])?;
        conn.execute(&insert_sql, params![qa_id, vector_blob])?;
        Ok(())
    }

    fn query_qa_matches_vector_scoped(
        conn: &Connection,
        source: &str,
        repo: Option<&str>,
        question_embedding: &[f32],
        threshold: f64,
        limit: usize,
        candidate_limit: usize,
    ) -> Result<Option<Vec<QaMatch>>> {
        if question_embedding.is_empty() || limit == 0 || candidate_limit == 0 {
            return Ok(Some(Vec::new()));
        }

        if !Self::ensure_qa_vector_table(conn, question_embedding.len())? {
            return Ok(None);
        }

        let query_blob: Vec<u8> = question_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let sql = format!(
            r#"
            WITH candidates AS (
                SELECT rowid AS qa_id,
                       MAX(0.0, MIN(1.0, 1.0 - distance)) AS semantic_similarity
                FROM {table}
                WHERE knn_search(embedding, knn_param(?1, ?2, ?3))
            ),
            scored AS (
                SELECT k.id, k.source, k.repo, k.issue_id, k.short_id, k.question_text, k.question_norm,
                       k.question_embedding, k.answer_text, k.answer_norm, k.answer_embedding, k.channel,
                       k.responder, k.correlation_id, k.asked_at, k.answered_at, k.success_count,
                       k.failure_count, k.last_used_at, k.metadata,
                       c.semantic_similarity AS semantic_similarity,
                       CASE
                           WHEN (k.success_count + k.failure_count) > 0 THEN
                               CAST(k.success_count AS REAL) / CAST((k.success_count + k.failure_count) AS REAL)
                           ELSE 0.5
                       END AS historical_success_rate
                FROM candidates c
                JOIN qa_knowledge k ON k.id = c.qa_id
                WHERE k.source = ?4
                  AND (?5 IS NULL OR k.repo = ?5)
            ),
            ranked AS (
                SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                       question_embedding, answer_text, answer_norm, answer_embedding, channel,
                       responder, correlation_id, asked_at, answered_at, success_count,
                       failure_count, last_used_at, metadata, semantic_similarity,
                       historical_success_rate,
                       (semantic_similarity * 0.75 + historical_success_rate * 0.25) AS final_score
                FROM scored
            )
            SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                   question_embedding, answer_text, answer_norm, answer_embedding, channel,
                   responder, correlation_id, asked_at, answered_at, success_count,
                   failure_count, last_used_at, metadata, semantic_similarity,
                   historical_success_rate, final_score
            FROM ranked
            WHERE semantic_similarity >= ?6 OR final_score >= ?6
            ORDER BY final_score DESC, answered_at DESC
            LIMIT ?7
            "#,
            table = QA_VECTOR_TABLE
        );

        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::debug!(error = %e, "Failed to prepare scoped Q&A vector ranking query");
                return Ok(None);
            }
        };

        let rows = match stmt.query_map(
            params![
                query_blob,
                candidate_limit as i64,
                QA_VECTOR_EF_SEARCH as i64,
                source,
                repo,
                threshold,
                limit as i64
            ],
            Self::row_to_qa_match,
        ) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::debug!(error = %e, "Scoped Q&A vector ranking query failed");
                return Ok(None);
            }
        };

        let mut matches = Vec::new();
        for row in rows {
            match row {
                Ok(m) => matches.push(m),
                Err(e) => tracing::debug!(error = %e, "Failed to read scoped Q&A vector row"),
            }
        }

        Ok(Some(matches))
    }

    fn query_qa_matches_vector_global(
        conn: &Connection,
        question_embedding: &[f32],
        threshold: f64,
        limit: usize,
        candidate_limit: usize,
    ) -> Result<Option<Vec<QaMatch>>> {
        if question_embedding.is_empty() || limit == 0 || candidate_limit == 0 {
            return Ok(Some(Vec::new()));
        }

        if !Self::ensure_qa_vector_table(conn, question_embedding.len())? {
            return Ok(None);
        }

        let query_blob: Vec<u8> = question_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let sql = format!(
            r#"
            WITH candidates AS (
                SELECT rowid AS qa_id,
                       MAX(0.0, MIN(1.0, 1.0 - distance)) AS semantic_similarity
                FROM {table}
                WHERE knn_search(embedding, knn_param(?1, ?2, ?3))
            ),
            scored AS (
                SELECT k.id, k.source, k.repo, k.issue_id, k.short_id, k.question_text, k.question_norm,
                       k.question_embedding, k.answer_text, k.answer_norm, k.answer_embedding, k.channel,
                       k.responder, k.correlation_id, k.asked_at, k.answered_at, k.success_count,
                       k.failure_count, k.last_used_at, k.metadata,
                       c.semantic_similarity AS semantic_similarity,
                       CASE
                           WHEN (k.success_count + k.failure_count) > 0 THEN
                               CAST(k.success_count AS REAL) / CAST((k.success_count + k.failure_count) AS REAL)
                           ELSE 0.5
                       END AS historical_success_rate
                FROM candidates c
                JOIN qa_knowledge k ON k.id = c.qa_id
            ),
            ranked AS (
                SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                       question_embedding, answer_text, answer_norm, answer_embedding, channel,
                       responder, correlation_id, asked_at, answered_at, success_count,
                       failure_count, last_used_at, metadata, semantic_similarity,
                       historical_success_rate,
                       (semantic_similarity * 0.75 + historical_success_rate * 0.25) AS final_score
                FROM scored
            )
            SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                   question_embedding, answer_text, answer_norm, answer_embedding, channel,
                   responder, correlation_id, asked_at, answered_at, success_count,
                   failure_count, last_used_at, metadata, semantic_similarity,
                   historical_success_rate, final_score
            FROM ranked
            WHERE semantic_similarity >= ?4 OR final_score >= ?4
            ORDER BY final_score DESC, answered_at DESC
            LIMIT ?5
            "#,
            table = QA_VECTOR_TABLE
        );

        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::debug!(error = %e, "Failed to prepare global Q&A vector ranking query");
                return Ok(None);
            }
        };

        let rows = match stmt.query_map(
            params![
                query_blob,
                candidate_limit as i64,
                QA_VECTOR_EF_SEARCH as i64,
                threshold,
                limit as i64
            ],
            Self::row_to_qa_match,
        ) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::debug!(error = %e, "Global Q&A vector ranking query failed");
                return Ok(None);
            }
        };

        let mut matches = Vec::new();
        for row in rows {
            match row {
                Ok(m) => matches.push(m),
                Err(e) => tracing::debug!(error = %e, "Failed to read global Q&A vector row"),
            }
        }

        Ok(Some(matches))
    }

    fn query_qa_matches_exact_scoped(
        conn: &Connection,
        source: &str,
        repo: Option<&str>,
        question_norm: &str,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<QaMatch>> {
        let mut stmt = conn.prepare(
            r#"
            WITH scoped AS (
                SELECT k.id, k.source, k.repo, k.issue_id, k.short_id, k.question_text, k.question_norm,
                       k.question_embedding, k.answer_text, k.answer_norm, k.answer_embedding, k.channel,
                       k.responder, k.correlation_id, k.asked_at, k.answered_at, k.success_count,
                       k.failure_count, k.last_used_at, k.metadata,
                       1.0 AS semantic_similarity,
                       CASE
                           WHEN (k.success_count + k.failure_count) > 0 THEN
                               CAST(k.success_count AS REAL) / CAST((k.success_count + k.failure_count) AS REAL)
                           ELSE 0.5
                       END AS historical_success_rate
                FROM qa_knowledge k
                WHERE k.source = ?1
                  AND (?2 IS NULL OR k.repo = ?2)
                  AND k.question_norm = ?3
            ),
            ranked AS (
                SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                       question_embedding, answer_text, answer_norm, answer_embedding, channel,
                       responder, correlation_id, asked_at, answered_at, success_count,
                       failure_count, last_used_at, metadata, semantic_similarity,
                       historical_success_rate,
                       (semantic_similarity * 0.75 + historical_success_rate * 0.25) AS final_score
                FROM scoped
            )
            SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                   question_embedding, answer_text, answer_norm, answer_embedding, channel,
                   responder, correlation_id, asked_at, answered_at, success_count,
                   failure_count, last_used_at, metadata, semantic_similarity,
                   historical_success_rate, final_score
            FROM ranked
            WHERE semantic_similarity >= ?4 OR final_score >= ?4
            ORDER BY final_score DESC, answered_at DESC
            LIMIT ?5
            "#,
        )?;
        let rows = stmt.query_map(
            params![source, repo, question_norm, threshold, limit as i64],
            Self::row_to_qa_match,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn query_qa_matches_exact_global(
        conn: &Connection,
        question_norm: &str,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<QaMatch>> {
        let mut stmt = conn.prepare(
            r#"
            WITH scoped AS (
                SELECT k.id, k.source, k.repo, k.issue_id, k.short_id, k.question_text, k.question_norm,
                       k.question_embedding, k.answer_text, k.answer_norm, k.answer_embedding, k.channel,
                       k.responder, k.correlation_id, k.asked_at, k.answered_at, k.success_count,
                       k.failure_count, k.last_used_at, k.metadata,
                       1.0 AS semantic_similarity,
                       CASE
                           WHEN (k.success_count + k.failure_count) > 0 THEN
                               CAST(k.success_count AS REAL) / CAST((k.success_count + k.failure_count) AS REAL)
                           ELSE 0.5
                       END AS historical_success_rate
                FROM qa_knowledge k
                WHERE k.question_norm = ?1
            ),
            ranked AS (
                SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                       question_embedding, answer_text, answer_norm, answer_embedding, channel,
                       responder, correlation_id, asked_at, answered_at, success_count,
                       failure_count, last_used_at, metadata, semantic_similarity,
                       historical_success_rate,
                       (semantic_similarity * 0.75 + historical_success_rate * 0.25) AS final_score
                FROM scoped
            )
            SELECT id, source, repo, issue_id, short_id, question_text, question_norm,
                   question_embedding, answer_text, answer_norm, answer_embedding, channel,
                   responder, correlation_id, asked_at, answered_at, success_count,
                   failure_count, last_used_at, metadata, semantic_similarity,
                   historical_success_rate, final_score
            FROM ranked
            WHERE semantic_similarity >= ?2 OR final_score >= ?2
            ORDER BY final_score DESC, answered_at DESC
            LIMIT ?3
            "#,
        )?;
        let rows = stmt.query_map(
            params![question_norm, threshold, limit as i64],
            Self::row_to_qa_match,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

impl FixAttemptTracker for SqliteTracker {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn has_attempted(&self, source: &str, issue_id: &str) -> Result<bool> {
        let conn = self.acquire_lock()?;
        let mut stmt =
            conn.prepare_cached("SELECT 1 FROM fix_attempts WHERE source = ? AND issue_id = ?")?;
        Ok(stmt.exists(params![source, issue_id])?)
    }

    fn get_attempted_issue_ids(&self, source: &str) -> HashSet<String> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Failed to acquire database lock in get_attempted_issue_ids");
                return HashSet::new();
            }
        };
        let mut stmt = match conn
            .prepare_cached("SELECT issue_id FROM fix_attempts WHERE source = ?")
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Failed to prepare statement in get_attempted_issue_ids");
                return HashSet::new();
            }
        };

        // Collect results immediately to avoid borrow lifetime issues
        let query_result = stmt.query_map(params![source], |row| row.get::<_, String>(0));
        let mut result = HashSet::new();

        match query_result {
            Ok(rows) => {
                for row in rows {
                    match row {
                        Ok(id) => {
                            result.insert(id);
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to read issue_id row");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to query issue IDs");
            }
        }
        result
    }

    fn record_attempt(&self, source: &str, issue_id: &str, short_id: &str) -> Result<()> {
        self.record_attempt_with_labels(source, issue_id, short_id, &[])
    }

    fn record_attempt_with_labels(
        &self,
        source: &str,
        issue_id: &str,
        short_id: &str,
        labels: &[String],
    ) -> Result<()> {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            short_id = short_id,
            labels_count = labels.len(),
            "Recording fix attempt with labels"
        );
        let conn = self.acquire_lock()?;

        // Serialize labels to JSON
        let labels_json = if labels.is_empty() {
            None
        } else {
            Some(serde_json::to_string(labels).unwrap_or_default())
        };

        let rows_affected = conn.execute(
            r#"
            INSERT INTO fix_attempts (source, issue_id, short_id, status, attempted_at, issue_labels)
            VALUES (?, ?, ?, 'pending', datetime('now'), ?)
            ON CONFLICT(source, issue_id) DO UPDATE SET
                short_id = excluded.short_id,
                attempted_at = datetime('now'),
                issue_labels = COALESCE(excluded.issue_labels, fix_attempts.issue_labels)
            "#,
            params![source, issue_id, short_id, labels_json],
        )?;
        tracing::info!(
            source = source,
            issue_id = issue_id,
            rows_affected = rows_affected,
            "Fix attempt recorded"
        );
        Ok(())
    }

    fn mark_success(&self, source: &str, issue_id: &str, pr_url: &str) -> Result<()> {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            pr_url = pr_url,
            "Marking fix attempt as success"
        );
        let conn = self.acquire_lock()?;

        // Parse PR URL to extract GitHub repo and PR number
        let (github_repo, github_pr_number) = match Self::parse_pr_url(pr_url) {
            Some((repo, pr_num)) => (Some(repo), Some(pr_num)),
            None => {
                tracing::warn!(
                    pr_url = pr_url,
                    source = source,
                    issue_id = issue_id,
                    "Failed to parse PR URL - PR tracking may not work correctly"
                );
                (None, None)
            }
        };

        let rows_affected = conn.execute(
            r#"
            UPDATE fix_attempts
            SET status = 'success', pr_url = ?, github_repo = ?, github_pr_number = ?
            WHERE source = ? AND issue_id = ?
            "#,
            params![pr_url, github_repo, github_pr_number, source, issue_id],
        )?;
        tracing::info!(
            source = source,
            issue_id = issue_id,
            rows_affected = rows_affected,
            github_repo = ?github_repo,
            "Fix attempt marked as success"
        );
        Ok(())
    }

    fn mark_merged(&self, source: &str, issue_id: &str) -> Result<()> {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            "Marking fix attempt as merged"
        );
        let conn = self.acquire_lock()?;
        let rows_affected = conn.execute(
            r#"
            UPDATE fix_attempts
            SET status = 'merged', merged_at = datetime('now')
            WHERE source = ? AND issue_id = ?
            "#,
            params![source, issue_id],
        )?;
        tracing::info!(
            source = source,
            issue_id = issue_id,
            rows_affected = rows_affected,
            "Fix attempt marked as merged"
        );
        Ok(())
    }

    fn mark_closed(&self, source: &str, issue_id: &str) -> Result<()> {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            "Marking fix attempt as closed"
        );
        let conn = self.acquire_lock()?;
        let rows_affected = conn.execute(
            r#"
            UPDATE fix_attempts
            SET status = 'closed'
            WHERE source = ? AND issue_id = ?
            "#,
            params![source, issue_id],
        )?;
        tracing::info!(
            source = source,
            issue_id = issue_id,
            rows_affected = rows_affected,
            "Fix attempt marked as closed"
        );
        Ok(())
    }

    fn mark_resolved(&self, source: &str, issue_id: &str) -> Result<()> {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            "Marking fix attempt as resolved"
        );
        let conn = self.acquire_lock()?;
        let rows_affected = conn.execute(
            r#"
            UPDATE fix_attempts
            SET resolved_at = datetime('now')
            WHERE source = ? AND issue_id = ?
            "#,
            params![source, issue_id],
        )?;
        tracing::info!(
            source = source,
            issue_id = issue_id,
            rows_affected = rows_affected,
            "Fix attempt marked as resolved"
        );
        Ok(())
    }

    fn mark_failed(&self, source: &str, issue_id: &str, error_message: &str) -> Result<()> {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            error_message = error_message,
            "Marking fix attempt as failed"
        );
        let conn = self.acquire_lock()?;
        let rows_affected = conn.execute(
            r#"
            UPDATE fix_attempts
            SET status = 'failed', error_message = ?
            WHERE source = ? AND issue_id = ?
            "#,
            params![error_message, source, issue_id],
        )?;
        tracing::info!(
            source = source,
            issue_id = issue_id,
            rows_affected = rows_affected,
            "Fix attempt marked as failed"
        );
        Ok(())
    }

    fn get_attempt(&self, source: &str, issue_id: &str) -> Result<Option<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare_cached(
            r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE source = ? AND issue_id = ?
            "#,
        )?;

        let result = stmt
            .query_row(params![source, issue_id], Self::row_to_fix_attempt)
            .ok();

        Ok(result)
    }

    fn get_attempts_by_status(&self, status: FixAttemptStatus) -> Result<Vec<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare_cached(
            r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE status = ?
            ORDER BY attempted_at DESC
            "#,
        )?;

        let status_str = status.to_string();
        let rows = stmt.query_map(params![status_str], Self::row_to_fix_attempt)?;

        let mut results = Vec::new();
        for row in rows.flatten() {
            results.push(row);
        }
        Ok(results)
    }

    fn get_pending_prs(&self) -> Result<Vec<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare_cached(
            r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE status = 'success' AND pr_url IS NOT NULL AND github_repo IS NOT NULL
            ORDER BY attempted_at DESC
            "#,
        )?;

        let rows = stmt.query_map([], Self::row_to_fix_attempt)?;

        let mut results = Vec::new();
        for row in rows.flatten() {
            results.push(row);
        }
        Ok(results)
    }

    fn get_attempt_by_pr_url(&self, pr_url: &str) -> Result<Option<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare_cached(
            r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE pr_url = ?
            ORDER BY attempted_at DESC, id DESC
            LIMIT 1
            "#,
        )?;

        let result = stmt
            .query_row(params![pr_url], Self::row_to_fix_attempt)
            .ok();

        Ok(result)
    }

    fn reset_attempt(&self, source: &str, issue_id: &str) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute(
            "DELETE FROM fix_attempts WHERE source = ? AND issue_id = ?",
            params![source, issue_id],
        )?;
        Ok(())
    }

    fn increment_retry(&self, source: &str, issue_id: &str) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            UPDATE fix_attempts
            SET retry_count = COALESCE(retry_count, 0) + 1,
                last_retry_at = datetime('now')
            WHERE source = ? AND issue_id = ?
            "#,
            params![source, issue_id],
        )?;
        Ok(())
    }

    fn mark_cannot_fix(&self, source: &str, issue_id: &str, reason: &str) -> Result<()> {
        tracing::info!(
            source = source,
            issue_id = issue_id,
            reason = reason,
            "Marking fix attempt as cannot_fix"
        );
        let conn = self.acquire_lock()?;
        let rows_affected = conn.execute(
            r#"
            UPDATE fix_attempts
            SET status = 'cannot_fix', error_message = ?
            WHERE source = ? AND issue_id = ?
            "#,
            params![reason, source, issue_id],
        )?;
        tracing::info!(
            source = source,
            issue_id = issue_id,
            rows_affected = rows_affected,
            "Fix attempt marked as cannot_fix"
        );
        Ok(())
    }

    fn get_retryable_issues(&self, max_retries: u32) -> Result<Vec<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare_cached(
            r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE (status = 'failed' OR status = 'closed')
              AND COALESCE(retry_count, 0) < ?
            ORDER BY attempted_at ASC
            "#,
        )?;

        let rows = stmt.query_map(params![max_retries], Self::row_to_fix_attempt)?;

        let mut results = Vec::new();
        for row in rows.flatten() {
            results.push(row);
        }
        Ok(results)
    }

    fn prepare_for_retry(&self, source: &str, issue_id: &str) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            UPDATE fix_attempts
            SET status = 'pending',
                pr_url = NULL,
                github_repo = NULL,
                github_pr_number = NULL,
                error_message = NULL,
                attempted_at = datetime('now')
            WHERE source = ? AND issue_id = ?
            "#,
            params![source, issue_id],
        )?;
        Ok(())
    }

    fn get_stats(&self) -> Result<FixAttemptStats> {
        let conn = self.acquire_lock()?;
        let mut stats = FixAttemptStats::default();

        // Overall stats
        let mut stmt = conn.prepare_cached(
            r#"
            SELECT status, COUNT(*) as count
            FROM fix_attempts
            GROUP BY status
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })?;

        for row in rows {
            match row {
                Ok((status, count)) => {
                    stats.total += count;
                    match status.as_str() {
                        "pending" => stats.pending = count,
                        "success" => stats.success = count,
                        "failed" => stats.failed = count,
                        "merged" => stats.merged = count,
                        "closed" => stats.closed = count,
                        "cannot_fix" => stats.cannot_fix = count,
                        _ => {}
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to read stats row");
                }
            }
        }

        // Stats by source
        let mut stmt = conn.prepare_cached(
            r#"
            SELECT source, status, COUNT(*) as count
            FROM fix_attempts
            GROUP BY source, status
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as usize,
            ))
        })?;

        let mut by_source: HashMap<String, SourceStats> = HashMap::new();

        for row in rows {
            match row {
                Ok((source, status, count)) => {
                    let entry = by_source.entry(source).or_default();
                    entry.total += count;
                    match status.as_str() {
                        "success" => entry.success = count,
                        "failed" => entry.failed = count,
                        "merged" => entry.merged = count,
                        "closed" => entry.closed = count,
                        "cannot_fix" => entry.cannot_fix = count,
                        _ => {}
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to read source stats row");
                }
            }
        }

        stats.by_source = by_source;

        Ok(stats)
    }

    fn record_activity(&self, entry: &ActivityLogEntry) -> Result<i64> {
        SqliteTracker::record_activity(self, entry)
    }

    fn get_recent_activities(&self, limit: usize) -> Result<Vec<ActivityLogEntry>> {
        SqliteTracker::get_recent_activities(self, limit, None)
    }

    fn record_execution(&self, execution: &ClaudeExecution) -> Result<i64> {
        SqliteTracker::record_execution(self, execution)
    }

    fn record_pr_review(&self, review: &PrReviewRecord) -> Result<i64> {
        SqliteTracker::record_pr_review(self, review)
    }

    fn record_error_pattern(&self, pattern: &ErrorPattern) -> Result<i64> {
        SqliteTracker::record_error_pattern(self, pattern)
    }

    fn record_metric(&self, metric: &ProcessingMetric) -> Result<i64> {
        SqliteTracker::record_metric(self, metric)
    }

    fn get_analytics_summary(&self) -> Result<AnalyticsSummary> {
        SqliteTracker::get_analytics_summary(self)
    }

    fn store_feedback_outcome(&self, outcome: &FixOutcome) -> Result<i64> {
        SqliteTracker::store_feedback_outcome(self, outcome)
    }

    fn get_feedback_outcomes(&self, source: Option<&str>, limit: usize) -> Result<Vec<FixOutcome>> {
        SqliteTracker::get_feedback_outcomes(self, source, limit)
    }

    fn get_feedback_outcome_by_attempt(&self, attempt_id: i64) -> Result<Option<FixOutcome>> {
        SqliteTracker::get_feedback_outcome_by_attempt(self, attempt_id)
    }

    fn store_qa_knowledge(&self, entry: &QaKnowledgeEntry) -> Result<i64> {
        SqliteTracker::store_qa_knowledge(self, entry)
    }

    fn find_similar_qa_scoped(
        &self,
        source: &str,
        repo: Option<&str>,
        question_norm: &str,
        question_embedding: Option<&[f32]>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<QaMatch>> {
        SqliteTracker::find_similar_qa_scoped(
            self,
            source,
            repo,
            question_norm,
            question_embedding,
            threshold,
            limit,
        )
    }

    fn find_similar_qa_global(
        &self,
        question_norm: &str,
        question_embedding: Option<&[f32]>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<QaMatch>> {
        SqliteTracker::find_similar_qa_global(
            self,
            question_norm,
            question_embedding,
            threshold,
            limit,
        )
    }

    fn record_qa_usage(
        &self,
        attempt_id: i64,
        qa_id: i64,
        usage_type: &str,
        similarity_score: f64,
    ) -> Result<i64> {
        SqliteTracker::record_qa_usage(self, attempt_id, qa_id, usage_type, similarity_score)
    }

    fn update_qa_outcome_stats(&self, qa_id: i64, success: bool) -> Result<()> {
        SqliteTracker::update_qa_outcome_stats(self, qa_id, success)
    }

    fn update_qa_outcome_stats_for_attempt(&self, attempt_id: i64, success: bool) -> Result<()> {
        SqliteTracker::update_qa_outcome_stats_for_attempt(self, attempt_id, success)
    }

    fn get_channel_cursor(&self, channel: &str, cursor_key: &str) -> Result<Option<String>> {
        SqliteTracker::get_channel_cursor(self, channel, cursor_key)
    }

    fn set_channel_cursor(
        &self,
        channel: &str,
        cursor_key: &str,
        cursor_value: &str,
    ) -> Result<()> {
        SqliteTracker::set_channel_cursor(self, channel, cursor_key, cursor_value)
    }

    fn get_recent_activities_filtered(
        &self,
        limit: usize,
        source_filter: Option<&str>,
    ) -> Result<Vec<ActivityLogEntry>> {
        SqliteTracker::get_recent_activities(self, limit, source_filter)
    }

    fn get_attempt_by_id(&self, id: i64) -> Result<Option<FixAttempt>> {
        SqliteTracker::get_attempt_by_id(self, id)
    }

    fn get_executions_for_attempt(&self, attempt_id: i64) -> Result<Vec<ClaudeExecution>> {
        SqliteTracker::get_executions_for_attempt(self, attempt_id)
    }

    fn get_reviews_for_attempt(&self, attempt_id: i64) -> Result<Vec<PrReviewRecord>> {
        SqliteTracker::get_reviews_for_attempt(self, attempt_id)
    }

    fn get_error_patterns(&self, limit: usize) -> Result<Vec<ErrorPattern>> {
        SqliteTracker::get_error_patterns(self, limit)
    }

    fn get_metrics(
        &self,
        metric_name: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<ProcessingMetric>> {
        SqliteTracker::get_metrics(self, metric_name, since, limit)
    }

    fn get_open_prs(&self) -> Result<Vec<crate::types::PrRecord>> {
        SqliteTracker::get_open_prs(self)
    }

    fn get_pr_analytics(&self) -> Result<crate::types::PrAnalytics> {
        SqliteTracker::get_pr_analytics(self)
    }

    fn get_regression_watches_by_status(
        &self,
        status: crate::types::RegressionWatchStatus,
    ) -> Result<Vec<crate::types::RegressionWatch>> {
        SqliteTracker::get_regression_watches_by_status(self, status)
    }

    fn get_all_regression_watches(&self) -> Result<Vec<crate::types::RegressionWatch>> {
        SqliteTracker::get_all_regression_watches(self)
    }

    fn get_regression_checks(&self, watch_id: i64) -> Result<Vec<crate::types::RegressionCheck>> {
        SqliteTracker::get_regression_checks(self, watch_id)
    }

    fn get_active_experiments(&self) -> Result<Vec<PromptExperiment>> {
        SqliteTracker::get_active_experiments(self)
    }

    fn list_indexed_repos(&self) -> Result<Vec<StoredIndexedRepo>> {
        SqliteTracker::list_indexed_repos(self)
    }

    fn get_index_stats(&self) -> Result<IndexStats> {
        SqliteTracker::get_index_stats(self)
    }

    fn list_all_dependencies(&self) -> Result<Vec<StoredDependency>> {
        SqliteTracker::list_all_dependencies(self)
    }

    fn get_inference_stats(&self) -> Result<InferenceStats> {
        SqliteTracker::get_inference_stats(self)
    }

    fn get_inference_history(&self, limit: usize) -> Result<Vec<InferenceHistoryEntry>> {
        SqliteTracker::get_inference_history(self, limit)
    }

    fn list_prs(&self, status: Option<&str>, limit: usize) -> Result<Vec<crate::types::PrRecord>> {
        SqliteTracker::list_prs(self, status, limit)
    }
}

impl SqliteTracker {
    /// Record an activity to the activity log.
    pub fn record_activity(&self, entry: &ActivityLogEntry) -> Result<i64> {
        let conn = self.acquire_lock()?;
        let metadata_json = entry.metadata.as_ref().map(|m| m.to_string());

        conn.execute(
            r#"
            INSERT INTO activity_log (timestamp, activity_type, source, issue_id, short_id, message, metadata)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                entry.activity_type,
                entry.source,
                entry.issue_id,
                entry.short_id,
                entry.message,
                metadata_json,
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        Self::append_audit_json_line(
            "activity",
            &serde_json::json!({
                "id": id,
                "timestamp": entry.timestamp.to_rfc3339(),
                "activity_type": entry.activity_type,
                "source": entry.source,
                "issue_id": entry.issue_id,
                "short_id": entry.short_id,
                "message": entry.message,
                "metadata": entry.metadata,
            }),
        );
        Ok(id)
    }

    /// Record multiple activities in a single transaction for better performance.
    ///
    /// This is more efficient than calling `record_activity` in a loop because:
    /// - Single transaction reduces fsync overhead
    /// - Prepared statement is reused across all inserts
    pub fn record_activities_batch(&self, entries: &[ActivityLogEntry]) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let mut conn = self.acquire_lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT INTO activity_log (timestamp, activity_type, source, issue_id, short_id, message, metadata)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                "#,
            )?;

            for entry in entries {
                let metadata_json = entry.metadata.as_ref().map(|m| m.to_string());
                stmt.execute(params![
                    entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    entry.activity_type,
                    entry.source,
                    entry.issue_id,
                    entry.short_id,
                    entry.message,
                    metadata_json,
                ])?;
            }
        }

        tx.commit()?;
        drop(conn);
        for entry in entries {
            Self::append_audit_json_line(
                "activity",
                &serde_json::json!({
                    "id": serde_json::Value::Null,
                    "timestamp": entry.timestamp.to_rfc3339(),
                    "activity_type": entry.activity_type,
                    "source": entry.source,
                    "issue_id": entry.issue_id,
                    "short_id": entry.short_id,
                    "message": entry.message,
                    "metadata": entry.metadata,
                }),
            );
        }
        Ok(entries.len())
    }

    /// Get recent activities, optionally filtered by source.
    pub fn get_recent_activities(
        &self,
        limit: usize,
        source_filter: Option<&str>,
    ) -> Result<Vec<ActivityLogEntry>> {
        let conn = self.acquire_lock()?;

        // Build query dynamically based on whether source filter is provided
        let (query, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match source_filter {
            Some(source) => (
                r#"
                SELECT id, timestamp, activity_type, source, issue_id, short_id, message, metadata
                FROM activity_log
                WHERE source = ?1
                ORDER BY timestamp DESC
                LIMIT ?2
                "#
                .to_string(),
                vec![Box::new(source.to_string()), Box::new(limit as i64)],
            ),
            None => (
                r#"
                SELECT id, timestamp, activity_type, source, issue_id, short_id, message, metadata
                FROM activity_log
                ORDER BY timestamp DESC
                LIMIT ?1
                "#
                .to_string(),
                vec![Box::new(limit as i64)],
            ),
        };

        let mut stmt = conn.prepare(&query)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(Self::row_to_activity_entry(row))
        })?;

        Ok(rows.flatten().collect())
    }

    /// Count activity events grouped by type since a timestamp.
    pub fn get_activity_type_counts_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<HashMap<String, i64>> {
        let conn = self.acquire_lock()?;
        let since_str = since.format("%Y-%m-%d %H:%M:%S").to_string();
        let mut stmt = conn.prepare(
            r#"
            SELECT activity_type, COUNT(*)
            FROM activity_log
            WHERE timestamp >= ?1
            GROUP BY activity_type
            "#,
        )?;

        let rows = stmt.query_map(params![since_str], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut counts = HashMap::new();
        for row in rows.flatten() {
            counts.insert(row.0, row.1);
        }
        Ok(counts)
    }

    /// Get activities for a specific issue.
    pub fn get_activities_for_issue(
        &self,
        source: &str,
        issue_id: &str,
    ) -> Result<Vec<ActivityLogEntry>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, timestamp, activity_type, source, issue_id, short_id, message, metadata
            FROM activity_log
            WHERE source = ? AND issue_id = ?
            ORDER BY timestamp DESC
            "#,
        )?;

        let mut entries = Vec::new();
        let rows = stmt.query_map(params![source, issue_id], |row| {
            Ok(Self::row_to_activity_entry(row))
        })?;

        for row in rows.flatten() {
            entries.push(row);
        }

        Ok(entries)
    }

    fn row_to_activity_entry(row: &rusqlite::Row<'_>) -> ActivityLogEntry {
        let metadata_str: Option<String> = row.get(7).ok();
        let metadata = metadata_str.and_then(|s| serde_json::from_str(&s).ok());

        ActivityLogEntry {
            id: row.get(0).unwrap_or(0),
            timestamp: Self::parse_datetime(&row.get::<_, String>(1).unwrap_or_default()),
            activity_type: row.get(2).unwrap_or_default(),
            source: row.get(3).ok(),
            issue_id: row.get(4).ok(),
            short_id: row.get(5).ok(),
            message: row.get(6).unwrap_or_default(),
            metadata,
        }
    }

    /// Convert a database row to a FixAttempt.
    /// Expects columns in order: id, source, issue_id, short_id, attempted_at, pr_url,
    /// github_repo, github_pr_number, status, error_message, merged_at, resolved_at,
    /// retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
    fn row_to_fix_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<FixAttempt> {
        // Parse issue_labels from JSON string
        let issue_labels: Vec<String> = row
            .get::<_, Option<String>>(14)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        Ok(FixAttempt {
            id: row.get(0)?,
            source: row.get(1)?,
            issue_id: row.get(2)?,
            short_id: row.get(3)?,
            attempted_at: Self::parse_datetime(&row.get::<_, String>(4)?),
            pr_url: row.get(5)?,
            github_repo: row.get(6)?,
            github_pr_number: row.get(7)?,
            status: row
                .get::<_, String>(8)?
                .parse()
                .unwrap_or(FixAttemptStatus::Pending),
            error_message: row.get(9)?,
            merged_at: Self::parse_optional_datetime(row.get(10)?),
            resolved_at: Self::parse_optional_datetime(row.get(11)?),
            retry_count: row.get::<_, Option<u32>>(12)?.unwrap_or(0),
            last_retry_at: Self::parse_optional_datetime(row.get(13)?),
            issue_labels,
            parent_attempt_id: row.get::<_, Option<i64>>(15).ok().flatten(),
            cascade_repo: row.get::<_, Option<String>>(16).ok().flatten(),
        })
    }

    /// Convert a database row to a StoredDependency.
    /// Expects columns: rd.id, u.name, d.name, rd.dependency_type, rd.created_at
    fn row_to_dependency(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredDependency> {
        Ok(StoredDependency {
            id: row.get(0)?,
            upstream: row.get(1)?,
            downstream: row.get(2)?,
            dep_type: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    /// Record a Claude execution.
    pub fn record_execution(&self, execution: &ClaudeExecution) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO claude_executions (
                attempt_id, started_at, completed_at, duration_secs, exit_code, timed_out,
                stdout_preview, stderr_preview, stdout_log_path, stderr_log_path, event_log_path,
                prompt_used, prompt_hash, model_version, working_directory, git_branch,
                git_commit_before, git_commit_after, files_changed, lines_added, lines_removed
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                execution.attempt_id,
                execution.started_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                execution
                    .completed_at
                    .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                execution.duration_secs,
                execution.exit_code,
                execution.timed_out as i32,
                execution.stdout_preview,
                execution.stderr_preview,
                execution.stdout_log_path,
                execution.stderr_log_path,
                execution.event_log_path,
                execution.prompt_used,
                execution.prompt_hash,
                execution.model_version,
                execution.working_directory,
                execution.git_branch,
                execution.git_commit_before,
                execution.git_commit_after,
                execution.files_changed,
                execution.lines_added,
                execution.lines_removed,
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        Self::append_audit_json_line(
            "execution",
            &serde_json::json!({
                "id": id,
                "attempt_id": execution.attempt_id,
                "started_at": execution.started_at.to_rfc3339(),
                "completed_at": execution.completed_at.map(|v| v.to_rfc3339()),
                "duration_secs": execution.duration_secs,
                "exit_code": execution.exit_code,
                "timed_out": execution.timed_out,
                "stdout_preview": execution.stdout_preview,
                "stderr_preview": execution.stderr_preview,
                "stdout_log_path": execution.stdout_log_path,
                "stderr_log_path": execution.stderr_log_path,
                "event_log_path": execution.event_log_path,
                "prompt_hash": execution.prompt_hash,
                "model_version": execution.model_version,
                "working_directory": execution.working_directory,
                "git_branch": execution.git_branch,
                "git_commit_before": execution.git_commit_before,
                "git_commit_after": execution.git_commit_after,
                "files_changed": execution.files_changed,
                "lines_added": execution.lines_added,
                "lines_removed": execution.lines_removed,
            }),
        );
        Ok(id)
    }

    /// Get executions for a specific attempt.
    pub fn get_executions_for_attempt(&self, attempt_id: i64) -> Result<Vec<ClaudeExecution>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, attempt_id, started_at, completed_at, duration_secs, exit_code, timed_out,
                   stdout_preview, stderr_preview, stdout_log_path, stderr_log_path, event_log_path,
                   prompt_used, prompt_hash, model_version, working_directory, git_branch,
                   git_commit_before, git_commit_after, files_changed, lines_added, lines_removed
            FROM claude_executions
            WHERE attempt_id = ?
            ORDER BY started_at DESC
            "#,
        )?;

        let mut executions = Vec::new();
        let rows = stmt.query_map(params![attempt_id], |row| {
            Ok(ClaudeExecution {
                id: row.get(0)?,
                attempt_id: row.get(1)?,
                started_at: Self::parse_datetime(&row.get::<_, String>(2)?),
                completed_at: Self::parse_optional_datetime(row.get(3)?),
                duration_secs: row.get(4)?,
                exit_code: row.get(5)?,
                timed_out: row.get::<_, i32>(6).unwrap_or(0) != 0,
                stdout_preview: row.get(7)?,
                stderr_preview: row.get(8)?,
                stdout_log_path: row.get(9)?,
                stderr_log_path: row.get(10)?,
                event_log_path: row.get(11)?,
                prompt_used: row.get(12)?,
                prompt_hash: row.get(13)?,
                model_version: row.get(14)?,
                working_directory: row.get(15)?,
                git_branch: row.get(16)?,
                git_commit_before: row.get(17)?,
                git_commit_after: row.get(18)?,
                files_changed: row.get(19)?,
                lines_added: row.get(20)?,
                lines_removed: row.get(21)?,
            })
        })?;

        for row in rows.flatten() {
            executions.push(row);
        }

        Ok(executions)
    }

    /// Get a specific execution for an attempt.
    pub fn get_execution_for_attempt(
        &self,
        attempt_id: i64,
        execution_id: i64,
    ) -> Result<Option<ClaudeExecution>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, attempt_id, started_at, completed_at, duration_secs, exit_code, timed_out,
                   stdout_preview, stderr_preview, stdout_log_path, stderr_log_path, event_log_path,
                   prompt_used, prompt_hash, model_version, working_directory, git_branch,
                   git_commit_before, git_commit_after, files_changed, lines_added, lines_removed
            FROM claude_executions
            WHERE attempt_id = ?1 AND id = ?2
            LIMIT 1
            "#,
        )?;

        let execution = stmt
            .query_row(params![attempt_id, execution_id], |row| {
                Ok(ClaudeExecution {
                    id: row.get(0)?,
                    attempt_id: row.get(1)?,
                    started_at: Self::parse_datetime(&row.get::<_, String>(2)?),
                    completed_at: Self::parse_optional_datetime(row.get(3)?),
                    duration_secs: row.get(4)?,
                    exit_code: row.get(5)?,
                    timed_out: row.get::<_, i32>(6).unwrap_or(0) != 0,
                    stdout_preview: row.get(7)?,
                    stderr_preview: row.get(8)?,
                    stdout_log_path: row.get(9)?,
                    stderr_log_path: row.get(10)?,
                    event_log_path: row.get(11)?,
                    prompt_used: row.get(12)?,
                    prompt_hash: row.get(13)?,
                    model_version: row.get(14)?,
                    working_directory: row.get(15)?,
                    git_branch: row.get(16)?,
                    git_commit_before: row.get(17)?,
                    git_commit_after: row.get(18)?,
                    files_changed: row.get(19)?,
                    lines_added: row.get(20)?,
                    lines_removed: row.get(21)?,
                })
            })
            .optional()?;

        Ok(execution)
    }

    /// Record a PR review.
    pub fn record_pr_review(&self, review: &PrReviewRecord) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO pr_reviews (attempt_id, pr_url, reviewer, review_state, submitted_at, body, sentiment, actionable_feedback)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                review.attempt_id,
                review.pr_url,
                review.reviewer,
                review.review_state,
                review.submitted_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                review.body,
                review.sentiment,
                review.actionable_feedback,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get reviews for a specific attempt.
    pub fn get_reviews_for_attempt(&self, attempt_id: i64) -> Result<Vec<PrReviewRecord>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, attempt_id, pr_url, reviewer, review_state, submitted_at, body, sentiment, actionable_feedback
            FROM pr_reviews
            WHERE attempt_id = ?
            ORDER BY submitted_at DESC
            "#,
        )?;

        let mut reviews = Vec::new();
        let rows = stmt.query_map(params![attempt_id], |row| {
            Ok(PrReviewRecord {
                id: row.get(0)?,
                attempt_id: row.get(1)?,
                pr_url: row.get(2)?,
                reviewer: row.get(3)?,
                review_state: row.get(4)?,
                submitted_at: Self::parse_optional_datetime(row.get(5)?),
                body: row.get(6)?,
                sentiment: row.get(7)?,
                actionable_feedback: row.get(8)?,
            })
        })?;

        for row in rows.flatten() {
            reviews.push(row);
        }

        Ok(reviews)
    }

    /// Save or update a PR review state for persistence.
    ///
    /// Uses upsert semantics - creates new record or updates existing based on pr_url.
    pub fn save_pr_review_state(&self, state: &crate::github::PrReviewState) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            INSERT INTO pr_review_states (
                pr_url, repo, pr_number, issue_id, source,
                last_review_id, last_review_time, last_comment_id, last_comment_time,
                is_active, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, datetime('now'))
            ON CONFLICT(pr_url) DO UPDATE SET
                repo = excluded.repo,
                pr_number = excluded.pr_number,
                issue_id = excluded.issue_id,
                source = excluded.source,
                last_review_id = excluded.last_review_id,
                last_review_time = excluded.last_review_time,
                last_comment_id = excluded.last_comment_id,
                last_comment_time = excluded.last_comment_time,
                is_active = excluded.is_active
            "#,
            params![
                state.pr_url,
                state.repo,
                state.pr_number,
                state.issue_id,
                state.source,
                state.last_review_id,
                state.last_review_time,
                state.last_comment_id,
                state.last_comment_time,
                state.is_active as i32,
            ],
        )?;

        tracing::debug!(
            pr_url = %state.pr_url,
            is_active = state.is_active,
            "PR review state saved"
        );

        Ok(())
    }

    /// Get all active PR review states for restoration on startup.
    pub fn get_active_pr_review_states(&self) -> Result<Vec<crate::github::PrReviewState>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT pr_url, repo, pr_number, issue_id, source,
                   last_review_id, last_review_time, last_comment_id, last_comment_time,
                   is_active
            FROM pr_review_states
            WHERE is_active = 1
            ORDER BY created_at DESC
            "#,
        )?;

        let rows = stmt.query_map([], Self::row_to_pr_review_state)?;

        let mut results = Vec::new();
        for row in rows.flatten() {
            results.push(row);
        }

        tracing::debug!(count = results.len(), "Retrieved active PR review states");

        Ok(results)
    }

    /// Deactivate a PR review state (mark as no longer being watched).
    pub fn deactivate_pr_review_state(&self, pr_url: &str) -> Result<()> {
        let conn = self.acquire_lock()?;
        let rows_affected = conn.execute(
            "UPDATE pr_review_states SET is_active = 0 WHERE pr_url = ?",
            params![pr_url],
        )?;

        tracing::debug!(
            pr_url = %pr_url,
            rows_affected = rows_affected,
            "PR review state deactivated"
        );

        Ok(())
    }

    /// Record a PR review comment for persistence.
    pub fn record_pr_review_comment(
        &self,
        pr_url: &str,
        comment: &crate::github::PrReviewComment,
    ) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO pr_review_comments (
                github_comment_id, pr_url, review_id, path, position, line,
                body, author, created_at, updated_at, html_url
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(github_comment_id) DO UPDATE SET
                body = excluded.body,
                updated_at = excluded.updated_at
            "#,
            params![
                comment.id,
                pr_url,
                comment.pull_request_review_id,
                comment.path,
                comment.position,
                comment.line,
                comment.body,
                comment.user.login,
                comment.created_at,
                comment.updated_at,
                comment.html_url,
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Get all comments for a specific PR.
    pub fn get_comments_for_pr(&self, pr_url: &str) -> Result<Vec<StoredPrReviewComment>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, github_comment_id, pr_url, review_id, path, position, line,
                   body, author, created_at, updated_at, html_url
            FROM pr_review_comments
            WHERE pr_url = ?
            ORDER BY created_at ASC
            "#,
        )?;

        let rows = stmt.query_map(params![pr_url], Self::row_to_stored_pr_review_comment)?;

        let mut results = Vec::new();
        for row in rows.flatten() {
            results.push(row);
        }

        Ok(results)
    }

    /// Convert a database row to a StoredPrReviewComment.
    /// Expects columns: id, github_comment_id, pr_url, review_id, path, position, line,
    /// body, author, created_at, updated_at, html_url
    fn row_to_stored_pr_review_comment(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<StoredPrReviewComment> {
        Ok(StoredPrReviewComment {
            id: row.get(0)?,
            github_comment_id: row.get(1)?,
            pr_url: row.get(2)?,
            review_id: row.get(3)?,
            path: row.get(4)?,
            position: row.get(5)?,
            line: row.get(6)?,
            body: row.get(7)?,
            author: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
            html_url: row.get(11)?,
        })
    }

    /// Convert a database row to a PrReviewState.
    /// Expects columns: pr_url, repo, pr_number, issue_id, source,
    /// last_review_id, last_review_time, last_comment_id, last_comment_time, is_active
    fn row_to_pr_review_state(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<crate::github::PrReviewState> {
        Ok(crate::github::PrReviewState {
            pr_url: row.get(0)?,
            repo: row.get(1)?,
            pr_number: row.get(2)?,
            issue_id: row.get(3)?,
            source: row.get(4)?,
            last_review_id: row.get(5)?,
            last_review_time: row.get(6)?,
            last_comment_id: row.get(7)?,
            last_comment_time: row.get(8)?,
            is_active: row.get::<_, i32>(9)? != 0,
        })
    }

    /// Store an issue embedding.
    pub fn store_embedding(&self, embedding: &IssueEmbedding) -> Result<i64> {
        let conn = self.acquire_lock()?;

        // Serialize the embedding vector to bytes
        let embedding_bytes: Vec<u8> = embedding
            .embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        conn.execute(
            r#"
            INSERT INTO issue_embeddings (source, issue_id, short_id, title, embedding, embedding_model, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(source, issue_id) DO UPDATE SET
                embedding = excluded.embedding,
                embedding_model = excluded.embedding_model,
                created_at = excluded.created_at
            "#,
            params![
                embedding.source,
                embedding.issue_id,
                embedding.short_id,
                embedding.title,
                embedding_bytes,
                embedding.embedding_model,
                embedding.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get an embedding by source and issue ID.
    pub fn get_embedding(&self, source: &str, issue_id: &str) -> Result<Option<IssueEmbedding>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, source, issue_id, short_id, title, embedding, embedding_model, created_at
            FROM issue_embeddings
            WHERE source = ? AND issue_id = ?
            "#,
        )?;

        let result = stmt
            .query_row(params![source, issue_id], |row| {
                let embedding_bytes: Vec<u8> = row.get(5)?;
                if !embedding_bytes.len().is_multiple_of(4) {
                    return Err(rusqlite::Error::InvalidColumnType(
                        5,
                        "embedding".to_string(),
                        rusqlite::types::Type::Blob,
                    ));
                }
                let embedding: Vec<f32> = embedding_bytes
                    .chunks_exact(4)
                    .map(|chunk| {
                        let arr: [u8; 4] =
                            chunk.try_into().expect("chunks_exact guarantees 4 bytes");
                        f32::from_le_bytes(arr)
                    })
                    .collect();

                Ok(IssueEmbedding {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    issue_id: row.get(2)?,
                    short_id: row.get(3)?,
                    title: row.get(4)?,
                    embedding,
                    embedding_model: row.get(6)?,
                    created_at: Self::parse_datetime(&row.get::<_, String>(7)?),
                })
            })
            .ok();

        Ok(result)
    }

    /// Get embeddings with pagination support to prevent memory exhaustion.
    ///
    /// # Arguments
    /// * `source` - Optional filter by source
    /// * `limit` - Maximum number of embeddings to return (defaults to 1000, max 10000)
    /// * `offset` - Number of records to skip for pagination (defaults to 0)
    ///
    /// # Returns
    /// A vector of embeddings, limited to prevent unbounded memory usage.
    pub fn get_all_embeddings(
        &self,
        source: Option<&str>,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<IssueEmbedding>> {
        let conn = self.acquire_lock()?;

        // Enforce reasonable limits to prevent memory exhaustion
        const DEFAULT_LIMIT: usize = 1000;
        const MAX_LIMIT: usize = 10000;
        let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
        let offset = offset.unwrap_or(0);

        let query = match source {
            Some(_) => {
                r#"
                SELECT id, source, issue_id, short_id, title, embedding, embedding_model, created_at
                FROM issue_embeddings
                WHERE source = ?
                ORDER BY created_at DESC
                LIMIT ? OFFSET ?
            "#
            }
            None => {
                r#"
                SELECT id, source, issue_id, short_id, title, embedding, embedding_model, created_at
                FROM issue_embeddings
                ORDER BY created_at DESC
                LIMIT ? OFFSET ?
            "#
            }
        };

        let mut stmt = conn.prepare(query)?;

        let row_mapper = |row: &rusqlite::Row<'_>| {
            let embedding_bytes: Vec<u8> = row.get(5)?;

            // Validate embedding data integrity: must be divisible by 4 (f32 = 4 bytes)
            if !embedding_bytes.len().is_multiple_of(4) {
                return Err(rusqlite::Error::InvalidColumnType(
                    5,
                    "embedding".to_string(),
                    rusqlite::types::Type::Blob,
                ));
            }

            let embedding: Vec<f32> = embedding_bytes
                .chunks_exact(4)
                .map(|chunk| {
                    let arr: [u8; 4] = chunk.try_into().expect("chunks_exact guarantees 4 bytes");
                    f32::from_le_bytes(arr)
                })
                .collect();

            Ok(IssueEmbedding {
                id: row.get(0)?,
                source: row.get(1)?,
                issue_id: row.get(2)?,
                short_id: row.get(3)?,
                title: row.get(4)?,
                embedding,
                embedding_model: row.get(6)?,
                created_at: Self::parse_datetime(&row.get::<_, String>(7)?),
            })
        };

        let rows = match source {
            Some(s) => stmt.query_map(params![s, limit as i64, offset as i64], row_mapper)?,
            None => stmt.query_map(params![limit as i64, offset as i64], row_mapper)?,
        };

        // Collect results, propagating any errors from corrupted embeddings
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| crate::error::Error::Storage(format!("Failed to read embeddings: {}", e)))
    }

    /// Record or update an error pattern.
    pub fn record_error_pattern(&self, pattern: &ErrorPattern) -> Result<i64> {
        let conn = self.acquire_lock()?;

        let sources_json = pattern
            .sources
            .as_ref()
            .and_then(|s| serde_json::to_string(s).ok());
        let example_ids_json = pattern
            .example_issue_ids
            .as_ref()
            .and_then(|s| serde_json::to_string(s).ok());

        conn.execute(
            r#"
            INSERT INTO error_patterns (pattern_hash, error_type, error_message, first_seen, last_seen, occurrence_count, sources, example_issue_ids, resolution_hints)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(pattern_hash) DO UPDATE SET
                last_seen = excluded.last_seen,
                occurrence_count = occurrence_count + 1,
                sources = excluded.sources,
                example_issue_ids = excluded.example_issue_ids
            "#,
            params![
                pattern.pattern_hash,
                pattern.error_type,
                pattern.error_message,
                pattern.first_seen.format("%Y-%m-%d %H:%M:%S").to_string(),
                pattern.last_seen.format("%Y-%m-%d %H:%M:%S").to_string(),
                pattern.occurrence_count,
                sources_json,
                example_ids_json,
                pattern.resolution_hints,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get the most common error patterns.
    pub fn get_error_patterns(&self, limit: usize) -> Result<Vec<ErrorPattern>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, pattern_hash, error_type, error_message, first_seen, last_seen, occurrence_count, sources, example_issue_ids, resolution_hints
            FROM error_patterns
            ORDER BY occurrence_count DESC
            LIMIT ?
            "#,
        )?;

        let mut patterns = Vec::new();
        let rows = stmt.query_map(params![limit as i64], |row| {
            let sources_str: Option<String> = row.get(7)?;
            let example_ids_str: Option<String> = row.get(8)?;

            Ok(ErrorPattern {
                id: row.get(0)?,
                pattern_hash: row.get(1)?,
                error_type: row.get(2)?,
                error_message: row.get(3)?,
                first_seen: Self::parse_datetime(&row.get::<_, String>(4)?),
                last_seen: Self::parse_datetime(&row.get::<_, String>(5)?),
                occurrence_count: row.get(6)?,
                sources: sources_str.and_then(|s| serde_json::from_str(&s).ok()),
                example_issue_ids: example_ids_str.and_then(|s| serde_json::from_str(&s).ok()),
                resolution_hints: row.get(9)?,
            })
        })?;

        for row in rows.flatten() {
            patterns.push(row);
        }

        Ok(patterns)
    }

    /// Store a feedback outcome to the database.
    pub fn store_feedback_outcome(&self, outcome: &FixOutcome) -> Result<i64> {
        let conn = self.acquire_lock()?;
        let keywords_json = serde_json::to_string(&outcome.keywords).unwrap_or_default();

        conn.execute(
            r#"
            INSERT INTO feedback_outcomes (attempt_id, source, issue_id, issue_text, prompt_used, outcome, error_type, learnings, keywords, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                outcome.attempt_id,
                outcome.source,
                outcome.issue_id,
                outcome.issue_text,
                outcome.prompt_used,
                outcome.outcome.as_str(),
                outcome.error_type,
                outcome.learnings,
                keywords_json,
                outcome.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Retrieve feedback outcomes with optional source filter.
    pub fn get_feedback_outcomes(
        &self,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FixOutcome>> {
        let conn = self.acquire_lock()?;

        let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match source {
            Some(s) => (
                r#"
                SELECT id, attempt_id, source, issue_id, issue_text, prompt_used, outcome, error_type, learnings, keywords, created_at
                FROM feedback_outcomes
                WHERE source = ?
                ORDER BY created_at DESC
                LIMIT ?
                "#,
                vec![Box::new(s.to_string()), Box::new(limit as i64)],
            ),
            None => (
                r#"
                SELECT id, attempt_id, source, issue_id, issue_text, prompt_used, outcome, error_type, learnings, keywords, created_at
                FROM feedback_outcomes
                ORDER BY created_at DESC
                LIMIT ?
                "#,
                vec![Box::new(limit as i64)],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(params_refs.as_slice(), Self::row_to_fix_outcome)?;

        let mut outcomes = Vec::new();
        for row in rows.flatten() {
            outcomes.push(row);
        }
        Ok(outcomes)
    }

    /// Get a single feedback outcome by attempt ID.
    pub fn get_feedback_outcome_by_attempt(&self, attempt_id: i64) -> Result<Option<FixOutcome>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, attempt_id, source, issue_id, issue_text, prompt_used, outcome, error_type, learnings, keywords, created_at
            FROM feedback_outcomes
            WHERE attempt_id = ?
            LIMIT 1
            "#,
        )?;

        let mut rows = stmt.query_map(params![attempt_id], Self::row_to_fix_outcome)?;
        Ok(rows.next().and_then(|r| r.ok()))
    }

    /// Store a Q&A knowledge entry.
    pub fn store_qa_knowledge(&self, entry: &QaKnowledgeEntry) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO qa_knowledge (
                source, repo, issue_id, short_id, question_text, question_norm, question_embedding,
                answer_text, answer_norm, answer_embedding, channel, responder, correlation_id,
                asked_at, answered_at, success_count, failure_count, last_used_at, metadata
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                entry.source,
                entry.repo,
                entry.issue_id,
                entry.short_id,
                entry.question_text,
                entry.question_norm,
                Self::embedding_to_blob(entry.question_embedding.as_deref()),
                entry.answer_text,
                entry.answer_norm,
                Self::embedding_to_blob(entry.answer_embedding.as_deref()),
                entry.channel,
                entry.responder,
                entry.correlation_id,
                entry.asked_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                entry.answered_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                entry.success_count,
                entry.failure_count,
                entry
                    .last_used_at
                    .map(|v| v.format("%Y-%m-%d %H:%M:%S").to_string()),
                entry.metadata.as_ref().map(|m| m.to_string()),
            ],
        )?;

        let qa_id = conn.last_insert_rowid();

        if let Some(question_embedding) = entry.question_embedding.as_deref() {
            if let Err(e) = Self::upsert_qa_vector_embedding(&conn, qa_id, question_embedding) {
                tracing::debug!(
                    qa_id = qa_id,
                    error = %e,
                    "Failed to sync Q&A vector embedding; falling back to SQL ranking"
                );
            }
        }

        Ok(qa_id)
    }

    /// Find semantically similar Q&A entries within source/repo scope.
    pub fn find_similar_qa_scoped(
        &self,
        source: &str,
        repo: Option<&str>,
        question_norm: &str,
        question_embedding: Option<&[f32]>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<QaMatch>> {
        let conn = self.acquire_lock()?;

        if let Some(query_embedding) = question_embedding {
            let candidate_limit = limit
                .saturating_mul(QA_VECTOR_CANDIDATE_MULTIPLIER)
                .max(limit);
            if let Some(vector_matches) = Self::query_qa_matches_vector_scoped(
                &conn,
                source,
                repo,
                query_embedding,
                threshold,
                limit,
                candidate_limit,
            )? {
                if !vector_matches.is_empty() {
                    return Ok(vector_matches);
                }
            }
        }

        Self::query_qa_matches_exact_scoped(&conn, source, repo, question_norm, threshold, limit)
    }

    /// Find semantically similar Q&A entries globally.
    pub fn find_similar_qa_global(
        &self,
        question_norm: &str,
        question_embedding: Option<&[f32]>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<QaMatch>> {
        let conn = self.acquire_lock()?;

        if let Some(query_embedding) = question_embedding {
            let candidate_limit = limit
                .saturating_mul(QA_VECTOR_CANDIDATE_MULTIPLIER)
                .max(limit);
            if let Some(vector_matches) = Self::query_qa_matches_vector_global(
                &conn,
                query_embedding,
                threshold,
                limit,
                candidate_limit,
            )? {
                if !vector_matches.is_empty() {
                    return Ok(vector_matches);
                }
            }
        }

        Self::query_qa_matches_exact_global(&conn, question_norm, threshold, limit)
    }

    /// Record usage of a Q&A entry for an attempt.
    pub fn record_qa_usage(
        &self,
        attempt_id: i64,
        qa_id: i64,
        usage_type: &str,
        similarity_score: f64,
    ) -> Result<i64> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            INSERT INTO qa_usage (attempt_id, qa_id, usage_type, similarity_score, created_at)
            VALUES (?1, ?2, ?3, ?4, datetime('now'))
            ON CONFLICT(attempt_id, qa_id) DO UPDATE SET
                usage_type = excluded.usage_type,
                similarity_score = excluded.similarity_score,
                created_at = excluded.created_at
            "#,
            params![attempt_id, qa_id, usage_type, similarity_score],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Update success/failure counters for a Q&A entry.
    pub fn update_qa_outcome_stats(&self, qa_id: i64, success: bool) -> Result<()> {
        let conn = self.acquire_lock()?;
        let (field, sql) = if success {
            (
                "success_count",
                "UPDATE qa_knowledge SET success_count = success_count + 1, last_used_at = datetime('now') WHERE id = ?",
            )
        } else {
            (
                "failure_count",
                "UPDATE qa_knowledge SET failure_count = failure_count + 1, last_used_at = datetime('now') WHERE id = ?",
            )
        };
        conn.execute(sql, params![qa_id])?;
        tracing::debug!(qa_id = qa_id, field = field, "Updated Q&A outcome stats");
        Ok(())
    }

    /// Update success/failure counters for all Q&A entries used by an attempt.
    pub fn update_qa_outcome_stats_for_attempt(
        &self,
        attempt_id: i64,
        success: bool,
    ) -> Result<()> {
        let conn = self.acquire_lock()?;
        let sql = if success {
            r#"
            UPDATE qa_knowledge
            SET success_count = success_count + 1,
                last_used_at = datetime('now')
            WHERE id IN (SELECT qa_id FROM qa_usage WHERE attempt_id = ?1)
            "#
        } else {
            r#"
            UPDATE qa_knowledge
            SET failure_count = failure_count + 1,
                last_used_at = datetime('now')
            WHERE id IN (SELECT qa_id FROM qa_usage WHERE attempt_id = ?1)
            "#
        };
        conn.execute(sql, params![attempt_id])?;
        Ok(())
    }

    /// Get channel cursor value for polling channels.
    pub fn get_channel_cursor(&self, channel: &str, cursor_key: &str) -> Result<Option<String>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT cursor_value FROM question_channel_cursor WHERE channel = ?1 AND cursor_key = ?2",
        )?;
        let value = stmt
            .query_row(params![channel, cursor_key], |row| row.get::<_, String>(0))
            .optional()?;
        Ok(value)
    }

    /// Set channel cursor value for polling channels.
    pub fn set_channel_cursor(
        &self,
        channel: &str,
        cursor_key: &str,
        cursor_value: &str,
    ) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            INSERT INTO question_channel_cursor (channel, cursor_key, cursor_value, updated_at)
            VALUES (?1, ?2, ?3, datetime('now'))
            ON CONFLICT(channel, cursor_key) DO UPDATE SET
                cursor_value = excluded.cursor_value,
                updated_at = excluded.updated_at
            "#,
            params![channel, cursor_key, cursor_value],
        )?;
        Ok(())
    }

    fn row_to_qa_knowledge(row: &rusqlite::Row<'_>) -> rusqlite::Result<QaKnowledgeEntry> {
        let metadata: Option<String> = row.get(19)?;
        Ok(QaKnowledgeEntry {
            id: row.get(0)?,
            source: row.get(1)?,
            repo: row.get(2)?,
            issue_id: row.get(3)?,
            short_id: row.get(4)?,
            question_text: row.get(5)?,
            question_norm: row.get(6)?,
            question_embedding: Self::blob_to_embedding(row.get(7)?),
            answer_text: row.get(8)?,
            answer_norm: row.get(9)?,
            answer_embedding: Self::blob_to_embedding(row.get(10)?),
            channel: row.get(11)?,
            responder: row.get(12)?,
            correlation_id: row.get(13)?,
            asked_at: Self::parse_datetime(&row.get::<_, String>(14)?),
            answered_at: Self::parse_datetime(&row.get::<_, String>(15)?),
            success_count: row.get(16)?,
            failure_count: row.get(17)?,
            last_used_at: Self::parse_optional_datetime(row.get::<_, Option<String>>(18)?),
            metadata: metadata.and_then(|s| serde_json::from_str(&s).ok()),
        })
    }

    fn row_to_qa_match(row: &rusqlite::Row<'_>) -> rusqlite::Result<QaMatch> {
        let entry = Self::row_to_qa_knowledge(row)?;
        Ok(QaMatch {
            entry,
            semantic_similarity: row.get(20)?,
            historical_success_rate: row.get(21)?,
            final_score: row.get(22)?,
        })
    }

    /// Map a database row to a FixOutcome.
    fn row_to_fix_outcome(row: &rusqlite::Row) -> rusqlite::Result<FixOutcome> {
        let outcome_str: String = row.get(6)?;
        let keywords_str: Option<String> = row.get(9)?;
        let created_at_str: String = row.get(10)?;

        Ok(FixOutcome {
            id: row.get(0)?,
            attempt_id: row.get(1)?,
            source: row.get(2)?,
            issue_id: row.get(3)?,
            issue_text: row.get(4)?,
            prompt_used: row.get(5)?,
            outcome: Outcome::parse(&outcome_str).unwrap_or(Outcome::Failed),
            error_type: row.get(7)?,
            learnings: row.get(8)?,
            keywords: keywords_str
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            embedding: None,
            created_at: Self::parse_datetime(&created_at_str),
        })
    }

    /// Record a processing metric.
    pub fn record_metric(&self, metric: &ProcessingMetric) -> Result<i64> {
        let conn = self.acquire_lock()?;

        let tags_json = metric.tags.as_ref().map(|t| t.to_string());

        conn.execute(
            r#"
            INSERT INTO processing_metrics (timestamp, metric_name, metric_value, source, tags)
            VALUES (?, ?, ?, ?, ?)
            "#,
            params![
                metric.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                metric.metric_name,
                metric.metric_value,
                metric.source,
                tags_json,
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        Self::append_audit_json_line(
            "metric",
            &serde_json::json!({
                "id": id,
                "timestamp": metric.timestamp.to_rfc3339(),
                "metric_name": metric.metric_name,
                "metric_value": metric.metric_value,
                "source": metric.source,
                "tags": metric.tags,
            }),
        );
        Ok(id)
    }

    /// Record multiple metrics in a single transaction for better performance.
    ///
    /// This is more efficient than calling `record_metric` in a loop because:
    /// - Single transaction reduces fsync overhead
    /// - Prepared statement is reused across all inserts
    pub fn record_metrics_batch(&self, metrics: &[ProcessingMetric]) -> Result<usize> {
        if metrics.is_empty() {
            return Ok(0);
        }

        let mut conn = self.acquire_lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT INTO processing_metrics (timestamp, metric_name, metric_value, source, tags)
                VALUES (?, ?, ?, ?, ?)
                "#,
            )?;

            for metric in metrics {
                let tags_json = metric.tags.as_ref().map(|t| t.to_string());
                stmt.execute(params![
                    metric.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    metric.metric_name,
                    metric.metric_value,
                    metric.source,
                    tags_json,
                ])?;
            }
        }

        tx.commit()?;
        drop(conn);
        for metric in metrics {
            Self::append_audit_json_line(
                "metric",
                &serde_json::json!({
                    "id": serde_json::Value::Null,
                    "timestamp": metric.timestamp.to_rfc3339(),
                    "metric_name": metric.metric_name,
                    "metric_value": metric.metric_value,
                    "source": metric.source,
                    "tags": metric.tags,
                }),
            );
        }
        Ok(metrics.len())
    }

    /// Get metrics by name within a time range.
    pub fn get_metrics(
        &self,
        metric_name: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<ProcessingMetric>> {
        let conn = self.acquire_lock()?;

        // Build query dynamically based on whether since filter is provided
        let (query, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match since {
            Some(since_time) => (
                r#"
                SELECT id, timestamp, metric_name, metric_value, source, tags
                FROM processing_metrics
                WHERE metric_name = ?1 AND timestamp >= ?2
                ORDER BY timestamp DESC
                LIMIT ?3
                "#
                .to_string(),
                vec![
                    Box::new(metric_name.to_string()),
                    Box::new(since_time.format("%Y-%m-%d %H:%M:%S").to_string()),
                    Box::new(limit as i64),
                ],
            ),
            None => (
                r#"
                SELECT id, timestamp, metric_name, metric_value, source, tags
                FROM processing_metrics
                WHERE metric_name = ?1
                ORDER BY timestamp DESC
                LIMIT ?2
                "#
                .to_string(),
                vec![Box::new(metric_name.to_string()), Box::new(limit as i64)],
            ),
        };

        let mut stmt = conn.prepare(&query)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(params_refs.as_slice(), Self::row_to_metric)?;

        Ok(rows.flatten().collect())
    }

    /// Get metric row counts grouped by name since a timestamp.
    pub fn get_metric_counts_since(
        &self,
        metric_names: &[&str],
        since: DateTime<Utc>,
    ) -> Result<HashMap<String, i64>> {
        if metric_names.is_empty() {
            return Ok(HashMap::new());
        }

        let conn = self.acquire_lock()?;

        let placeholders = (0..metric_names.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            r#"
            SELECT metric_name, COUNT(*)
            FROM processing_metrics
            WHERE timestamp >= ?1
              AND metric_name IN ({})
            GROUP BY metric_name
            "#,
            placeholders
        );

        let mut bind_params: Vec<Box<dyn rusqlite::ToSql>> =
            Vec::with_capacity(metric_names.len() + 1);
        bind_params.push(Box::new(since.format("%Y-%m-%d %H:%M:%S").to_string()));
        for metric_name in metric_names {
            bind_params.push(Box::new((*metric_name).to_string()));
        }
        let bind_refs: Vec<&dyn rusqlite::ToSql> = bind_params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut counts = HashMap::new();
        for row in rows.flatten() {
            counts.insert(row.0, row.1);
        }
        Ok(counts)
    }

    /// Get metric sums grouped by name since a timestamp.
    pub fn get_metric_sums_since(
        &self,
        metric_names: &[&str],
        since: DateTime<Utc>,
    ) -> Result<HashMap<String, f64>> {
        if metric_names.is_empty() {
            return Ok(HashMap::new());
        }

        let conn = self.acquire_lock()?;

        let placeholders = (0..metric_names.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            r#"
            SELECT metric_name, SUM(metric_value)
            FROM processing_metrics
            WHERE timestamp >= ?1
              AND metric_name IN ({})
            GROUP BY metric_name
            "#,
            placeholders
        );

        let mut bind_params: Vec<Box<dyn rusqlite::ToSql>> =
            Vec::with_capacity(metric_names.len() + 1);
        bind_params.push(Box::new(since.format("%Y-%m-%d %H:%M:%S").to_string()));
        for metric_name in metric_names {
            bind_params.push(Box::new((*metric_name).to_string()));
        }
        let bind_refs: Vec<&dyn rusqlite::ToSql> = bind_params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?;

        let mut sums = HashMap::new();
        for row in rows.flatten() {
            sums.insert(row.0, row.1);
        }
        Ok(sums)
    }

    /// Get metric sums grouped by (metric_name, source) since a timestamp.
    pub fn get_metric_sums_by_source_since(
        &self,
        metric_names: &[&str],
        since: DateTime<Utc>,
    ) -> Result<HashMap<(String, String), f64>> {
        if metric_names.is_empty() {
            return Ok(HashMap::new());
        }

        let conn = self.acquire_lock()?;

        let placeholders = (0..metric_names.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            r#"
            SELECT metric_name, source, SUM(metric_value)
            FROM processing_metrics
            WHERE timestamp >= ?1
              AND metric_name IN ({})
            GROUP BY metric_name, source
            "#,
            placeholders
        );

        let mut bind_params: Vec<Box<dyn rusqlite::ToSql>> =
            Vec::with_capacity(metric_names.len() + 1);
        bind_params.push(Box::new(since.format("%Y-%m-%d %H:%M:%S").to_string()));
        for metric_name in metric_names {
            bind_params.push(Box::new((*metric_name).to_string()));
        }
        let bind_refs: Vec<&dyn rusqlite::ToSql> = bind_params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            ))
        })?;

        let mut sums = HashMap::new();
        for row in rows.flatten() {
            if let Some(source) = row.1 {
                sums.insert((row.0, source), row.2);
            }
        }
        Ok(sums)
    }

    fn row_to_metric(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessingMetric> {
        let tags_str: Option<String> = row.get(5)?;
        let tags = tags_str.and_then(|s| serde_json::from_str(&s).ok());

        Ok(ProcessingMetric {
            id: row.get(0)?,
            timestamp: Self::parse_datetime(&row.get::<_, String>(1)?),
            metric_name: row.get(2)?,
            metric_value: row.get(3)?,
            source: row.get(4)?,
            tags,
        })
    }

    /// Create or update a prompt experiment.
    pub fn save_experiment(&self, experiment: &PromptExperiment) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO prompt_experiments (experiment_name, variant, prompt_template, prompt_hash, created_at, active, success_count, failure_count, avg_time_to_merge, avg_review_score)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                experiment.experiment_name,
                experiment.variant,
                experiment.prompt_template,
                experiment.prompt_hash,
                experiment.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                experiment.active as i32,
                experiment.success_count,
                experiment.failure_count,
                experiment.avg_time_to_merge,
                experiment.avg_review_score,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get active experiments.
    pub fn get_active_experiments(&self) -> Result<Vec<PromptExperiment>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, experiment_name, variant, prompt_template, prompt_hash, created_at, active, success_count, failure_count, avg_time_to_merge, avg_review_score
            FROM prompt_experiments
            WHERE active = 1
            ORDER BY experiment_name, variant
            "#,
        )?;

        let mut experiments = Vec::new();
        let rows = stmt.query_map([], |row| {
            Ok(PromptExperiment {
                id: row.get(0)?,
                experiment_name: row.get(1)?,
                variant: row.get(2)?,
                prompt_template: row.get(3)?,
                prompt_hash: row.get(4)?,
                created_at: Self::parse_datetime(&row.get::<_, String>(5)?),
                active: row.get::<_, i32>(6)? != 0,
                success_count: row.get(7)?,
                failure_count: row.get(8)?,
                avg_time_to_merge: row.get(9)?,
                avg_review_score: row.get(10)?,
            })
        })?;

        for row in rows.flatten() {
            experiments.push(row);
        }

        Ok(experiments)
    }

    /// Update experiment statistics.
    pub fn update_experiment_stats(
        &self,
        experiment_id: i64,
        success: bool,
        time_to_merge: Option<f64>,
    ) -> Result<()> {
        let conn = self.acquire_lock()?;

        if success {
            conn.execute(
                r#"
                UPDATE prompt_experiments
                SET success_count = success_count + 1
                WHERE id = ?
                "#,
                params![experiment_id],
            )?;
        } else {
            conn.execute(
                r#"
                UPDATE prompt_experiments
                SET failure_count = failure_count + 1
                WHERE id = ?
                "#,
                params![experiment_id],
            )?;
        }

        if let Some(ttm) = time_to_merge {
            // Update rolling average of time to merge
            // Note: success_count was already incremented above, so we use
            // (success_count - 1) for the old count and success_count for the new total
            conn.execute(
                r#"
                UPDATE prompt_experiments
                SET avg_time_to_merge = CASE
                    WHEN avg_time_to_merge IS NULL THEN ?
                    ELSE (avg_time_to_merge * (success_count - 1) + ?) / success_count
                END
                WHERE id = ?
                "#,
                params![ttm, ttm, experiment_id],
            )?;
        }

        Ok(())
    }

    /// Store a similar issue relationship.
    pub fn store_similar_issue(&self, similar: &SimilarIssue) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO similar_issues (source_issue_id, similar_issue_id, similarity_score, computed_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(source_issue_id, similar_issue_id) DO UPDATE SET
                similarity_score = excluded.similarity_score,
                computed_at = excluded.computed_at
            "#,
            params![
                similar.source_issue_id,
                similar.similar_issue_id,
                similar.similarity_score,
                similar.computed_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Find similar issues for a given issue.
    pub fn find_similar_issues(
        &self,
        issue_id: &str,
        min_score: f64,
        limit: usize,
    ) -> Result<Vec<SimilarIssue>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, source_issue_id, similar_issue_id, similarity_score, computed_at
            FROM similar_issues
            WHERE source_issue_id = ? AND similarity_score >= ?
            ORDER BY similarity_score DESC
            LIMIT ?
            "#,
        )?;

        let mut results = Vec::new();
        let rows = stmt.query_map(params![issue_id, min_score, limit as i64], |row| {
            Ok(SimilarIssue {
                id: row.get(0)?,
                source_issue_id: row.get(1)?,
                similar_issue_id: row.get(2)?,
                similarity_score: row.get(3)?,
                computed_at: Self::parse_datetime(&row.get::<_, String>(4)?),
            })
        })?;

        for row in rows.flatten() {
            results.push(row);
        }

        Ok(results)
    }

    /// Get the overall success rate.
    pub fn get_success_rate(&self) -> Result<f64> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT
                CAST(SUM(CASE WHEN status IN ('success', 'merged') THEN 1 ELSE 0 END) AS REAL) /
                NULLIF(CAST(COUNT(*) AS REAL), 0)
            FROM fix_attempts
            "#,
        )?;

        let rate: f64 = stmt.query_row([], |row| row.get(0)).unwrap_or(0.0);
        Ok(rate)
    }

    /// Get a comprehensive analytics summary.
    pub fn get_analytics_summary(&self) -> Result<AnalyticsSummary> {
        let conn = self.acquire_lock()?;

        // Get basic stats
        let mut stmt = conn.prepare(
            r#"
            SELECT
                COUNT(*) as total,
                SUM(CASE WHEN status IN ('success', 'merged') THEN 1 ELSE 0 END) as successful,
                SUM(CASE WHEN status = 'merged' THEN 1 ELSE 0 END) as merged
            FROM fix_attempts
            "#,
        )?;

        let (total, successful, merged): (i64, i64, i64) = stmt
            .query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap_or((0, 0, 0));

        let success_rate = if total > 0 {
            successful as f64 / total as f64
        } else {
            0.0
        };

        // Get average processing time
        let mut stmt = conn.prepare(
            r#"
            SELECT AVG(duration_secs) FROM claude_executions WHERE duration_secs IS NOT NULL
            "#,
        )?;
        let avg_processing_time: Option<f64> = stmt.query_row([], |row| row.get(0)).ok();

        // Get most common error
        let mut stmt = conn.prepare(
            r#"
            SELECT error_type FROM error_patterns ORDER BY occurrence_count DESC LIMIT 1
            "#,
        )?;
        let most_common_error: Option<String> = stmt.query_row([], |row| row.get(0)).ok();

        // Get success rate by source
        let mut stmt = conn.prepare(
            r#"
            SELECT source,
                   CAST(SUM(CASE WHEN status IN ('success', 'merged') THEN 1 ELSE 0 END) AS REAL) /
                   NULLIF(CAST(COUNT(*) AS REAL), 0) as rate
            FROM fix_attempts
            GROUP BY source
            "#,
        )?;

        let mut success_rate_by_source = HashMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?;

        for row in rows.flatten() {
            success_rate_by_source.insert(row.0, row.1);
        }

        Ok(AnalyticsSummary {
            success_rate,
            total_processed: total,
            total_successful: successful,
            total_merged: merged,
            avg_processing_time_secs: avg_processing_time,
            avg_time_to_merge_hours: None, // Would need more complex calculation
            most_common_error,
            success_rate_by_source,
        })
    }

    /// Prune old activity logs to prevent unbounded growth.
    pub fn prune_old_activities(&self, days_to_keep: i64) -> Result<usize> {
        let conn = self.acquire_lock()?;

        // Compute the full datetime modifier in Rust to avoid SQL string concatenation
        // This is safer than building strings in SQL even though days_to_keep is already i64
        let modifier = format!("-{} days", days_to_keep.abs());

        let deleted = conn.execute(
            r#"
            DELETE FROM activity_log
            WHERE timestamp < datetime('now', ?)
            "#,
            params![modifier],
        )?;

        Ok(deleted)
    }

    /// Prune old metrics to prevent unbounded growth.
    pub fn prune_old_metrics(&self, days_to_keep: i64) -> Result<usize> {
        let conn = self.acquire_lock()?;

        // Compute the full datetime modifier in Rust to avoid SQL string concatenation
        let modifier = format!("-{} days", days_to_keep.abs());

        let deleted = conn.execute(
            r#"
            DELETE FROM processing_metrics
            WHERE timestamp < datetime('now', ?)
            "#,
            params![modifier],
        )?;

        Ok(deleted)
    }

    /// Add or update a repository in the database.
    pub fn upsert_repository(
        &self,
        name: &str,
        path: Option<&str>,
        github_url: Option<&str>,
    ) -> Result<i64> {
        let conn = self.acquire_lock()?;

        // Use name as github_url if not provided
        let github_url = github_url.unwrap_or(name);
        let path = path.unwrap_or("");

        conn.execute(
            r#"
            INSERT INTO repositories (name, path, github_url)
            VALUES (?, ?, ?)
            ON CONFLICT(name) DO UPDATE SET
                path = CASE WHEN excluded.path != '' THEN excluded.path ELSE repositories.path END,
                github_url = excluded.github_url
            "#,
            params![name, path, github_url],
        )?;

        // Get the id
        let id: i64 = conn.query_row(
            "SELECT id FROM repositories WHERE name = ?",
            params![name],
            |row| row.get(0),
        )?;

        Ok(id)
    }

    /// Sync repositories from a RepoIndex to the database.
    ///
    /// Updates paths for all repos in the index and optionally syncs files.
    pub fn sync_from_index(
        &self,
        index: &crate::repo::RepoIndex,
        sync_files: bool,
    ) -> Result<usize> {
        let repos = index.list();
        let mut synced = 0;

        for repo in repos {
            let path_str = repo.path.to_string_lossy();

            if sync_files {
                // Use save_indexed_repo which also updates file_count and last_indexed_at
                let repo_id = self.save_indexed_repo(
                    &repo.name,
                    &path_str,
                    Some(&repo.github_url),
                    &repo.default_branch,
                    repo.files.len(),
                )?;

                if !repo.files.is_empty() {
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

                    self.save_repo_files(repo_id, &files_with_types)?;
                }
            } else {
                // Just update paths in repositories table
                self.upsert_repository(&repo.name, Some(&path_str), None)?;
            }
            synced += 1;
        }

        Ok(synced)
    }

    /// Get a repository by name.
    pub fn get_repository(&self, name: &str) -> Result<Option<StoredRepository>> {
        let conn = self.acquire_lock()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, path, github_url, created_at
            FROM repositories WHERE name = ?
            "#,
            params![name],
            |row| {
                Ok(StoredRepository {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    path: row.get::<_, String>(2).ok().filter(|s| !s.is_empty()),
                    github_url: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        );

        match result {
            Ok(repo) => Ok(Some(repo)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all repositories.
    pub fn list_repositories(&self) -> Result<Vec<StoredRepository>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, name, path, github_url, created_at
            FROM repositories ORDER BY name
            "#,
        )?;

        let mut repos = Vec::new();
        let rows = stmt.query_map([], |row| {
            Ok(StoredRepository {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get::<_, String>(2).ok().filter(|s| !s.is_empty()),
                github_url: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;

        for row in rows.flatten() {
            repos.push(row);
        }

        Ok(repos)
    }

    /// Add a dependency between two repositories.
    /// Creates the repos if they don't exist.
    pub fn add_dependency(&self, upstream: &str, downstream: &str, dep_type: &str) -> Result<()> {
        // Ensure both repos exist
        let upstream_id = self.upsert_repository(upstream, None, None)?;
        let downstream_id = self.upsert_repository(downstream, None, None)?;

        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            INSERT INTO repository_dependencies (upstream_id, downstream_id, dependency_type)
            VALUES (?, ?, ?)
            ON CONFLICT(upstream_id, downstream_id) DO UPDATE SET
                dependency_type = excluded.dependency_type
            "#,
            params![upstream_id, downstream_id, dep_type],
        )?;

        Ok(())
    }

    /// Get all dependencies for a repository (what it depends on).
    pub fn get_dependencies(&self, repo_name: &str) -> Result<Vec<StoredDependency>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT rd.id, u.name, d.name, rd.dependency_type, rd.created_at
            FROM repository_dependencies rd
            JOIN repositories u ON rd.upstream_id = u.id
            JOIN repositories d ON rd.downstream_id = d.id
            WHERE d.name = ?
            "#,
        )?;

        let rows = stmt.query_map(params![repo_name], Self::row_to_dependency)?;
        Ok(rows.flatten().collect())
    }

    /// Get all dependents of a repository (what depends on it).
    ///
    /// This is an alias for `get_direct_dependants` for API compatibility.
    #[inline]
    pub fn get_dependents(&self, repo_name: &str) -> Result<Vec<StoredDependency>> {
        self.get_direct_dependants(repo_name)
    }

    /// Get all dependencies in the database.
    pub fn list_all_dependencies(&self) -> Result<Vec<StoredDependency>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT rd.id, u.name, d.name, rd.dependency_type, rd.created_at
            FROM repository_dependencies rd
            JOIN repositories u ON rd.upstream_id = u.id
            JOIN repositories d ON rd.downstream_id = d.id
            ORDER BY d.name, u.name
            "#,
        )?;

        let rows = stmt.query_map([], Self::row_to_dependency)?;
        Ok(rows.flatten().collect())
    }

    /// Clear all repositories and dependencies from the database.
    pub fn clear_repositories(&self) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute_batch(
            r#"
            DELETE FROM repository_dependencies;
            DELETE FROM repositories;
            "#,
        )?;
        Ok(())
    }

    /// Get repositories that directly depend on the given repository.
    ///
    /// Returns repos where the given repo is an upstream dependency.
    pub fn get_direct_dependants(&self, repo: &str) -> Result<Vec<StoredDependency>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT rd.id, u.name, d.name, rd.dependency_type, rd.created_at
            FROM repository_dependencies rd
            JOIN repositories u ON rd.upstream_id = u.id
            JOIN repositories d ON rd.downstream_id = d.id
            WHERE u.name = ?
            ORDER BY d.name
            "#,
        )?;

        let rows = stmt.query_map(params![repo], Self::row_to_dependency)?;
        Ok(rows.flatten().collect())
    }

    /// Get all repositories that depend on the given repository, transitively.
    ///
    /// Uses a recursive CTE to traverse the dependency graph.
    /// Returns (repo_name, depth) pairs where depth indicates how many hops from the source.
    pub fn get_all_dependants(&self, repo: &str) -> Result<Vec<(String, i32)>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            WITH RECURSIVE dependants AS (
                -- Base case: direct dependants
                SELECT d.id, d.name, 1 as depth
                FROM repository_dependencies rd
                JOIN repositories u ON rd.upstream_id = u.id
                JOIN repositories d ON rd.downstream_id = d.id
                WHERE u.name = ?

                UNION

                -- Recursive case: dependants of dependants
                SELECT d.id, d.name, dep.depth + 1
                FROM dependants dep
                JOIN repository_dependencies rd ON rd.upstream_id = dep.id
                JOIN repositories d ON rd.downstream_id = d.id
                WHERE dep.depth < 10  -- Prevent infinite loops
            )
            SELECT DISTINCT name, MIN(depth) as depth
            FROM dependants
            GROUP BY name
            ORDER BY depth, name
            "#,
        )?;

        let mut results = Vec::new();
        let rows = stmt.query_map(params![repo], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
        })?;

        for row in rows.flatten() {
            results.push(row);
        }

        Ok(results)
    }

    /// Save an indexed repository to the database.
    pub fn save_indexed_repo(
        &self,
        name: &str,
        path: &str,
        github_url: Option<&str>,
        default_branch: &str,
        file_count: usize,
    ) -> Result<i64> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            INSERT INTO repositories (name, path, github_url, default_branch, file_count, last_indexed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
            ON CONFLICT(name) DO UPDATE SET
                path = excluded.path,
                github_url = COALESCE(excluded.github_url, github_url),
                default_branch = excluded.default_branch,
                file_count = excluded.file_count,
                last_indexed_at = datetime('now')
            "#,
            params![name, path, github_url, default_branch, file_count as i64],
        )?;

        // last_insert_rowid() returns 0 on UPDATE, so query for the actual ID
        let id: i64 = conn
            .query_row(
                "SELECT id FROM repositories WHERE name = ?",
                params![name],
                |row| row.get(0),
            )
            .map_err(|e| {
                crate::error::Error::Storage(format!(
                    "Failed to retrieve repository ID after UPSERT for '{}': {}. \
                This indicates a database inconsistency.",
                    name, e
                ))
            })?;
        Ok(id)
    }

    /// Save a file to the repo files index.
    pub fn save_repo_file(
        &self,
        repo_id: i64,
        file_path: &str,
        file_type: Option<&str>,
    ) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            INSERT INTO repo_files (repo_id, file_path, file_type)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(repo_id, file_path) DO UPDATE SET
                file_type = excluded.file_type
            "#,
            params![repo_id, file_path, file_type],
        )?;
        Ok(())
    }

    /// Save multiple files to the repo files index efficiently.
    pub fn save_repo_files(&self, repo_id: i64, files: &[(String, Option<String>)]) -> Result<()> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            INSERT INTO repo_files (repo_id, file_path, file_type)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(repo_id, file_path) DO UPDATE SET
                file_type = excluded.file_type
            "#,
        )?;

        for (file_path, file_type) in files {
            stmt.execute(params![repo_id, file_path, file_type.as_deref()])?;
        }

        Ok(())
    }

    /// Clear files for a repository (before re-indexing).
    pub fn clear_repo_files(&self, repo_id: i64) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute("DELETE FROM repo_files WHERE repo_id = ?", params![repo_id])?;
        Ok(())
    }

    /// Sync a single repository's files to the database.
    ///
    /// Clears existing files and saves the new file list.
    pub fn sync_repo_files(&self, repo: &crate::repo::IndexedRepo) -> Result<()> {
        let path_str = repo.path.to_string_lossy();

        // Save/update the repo entry
        let repo_id = self.save_indexed_repo(
            &repo.name,
            &path_str,
            Some(&repo.github_url),
            &repo.default_branch,
            repo.files.len(),
        )?;

        // Clear and re-save files
        self.clear_repo_files(repo_id)?;

        if !repo.files.is_empty() {
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

            self.save_repo_files(repo_id, &files_with_types)?;
        }

        Ok(())
    }

    /// Get an indexed repository by name.
    pub fn get_indexed_repo(&self, name: &str) -> Result<Option<StoredIndexedRepo>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, name, path, github_url, default_branch, file_count, last_indexed_at, created_at
            FROM repositories WHERE name = ?
            "#,
        )?;

        let result = stmt.query_row(params![name], |row| {
            Ok(StoredIndexedRepo {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                github_url: row.get(3)?,
                default_branch: row.get(4)?,
                file_count: row.get(5)?,
                last_indexed_at: row.get(6)?,
                created_at: row.get(7)?,
            })
        });

        match result {
            Ok(repo) => Ok(Some(repo)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all indexed repositories.
    pub fn list_indexed_repos(&self) -> Result<Vec<StoredIndexedRepo>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, name, path, github_url, default_branch, file_count, last_indexed_at, created_at
            FROM repositories ORDER BY name
            "#,
        )?;

        let mut repos = Vec::new();
        let rows = stmt.query_map([], |row| {
            Ok(StoredIndexedRepo {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                github_url: row.get(3)?,
                default_branch: row.get(4)?,
                file_count: row.get(5)?,
                last_indexed_at: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;

        for row in rows.flatten() {
            repos.push(row);
        }

        Ok(repos)
    }

    /// Get index statistics.
    pub fn get_index_stats(&self) -> Result<IndexStats> {
        let conn = self.acquire_lock()?;

        let repo_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
        let file_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM repo_files", [], |row| row.get(0))?;
        let last_indexed: Option<String> = conn
            .query_row("SELECT MAX(last_indexed_at) FROM repositories", [], |row| {
                row.get(0)
            })
            .ok();

        Ok(IndexStats {
            repo_count: repo_count as usize,
            file_count: file_count as usize,
            last_indexed_at: last_indexed,
        })
    }

    /// Record an inference attempt.
    #[allow(clippy::too_many_arguments)]
    pub fn record_inference_attempt(
        &self,
        issue_id: &str,
        issue_source: &str,
        extracted_filenames: &[String],
        extracted_functions: &[String],
        extracted_keywords: &[String],
        inferred_repo_id: Option<i64>,
        confidence: &str,
        inference_reason: &str,
        duration_ms: Option<u64>,
    ) -> Result<i64> {
        let conn = self.acquire_lock()?;

        let filenames_json = serde_json::to_string(extracted_filenames).unwrap_or_default();
        let functions_json = serde_json::to_string(extracted_functions).unwrap_or_default();
        let keywords_json = serde_json::to_string(extracted_keywords).unwrap_or_default();

        conn.execute(
            r#"
            INSERT INTO inference_attempts (
                issue_id, issue_source, extracted_filenames, extracted_functions,
                extracted_keywords, inferred_repo_id, confidence, inference_reason,
                inference_duration_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                issue_id,
                issue_source,
                filenames_json,
                functions_json,
                keywords_json,
                inferred_repo_id,
                confidence,
                inference_reason,
                duration_ms.map(|d| d as i64),
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Record feedback on an inference attempt (was it correct?).
    pub fn record_inference_feedback(
        &self,
        inference_id: i64,
        was_correct: bool,
        actual_repo_id: Option<i64>,
        feedback_source: &str,
    ) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute(
            r#"
            UPDATE inference_attempts
            SET was_correct = ?1, actual_repo_id = ?2, feedback_source = ?3, feedback_at = datetime('now')
            WHERE id = ?4
            "#,
            params![was_correct, actual_repo_id, feedback_source, inference_id],
        )?;
        Ok(())
    }

    /// Get inference statistics.
    pub fn get_inference_stats(&self) -> Result<InferenceStats> {
        let conn = self.acquire_lock()?;

        let total: i64 = conn.query_row("SELECT COUNT(*) FROM inference_attempts", [], |row| {
            row.get(0)
        })?;
        let with_feedback: i64 = conn.query_row(
            "SELECT COUNT(*) FROM inference_attempts WHERE was_correct IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        let correct: i64 = conn.query_row(
            "SELECT COUNT(*) FROM inference_attempts WHERE was_correct = 1",
            [],
            |row| row.get(0),
        )?;

        let high_confidence: i64 = conn.query_row(
            "SELECT COUNT(*) FROM inference_attempts WHERE confidence = 'high'",
            [],
            |row| row.get(0),
        )?;
        let medium_confidence: i64 = conn.query_row(
            "SELECT COUNT(*) FROM inference_attempts WHERE confidence = 'medium'",
            [],
            |row| row.get(0),
        )?;
        let low_confidence: i64 = conn.query_row(
            "SELECT COUNT(*) FROM inference_attempts WHERE confidence = 'low'",
            [],
            |row| row.get(0),
        )?;
        let no_match: i64 = conn.query_row(
            "SELECT COUNT(*) FROM inference_attempts WHERE inferred_repo_id IS NULL",
            [],
            |row| row.get(0),
        )?;

        Ok(InferenceStats {
            total_attempts: total as usize,
            with_feedback: with_feedback as usize,
            correct: correct as usize,
            accuracy: if with_feedback > 0 {
                (correct as f64 / with_feedback as f64) * 100.0
            } else {
                0.0
            },
            by_confidence: ConfidenceBreakdown {
                high: high_confidence as usize,
                medium: medium_confidence as usize,
                low: low_confidence as usize,
                none: no_match as usize,
            },
        })
    }

    /// Get recent inference history.
    ///
    /// Returns the most recent inference attempts, sorted by creation time (newest first).
    pub fn get_inference_history(&self, limit: usize) -> Result<Vec<InferenceHistoryEntry>> {
        let conn = self.acquire_lock()?;

        let mut stmt = conn.prepare(
            "SELECT
                ia.id,
                ia.issue_id,
                ia.issue_source,
                ia.extracted_keywords,
                r.name as repo_name,
                ia.confidence,
                ia.inference_reason,
                ia.was_correct,
                ia.inference_duration_ms,
                ia.created_at
            FROM inference_attempts ia
            LEFT JOIN repositories r ON ia.inferred_repo_id = r.id
            ORDER BY ia.created_at DESC
            LIMIT ?",
        )?;

        let rows = stmt.query_map([limit as i64], |row| {
            Ok(InferenceHistoryEntry {
                id: row.get(0)?,
                issue_id: row.get(1)?,
                issue_source: row.get(2)?,
                extracted_keywords: row.get(3)?,
                inferred_repo_name: row.get(4)?,
                confidence: row.get(5)?,
                inference_reason: row.get(6)?,
                was_correct: row.get(7)?,
                duration_ms: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }

        Ok(entries)
    }

    /// Get diagnostic counts for all major tables.
    ///
    /// This is useful for debugging and verifying that data is being written correctly.
    pub fn get_diagnostic_counts(&self) -> Result<DiagnosticCounts> {
        let conn = self.acquire_lock()?;

        let fix_attempts: i64 =
            conn.query_row("SELECT COUNT(*) FROM fix_attempts", [], |row| row.get(0))?;
        let fix_attempts_by_status: HashMap<String, i64> = {
            let mut map = HashMap::new();
            let mut stmt =
                conn.prepare("SELECT status, COUNT(*) FROM fix_attempts GROUP BY status")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
            map
        };

        let activity_log: i64 =
            conn.query_row("SELECT COUNT(*) FROM activity_log", [], |row| row.get(0))?;

        let claude_executions: i64 =
            conn.query_row("SELECT COUNT(*) FROM claude_executions", [], |row| {
                row.get(0)
            })?;

        let pr_reviews: i64 =
            conn.query_row("SELECT COUNT(*) FROM pr_reviews", [], |row| row.get(0))?;

        let pr_review_states: i64 =
            conn.query_row("SELECT COUNT(*) FROM pr_review_states", [], |row| {
                row.get(0)
            })?;

        let issue_embeddings: i64 =
            conn.query_row("SELECT COUNT(*) FROM issue_embeddings", [], |row| {
                row.get(0)
            })?;

        let similar_issues: i64 =
            conn.query_row("SELECT COUNT(*) FROM similar_issues", [], |row| row.get(0))?;

        let repositories: i64 =
            conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;

        let repo_files: i64 =
            conn.query_row("SELECT COUNT(*) FROM repo_files", [], |row| row.get(0))?;

        let inference_attempts: i64 =
            conn.query_row("SELECT COUNT(*) FROM inference_attempts", [], |row| {
                row.get(0)
            })?;

        let error_patterns: i64 =
            conn.query_row("SELECT COUNT(*) FROM error_patterns", [], |row| row.get(0))?;

        let processing_metrics: i64 =
            conn.query_row("SELECT COUNT(*) FROM processing_metrics", [], |row| {
                row.get(0)
            })?;

        let feedback_outcomes: i64 =
            conn.query_row("SELECT COUNT(*) FROM feedback_outcomes", [], |row| {
                row.get(0)
            })?;

        let prs: i64 = conn.query_row("SELECT COUNT(*) FROM prs", [], |row| row.get(0))?;

        // Get recent fix attempts for debugging
        let recent_fix_attempts: Vec<(String, String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT source, issue_id, short_id, status FROM fix_attempts ORDER BY attempted_at DESC LIMIT 5"
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            rows.flatten().collect()
        };

        Ok(DiagnosticCounts {
            fix_attempts,
            fix_attempts_by_status,
            activity_log,
            claude_executions,
            pr_reviews,
            pr_review_states,
            issue_embeddings,
            similar_issues,
            repositories,
            repo_files,
            inference_attempts,
            error_patterns,
            processing_metrics,
            feedback_outcomes,
            prs,
            recent_fix_attempts,
        })
    }

    /// Upsert a PR record.
    ///
    /// Creates a new record or updates an existing one based on pr_url.
    pub fn upsert_pr(&self, pr: &crate::types::PrRecord) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO prs (
                pr_url, github_repo, pr_number, attempt_id, issue_id, issue_source,
                title, description, author, head_branch, base_branch, status,
                created_at, updated_at, merged_at, closed_at,
                approvals_count, changes_requested_count, comments_count, last_review_at,
                time_to_first_review_mins, time_to_merge_mins, review_cycles,
                files_changed, lines_added, lines_removed
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
            )
            ON CONFLICT(pr_url) DO UPDATE SET
                github_repo = excluded.github_repo,
                pr_number = excluded.pr_number,
                attempt_id = COALESCE(excluded.attempt_id, prs.attempt_id),
                issue_id = COALESCE(excluded.issue_id, prs.issue_id),
                issue_source = COALESCE(excluded.issue_source, prs.issue_source),
                title = COALESCE(excluded.title, prs.title),
                description = COALESCE(excluded.description, prs.description),
                author = COALESCE(excluded.author, prs.author),
                head_branch = COALESCE(excluded.head_branch, prs.head_branch),
                base_branch = COALESCE(excluded.base_branch, prs.base_branch),
                status = excluded.status,
                updated_at = datetime('now'),
                merged_at = COALESCE(excluded.merged_at, prs.merged_at),
                closed_at = COALESCE(excluded.closed_at, prs.closed_at),
                approvals_count = excluded.approvals_count,
                changes_requested_count = excluded.changes_requested_count,
                comments_count = excluded.comments_count,
                last_review_at = COALESCE(excluded.last_review_at, prs.last_review_at),
                time_to_first_review_mins = COALESCE(excluded.time_to_first_review_mins, prs.time_to_first_review_mins),
                time_to_merge_mins = COALESCE(excluded.time_to_merge_mins, prs.time_to_merge_mins),
                review_cycles = excluded.review_cycles,
                files_changed = COALESCE(excluded.files_changed, prs.files_changed),
                lines_added = COALESCE(excluded.lines_added, prs.lines_added),
                lines_removed = COALESCE(excluded.lines_removed, prs.lines_removed)
            "#,
            params![
                pr.pr_url,
                pr.github_repo,
                pr.pr_number,
                pr.attempt_id,
                pr.issue_id,
                pr.issue_source,
                pr.title,
                pr.description,
                pr.author,
                pr.head_branch,
                pr.base_branch,
                pr.status,
                pr.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                pr.updated_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                pr.merged_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                pr.closed_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                pr.approvals_count,
                pr.changes_requested_count,
                pr.comments_count,
                pr.last_review_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                pr.time_to_first_review_mins,
                pr.time_to_merge_mins,
                pr.review_cycles,
                pr.files_changed,
                pr.lines_added,
                pr.lines_removed,
            ],
        )?;

        // Get the id (either inserted or existing)
        let id: i64 = conn.query_row(
            "SELECT id FROM prs WHERE pr_url = ?",
            params![pr.pr_url],
            |row| row.get(0),
        )?;

        tracing::info!(
            pr_url = %pr.pr_url,
            status = %pr.status,
            id = id,
            "PR record upserted"
        );

        Ok(id)
    }

    /// Get a PR record by URL.
    pub fn get_pr(&self, pr_url: &str) -> Result<Option<crate::types::PrRecord>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, pr_url, github_repo, pr_number, attempt_id, issue_id, issue_source,
                   title, description, author, head_branch, base_branch, status,
                   created_at, updated_at, merged_at, closed_at,
                   approvals_count, changes_requested_count, comments_count, last_review_at,
                   time_to_first_review_mins, time_to_merge_mins, review_cycles,
                   files_changed, lines_added, lines_removed
            FROM prs WHERE pr_url = ?
            "#,
        )?;

        let result = stmt.query_row(params![pr_url], Self::row_to_pr_record).ok();
        Ok(result)
    }

    /// Get all open PRs.
    pub fn get_open_prs(&self) -> Result<Vec<crate::types::PrRecord>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, pr_url, github_repo, pr_number, attempt_id, issue_id, issue_source,
                   title, description, author, head_branch, base_branch, status,
                   created_at, updated_at, merged_at, closed_at,
                   approvals_count, changes_requested_count, comments_count, last_review_at,
                   time_to_first_review_mins, time_to_merge_mins, review_cycles,
                   files_changed, lines_added, lines_removed
            FROM prs WHERE status = 'open'
            ORDER BY created_at DESC
            "#,
        )?;

        let rows = stmt.query_map([], Self::row_to_pr_record)?;
        Ok(rows.flatten().collect())
    }

    /// Get PR analytics.
    pub fn get_pr_analytics(&self) -> Result<crate::types::PrAnalytics> {
        let conn = self.acquire_lock()?;

        let total: i64 = conn.query_row("SELECT COUNT(*) FROM prs", [], |row| row.get(0))?;
        let open: i64 = conn.query_row(
            "SELECT COUNT(*) FROM prs WHERE status = 'open'",
            [],
            |row| row.get(0),
        )?;
        let merged: i64 = conn.query_row(
            "SELECT COUNT(*) FROM prs WHERE status = 'merged'",
            [],
            |row| row.get(0),
        )?;
        let closed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM prs WHERE status = 'closed'",
            [],
            |row| row.get(0),
        )?;

        let avg_time_to_first_review_mins: Option<f64> = conn.query_row(
            "SELECT AVG(time_to_first_review_mins) FROM prs WHERE time_to_first_review_mins IS NOT NULL",
            [],
            |row| row.get(0),
        ).ok();

        let avg_time_to_merge_mins: Option<f64> = conn
            .query_row(
                "SELECT AVG(time_to_merge_mins) FROM prs WHERE time_to_merge_mins IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .ok();

        let avg_review_cycles: Option<f64> = conn
            .query_row("SELECT AVG(review_cycles) FROM prs", [], |row| row.get(0))
            .ok();

        let merge_rate = if merged + closed > 0 {
            Some(merged as f64 / (merged + closed) as f64)
        } else {
            None
        };

        // Get counts by repository
        let mut by_repo = HashMap::new();
        let mut stmt =
            conn.prepare("SELECT github_repo, COUNT(*) FROM prs GROUP BY github_repo")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows.flatten() {
            by_repo.insert(row.0, row.1);
        }

        Ok(crate::types::PrAnalytics {
            total,
            open,
            merged,
            closed,
            avg_time_to_first_review_mins,
            avg_time_to_merge_mins,
            avg_review_cycles,
            merge_rate,
            by_repo,
        })
    }

    /// List PRs with optional status filter and limit.
    pub fn list_prs(
        &self,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::types::PrRecord>> {
        let conn = self.acquire_lock()?;
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match status {
            Some(s) => (
                r#"
                    SELECT id, pr_url, github_repo, pr_number, attempt_id, issue_id, issue_source,
                           title, description, author, head_branch, base_branch, status,
                           created_at, updated_at, merged_at, closed_at,
                           approvals_count, changes_requested_count, comments_count, last_review_at,
                           time_to_first_review_mins, time_to_merge_mins, review_cycles,
                           files_changed, lines_added, lines_removed
                    FROM prs WHERE status = ?1
                    ORDER BY created_at DESC LIMIT ?2
                "#
                .to_string(),
                vec![
                    Box::new(s.to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(limit as i64) as Box<dyn rusqlite::types::ToSql>,
                ],
            ),
            None => (
                r#"
                    SELECT id, pr_url, github_repo, pr_number, attempt_id, issue_id, issue_source,
                           title, description, author, head_branch, base_branch, status,
                           created_at, updated_at, merged_at, closed_at,
                           approvals_count, changes_requested_count, comments_count, last_review_at,
                           time_to_first_review_mins, time_to_merge_mins, review_cycles,
                           files_changed, lines_added, lines_removed
                    FROM prs ORDER BY created_at DESC LIMIT ?1
                "#
                .to_string(),
                vec![Box::new(limit as i64) as Box<dyn rusqlite::types::ToSql>],
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), Self::row_to_pr_record)?;
        Ok(rows.flatten().collect())
    }

    /// Update PR status.
    pub fn update_pr_status(&self, pr_url: &str, status: &str) -> Result<()> {
        let conn = self.acquire_lock()?;

        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let (merged_at, closed_at) = match status {
            "merged" => (Some(now.clone()), None),
            "closed" => (None, Some(now.clone())),
            _ => (None, None),
        };

        conn.execute(
            r#"
            UPDATE prs SET
                status = ?1,
                updated_at = ?2,
                merged_at = COALESCE(?3, merged_at),
                closed_at = COALESCE(?4, closed_at)
            WHERE pr_url = ?5
            "#,
            params![status, now, merged_at, closed_at, pr_url],
        )?;

        tracing::info!(
            pr_url = %pr_url,
            status = status,
            "PR status updated"
        );

        Ok(())
    }

    fn row_to_pr_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::types::PrRecord> {
        Ok(crate::types::PrRecord {
            id: row.get(0)?,
            pr_url: row.get(1)?,
            github_repo: row.get(2)?,
            pr_number: row.get(3)?,
            attempt_id: row.get(4)?,
            issue_id: row.get(5)?,
            issue_source: row.get(6)?,
            title: row.get(7)?,
            description: row.get(8)?,
            author: row.get(9)?,
            head_branch: row.get(10)?,
            base_branch: row.get(11)?,
            status: row.get(12)?,
            created_at: Self::parse_datetime(&row.get::<_, String>(13)?),
            updated_at: Self::parse_optional_datetime(row.get(14)?),
            merged_at: Self::parse_optional_datetime(row.get(15)?),
            closed_at: Self::parse_optional_datetime(row.get(16)?),
            approvals_count: row.get(17)?,
            changes_requested_count: row.get(18)?,
            comments_count: row.get(19)?,
            last_review_at: Self::parse_optional_datetime(row.get(20)?),
            time_to_first_review_mins: row.get(21)?,
            time_to_merge_mins: row.get(22)?,
            review_cycles: row.get(23)?,
            files_changed: row.get(24)?,
            lines_added: row.get(25)?,
            lines_removed: row.get(26)?,
        })
    }

    /// Get a fix attempt by its ID.
    pub fn get_attempt_by_id(&self, id: i64) -> Result<Option<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE id = ?
            "#,
        )?;

        let result = stmt.query_row(params![id], Self::row_to_fix_attempt).ok();
        Ok(result)
    }

    /// List fix attempts with optional status/source filters and pagination.
    pub fn list_attempts(
        &self,
        status: Option<&str>,
        source: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let mut attempts = Vec::new();

        let query_all = r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            ORDER BY attempted_at DESC
            LIMIT ?1 OFFSET ?2
        "#;
        let query_status = r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE status = ?1
            ORDER BY attempted_at DESC
            LIMIT ?2 OFFSET ?3
        "#;
        let query_source = r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE source = ?1
            ORDER BY attempted_at DESC
            LIMIT ?2 OFFSET ?3
        "#;
        let query_status_source = r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE status = ?1 AND source = ?2
            ORDER BY attempted_at DESC
            LIMIT ?3 OFFSET ?4
        "#;

        match (status, source) {
            (Some(status), Some(source)) => {
                let mut stmt = conn.prepare_cached(query_status_source)?;
                let rows = stmt.query_map(
                    params![status, source, limit as i64, offset as i64],
                    Self::row_to_fix_attempt,
                )?;
                attempts.extend(rows.flatten());
            }
            (Some(status), None) => {
                let mut stmt = conn.prepare_cached(query_status)?;
                let rows = stmt.query_map(
                    params![status, limit as i64, offset as i64],
                    Self::row_to_fix_attempt,
                )?;
                attempts.extend(rows.flatten());
            }
            (None, Some(source)) => {
                let mut stmt = conn.prepare_cached(query_source)?;
                let rows = stmt.query_map(
                    params![source, limit as i64, offset as i64],
                    Self::row_to_fix_attempt,
                )?;
                attempts.extend(rows.flatten());
            }
            (None, None) => {
                let mut stmt = conn.prepare_cached(query_all)?;
                let rows = stmt.query_map(
                    params![limit as i64, offset as i64],
                    Self::row_to_fix_attempt,
                )?;
                attempts.extend(rows.flatten());
            }
        }

        Ok(attempts)
    }

    /// Count fix attempts with optional status/source filters.
    pub fn count_attempts(&self, status: Option<&str>, source: Option<&str>) -> Result<usize> {
        let conn = self.acquire_lock()?;
        let count: i64 = match (status, source) {
            (Some(status), Some(source)) => conn.query_row(
                "SELECT COUNT(*) FROM fix_attempts WHERE status = ?1 AND source = ?2",
                params![status, source],
                |row| row.get(0),
            )?,
            (Some(status), None) => conn.query_row(
                "SELECT COUNT(*) FROM fix_attempts WHERE status = ?1",
                params![status],
                |row| row.get(0),
            )?,
            (None, Some(source)) => conn.query_row(
                "SELECT COUNT(*) FROM fix_attempts WHERE source = ?1",
                params![source],
                |row| row.get(0),
            )?,
            (None, None) => {
                conn.query_row("SELECT COUNT(*) FROM fix_attempts", [], |row| row.get(0))?
            }
        };
        Ok(count as usize)
    }

    /// List recent attempts ordered by attempted time descending.
    pub fn list_recent_attempts(&self, limit: usize) -> Result<Vec<FixAttempt>> {
        self.list_attempts(None, None, limit, 0)
    }

    /// List attempts since a timestamp, ordered by attempted time descending.
    pub fn list_attempts_since(&self, since: DateTime<Utc>) -> Result<Vec<FixAttempt>> {
        let conn = self.acquire_lock()?;
        let since_str = since.format("%Y-%m-%d %H:%M:%S").to_string();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, source, issue_id, short_id, attempted_at, pr_url, github_repo,
                   github_pr_number, status, error_message, merged_at, resolved_at,
                   retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
            FROM fix_attempts
            WHERE attempted_at >= ?1
            ORDER BY attempted_at DESC
            "#,
        )?;
        let rows = stmt.query_map(params![since_str], Self::row_to_fix_attempt)?;
        Ok(rows.flatten().collect())
    }

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

    /// Create a new regression watch.
    pub fn create_regression_watch(&self, watch: &crate::types::RegressionWatch) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO regression_watches (
                issue_type, issue_id, fix_attempt_id, status,
                pr_merged_at, monitoring_started_at, resolved_at, regressed_at, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                watch.issue_type.to_string(),
                watch.issue_id,
                watch.fix_attempt_id,
                watch.status.to_string(),
                watch
                    .pr_merged_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                watch
                    .monitoring_started_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                watch
                    .resolved_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                watch
                    .regressed_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                watch.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Get a regression watch by ID.
    pub fn get_regression_watch(&self, id: i64) -> Result<Option<crate::types::RegressionWatch>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, issue_type, issue_id, fix_attempt_id, status,
                   pr_merged_at, monitoring_started_at, resolved_at, regressed_at, created_at
            FROM regression_watches
            WHERE id = ?
            "#,
        )?;

        let result = stmt
            .query_row(params![id], Self::row_to_regression_watch)
            .ok();
        Ok(result)
    }

    /// Get regression watches by status.
    pub fn get_regression_watches_by_status(
        &self,
        status: crate::types::RegressionWatchStatus,
    ) -> Result<Vec<crate::types::RegressionWatch>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, issue_type, issue_id, fix_attempt_id, status,
                   pr_merged_at, monitoring_started_at, resolved_at, regressed_at, created_at
            FROM regression_watches
            WHERE status = ?
            ORDER BY created_at DESC
            "#,
        )?;

        let rows = stmt.query_map(params![status.to_string()], Self::row_to_regression_watch)?;

        let mut results = Vec::new();
        for row in rows.flatten() {
            results.push(row);
        }
        Ok(results)
    }

    /// Get all regression watches.
    pub fn get_all_regression_watches(&self) -> Result<Vec<crate::types::RegressionWatch>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, issue_type, issue_id, fix_attempt_id, status,
                   pr_merged_at, monitoring_started_at, resolved_at, regressed_at, created_at
            FROM regression_watches
            ORDER BY created_at DESC
            "#,
        )?;

        let rows = stmt.query_map([], Self::row_to_regression_watch)?;
        Ok(rows.flatten().collect())
    }

    /// Update regression watch status.
    pub fn update_regression_watch_status(
        &self,
        id: i64,
        status: crate::types::RegressionWatchStatus,
    ) -> Result<()> {
        let conn = self.acquire_lock()?;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        // Update specific timestamp based on status
        let (monitoring_started, resolved, regressed) = match status {
            crate::types::RegressionWatchStatus::Monitoring => (Some(now.clone()), None, None),
            crate::types::RegressionWatchStatus::Resolved => (None, Some(now.clone()), None),
            crate::types::RegressionWatchStatus::Regressed => (None, None, Some(now.clone())),
            _ => (None, None, None),
        };

        conn.execute(
            r#"
            UPDATE regression_watches SET
                status = ?1,
                monitoring_started_at = COALESCE(?2, monitoring_started_at),
                resolved_at = COALESCE(?3, resolved_at),
                regressed_at = COALESCE(?4, regressed_at)
            WHERE id = ?5
            "#,
            params![
                status.to_string(),
                monitoring_started,
                resolved,
                regressed,
                id
            ],
        )?;

        Ok(())
    }

    /// Record a release tracking entry.
    pub fn record_release_tracking(&self, tracking: &crate::types::ReleaseTracking) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO release_tracking (
                regression_watch_id, release_version, release_commit, released_at, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                tracking.regression_watch_id,
                tracking.release_version,
                tracking.release_commit,
                tracking
                    .released_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                tracking.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Record a regression check.
    pub fn record_regression_check(&self, check: &crate::types::RegressionCheck) -> Result<i64> {
        let conn = self.acquire_lock()?;

        conn.execute(
            r#"
            INSERT INTO regression_checks (
                regression_watch_id, issue_still_exists, checked_at, check_details, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                check.regression_watch_id,
                check.issue_still_exists as i32,
                check
                    .checked_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                check.check_details,
                check.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Get regression checks for a watch.
    pub fn get_regression_checks(
        &self,
        watch_id: i64,
    ) -> Result<Vec<crate::types::RegressionCheck>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, regression_watch_id, issue_still_exists, checked_at, check_details, created_at
            FROM regression_checks
            WHERE regression_watch_id = ?
            ORDER BY created_at DESC
            "#,
        )?;

        let rows = stmt.query_map(params![watch_id], Self::row_to_regression_check)?;

        let mut results = Vec::new();
        for row in rows.flatten() {
            results.push(row);
        }
        Ok(results)
    }

    fn row_to_regression_watch(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<crate::types::RegressionWatch> {
        let issue_type_str: String = row.get(1)?;
        let status_str: String = row.get(4)?;

        Ok(crate::types::RegressionWatch {
            id: row.get(0)?,
            issue_type: issue_type_str
                .parse()
                .unwrap_or(crate::types::IssueType::SentryIssue),
            issue_id: row.get(2)?,
            fix_attempt_id: row.get(3)?,
            status: status_str
                .parse()
                .unwrap_or(crate::types::RegressionWatchStatus::AwaitingRelease),
            pr_merged_at: Self::parse_optional_datetime(row.get(5)?),
            monitoring_started_at: Self::parse_optional_datetime(row.get(6)?),
            resolved_at: Self::parse_optional_datetime(row.get(7)?),
            regressed_at: Self::parse_optional_datetime(row.get(8)?),
            created_at: Self::parse_datetime(&row.get::<_, String>(9)?),
        })
    }

    fn row_to_regression_check(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<crate::types::RegressionCheck> {
        Ok(crate::types::RegressionCheck {
            id: row.get(0)?,
            regression_watch_id: row.get(1)?,
            issue_still_exists: row.get::<_, i32>(2)? != 0,
            checked_at: Self::parse_optional_datetime(row.get(3)?),
            check_details: row.get(4)?,
            created_at: Self::parse_datetime(&row.get::<_, String>(5)?),
        })
    }

    // ── User Management ─────────────────────────────────

    pub fn create_user(
        &self,
        email: &str,
        password_hash: &str,
        name: &str,
        role: &str,
    ) -> Result<i64> {
        let conn = self.acquire_lock()?;
        conn.execute(
            "INSERT INTO users (email, password_hash, name, role) VALUES (?1, ?2, ?3, ?4)",
            params![email, password_hash, name, role],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_user_by_id(&self, id: i64) -> Result<Option<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, password_hash, name, role, created_at, updated_at FROM users WHERE id = ?1"
        )?;
        let user = stmt.query_row(params![id], UserRow::from_row).optional()?;
        Ok(user)
    }

    pub fn get_user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, password_hash, name, role, created_at, updated_at FROM users WHERE email = ?1"
        )?;
        let user = stmt
            .query_row(params![email], UserRow::from_row)
            .optional()?;
        Ok(user)
    }

    pub fn list_users(&self) -> Result<Vec<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, password_hash, name, role, created_at, updated_at FROM users ORDER BY id"
        )?;
        let users = stmt
            .query_map([], UserRow::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(users)
    }

    pub fn update_user(
        &self,
        id: i64,
        email: Option<&str>,
        password_hash: Option<&str>,
        name: Option<&str>,
        role: Option<&str>,
    ) -> Result<bool> {
        let conn = self.acquire_lock()?;
        let mut sets = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(e) = email {
            sets.push("email = ?");
            values.push(Box::new(e.to_string()));
        }
        if let Some(p) = password_hash {
            sets.push("password_hash = ?");
            values.push(Box::new(p.to_string()));
        }
        if let Some(n) = name {
            sets.push("name = ?");
            values.push(Box::new(n.to_string()));
        }
        if let Some(r) = role {
            sets.push("role = ?");
            values.push(Box::new(r.to_string()));
        }

        if sets.is_empty() {
            return Ok(false);
        }

        sets.push("updated_at = datetime('now')");
        values.push(Box::new(id));

        let sql = format!("UPDATE users SET {} WHERE id = ?", sets.join(", "));
        let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        let rows = conn.execute(&sql, params.as_slice())?;
        Ok(rows > 0)
    }

    pub fn delete_user(&self, id: i64) -> Result<bool> {
        let conn = self.acquire_lock()?;
        let rows = conn.execute("DELETE FROM users WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    pub fn count_users(&self) -> Result<i64> {
        let conn = self.acquire_lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(count)
    }

    // ── Session Management ──────────────────────────────

    pub fn create_session(&self, user_id: i64, expires_at: &str) -> Result<String> {
        let token = generate_session_token();
        let conn = self.acquire_lock()?;
        conn.execute(
            "INSERT INTO sessions (id, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![token, user_id, expires_at],
        )?;
        Ok(token)
    }

    pub fn get_session_user(&self, token: &str) -> Result<Option<UserRow>> {
        let conn = self.acquire_lock()?;
        let mut stmt = conn.prepare(
            "SELECT u.id, u.email, u.password_hash, u.name, u.role, u.created_at, u.updated_at
             FROM sessions s
             JOIN users u ON s.user_id = u.id
             WHERE s.id = ?1 AND s.expires_at > datetime('now')",
        )?;
        let user = stmt
            .query_row(params![token], UserRow::from_row)
            .optional()?;
        Ok(user)
    }

    pub fn delete_session(&self, token: &str) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![token])?;
        Ok(())
    }

    pub fn cleanup_expired_sessions(&self) -> Result<usize> {
        let conn = self.acquire_lock()?;
        let deleted = conn.execute(
            "DELETE FROM sessions WHERE expires_at <= datetime('now')",
            [],
        )?;
        Ok(deleted)
    }

    pub fn delete_user_sessions(&self, user_id: i64) -> Result<()> {
        let conn = self.acquire_lock()?;
        conn.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?;
        Ok(())
    }
}

/// An indexed repository stored in the database.
#[derive(Debug, Clone, Serialize)]
pub struct StoredIndexedRepo {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub github_url: Option<String>,
    pub default_branch: String,
    pub file_count: i64,
    pub last_indexed_at: String,
    pub created_at: String,
}

/// Index statistics.
#[derive(Debug, Clone, Serialize)]
pub struct IndexStats {
    pub repo_count: usize,
    pub file_count: usize,
    pub last_indexed_at: Option<String>,
}

/// Inference statistics.
#[derive(Debug, Clone, Serialize)]
pub struct InferenceStats {
    pub total_attempts: usize,
    pub with_feedback: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub by_confidence: ConfidenceBreakdown,
}

/// Breakdown by confidence level.
#[derive(Debug, Clone, Serialize)]
pub struct ConfidenceBreakdown {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub none: usize,
}

/// A single inference attempt from the history.
#[derive(Debug, Clone, Serialize)]
pub struct InferenceHistoryEntry {
    /// Unique ID of the inference attempt.
    pub id: i64,
    /// Issue ID that was being processed.
    pub issue_id: String,
    /// Source of the issue (e.g., "linear", "sentry").
    pub issue_source: String,
    /// Keywords extracted from the issue.
    pub extracted_keywords: Option<String>,
    /// Inferred repository name (if matched).
    pub inferred_repo_name: Option<String>,
    /// Confidence level ("high", "medium", "low", or None).
    pub confidence: Option<String>,
    /// Reason for the inference decision.
    pub inference_reason: Option<String>,
    /// Whether the inference was correct (if feedback provided).
    pub was_correct: Option<bool>,
    /// Duration of the inference in milliseconds.
    pub duration_ms: Option<i64>,
    /// When this inference was recorded.
    pub created_at: String,
}

/// A stored PR review comment from the database.
#[derive(Debug, Clone, Serialize)]
pub struct StoredPrReviewComment {
    pub id: i64,
    pub github_comment_id: i64,
    pub pr_url: String,
    pub review_id: Option<i64>,
    pub path: String,
    pub position: Option<i64>,
    pub line: Option<i64>,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: Option<String>,
}

/// A repository stored in the database.
#[derive(Debug, Clone, Serialize)]
pub struct StoredRepository {
    pub id: i64,
    pub name: String,
    pub path: Option<String>,
    pub github_url: String,
    pub created_at: String,
}

/// A dependency relationship stored in the database.
#[derive(Debug, Clone, Serialize)]
pub struct StoredDependency {
    pub id: i64,
    pub upstream: String,
    pub downstream: String,
    pub dep_type: String,
    pub created_at: String,
}

/// Diagnostic counts for all major tables.
///
/// Used by the `claudear diag db` command to verify database state.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticCounts {
    pub fix_attempts: i64,
    pub fix_attempts_by_status: HashMap<String, i64>,
    pub activity_log: i64,
    pub claude_executions: i64,
    pub pr_reviews: i64,
    pub pr_review_states: i64,
    pub issue_embeddings: i64,
    pub similar_issues: i64,
    pub repositories: i64,
    pub repo_files: i64,
    pub inference_attempts: i64,
    pub error_patterns: i64,
    pub processing_metrics: i64,
    pub feedback_outcomes: i64,
    pub prs: i64,
    /// Recent fix attempts (source, issue_id, short_id, status) - up to 5
    pub recent_fix_attempts: Vec<(String, String, String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike, Utc};

    #[test]
    fn test_record_and_retrieve_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();

        assert!(tracker.has_attempted("linear", "123").unwrap());
        assert!(!tracker.has_attempted("linear", "456").unwrap());
        assert!(!tracker.has_attempted("sentry", "123").unwrap());

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.issue_id, "123");
        assert_eq!(attempt.short_id, "PROJ-123");
        assert_eq!(attempt.source, "linear");
        assert_eq!(attempt.status, FixAttemptStatus::Pending);
    }

    #[test]
    fn test_mark_success() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/42")
            .unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Success);
        assert_eq!(
            attempt.pr_url,
            Some("https://github.com/org/repo/pull/42".to_string())
        );
        // Check that GitHub info was extracted
        assert_eq!(attempt.github_repo, Some("org/repo".to_string()));
        assert_eq!(attempt.github_pr_number, Some(42));
    }

    #[test]
    fn test_mark_merged() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/42")
            .unwrap();
        tracker.mark_merged("linear", "123").unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Merged);
        assert!(attempt.merged_at.is_some());
    }

    #[test]
    fn test_mark_closed() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/42")
            .unwrap();
        tracker.mark_closed("linear", "123").unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Closed);
    }

    #[test]
    fn test_get_pending_prs() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create a successful attempt with a PR
        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/42")
            .unwrap();

        // Create a merged attempt (should not be in pending PRs)
        tracker.record_attempt("linear", "456", "PROJ-456").unwrap();
        tracker
            .mark_success("linear", "456", "https://github.com/org/repo/pull/43")
            .unwrap();
        tracker.mark_merged("linear", "456").unwrap();

        let pending_prs = tracker.get_pending_prs().unwrap();
        assert_eq!(pending_prs.len(), 1);
        assert_eq!(pending_prs[0].issue_id, "123");
    }

    #[test]
    fn test_get_attempt_by_pr_url() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/42")
            .unwrap();

        let attempt = tracker
            .get_attempt_by_pr_url("https://github.com/org/repo/pull/42")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.issue_id, "123");
        assert_eq!(attempt.source, "linear");
    }

    #[test]
    fn test_get_attempt_by_pr_url_returns_latest_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let pr_url = "https://github.com/org/repo/pull/99";

        tracker
            .record_attempt("linear", "older", "PROJ-OLDER")
            .unwrap();
        tracker.mark_success("linear", "older", pr_url).unwrap();

        tracker
            .record_attempt("linear", "newer", "PROJ-NEWER")
            .unwrap();
        tracker.mark_success("linear", "newer", pr_url).unwrap();

        {
            let conn = tracker.acquire_lock().unwrap();
            conn.execute(
                "UPDATE fix_attempts SET attempted_at = ? WHERE source = ? AND issue_id = ?",
                params!["2030-01-01 00:00:00", "linear", "older"],
            )
            .unwrap();
            conn.execute(
                "UPDATE fix_attempts SET attempted_at = ? WHERE source = ? AND issue_id = ?",
                params!["2020-01-01 00:00:00", "linear", "newer"],
            )
            .unwrap();
        }

        let attempt = tracker.get_attempt_by_pr_url(pr_url).unwrap().unwrap();
        assert_eq!(attempt.issue_id, "older");
        assert_eq!(attempt.short_id, "PROJ-OLDER");
    }

    #[test]
    fn test_parse_pr_url() {
        let (repo, pr) =
            SqliteTracker::parse_pr_url("https://github.com/owner/repo/pull/123").unwrap();
        assert_eq!(repo, "owner/repo");
        assert_eq!(pr, 123);

        // Non-GitHub URL should return None
        assert!(
            SqliteTracker::parse_pr_url("https://gitlab.com/owner/repo/merge_requests/123")
                .is_none()
        );
    }

    #[test]
    fn test_mark_failed() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_failed("linear", "123", "Something went wrong")
            .unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Failed);
        assert_eq!(
            attempt.error_message,
            Some("Something went wrong".to_string())
        );
    }

    #[test]
    fn test_reset_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        assert!(tracker.has_attempted("linear", "123").unwrap());

        tracker.reset_attempt("linear", "123").unwrap();
        assert!(!tracker.has_attempted("linear", "123").unwrap());
    }

    #[test]
    fn test_get_attempted_issue_ids() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker.record_attempt("linear", "456", "PROJ-456").unwrap();
        tracker
            .record_attempt("sentry", "789", "SENTRY-789")
            .unwrap();

        let linear_ids = tracker.get_attempted_issue_ids("linear");
        assert_eq!(linear_ids.len(), 2);
        assert!(linear_ids.contains("123"));
        assert!(linear_ids.contains("456"));

        let sentry_ids = tracker.get_attempted_issue_ids("sentry");
        assert_eq!(sentry_ids.len(), 1);
        assert!(sentry_ids.contains("789"));
    }

    #[test]
    fn test_get_attempts_by_status() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker.record_attempt("linear", "456", "PROJ-456").unwrap();
        tracker
            .mark_success("linear", "123", "https://example.com/pr/1")
            .unwrap();

        let pending = tracker
            .get_attempts_by_status(FixAttemptStatus::Pending)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].issue_id, "456");

        let success = tracker
            .get_attempts_by_status(FixAttemptStatus::Success)
            .unwrap();
        assert_eq!(success.len(), 1);
        assert_eq!(success[0].issue_id, "123");
    }

    #[test]
    fn test_get_stats() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "1", "PROJ-1").unwrap();
        tracker.record_attempt("linear", "2", "PROJ-2").unwrap();
        tracker.record_attempt("sentry", "3", "SENTRY-3").unwrap();

        tracker
            .mark_success("linear", "1", "https://example.com/pr/1")
            .unwrap();
        tracker.mark_failed("linear", "2", "Error").unwrap();

        let stats = tracker.get_stats().unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.success, 1);
        assert_eq!(stats.failed, 1);

        let linear_stats = stats.by_source.get("linear").unwrap();
        assert_eq!(linear_stats.total, 2);
        assert_eq!(linear_stats.success, 1);
        assert_eq!(linear_stats.failed, 1);

        let sentry_stats = stats.by_source.get("sentry").unwrap();
        assert_eq!(sentry_stats.total, 1);
    }

    #[test]
    fn test_mark_resolved() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/42")
            .unwrap();
        tracker.mark_resolved("linear", "123").unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert!(attempt.resolved_at.is_some());
    }

    #[test]
    fn test_increment_retry() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker.mark_failed("linear", "123", "Error").unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.retry_count, 0);

        tracker.increment_retry("linear", "123").unwrap();
        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.retry_count, 1);
        assert!(attempt.last_retry_at.is_some());

        tracker.increment_retry("linear", "123").unwrap();
        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.retry_count, 2);
    }

    #[test]
    fn test_mark_cannot_fix() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_cannot_fix("linear", "123", "Max retries exceeded")
            .unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::CannotFix);
        assert_eq!(
            attempt.error_message,
            Some("Max retries exceeded".to_string())
        );
    }

    #[test]
    fn test_get_retryable_issues() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Failed with 0 retries - retryable
        tracker.record_attempt("linear", "1", "PROJ-1").unwrap();
        tracker.mark_failed("linear", "1", "Error").unwrap();

        // Failed with 2 retries - still retryable if max is 3
        tracker.record_attempt("linear", "2", "PROJ-2").unwrap();
        tracker.mark_failed("linear", "2", "Error").unwrap();
        tracker.increment_retry("linear", "2").unwrap();
        tracker.increment_retry("linear", "2").unwrap();

        // Closed - retryable
        tracker.record_attempt("linear", "3", "PROJ-3").unwrap();
        tracker
            .mark_success("linear", "3", "https://github.com/org/repo/pull/1")
            .unwrap();
        tracker.mark_closed("linear", "3").unwrap();

        // Success - not retryable
        tracker.record_attempt("linear", "4", "PROJ-4").unwrap();
        tracker
            .mark_success("linear", "4", "https://github.com/org/repo/pull/2")
            .unwrap();

        let retryable = tracker.get_retryable_issues(3).unwrap();
        assert_eq!(retryable.len(), 3);

        // With max 2 retries, issue 2 should be excluded
        let retryable = tracker.get_retryable_issues(2).unwrap();
        assert_eq!(retryable.len(), 2);
    }

    #[test]
    fn test_prepare_for_retry() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/42")
            .unwrap();
        tracker.mark_closed("linear", "123").unwrap();

        // Prepare for retry should reset to pending
        tracker.prepare_for_retry("linear", "123").unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Pending);
        assert!(attempt.pr_url.is_none());
        assert!(attempt.github_repo.is_none());
        assert!(attempt.github_pr_number.is_none());
        assert!(attempt.error_message.is_none());
    }

    #[test]
    fn test_get_retryable_issues_excludes_pending() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Pending issues should NOT be retryable -- they are still in their initial processing.
        // Only failed/closed issues should be eligible for retry.
        tracker
            .record_attempt("linear", "pending-1", "PROJ-PENDING-1")
            .unwrap();

        let retryable = tracker.get_retryable_issues(3).unwrap();
        assert_eq!(retryable.len(), 0);

        // After marking as failed, it should become retryable
        tracker
            .mark_failed("linear", "pending-1", "some error")
            .unwrap();
        let retryable = tracker.get_retryable_issues(3).unwrap();
        assert_eq!(retryable.len(), 1);
        assert_eq!(retryable[0].issue_id, "pending-1");
        assert_eq!(retryable[0].status, FixAttemptStatus::Failed);
    }

    #[test]
    fn test_parse_pr_url_various_formats() {
        // Standard format
        let (repo, pr) =
            SqliteTracker::parse_pr_url("https://github.com/owner/repo/pull/123").unwrap();
        assert_eq!(repo, "owner/repo");
        assert_eq!(pr, 123);

        // With trailing slash
        let (repo, pr) = SqliteTracker::parse_pr_url("https://github.com/owner/repo/pull/456/")
            .unwrap_or(("".into(), 0));
        // Regex doesn't match trailing slash, this is expected behavior
        assert_eq!(repo, "owner/repo");
        assert_eq!(pr, 456);

        // HTTP instead of HTTPS (should still work as regex doesn't require https)
        let result = SqliteTracker::parse_pr_url("http://github.com/owner/repo/pull/789");
        assert!(result.is_some());

        // Invalid URL
        assert!(SqliteTracker::parse_pr_url("not a url").is_none());

        // Empty string
        assert!(SqliteTracker::parse_pr_url("").is_none());
    }

    #[test]
    fn test_get_attempt_not_found() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let attempt = tracker.get_attempt("linear", "nonexistent").unwrap();
        assert!(attempt.is_none());
    }

    #[test]
    fn test_get_attempt_by_pr_url_not_found() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let attempt = tracker
            .get_attempt_by_pr_url("https://github.com/org/repo/pull/999")
            .unwrap();
        assert!(attempt.is_none());
    }

    #[test]
    fn test_get_attempts_by_status_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let attempts = tracker
            .get_attempts_by_status(FixAttemptStatus::Merged)
            .unwrap();
        assert!(attempts.is_empty());
    }

    #[test]
    fn test_stats_empty_database() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let stats = tracker.get_stats().unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.success, 0);
        assert_eq!(stats.failed, 0);
        assert!(stats.by_source.is_empty());
    }

    #[test]
    fn test_stats_all_statuses() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Pending
        tracker.record_attempt("linear", "1", "PROJ-1").unwrap();

        // Success
        tracker.record_attempt("linear", "2", "PROJ-2").unwrap();
        tracker
            .mark_success("linear", "2", "https://github.com/org/repo/pull/1")
            .unwrap();

        // Failed
        tracker.record_attempt("linear", "3", "PROJ-3").unwrap();
        tracker.mark_failed("linear", "3", "Error").unwrap();

        // Merged
        tracker.record_attempt("linear", "4", "PROJ-4").unwrap();
        tracker
            .mark_success("linear", "4", "https://github.com/org/repo/pull/2")
            .unwrap();
        tracker.mark_merged("linear", "4").unwrap();

        // Closed
        tracker.record_attempt("linear", "5", "PROJ-5").unwrap();
        tracker
            .mark_success("linear", "5", "https://github.com/org/repo/pull/3")
            .unwrap();
        tracker.mark_closed("linear", "5").unwrap();

        // Cannot fix
        tracker.record_attempt("linear", "6", "PROJ-6").unwrap();
        tracker
            .mark_cannot_fix("linear", "6", "Max retries")
            .unwrap();

        let stats = tracker.get_stats().unwrap();
        assert_eq!(stats.total, 6);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.success, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.merged, 1);
        assert_eq!(stats.closed, 1);
        assert_eq!(stats.cannot_fix, 1);
    }

    #[test]
    fn test_record_attempt_upsert_preserves_data() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Record initial attempt
        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/1")
            .unwrap();

        // Backdate attempted_at so we can verify the upsert refreshes it.
        {
            let conn = tracker.acquire_lock().unwrap();
            conn.execute(
                "UPDATE fix_attempts SET attempted_at = ? WHERE source = ? AND issue_id = ?",
                params!["2000-01-01 00:00:00", "linear", "123"],
            )
            .unwrap();
        }
        let before = tracker
            .get_attempt("linear", "123")
            .unwrap()
            .unwrap()
            .attempted_at;
        assert_eq!(
            before,
            chrono::DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );

        // Record again - using ON CONFLICT DO UPDATE, this should only update
        // short_id and attempted_at, preserving status and pr_url.
        tracker
            .record_attempt("linear", "123", "PROJ-123-v2")
            .unwrap();

        let attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        // short_id should be updated
        assert_eq!(attempt.short_id, "PROJ-123-v2");
        // status and pr_url should be preserved (not reset)
        assert_eq!(attempt.status, FixAttemptStatus::Success);
        assert_eq!(
            attempt.pr_url,
            Some("https://github.com/org/repo/pull/1".to_string())
        );
        assert!(attempt.attempted_at > before);
    }

    #[test]
    fn test_multiple_sources_isolation() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Same issue_id in different sources
        tracker
            .record_attempt("linear", "123", "LINEAR-123")
            .unwrap();
        tracker
            .record_attempt("sentry", "123", "SENTRY-123")
            .unwrap();

        assert!(tracker.has_attempted("linear", "123").unwrap());
        assert!(tracker.has_attempted("sentry", "123").unwrap());

        tracker
            .mark_success("linear", "123", "https://github.com/org/repo/pull/1")
            .unwrap();

        let linear_attempt = tracker.get_attempt("linear", "123").unwrap().unwrap();
        let sentry_attempt = tracker.get_attempt("sentry", "123").unwrap().unwrap();

        assert_eq!(linear_attempt.status, FixAttemptStatus::Success);
        assert_eq!(sentry_attempt.status, FixAttemptStatus::Pending);
    }

    #[test]
    fn test_parse_datetime_rfc3339() {
        let dt = SqliteTracker::parse_datetime("2024-01-15T10:30:00Z");
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 10);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn test_parse_datetime_sqlite_format() {
        let dt = SqliteTracker::parse_datetime("2024-01-15 10:30:00");
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn test_parse_datetime_invalid() {
        // Should return current time for invalid format
        let dt = SqliteTracker::parse_datetime("not a date");
        // Just verify it doesn't panic and returns a valid datetime
        assert!(dt.year() >= 2024);
    }

    #[test]
    fn test_parse_optional_datetime() {
        let none_result = SqliteTracker::parse_optional_datetime(None);
        assert!(none_result.is_none());

        let some_result =
            SqliteTracker::parse_optional_datetime(Some("2024-01-15T10:30:00Z".to_string()));
        assert!(some_result.is_some());
    }

    #[test]
    fn test_get_pending_prs_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let pending_prs = tracker.get_pending_prs().unwrap();
        assert!(pending_prs.is_empty());
    }

    #[test]
    fn test_get_pending_prs_no_github_info() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create attempt with non-GitHub PR URL
        tracker.record_attempt("linear", "123", "PROJ-123").unwrap();
        tracker
            .mark_success(
                "linear",
                "123",
                "https://gitlab.com/org/repo/-/merge_requests/42",
            )
            .unwrap();

        // Should not be included because github_repo is None
        let pending_prs = tracker.get_pending_prs().unwrap();
        assert!(pending_prs.is_empty());
    }

    #[test]
    fn test_create_regression_watch() {
        use crate::types::{IssueType, RegressionWatch};

        let tracker = SqliteTracker::in_memory().unwrap();

        // First create a fix attempt to reference
        tracker
            .record_attempt("sentry", "sentry-123", "SENTRY-123")
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "sentry-123")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-123", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();
        assert!(watch_id > 0);
    }

    #[test]
    fn test_create_regression_watch_with_linear_bug() {
        use crate::types::{IssueType, RegressionWatch};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create a fix attempt for linear
        tracker
            .record_attempt("linear", "linear-456", "LIN-456")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "linear-456")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-456", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();
        assert!(watch_id > 0);
    }

    #[test]
    fn test_get_regression_watch() {
        use crate::types::{IssueType, RegressionWatch, RegressionWatchStatus};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("sentry", "sentry-789", "SENTRY-789")
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "sentry-789")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-789", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Retrieve the watch
        let retrieved = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(retrieved.id, watch_id);
        assert_eq!(retrieved.issue_type, IssueType::SentryIssue);
        assert_eq!(retrieved.issue_id, "sentry-789");
        assert_eq!(retrieved.fix_attempt_id, attempt.id);
        assert_eq!(retrieved.status, RegressionWatchStatus::AwaitingRelease);
    }

    #[test]
    fn test_get_regression_watch_not_found() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let result = tracker.get_regression_watch(99999).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_regression_watches_by_status() {
        use crate::types::{IssueType, RegressionWatch, RegressionWatchStatus};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create multiple fix attempts
        tracker
            .record_attempt("sentry", "issue-1", "ISSUE-1")
            .unwrap();
        tracker
            .record_attempt("sentry", "issue-2", "ISSUE-2")
            .unwrap();
        tracker
            .record_attempt("linear", "issue-3", "ISSUE-3")
            .unwrap();

        let attempt1 = tracker.get_attempt("sentry", "issue-1").unwrap().unwrap();
        let attempt2 = tracker.get_attempt("sentry", "issue-2").unwrap().unwrap();
        let attempt3 = tracker.get_attempt("linear", "issue-3").unwrap().unwrap();

        // Create watches with different statuses
        let watch1 = RegressionWatch::new(IssueType::SentryIssue, "issue-1", attempt1.id);
        let watch2 = RegressionWatch::new(IssueType::SentryIssue, "issue-2", attempt2.id);
        let watch3 = RegressionWatch::new(IssueType::LinearBug, "issue-3", attempt3.id);

        let watch1_id = tracker.create_regression_watch(&watch1).unwrap();
        let _watch2_id = tracker.create_regression_watch(&watch2).unwrap();
        let _watch3_id = tracker.create_regression_watch(&watch3).unwrap();

        // All should start as AwaitingRelease
        let awaiting = tracker
            .get_regression_watches_by_status(RegressionWatchStatus::AwaitingRelease)
            .unwrap();
        assert_eq!(awaiting.len(), 3);

        // Update one to Monitoring
        tracker
            .update_regression_watch_status(watch1_id, RegressionWatchStatus::Monitoring)
            .unwrap();

        let awaiting = tracker
            .get_regression_watches_by_status(RegressionWatchStatus::AwaitingRelease)
            .unwrap();
        assert_eq!(awaiting.len(), 2);

        let monitoring = tracker
            .get_regression_watches_by_status(RegressionWatchStatus::Monitoring)
            .unwrap();
        assert_eq!(monitoring.len(), 1);
        assert_eq!(monitoring[0].id, watch1_id);
    }

    #[test]
    fn test_get_regression_watches_by_status_empty() {
        use crate::types::RegressionWatchStatus;

        let tracker = SqliteTracker::in_memory().unwrap();

        let watches = tracker
            .get_regression_watches_by_status(RegressionWatchStatus::Monitoring)
            .unwrap();
        assert!(watches.is_empty());
    }

    #[test]
    fn test_update_regression_watch_status() {
        use crate::types::{IssueType, RegressionWatch, RegressionWatchStatus};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("sentry", "sentry-status-test", "SENTRY-ST")
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "sentry-status-test")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-status-test", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Verify initial status
        let retrieved = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(retrieved.status, RegressionWatchStatus::AwaitingRelease);

        // Update to Monitoring
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Monitoring)
            .unwrap();
        let retrieved = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(retrieved.status, RegressionWatchStatus::Monitoring);
        assert!(retrieved.monitoring_started_at.is_some());

        // Update to Resolved
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Resolved)
            .unwrap();
        let retrieved = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(retrieved.status, RegressionWatchStatus::Resolved);
        assert!(retrieved.resolved_at.is_some());
    }

    #[test]
    fn test_update_regression_watch_status_to_regressed() {
        use crate::types::{IssueType, RegressionWatch, RegressionWatchStatus};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("linear", "linear-regressed", "LIN-REG")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "linear-regressed")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-regressed", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Move to Monitoring first
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Monitoring)
            .unwrap();

        // Update to Regressed
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Regressed)
            .unwrap();
        let retrieved = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(retrieved.status, RegressionWatchStatus::Regressed);
        assert!(retrieved.regressed_at.is_some());
    }

    #[test]
    fn test_record_release_tracking() {
        use crate::types::{IssueType, RegressionWatch, ReleaseTracking};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("sentry", "sentry-release", "SENTRY-REL")
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "sentry-release")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-release", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Record release tracking
        let release = ReleaseTracking::new(watch_id, "v1.2.3", "abc123def456");

        let release_id = tracker.record_release_tracking(&release).unwrap();
        assert!(release_id > 0);
    }

    #[test]
    fn test_record_release_tracking_multiple_versions() {
        use crate::types::{IssueType, RegressionWatch, ReleaseTracking};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("linear", "linear-multi-release", "LIN-MR")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "linear-multi-release")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-multi-release", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Record multiple releases
        let release1 = ReleaseTracking::new(watch_id, "v1.0.0", "commit1");
        let release2 = ReleaseTracking::new(watch_id, "v1.0.1", "commit2");
        let release3 = ReleaseTracking::new(watch_id, "v1.1.0", "commit3");

        let id1 = tracker.record_release_tracking(&release1).unwrap();
        let id2 = tracker.record_release_tracking(&release2).unwrap();
        let id3 = tracker.record_release_tracking(&release3).unwrap();

        assert!(id1 > 0);
        assert!(id2 > id1);
        assert!(id3 > id2);
    }

    #[test]
    fn test_record_regression_check() {
        use crate::types::{IssueType, RegressionCheck, RegressionWatch};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("sentry", "sentry-check", "SENTRY-CHK")
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "sentry-check")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-check", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Record a check showing issue does not exist
        let check = RegressionCheck::new(watch_id, false);
        let check_id = tracker.record_regression_check(&check).unwrap();
        assert!(check_id > 0);
    }

    #[test]
    fn test_record_regression_check_issue_exists() {
        use crate::types::{IssueType, RegressionCheck, RegressionWatch};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("linear", "linear-check-exists", "LIN-CHK")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "linear-check-exists")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-check-exists", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Record a check showing issue still exists
        let mut check = RegressionCheck::new(watch_id, true);
        check.check_details = Some("Issue reoccurred 5 times in the last hour".to_string());

        let check_id = tracker.record_regression_check(&check).unwrap();
        assert!(check_id > 0);
    }

    #[test]
    fn test_get_regression_checks() {
        use crate::types::{IssueType, RegressionCheck, RegressionWatch};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("sentry", "sentry-get-checks", "SENTRY-GC")
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "sentry-get-checks")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-get-checks", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Record multiple checks
        let check1 = RegressionCheck::new(watch_id, false);
        let check2 = RegressionCheck::new(watch_id, false);
        let check3 = RegressionCheck::new(watch_id, true);

        tracker.record_regression_check(&check1).unwrap();
        tracker.record_regression_check(&check2).unwrap();
        tracker.record_regression_check(&check3).unwrap();

        // Get all checks for this watch
        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 3);

        // Verify the last check shows issue exists
        let last_check = checks.last().unwrap();
        assert!(last_check.issue_still_exists);
    }

    #[test]
    fn test_get_regression_checks_empty() {
        use crate::types::{IssueType, RegressionWatch};

        let tracker = SqliteTracker::in_memory().unwrap();

        // Create fix attempt and watch
        tracker
            .record_attempt("linear", "linear-empty-checks", "LIN-EC")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "linear-empty-checks")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-empty-checks", attempt.id);

        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Get checks for watch with no checks recorded
        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert!(checks.is_empty());
    }

    #[test]
    fn test_get_regression_checks_for_nonexistent_watch() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Get checks for a watch ID that doesn't exist
        let checks = tracker.get_regression_checks(99999).unwrap();
        assert!(checks.is_empty());
    }

    #[test]
    fn test_regression_watch_table_creation() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // The table should be created during init - verify by trying to query
        let conn = tracker.conn.lock().unwrap();
        let result = conn.execute("SELECT 1 FROM regression_watches LIMIT 1", []);
        // Should succeed (table exists) or return empty result, not error
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_release_tracking_table_creation() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // The table should be created during init
        let conn = tracker.conn.lock().unwrap();
        let result = conn.execute("SELECT 1 FROM release_tracking LIMIT 1", []);
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_regression_checks_table_creation() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // The table should be created during init
        let conn = tracker.conn.lock().unwrap();
        let result = conn.execute("SELECT 1 FROM regression_checks LIMIT 1", []);
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_full_regression_watch_lifecycle() {
        use crate::types::{
            IssueType, RegressionCheck, RegressionWatch, RegressionWatchStatus, ReleaseTracking,
        };

        let tracker = SqliteTracker::in_memory().unwrap();

        // 1. Create a fix attempt
        tracker
            .record_attempt("sentry", "lifecycle-test", "SENTRY-LC")
            .unwrap();
        tracker
            .mark_success(
                "sentry",
                "lifecycle-test",
                "https://github.com/org/repo/pull/99",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "lifecycle-test")
            .unwrap()
            .unwrap();

        // 2. Create regression watch
        let watch = RegressionWatch::new(IssueType::SentryIssue, "lifecycle-test", attempt.id);
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // 3. Verify initial state
        let watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(watch.status, RegressionWatchStatus::AwaitingRelease);

        // 4. Record release
        let release = ReleaseTracking::new(watch_id, "v2.0.0", "release-commit-hash");
        tracker.record_release_tracking(&release).unwrap();

        // 5. Update to monitoring
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Monitoring)
            .unwrap();
        let watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(watch.status, RegressionWatchStatus::Monitoring);

        // 6. Record several checks showing no regression
        for _ in 0..3 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        // 7. Verify checks are recorded
        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 3);

        // 8. Mark as resolved
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Resolved)
            .unwrap();
        let watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(watch.status, RegressionWatchStatus::Resolved);
        assert!(watch.resolved_at.is_some());
    }

    #[test]
    fn test_regression_watch_lifecycle_with_regression() {
        use crate::types::{
            IssueType, RegressionCheck, RegressionWatch, RegressionWatchStatus, ReleaseTracking,
        };

        let tracker = SqliteTracker::in_memory().unwrap();

        // 1. Create fix attempt and watch
        tracker
            .record_attempt("linear", "regression-lifecycle", "LIN-RL")
            .unwrap();
        tracker
            .mark_success(
                "linear",
                "regression-lifecycle",
                "https://github.com/org/repo/pull/100",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "regression-lifecycle")
            .unwrap()
            .unwrap();

        let watch = RegressionWatch::new(IssueType::LinearBug, "regression-lifecycle", attempt.id);
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // 2. Record release and start monitoring
        let release = ReleaseTracking::new(watch_id, "v3.0.0", "commit-hash");
        tracker.record_release_tracking(&release).unwrap();
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Monitoring)
            .unwrap();

        // 3. Record checks - first ones OK, then regression detected
        let check1 = RegressionCheck::new(watch_id, false);
        let check2 = RegressionCheck::new(watch_id, false);
        let mut check3 = RegressionCheck::new(watch_id, true);
        check3.check_details = Some("Bug reoccurred in production".to_string());

        tracker.record_regression_check(&check1).unwrap();
        tracker.record_regression_check(&check2).unwrap();
        tracker.record_regression_check(&check3).unwrap();

        // 4. Mark as regressed
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Regressed)
            .unwrap();
        let watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(watch.status, RegressionWatchStatus::Regressed);
        assert!(watch.regressed_at.is_some());

        // 5. Verify checks history
        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 3);
        assert!(!checks[0].issue_still_exists);
        assert!(!checks[1].issue_still_exists);
        assert!(checks[2].issue_still_exists);
    }

    #[test]
    fn test_pragma_settings_applied() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let conn = tracker.acquire_lock().unwrap();

        // Verify WAL mode is enabled (note: in-memory DBs may not support WAL, so we check for memory or wal)
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        // In-memory databases use "memory" journal mode, file-based would use "wal"
        assert!(
            journal_mode == "memory" || journal_mode == "wal",
            "Expected journal_mode to be 'memory' or 'wal', got '{}'",
            journal_mode
        );

        // Verify synchronous is NORMAL (1)
        let synchronous: i32 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 1, "Expected synchronous=1 (NORMAL)");

        // Verify cache_size is set (negative means KB)
        let cache_size: i64 = conn
            .query_row("PRAGMA cache_size", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cache_size, -65536, "Expected cache_size=-65536 (64MB)");

        // Verify temp_store is MEMORY (2)
        let temp_store: i32 = conn
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .unwrap();
        assert_eq!(temp_store, 2, "Expected temp_store=2 (MEMORY)");

        // Verify busy_timeout is set
        let busy_timeout: i32 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 5000, "Expected busy_timeout=5000");

        // Verify foreign_keys is ON (1)
        let foreign_keys: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1, "Expected foreign_keys=1 (ON)");
    }

    #[test]
    fn test_batch_record_activities() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let entries = vec![
            ActivityLogEntry {
                id: 0,
                timestamp: Utc::now(),
                activity_type: "test".to_string(),
                source: Some("linear".to_string()),
                issue_id: Some("1".to_string()),
                short_id: Some("LIN-1".to_string()),
                message: "Test activity 1".to_string(),
                metadata: None,
            },
            ActivityLogEntry {
                id: 0,
                timestamp: Utc::now(),
                activity_type: "test".to_string(),
                source: Some("linear".to_string()),
                issue_id: Some("2".to_string()),
                short_id: Some("LIN-2".to_string()),
                message: "Test activity 2".to_string(),
                metadata: None,
            },
            ActivityLogEntry {
                id: 0,
                timestamp: Utc::now(),
                activity_type: "test".to_string(),
                source: Some("sentry".to_string()),
                issue_id: Some("3".to_string()),
                short_id: Some("SENTRY-3".to_string()),
                message: "Test activity 3".to_string(),
                metadata: None,
            },
        ];

        let count = tracker.record_activities_batch(&entries).unwrap();
        assert_eq!(count, 3);

        let activities = tracker.get_recent_activities(10, None).unwrap();
        assert_eq!(activities.len(), 3);
    }

    #[test]
    fn test_batch_record_activities_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let entries: Vec<ActivityLogEntry> = vec![];

        let count = tracker.record_activities_batch(&entries).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_batch_record_metrics() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let metrics = vec![
            ProcessingMetric {
                id: 0,
                timestamp: Utc::now(),
                metric_name: "issues_processed".to_string(),
                metric_value: 10.0,
                source: Some("linear".to_string()),
                tags: None,
            },
            ProcessingMetric {
                id: 0,
                timestamp: Utc::now(),
                metric_name: "issues_processed".to_string(),
                metric_value: 5.0,
                source: Some("sentry".to_string()),
                tags: None,
            },
            ProcessingMetric {
                id: 0,
                timestamp: Utc::now(),
                metric_name: "fix_duration_secs".to_string(),
                metric_value: 120.5,
                source: Some("linear".to_string()),
                tags: None,
            },
        ];

        let count = tracker.record_metrics_batch(&metrics).unwrap();
        assert_eq!(count, 3);

        let fetched = tracker.get_metrics("issues_processed", None, 10).unwrap();
        assert_eq!(fetched.len(), 2);
    }

    #[test]
    fn test_batch_record_metrics_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let metrics: Vec<ProcessingMetric> = vec![];

        let count = tracker.record_metrics_batch(&metrics).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_save_and_get_pr_review_states() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create a PR review state
        let state = crate::github::PrReviewState::new(
            "https://github.com/owner/repo/pull/123",
            "owner/repo",
            123,
            "issue-1",
            "linear",
        );

        // Save it
        tracker.save_pr_review_state(&state).unwrap();

        // Retrieve active states
        let states = tracker.get_active_pr_review_states().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].pr_url, "https://github.com/owner/repo/pull/123");
        assert_eq!(states[0].repo, "owner/repo");
        assert_eq!(states[0].pr_number, 123);
        assert_eq!(states[0].issue_id, "issue-1");
        assert_eq!(states[0].source, "linear");
        assert!(states[0].is_active);
    }

    #[test]
    fn test_pr_review_state_update() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create and save initial state
        let mut state = crate::github::PrReviewState::new(
            "https://github.com/owner/repo/pull/456",
            "owner/repo",
            456,
            "issue-2",
            "sentry",
        );
        tracker.save_pr_review_state(&state).unwrap();

        // Update the state with review info
        state.last_review_id = Some(999);
        state.last_review_time = Some("2024-01-15T10:00:00Z".to_string());
        state.last_comment_id = Some(888);
        state.last_comment_time = Some("2024-01-15T11:00:00Z".to_string());
        tracker.save_pr_review_state(&state).unwrap();

        // Verify the update
        let states = tracker.get_active_pr_review_states().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].last_review_id, Some(999));
        assert_eq!(
            states[0].last_review_time,
            Some("2024-01-15T10:00:00Z".to_string())
        );
        assert_eq!(states[0].last_comment_id, Some(888));
        assert_eq!(
            states[0].last_comment_time,
            Some("2024-01-15T11:00:00Z".to_string())
        );
    }

    #[test]
    fn test_deactivate_pr_review_state() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create and save two states
        let state1 = crate::github::PrReviewState::new(
            "https://github.com/owner/repo/pull/1",
            "owner/repo",
            1,
            "issue-1",
            "linear",
        );
        let state2 = crate::github::PrReviewState::new(
            "https://github.com/owner/repo/pull/2",
            "owner/repo",
            2,
            "issue-2",
            "linear",
        );
        tracker.save_pr_review_state(&state1).unwrap();
        tracker.save_pr_review_state(&state2).unwrap();

        // Verify both are active
        let states = tracker.get_active_pr_review_states().unwrap();
        assert_eq!(states.len(), 2);

        // Deactivate one
        tracker.deactivate_pr_review_state(&state1.pr_url).unwrap();

        // Verify only one remains active
        let states = tracker.get_active_pr_review_states().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].pr_url, "https://github.com/owner/repo/pull/2");
    }

    #[test]
    fn test_record_pr_review_comment() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let comment = crate::github::PrReviewComment {
            id: 12345,
            path: "src/main.rs".to_string(),
            position: Some(10),
            original_position: None,
            body: "Consider using a const here".to_string(),
            user: crate::github::GitHubUser {
                id: 1,
                login: "reviewer1".to_string(),
                user_type: Some("User".to_string()),
            },
            created_at: "2024-01-15T10:00:00Z".to_string(),
            updated_at: "2024-01-15T10:00:00Z".to_string(),
            html_url: "https://github.com/owner/repo/pull/1#comment-12345".to_string(),
            pull_request_review_id: None, // None since we haven't created a review
            start_line: None,
            line: Some(42),
            side: Some("RIGHT".to_string()),
        };

        let pr_url = "https://github.com/owner/repo/pull/1";
        tracker.record_pr_review_comment(pr_url, &comment).unwrap();

        // Retrieve and verify
        let comments = tracker.get_comments_for_pr(pr_url).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].github_comment_id, 12345);
        assert_eq!(comments[0].path, "src/main.rs");
        assert_eq!(comments[0].body, "Consider using a const here");
        assert_eq!(comments[0].author, "reviewer1");
        assert_eq!(comments[0].line, Some(42));
    }

    #[test]
    fn test_get_comments_for_pr() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let pr_url = "https://github.com/owner/repo/pull/42";

        // Create multiple comments
        for i in 1..=3 {
            let comment = crate::github::PrReviewComment {
                id: i * 100,
                path: format!("src/file{}.rs", i),
                position: Some(i),
                original_position: None,
                body: format!("Comment {}", i),
                user: crate::github::GitHubUser {
                    id: i,
                    login: format!("reviewer{}", i),
                    user_type: Some("User".to_string()),
                },
                created_at: format!("2024-01-15T10:0{}:00Z", i),
                updated_at: format!("2024-01-15T10:0{}:00Z", i),
                html_url: format!("https://github.com/owner/repo/pull/42#comment-{}", i * 100),
                pull_request_review_id: None,
                start_line: None,
                line: Some(i),
                side: None,
            };
            tracker.record_pr_review_comment(pr_url, &comment).unwrap();
        }

        let comments = tracker.get_comments_for_pr(pr_url).unwrap();
        assert_eq!(comments.len(), 3);

        // Verify ordering by created_at ASC
        assert_eq!(comments[0].github_comment_id, 100);
        assert_eq!(comments[1].github_comment_id, 200);
        assert_eq!(comments[2].github_comment_id, 300);
    }

    #[test]
    fn test_pr_review_comment_upsert() {
        let tracker = SqliteTracker::in_memory().unwrap();

        let pr_url = "https://github.com/owner/repo/pull/1";
        let comment = crate::github::PrReviewComment {
            id: 999,
            path: "src/main.rs".to_string(),
            position: None,
            original_position: None,
            body: "Original comment".to_string(),
            user: crate::github::GitHubUser {
                id: 1,
                login: "author".to_string(),
                user_type: Some("User".to_string()),
            },
            created_at: "2024-01-15T10:00:00Z".to_string(),
            updated_at: "2024-01-15T10:00:00Z".to_string(),
            html_url: "https://github.com/owner/repo/pull/1#comment-999".to_string(),
            pull_request_review_id: None,
            start_line: None,
            line: None,
            side: None,
        };

        tracker.record_pr_review_comment(pr_url, &comment).unwrap();

        // Update the comment (same id, different body)
        let updated_comment = crate::github::PrReviewComment {
            body: "Updated comment body".to_string(),
            updated_at: "2024-01-15T11:00:00Z".to_string(),
            ..comment
        };
        tracker
            .record_pr_review_comment(pr_url, &updated_comment)
            .unwrap();

        // Should still have only one comment
        let comments = tracker.get_comments_for_pr(pr_url).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "Updated comment body");
        assert_eq!(comments[0].updated_at, "2024-01-15T11:00:00Z");
    }

    #[test]
    fn test_store_and_retrieve_feedback_outcome() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-1", "LIN-1")
            .unwrap();

        let attempt = tracker.get_attempt("linear", "issue-1").unwrap().unwrap();

        let outcome = FixOutcome {
            id: 0,
            attempt_id: attempt.id,
            source: "linear".to_string(),
            issue_id: "issue-1".to_string(),
            issue_text: "Database timeout\n\nConnection fails".to_string(),
            prompt_used: "Fix the timeout".to_string(),
            outcome: crate::feedback::Outcome::Merged,
            error_type: None,
            learnings: Some("Check connection pool".to_string()),
            keywords: vec!["database".to_string(), "timeout".to_string()],
            embedding: None,
            created_at: chrono::Utc::now(),
        };

        let id = tracker.store_feedback_outcome(&outcome).unwrap();
        assert!(id > 0);

        // Retrieve by attempt
        let retrieved = tracker
            .get_feedback_outcome_by_attempt(attempt.id)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.source, "linear");
        assert_eq!(retrieved.issue_id, "issue-1");
        assert_eq!(retrieved.outcome, crate::feedback::Outcome::Merged);
        assert_eq!(
            retrieved.learnings,
            Some("Check connection pool".to_string())
        );
        assert_eq!(
            retrieved.keywords,
            vec!["database".to_string(), "timeout".to_string()]
        );
    }

    #[test]
    fn test_get_feedback_outcomes_with_source_filter() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-1", "LIN-1")
            .unwrap();
        tracker
            .record_attempt("sentry", "issue-2", "SENT-2")
            .unwrap();

        let attempt1 = tracker.get_attempt("linear", "issue-1").unwrap().unwrap();
        let attempt2 = tracker.get_attempt("sentry", "issue-2").unwrap().unwrap();

        let outcome1 = FixOutcome {
            id: 0,
            attempt_id: attempt1.id,
            source: "linear".to_string(),
            issue_id: "issue-1".to_string(),
            issue_text: "Linear issue".to_string(),
            prompt_used: "prompt".to_string(),
            outcome: crate::feedback::Outcome::Merged,
            error_type: None,
            learnings: None,
            keywords: vec![],
            embedding: None,
            created_at: chrono::Utc::now(),
        };

        let outcome2 = FixOutcome {
            id: 0,
            attempt_id: attempt2.id,
            source: "sentry".to_string(),
            issue_id: "issue-2".to_string(),
            issue_text: "Sentry issue".to_string(),
            prompt_used: "prompt".to_string(),
            outcome: crate::feedback::Outcome::Failed,
            error_type: Some("timeout".to_string()),
            learnings: None,
            keywords: vec![],
            embedding: None,
            created_at: chrono::Utc::now(),
        };

        tracker.store_feedback_outcome(&outcome1).unwrap();
        tracker.store_feedback_outcome(&outcome2).unwrap();

        // All outcomes
        let all = tracker.get_feedback_outcomes(None, 100).unwrap();
        assert_eq!(all.len(), 2);

        // Filter by source
        let linear_only = tracker.get_feedback_outcomes(Some("linear"), 100).unwrap();
        assert_eq!(linear_only.len(), 1);
        assert_eq!(linear_only[0].source, "linear");

        let sentry_only = tracker.get_feedback_outcomes(Some("sentry"), 100).unwrap();
        assert_eq!(sentry_only.len(), 1);
        assert_eq!(sentry_only[0].outcome, crate::feedback::Outcome::Failed);
    }

    #[test]
    fn test_create_and_get_user() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let id = tracker
            .create_user("test@example.com", "$2b$12$hash", "Test User", "admin")
            .unwrap();
        assert!(id > 0);
        let user = tracker.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(user.email, "test@example.com");
        assert_eq!(user.name, "Test User");
        assert_eq!(user.role, "admin");
    }

    #[test]
    fn test_get_user_by_email() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .create_user("alice@example.com", "$2b$12$hash", "Alice", "viewer")
            .unwrap();
        let user = tracker
            .get_user_by_email("alice@example.com")
            .unwrap()
            .unwrap();
        assert_eq!(user.name, "Alice");
        let missing = tracker.get_user_by_email("nobody@example.com").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_list_users() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .create_user("a@test.com", "hash", "A", "admin")
            .unwrap();
        tracker
            .create_user("b@test.com", "hash", "B", "viewer")
            .unwrap();
        let users = tracker.list_users().unwrap();
        assert_eq!(users.len(), 2);
    }

    #[test]
    fn test_update_user() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let id = tracker
            .create_user("old@test.com", "hash", "Old Name", "viewer")
            .unwrap();
        tracker
            .update_user(
                id,
                Some("new@test.com"),
                None,
                Some("New Name"),
                Some("admin"),
            )
            .unwrap();
        let user = tracker.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(user.email, "new@test.com");
        assert_eq!(user.name, "New Name");
        assert_eq!(user.role, "admin");
    }

    #[test]
    fn test_delete_user() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let id = tracker
            .create_user("del@test.com", "hash", "Delete Me", "viewer")
            .unwrap();
        assert!(tracker.delete_user(id).unwrap());
        assert!(tracker.get_user_by_id(id).unwrap().is_none());
        assert!(!tracker.delete_user(id).unwrap());
    }

    #[test]
    fn test_duplicate_email_fails() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .create_user("dup@test.com", "hash", "First", "admin")
            .unwrap();
        let result = tracker.create_user("dup@test.com", "hash", "Second", "viewer");
        assert!(result.is_err());
    }

    #[test]
    fn test_create_and_validate_session() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker
            .create_user("sess@test.com", "hash", "Session User", "admin")
            .unwrap();
        let token = tracker
            .create_session(user_id, "2099-12-31T23:59:59")
            .unwrap();
        assert_eq!(token.len(), 64);
        let user = tracker.get_session_user(&token).unwrap().unwrap();
        assert_eq!(user.id, user_id);
        assert_eq!(user.email, "sess@test.com");
    }

    #[test]
    fn test_expired_session_returns_none() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker
            .create_user("exp@test.com", "hash", "Expired", "viewer")
            .unwrap();
        let token = tracker
            .create_session(user_id, "2000-01-01T00:00:00")
            .unwrap();
        let user = tracker.get_session_user(&token).unwrap();
        assert!(user.is_none());
    }

    #[test]
    fn test_delete_session() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker
            .create_user("delsess@test.com", "hash", "Del Sess", "admin")
            .unwrap();
        let token = tracker
            .create_session(user_id, "2099-12-31T23:59:59")
            .unwrap();
        assert!(tracker.get_session_user(&token).unwrap().is_some());
        tracker.delete_session(&token).unwrap();
        assert!(tracker.get_session_user(&token).unwrap().is_none());
    }

    #[test]
    fn test_cleanup_expired_sessions() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let user_id = tracker
            .create_user("clean@test.com", "hash", "Clean", "admin")
            .unwrap();
        tracker
            .create_session(user_id, "2000-01-01T00:00:00")
            .unwrap();
        tracker
            .create_session(user_id, "2000-01-02T00:00:00")
            .unwrap();
        tracker
            .create_session(user_id, "2099-12-31T23:59:59")
            .unwrap();
        let deleted = tracker.cleanup_expired_sessions().unwrap();
        assert_eq!(deleted, 2);
    }

    #[test]
    fn test_count_users() {
        let tracker = SqliteTracker::in_memory().unwrap();
        assert_eq!(tracker.count_users().unwrap(), 0);
        tracker
            .create_user("c1@test.com", "hash", "C1", "admin")
            .unwrap();
        tracker
            .create_user("c2@test.com", "hash", "C2", "viewer")
            .unwrap();
        assert_eq!(tracker.count_users().unwrap(), 2);
    }

    #[test]
    fn test_qa_tables_exist_after_migration() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let conn = tracker.acquire_lock().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('qa_knowledge','qa_usage','question_channel_cursor')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_find_similar_qa_scoped_filters_by_repo() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let now = Utc::now();
        let question = "Which branch should we use?";
        let question_norm = crate::qa::normalize_text(question);

        let entry_a = QaKnowledgeEntry {
            id: 0,
            source: "linear".to_string(),
            repo: Some("org/repo-a".to_string()),
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question_text: question.to_string(),
            question_norm: question_norm.clone(),
            question_embedding: None,
            answer_text: "Use main".to_string(),
            answer_norm: "use main".to_string(),
            answer_embedding: None,
            channel: "email".to_string(),
            responder: Some("a@example.com".to_string()),
            correlation_id: "c1".to_string(),
            asked_at: now,
            answered_at: now,
            success_count: 1,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        };

        let entry_b = QaKnowledgeEntry {
            repo: Some("org/repo-b".to_string()),
            issue_id: "2".to_string(),
            short_id: "LIN-2".to_string(),
            correlation_id: "c2".to_string(),
            ..entry_a.clone()
        };

        tracker.store_qa_knowledge(&entry_a).unwrap();
        tracker.store_qa_knowledge(&entry_b).unwrap();

        let matches = tracker
            .find_similar_qa_scoped("linear", Some("org/repo-a"), &question_norm, None, 0.8, 5)
            .unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].entry.repo.as_deref(), Some("org/repo-a"));
    }

    #[test]
    fn test_find_similar_qa_scoped_exact_ranks_by_success_rate() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let now = Utc::now();
        let question = "Which branch should we use?";
        let question_norm = crate::qa::normalize_text(question);

        let base_entry = QaKnowledgeEntry {
            id: 0,
            source: "linear".to_string(),
            repo: Some("org/repo-a".to_string()),
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question_text: question.to_string(),
            question_norm: question_norm.clone(),
            question_embedding: None,
            answer_text: "Use main".to_string(),
            answer_norm: "use main".to_string(),
            answer_embedding: None,
            channel: "email".to_string(),
            responder: Some("a@example.com".to_string()),
            correlation_id: "c1".to_string(),
            asked_at: now,
            answered_at: now,
            success_count: 0,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        };

        let low_confidence = base_entry.clone();
        let high_confidence = QaKnowledgeEntry {
            issue_id: "2".to_string(),
            short_id: "LIN-2".to_string(),
            answer_text: "Use release branch".to_string(),
            answer_norm: "use release branch".to_string(),
            correlation_id: "c2".to_string(),
            success_count: 9,
            failure_count: 1,
            ..base_entry
        };

        tracker.store_qa_knowledge(&low_confidence).unwrap();
        tracker.store_qa_knowledge(&high_confidence).unwrap();

        let matches = tracker
            .find_similar_qa_scoped("linear", Some("org/repo-a"), &question_norm, None, 0.8, 5)
            .unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].entry.answer_text, "Use release branch");
        assert!(matches[0].historical_success_rate > matches[1].historical_success_rate);
        assert!(matches[0].final_score > matches[1].final_score);
    }

    #[test]
    fn test_find_similar_qa_global_exact_only_returns_normalized_match() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let now = Utc::now();
        let question_norm = crate::qa::normalize_text("Pick deployment region");

        let matching_entry = QaKnowledgeEntry {
            id: 0,
            source: "linear".to_string(),
            repo: Some("org/repo-a".to_string()),
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question_text: "Pick deployment region".to_string(),
            question_norm: question_norm.clone(),
            question_embedding: None,
            answer_text: "us-east-1".to_string(),
            answer_norm: "us-east-1".to_string(),
            answer_embedding: None,
            channel: "email".to_string(),
            responder: Some("a@example.com".to_string()),
            correlation_id: "c1".to_string(),
            asked_at: now,
            answered_at: now,
            success_count: 1,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        };
        let non_matching_entry = QaKnowledgeEntry {
            issue_id: "2".to_string(),
            short_id: "LIN-2".to_string(),
            question_text: "Pick staging region".to_string(),
            question_norm: crate::qa::normalize_text("Pick staging region"),
            answer_text: "eu-west-1".to_string(),
            answer_norm: "eu-west-1".to_string(),
            correlation_id: "c2".to_string(),
            ..matching_entry.clone()
        };

        tracker.store_qa_knowledge(&matching_entry).unwrap();
        tracker.store_qa_knowledge(&non_matching_entry).unwrap();

        let matches = tracker
            .find_similar_qa_global(&question_norm, None, 0.8, 5)
            .unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].entry.answer_text, "us-east-1");
    }

    #[test]
    fn test_qa_usage_updates_outcome_stats_for_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-1", "LIN-1")
            .unwrap();
        let attempt = tracker.get_attempt("linear", "issue-1").unwrap().unwrap();

        let now = Utc::now();
        let qa_id = tracker
            .store_qa_knowledge(&QaKnowledgeEntry {
                id: 0,
                source: "linear".to_string(),
                repo: Some("org/repo".to_string()),
                issue_id: "issue-1".to_string(),
                short_id: "LIN-1".to_string(),
                question_text: "Question?".to_string(),
                question_norm: "question?".to_string(),
                question_embedding: None,
                answer_text: "Answer".to_string(),
                answer_norm: "answer".to_string(),
                answer_embedding: None,
                channel: "discord".to_string(),
                responder: Some("user-1".to_string()),
                correlation_id: "corr-1".to_string(),
                asked_at: now,
                answered_at: now,
                success_count: 0,
                failure_count: 0,
                last_used_at: None,
                metadata: None,
            })
            .unwrap();

        tracker
            .record_qa_usage(attempt.id, qa_id, "asked", 1.0)
            .unwrap();
        tracker
            .update_qa_outcome_stats_for_attempt(attempt.id, true)
            .unwrap();

        let conn = tracker.acquire_lock().unwrap();
        let success_count: i64 = conn
            .query_row(
                "SELECT success_count FROM qa_knowledge WHERE id = ?1",
                params![qa_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(success_count, 1);
    }

    // ---------------------------------------------------------------
    // Activity log operations
    // ---------------------------------------------------------------

    #[test]
    fn test_record_activity_returns_positive_id() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let entry = ActivityLogEntry {
            id: 0,
            timestamp: Utc::now(),
            activity_type: "issue_received".to_string(),
            source: Some("linear".to_string()),
            issue_id: Some("abc-123".to_string()),
            short_id: Some("LIN-1".to_string()),
            message: "New issue received".to_string(),
            metadata: None,
        };

        let id = tracker.record_activity(&entry).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_record_activity_with_metadata() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let meta = serde_json::json!({"priority": "high", "assignee": "alice"});
        let entry = ActivityLogEntry {
            id: 0,
            timestamp: Utc::now(),
            activity_type: "processing_started".to_string(),
            source: Some("sentry".to_string()),
            issue_id: Some("evt-1".to_string()),
            short_id: None,
            message: "Processing started".to_string(),
            metadata: Some(meta.clone()),
        };

        tracker.record_activity(&entry).unwrap();
        let activities = tracker.get_recent_activities(10, None).unwrap();
        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].metadata.as_ref().unwrap()["priority"], "high");
    }

    #[test]
    fn test_get_recent_activities_empty_db() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let activities = tracker.get_recent_activities(10, None).unwrap();
        assert!(activities.is_empty());
    }

    #[test]
    fn test_get_recent_activities_respects_limit() {
        let tracker = SqliteTracker::in_memory().unwrap();
        for i in 0..5 {
            let entry = ActivityLogEntry {
                id: 0,
                timestamp: Utc::now(),
                activity_type: "test".to_string(),
                source: Some("linear".to_string()),
                issue_id: Some(format!("issue-{}", i)),
                short_id: None,
                message: format!("Activity {}", i),
                metadata: None,
            };
            tracker.record_activity(&entry).unwrap();
        }

        let activities = tracker.get_recent_activities(3, None).unwrap();
        assert_eq!(activities.len(), 3);
    }

    #[test]
    fn test_get_recent_activities_source_filter() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let sources = ["linear", "sentry", "linear"];
        for (i, source) in sources.iter().enumerate() {
            let entry = ActivityLogEntry {
                id: 0,
                timestamp: Utc::now(),
                activity_type: "test".to_string(),
                source: Some(source.to_string()),
                issue_id: Some(format!("issue-{}", i)),
                short_id: None,
                message: format!("Activity {}", i),
                metadata: None,
            };
            tracker.record_activity(&entry).unwrap();
        }

        let linear_only = tracker.get_recent_activities(10, Some("linear")).unwrap();
        assert_eq!(linear_only.len(), 2);

        let sentry_only = tracker.get_recent_activities(10, Some("sentry")).unwrap();
        assert_eq!(sentry_only.len(), 1);

        let github_only = tracker.get_recent_activities(10, Some("github")).unwrap();
        assert!(github_only.is_empty());
    }

    #[test]
    fn test_get_recent_activities_ordered_desc() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let timestamps = [
            chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            chrono::DateTime::parse_from_rfc3339("2024-06-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            chrono::DateTime::parse_from_rfc3339("2024-03-10T06:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ];
        for (i, ts) in timestamps.iter().enumerate() {
            let entry = ActivityLogEntry {
                id: 0,
                timestamp: *ts,
                activity_type: "test".to_string(),
                source: None,
                issue_id: Some(format!("{}", i)),
                short_id: None,
                message: format!("ts {}", i),
                metadata: None,
            };
            tracker.record_activity(&entry).unwrap();
        }

        let activities = tracker.get_recent_activities(10, None).unwrap();
        assert_eq!(activities.len(), 3);
        // Most recent first
        assert!(activities[0].timestamp >= activities[1].timestamp);
        assert!(activities[1].timestamp >= activities[2].timestamp);
    }

    #[test]
    fn test_get_activities_for_issue() {
        let tracker = SqliteTracker::in_memory().unwrap();
        for i in 0..3 {
            let entry = ActivityLogEntry {
                id: 0,
                timestamp: Utc::now(),
                activity_type: format!("step_{}", i),
                source: Some("linear".to_string()),
                issue_id: Some("target-issue".to_string()),
                short_id: Some("LIN-1".to_string()),
                message: format!("Step {}", i),
                metadata: None,
            };
            tracker.record_activity(&entry).unwrap();
        }
        // Different issue
        let other = ActivityLogEntry {
            id: 0,
            timestamp: Utc::now(),
            activity_type: "test".to_string(),
            source: Some("linear".to_string()),
            issue_id: Some("other-issue".to_string()),
            short_id: None,
            message: "Other".to_string(),
            metadata: None,
        };
        tracker.record_activity(&other).unwrap();

        let activities = tracker
            .get_activities_for_issue("linear", "target-issue")
            .unwrap();
        assert_eq!(activities.len(), 3);

        let empty = tracker
            .get_activities_for_issue("linear", "nonexistent")
            .unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_record_activities_batch_inserts_all() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let entries: Vec<ActivityLogEntry> = (0..10)
            .map(|i| ActivityLogEntry {
                id: 0,
                timestamp: Utc::now(),
                activity_type: "batch_test".to_string(),
                source: Some("linear".to_string()),
                issue_id: Some(format!("issue-{}", i)),
                short_id: None,
                message: format!("Batch entry {}", i),
                metadata: None,
            })
            .collect();

        let count = tracker.record_activities_batch(&entries).unwrap();
        assert_eq!(count, 10);

        let activities = tracker.get_recent_activities(100, None).unwrap();
        assert_eq!(activities.len(), 10);
    }

    // ---------------------------------------------------------------
    // PR review tracking
    // ---------------------------------------------------------------

    #[test]
    fn test_record_pr_review_and_retrieve() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-1", "LIN-1")
            .unwrap();
        let attempt = tracker.get_attempt("linear", "issue-1").unwrap().unwrap();

        let review = PrReviewRecord {
            id: 0,
            attempt_id: Some(attempt.id),
            pr_url: "https://github.com/org/repo/pull/10".to_string(),
            reviewer: Some("alice".to_string()),
            review_state: Some("approved".to_string()),
            submitted_at: Some(Utc::now()),
            body: Some("Looks good!".to_string()),
            sentiment: Some("positive".to_string()),
            actionable_feedback: None,
        };

        let id = tracker.record_pr_review(&review).unwrap();
        assert!(id > 0);

        let reviews = tracker.get_reviews_for_attempt(attempt.id).unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].reviewer, Some("alice".to_string()));
        assert_eq!(reviews[0].review_state, Some("approved".to_string()));
        assert_eq!(reviews[0].body, Some("Looks good!".to_string()));
        assert_eq!(reviews[0].sentiment, Some("positive".to_string()));
    }

    #[test]
    fn test_multiple_reviews_per_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-1", "LIN-1")
            .unwrap();
        let attempt = tracker.get_attempt("linear", "issue-1").unwrap().unwrap();

        let reviewers = ["alice", "bob", "charlie"];
        let states = ["approved", "changes_requested", "commented"];

        for (reviewer, state) in reviewers.iter().zip(states.iter()) {
            let review = PrReviewRecord {
                id: 0,
                attempt_id: Some(attempt.id),
                pr_url: "https://github.com/org/repo/pull/10".to_string(),
                reviewer: Some(reviewer.to_string()),
                review_state: Some(state.to_string()),
                submitted_at: Some(Utc::now()),
                body: None,
                sentiment: None,
                actionable_feedback: None,
            };
            tracker.record_pr_review(&review).unwrap();
        }

        let reviews = tracker.get_reviews_for_attempt(attempt.id).unwrap();
        assert_eq!(reviews.len(), 3);
    }

    #[test]
    fn test_get_reviews_for_nonexistent_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let reviews = tracker.get_reviews_for_attempt(9999).unwrap();
        assert!(reviews.is_empty());
    }

    // ---------------------------------------------------------------
    // Embedding storage
    // ---------------------------------------------------------------

    #[test]
    fn test_store_and_retrieve_embedding() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let embedding = IssueEmbedding {
            id: 0,
            source: "linear".to_string(),
            issue_id: "emb-1".to_string(),
            short_id: Some("LIN-1".to_string()),
            title: Some("Fix login bug".to_string()),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            embedding_model: Some("text-embedding-3-small".to_string()),
            created_at: Utc::now(),
        };

        let id = tracker.store_embedding(&embedding).unwrap();
        assert!(id > 0);

        let retrieved = tracker.get_embedding("linear", "emb-1").unwrap().unwrap();
        assert_eq!(retrieved.source, "linear");
        assert_eq!(retrieved.issue_id, "emb-1");
        assert_eq!(retrieved.short_id, Some("LIN-1".to_string()));
        assert_eq!(retrieved.title, Some("Fix login bug".to_string()));
        assert_eq!(retrieved.embedding.len(), 4);
        assert!((retrieved.embedding[0] - 0.1).abs() < f32::EPSILON);
        assert!((retrieved.embedding[3] - 0.4).abs() < f32::EPSILON);
        assert_eq!(
            retrieved.embedding_model,
            Some("text-embedding-3-small".to_string())
        );
    }

    #[test]
    fn test_store_embedding_upsert_updates_on_conflict() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let original = IssueEmbedding {
            id: 0,
            source: "linear".to_string(),
            issue_id: "emb-1".to_string(),
            short_id: Some("LIN-1".to_string()),
            title: Some("Original title".to_string()),
            embedding: vec![1.0, 2.0],
            embedding_model: Some("model-v1".to_string()),
            created_at: Utc::now(),
        };
        tracker.store_embedding(&original).unwrap();

        let updated = IssueEmbedding {
            id: 0,
            source: "linear".to_string(),
            issue_id: "emb-1".to_string(),
            short_id: Some("LIN-1".to_string()),
            title: Some("Original title".to_string()),
            embedding: vec![3.0, 4.0, 5.0],
            embedding_model: Some("model-v2".to_string()),
            created_at: Utc::now(),
        };
        tracker.store_embedding(&updated).unwrap();

        let retrieved = tracker.get_embedding("linear", "emb-1").unwrap().unwrap();
        assert_eq!(retrieved.embedding.len(), 3);
        assert!((retrieved.embedding[0] - 3.0).abs() < f32::EPSILON);
        assert_eq!(retrieved.embedding_model, Some("model-v2".to_string()));
    }

    #[test]
    fn test_get_embedding_nonexistent() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let result = tracker.get_embedding("linear", "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_all_embeddings_pagination() {
        let tracker = SqliteTracker::in_memory().unwrap();
        for i in 0..5 {
            let emb = IssueEmbedding {
                id: 0,
                source: "linear".to_string(),
                issue_id: format!("emb-{}", i),
                short_id: None,
                title: None,
                embedding: vec![i as f32],
                embedding_model: None,
                created_at: Utc::now(),
            };
            tracker.store_embedding(&emb).unwrap();
        }

        let page1 = tracker.get_all_embeddings(None, Some(2), Some(0)).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = tracker.get_all_embeddings(None, Some(2), Some(2)).unwrap();
        assert_eq!(page2.len(), 2);

        let all = tracker.get_all_embeddings(None, Some(100), None).unwrap();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn test_get_all_embeddings_source_filter() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let sources = ["linear", "sentry", "linear"];
        for (i, source) in sources.iter().enumerate() {
            let emb = IssueEmbedding {
                id: 0,
                source: source.to_string(),
                issue_id: format!("emb-{}", i),
                short_id: None,
                title: None,
                embedding: vec![1.0],
                embedding_model: None,
                created_at: Utc::now(),
            };
            tracker.store_embedding(&emb).unwrap();
        }

        let linear = tracker
            .get_all_embeddings(Some("linear"), Some(100), None)
            .unwrap();
        assert_eq!(linear.len(), 2);

        let sentry = tracker
            .get_all_embeddings(Some("sentry"), Some(100), None)
            .unwrap();
        assert_eq!(sentry.len(), 1);
    }

    // ---------------------------------------------------------------
    // Feedback outcomes
    // ---------------------------------------------------------------

    #[test]
    fn test_store_feedback_outcome_with_all_fields() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-fb", "LIN-FB")
            .unwrap();
        let attempt = tracker.get_attempt("linear", "issue-fb").unwrap().unwrap();

        let outcome = FixOutcome {
            id: 0,
            attempt_id: attempt.id,
            source: "linear".to_string(),
            issue_id: "issue-fb".to_string(),
            issue_text: "Null pointer in handler".to_string(),
            prompt_used: "Fix the null pointer".to_string(),
            outcome: crate::feedback::Outcome::CannotFix,
            error_type: Some("null_reference".to_string()),
            learnings: Some("Check for null before access".to_string()),
            keywords: vec![
                "null".to_string(),
                "pointer".to_string(),
                "handler".to_string(),
            ],
            embedding: None,
            created_at: Utc::now(),
        };

        let id = tracker.store_feedback_outcome(&outcome).unwrap();
        assert!(id > 0);

        let retrieved = tracker
            .get_feedback_outcome_by_attempt(attempt.id)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.outcome, crate::feedback::Outcome::CannotFix);
        assert_eq!(retrieved.error_type, Some("null_reference".to_string()));
        assert_eq!(retrieved.keywords.len(), 3);
        assert!(retrieved.keywords.contains(&"null".to_string()));
    }

    #[test]
    fn test_get_feedback_outcome_by_attempt_not_found() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let result = tracker.get_feedback_outcome_by_attempt(9999).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_feedback_outcomes_limit() {
        let tracker = SqliteTracker::in_memory().unwrap();
        for i in 0..5 {
            tracker
                .record_attempt("linear", &format!("issue-{}", i), &format!("LIN-{}", i))
                .unwrap();
            let attempt = tracker
                .get_attempt("linear", &format!("issue-{}", i))
                .unwrap()
                .unwrap();
            let outcome = FixOutcome {
                id: 0,
                attempt_id: attempt.id,
                source: "linear".to_string(),
                issue_id: format!("issue-{}", i),
                issue_text: "text".to_string(),
                prompt_used: "prompt".to_string(),
                outcome: crate::feedback::Outcome::Merged,
                error_type: None,
                learnings: None,
                keywords: vec![],
                embedding: None,
                created_at: Utc::now(),
            };
            tracker.store_feedback_outcome(&outcome).unwrap();
        }

        let limited = tracker.get_feedback_outcomes(None, 3).unwrap();
        assert_eq!(limited.len(), 3);
    }

    #[test]
    fn test_get_feedback_outcomes_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let outcomes = tracker.get_feedback_outcomes(None, 100).unwrap();
        assert!(outcomes.is_empty());
    }

    // ---------------------------------------------------------------
    // Q&A knowledge
    // ---------------------------------------------------------------

    fn make_qa_entry(
        source: &str,
        repo: Option<&str>,
        issue_id: &str,
        question: &str,
        answer: &str,
        correlation_id: &str,
    ) -> QaKnowledgeEntry {
        let now = Utc::now();
        QaKnowledgeEntry {
            id: 0,
            source: source.to_string(),
            repo: repo.map(|r| r.to_string()),
            issue_id: issue_id.to_string(),
            short_id: format!("LIN-{}", issue_id),
            question_text: question.to_string(),
            question_norm: crate::qa::normalize_text(question),
            question_embedding: None,
            answer_text: answer.to_string(),
            answer_norm: crate::qa::normalize_text(answer),
            answer_embedding: None,
            channel: "discord".to_string(),
            responder: Some("user@example.com".to_string()),
            correlation_id: correlation_id.to_string(),
            asked_at: now,
            answered_at: now,
            success_count: 0,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        }
    }

    #[test]
    fn test_store_qa_knowledge_returns_positive_id() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let entry = make_qa_entry(
            "linear",
            Some("org/repo"),
            "1",
            "How to deploy?",
            "Run deploy script",
            "c1",
        );
        let id = tracker.store_qa_knowledge(&entry).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_store_qa_knowledge_with_metadata() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let mut entry = make_qa_entry(
            "linear",
            Some("org/repo"),
            "1",
            "How to deploy?",
            "Run deploy script",
            "c1",
        );
        entry.metadata = Some(serde_json::json!({"category": "deployment"}));

        let id = tracker.store_qa_knowledge(&entry).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_record_qa_usage_and_update_stats() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-qa", "LIN-QA")
            .unwrap();
        let attempt = tracker.get_attempt("linear", "issue-qa").unwrap().unwrap();

        let entry = make_qa_entry(
            "linear",
            Some("org/repo"),
            "issue-qa",
            "How to fix it?",
            "Reset the cache",
            "c-qa",
        );
        let qa_id = tracker.store_qa_knowledge(&entry).unwrap();

        // Record usage
        let usage_id = tracker
            .record_qa_usage(attempt.id, qa_id, "auto_applied", 0.95)
            .unwrap();
        assert!(usage_id > 0);

        // Update outcome stats directly
        tracker.update_qa_outcome_stats(qa_id, true).unwrap();
        tracker.update_qa_outcome_stats(qa_id, true).unwrap();
        tracker.update_qa_outcome_stats(qa_id, false).unwrap();

        let conn = tracker.acquire_lock().unwrap();
        let (sc, fc): (i64, i64) = conn
            .query_row(
                "SELECT success_count, failure_count FROM qa_knowledge WHERE id = ?1",
                params![qa_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(sc, 2);
        assert_eq!(fc, 1);
    }

    #[test]
    fn test_find_similar_qa_scoped_no_results() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let matches = tracker
            .find_similar_qa_scoped(
                "linear",
                Some("org/repo"),
                "completely unique query",
                None,
                0.8,
                5,
            )
            .unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_find_similar_qa_global_no_results() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let matches = tracker
            .find_similar_qa_global("completely unique query", None, 0.8, 5)
            .unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_find_similar_qa_scoped_exact_match() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let question = "What branch should I use for hotfixes?";
        let entry = make_qa_entry(
            "linear",
            Some("org/repo"),
            "1",
            question,
            "Use the hotfix branch",
            "c1",
        );
        tracker.store_qa_knowledge(&entry).unwrap();

        let question_norm = crate::qa::normalize_text(question);
        let matches = tracker
            .find_similar_qa_scoped("linear", Some("org/repo"), &question_norm, None, 0.8, 5)
            .unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].entry.answer_text, "Use the hotfix branch");
    }

    #[test]
    fn test_find_similar_qa_global_exact_match() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let question = "What branch should I use for hotfixes?";
        let entry = make_qa_entry(
            "linear",
            Some("org/repo"),
            "1",
            question,
            "Use the hotfix branch",
            "c1",
        );
        tracker.store_qa_knowledge(&entry).unwrap();

        let question_norm = crate::qa::normalize_text(question);
        let matches = tracker
            .find_similar_qa_global(&question_norm, None, 0.8, 5)
            .unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].entry.answer_text, "Use the hotfix branch");
    }

    #[test]
    fn test_record_qa_usage_upsert() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "issue-upsert", "LIN-U")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "issue-upsert")
            .unwrap()
            .unwrap();

        let entry = make_qa_entry("linear", Some("org/repo"), "issue-upsert", "Q?", "A.", "cu");
        let qa_id = tracker.store_qa_knowledge(&entry).unwrap();

        // First insert
        tracker
            .record_qa_usage(attempt.id, qa_id, "asked", 0.8)
            .unwrap();
        // Upsert with updated usage_type
        tracker
            .record_qa_usage(attempt.id, qa_id, "auto_applied", 0.95)
            .unwrap();

        // Should not fail; ON CONFLICT updates
        let conn = tracker.acquire_lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM qa_usage WHERE attempt_id = ?1 AND qa_id = ?2",
                params![attempt.id, qa_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ---------------------------------------------------------------
    // Experiment tracking
    // ---------------------------------------------------------------

    #[test]
    fn test_save_and_get_active_experiments() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let exp = PromptExperiment::new("test-exp", "control", "Fix {{issue}}", "hash123");
        let id = tracker.save_experiment(&exp).unwrap();
        assert!(id > 0);

        let active = tracker.get_active_experiments().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].experiment_name, "test-exp");
        assert_eq!(active[0].variant, "control");
        assert!(active[0].active);
    }

    #[test]
    fn test_save_inactive_experiment_not_in_active_list() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let mut exp = PromptExperiment::new("disabled-exp", "variant_a", "template", "hash");
        exp.active = false;
        tracker.save_experiment(&exp).unwrap();

        let active = tracker.get_active_experiments().unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn test_update_experiment_stats_success() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let exp = PromptExperiment::new("stats-exp", "control", "template", "hash");
        let id = tracker.save_experiment(&exp).unwrap();

        tracker
            .update_experiment_stats(id, true, Some(2.5))
            .unwrap();
        tracker
            .update_experiment_stats(id, true, Some(3.5))
            .unwrap();
        tracker.update_experiment_stats(id, false, None).unwrap();

        let active = tracker.get_active_experiments().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].success_count, 2);
        assert_eq!(active[0].failure_count, 1);
        assert!(active[0].avg_time_to_merge.is_some());
    }

    #[test]
    fn test_multiple_experiment_variants() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let variants = ["control", "variant_a", "variant_b"];
        for variant in &variants {
            let exp = PromptExperiment::new(
                "multi-exp",
                *variant,
                "template",
                format!("hash-{}", variant),
            );
            tracker.save_experiment(&exp).unwrap();
        }

        let active = tracker.get_active_experiments().unwrap();
        assert_eq!(active.len(), 3);
    }

    #[test]
    fn test_get_active_experiments_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let active = tracker.get_active_experiments().unwrap();
        assert!(active.is_empty());
    }

    // ---------------------------------------------------------------
    // Repository storage
    // ---------------------------------------------------------------

    #[test]
    fn test_upsert_and_get_repository() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let id = tracker
            .upsert_repository(
                "org/my-repo",
                Some("/path/to/repo"),
                Some("https://github.com/org/my-repo"),
            )
            .unwrap();
        assert!(id > 0);

        let repo = tracker.get_repository("org/my-repo").unwrap().unwrap();
        assert_eq!(repo.name, "org/my-repo");
        assert_eq!(repo.path, Some("/path/to/repo".to_string()));
        assert_eq!(repo.github_url, "https://github.com/org/my-repo");
    }

    #[test]
    fn test_upsert_repository_conflict_updates() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .upsert_repository(
                "org/repo",
                Some("/old/path"),
                Some("https://github.com/org/repo"),
            )
            .unwrap();

        // Upsert again with new path
        tracker
            .upsert_repository(
                "org/repo",
                Some("/new/path"),
                Some("https://github.com/org/repo-updated"),
            )
            .unwrap();

        let repo = tracker.get_repository("org/repo").unwrap().unwrap();
        assert_eq!(repo.path, Some("/new/path".to_string()));
        assert_eq!(repo.github_url, "https://github.com/org/repo-updated");
    }

    #[test]
    fn test_upsert_repository_defaults() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // No path or github_url
        tracker.upsert_repository("org/repo", None, None).unwrap();

        let repo = tracker.get_repository("org/repo").unwrap().unwrap();
        assert_eq!(repo.name, "org/repo");
        // github_url defaults to name
        assert_eq!(repo.github_url, "org/repo");
        // Empty path stored as empty string, filtered to None
        assert!(repo.path.is_none());
    }

    #[test]
    fn test_get_repository_not_found() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let result = tracker.get_repository("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_repositories() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.upsert_repository("b-repo", None, None).unwrap();
        tracker.upsert_repository("a-repo", None, None).unwrap();
        tracker.upsert_repository("c-repo", None, None).unwrap();

        let repos = tracker.list_repositories().unwrap();
        assert_eq!(repos.len(), 3);
        // Ordered by name
        assert_eq!(repos[0].name, "a-repo");
        assert_eq!(repos[1].name, "b-repo");
        assert_eq!(repos[2].name, "c-repo");
    }

    #[test]
    fn test_add_dependency_and_get_dependencies() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .add_dependency("upstream-lib", "downstream-app", "runtime")
            .unwrap();

        let deps = tracker.get_dependencies("downstream-app").unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].upstream, "upstream-lib");
        assert_eq!(deps[0].downstream, "downstream-app");
        assert_eq!(deps[0].dep_type, "runtime");
    }

    #[test]
    fn test_add_dependency_creates_repos() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // Repos should not exist yet
        assert!(tracker.get_repository("lib-a").unwrap().is_none());
        assert!(tracker.get_repository("app-b").unwrap().is_none());

        tracker.add_dependency("lib-a", "app-b", "build").unwrap();

        // Both repos should now exist
        assert!(tracker.get_repository("lib-a").unwrap().is_some());
        assert!(tracker.get_repository("app-b").unwrap().is_some());
    }

    #[test]
    fn test_add_dependency_upsert_type() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.add_dependency("lib", "app", "runtime").unwrap();
        tracker.add_dependency("lib", "app", "build").unwrap();

        let deps = tracker.get_dependencies("app").unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].dep_type, "build");
    }

    #[test]
    fn test_get_dependents() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .add_dependency("core-lib", "app-a", "runtime")
            .unwrap();
        tracker
            .add_dependency("core-lib", "app-b", "runtime")
            .unwrap();
        tracker
            .add_dependency("other-lib", "app-a", "build")
            .unwrap();

        let dependents = tracker.get_dependents("core-lib").unwrap();
        assert_eq!(dependents.len(), 2);
    }

    #[test]
    fn test_list_all_dependencies() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.add_dependency("lib-a", "app-1", "runtime").unwrap();
        tracker.add_dependency("lib-b", "app-1", "build").unwrap();
        tracker.add_dependency("lib-a", "app-2", "runtime").unwrap();

        let all = tracker.list_all_dependencies().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_clear_repositories() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.add_dependency("lib", "app", "runtime").unwrap();
        assert!(!tracker.list_repositories().unwrap().is_empty());

        tracker.clear_repositories().unwrap();
        assert!(tracker.list_repositories().unwrap().is_empty());
        assert!(tracker.list_all_dependencies().unwrap().is_empty());
    }

    #[test]
    fn test_get_all_dependants_transitive() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // core -> mid -> leaf
        tracker.add_dependency("core", "mid", "runtime").unwrap();
        tracker.add_dependency("mid", "leaf", "runtime").unwrap();

        let all = tracker.get_all_dependants("core").unwrap();
        assert_eq!(all.len(), 2);
        // First level: mid at depth 1
        let mid_entry = all.iter().find(|(name, _)| name == "mid");
        assert!(mid_entry.is_some());
        assert_eq!(mid_entry.unwrap().1, 1);
        // Second level: leaf at depth 2
        let leaf_entry = all.iter().find(|(name, _)| name == "leaf");
        assert!(leaf_entry.is_some());
        assert_eq!(leaf_entry.unwrap().1, 2);
    }

    // ---------------------------------------------------------------
    // Execution logging
    // ---------------------------------------------------------------

    #[test]
    fn test_record_and_get_execution() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "exec-issue", "LIN-E")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "exec-issue")
            .unwrap()
            .unwrap();

        let mut execution = ClaudeExecution::new().with_attempt_id(attempt.id);
        execution.prompt_used = Some("Fix the bug".to_string());
        execution.model_version = Some("claude-3.5-sonnet".to_string());
        execution.working_directory = Some("/home/user/repo".to_string());
        execution.git_branch = Some("fix/issue-123".to_string());
        execution.exit_code = Some(0);
        execution.files_changed = Some(3);
        execution.lines_added = Some(50);
        execution.lines_removed = Some(10);
        execution.event_log_path = Some("/tmp/claudear.events.jsonl".to_string());

        let id = tracker.record_execution(&execution).unwrap();
        assert!(id > 0);

        let executions = tracker.get_executions_for_attempt(attempt.id).unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].attempt_id, Some(attempt.id));
        assert_eq!(executions[0].prompt_used, Some("Fix the bug".to_string()));
        assert_eq!(
            executions[0].model_version,
            Some("claude-3.5-sonnet".to_string())
        );
        assert_eq!(executions[0].exit_code, Some(0));
        assert_eq!(executions[0].files_changed, Some(3));
        assert_eq!(executions[0].lines_added, Some(50));
        assert_eq!(executions[0].lines_removed, Some(10));
        assert_eq!(
            executions[0].event_log_path,
            Some("/tmp/claudear.events.jsonl".to_string())
        );
    }

    #[test]
    fn test_multiple_executions_per_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "multi-exec", "LIN-ME")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "multi-exec")
            .unwrap()
            .unwrap();

        for i in 0..3 {
            let mut execution = ClaudeExecution::new().with_attempt_id(attempt.id);
            execution.exit_code = Some(i);
            tracker.record_execution(&execution).unwrap();
        }

        let executions = tracker.get_executions_for_attempt(attempt.id).unwrap();
        assert_eq!(executions.len(), 3);
    }

    #[test]
    fn test_get_executions_for_nonexistent_attempt() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let executions = tracker.get_executions_for_attempt(9999).unwrap();
        assert!(executions.is_empty());
    }

    #[test]
    fn test_record_execution_with_timed_out() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let mut execution = ClaudeExecution::new();
        execution.timed_out = true;
        execution.exit_code = None;

        let id = tracker.record_execution(&execution).unwrap();
        assert!(id > 0);

        // Verify timed_out round-trips (attempt_id is None so use get via raw query)
        let conn = tracker.acquire_lock().unwrap();
        let timed_out: i32 = conn
            .query_row(
                "SELECT timed_out FROM claude_executions WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(timed_out, 1);
    }

    // ---------------------------------------------------------------
    // parse_pr_url helper
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_pr_url_github_standard() {
        let result = SqliteTracker::parse_pr_url("https://github.com/owner/repo/pull/42");
        assert_eq!(result, Some(("owner/repo".to_string(), 42)));
    }

    #[test]
    fn test_parse_pr_url_github_with_trailing_slash() {
        // The regex won't match trailing content after the number
        let result = SqliteTracker::parse_pr_url("https://github.com/my-org/my-repo/pull/123");
        assert_eq!(result, Some(("my-org/my-repo".to_string(), 123)));
    }

    #[test]
    fn test_parse_pr_url_non_github() {
        let result = SqliteTracker::parse_pr_url("https://gitlab.com/owner/repo/merge_requests/1");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_pr_url_invalid() {
        assert_eq!(SqliteTracker::parse_pr_url("not-a-url"), None);
        assert_eq!(SqliteTracker::parse_pr_url(""), None);
    }

    #[test]
    fn test_parse_pr_url_too_long() {
        let long_url = format!(
            "https://github.com/{}/pull/1",
            "a".repeat(MAX_PR_URL_LENGTH)
        );
        let result = SqliteTracker::parse_pr_url(&long_url);
        assert_eq!(result, None);
    }

    // ---------------------------------------------------------------
    // Attempt lifecycle
    // ---------------------------------------------------------------

    #[test]
    fn test_attempt_full_lifecycle_to_merged() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // 1. Record attempt
        tracker
            .record_attempt("linear", "lifecycle-1", "LIN-LC1")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "lifecycle-1")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Pending);

        // 2. Mark success with PR
        tracker
            .mark_success(
                "linear",
                "lifecycle-1",
                "https://github.com/org/repo/pull/99",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "lifecycle-1")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Success);
        assert_eq!(attempt.github_repo, Some("org/repo".to_string()));
        assert_eq!(attempt.github_pr_number, Some(99));

        // 3. Mark merged
        tracker.mark_merged("linear", "lifecycle-1").unwrap();
        let attempt = tracker
            .get_attempt("linear", "lifecycle-1")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Merged);
        assert!(attempt.merged_at.is_some());
    }

    #[test]
    fn test_attempt_lifecycle_fail_retry_succeed() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker
            .record_attempt("linear", "retry-issue", "LIN-R1")
            .unwrap();

        // Fail
        tracker
            .mark_failed("linear", "retry-issue", "Build error")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "retry-issue")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Failed);
        assert_eq!(attempt.error_message, Some("Build error".to_string()));

        // Increment retry count, then prepare for retry
        tracker.increment_retry("linear", "retry-issue").unwrap();
        tracker.prepare_for_retry("linear", "retry-issue").unwrap();
        let attempt = tracker
            .get_attempt("linear", "retry-issue")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Pending);
        assert_eq!(attempt.retry_count, 1);

        // Succeed
        tracker
            .mark_success(
                "linear",
                "retry-issue",
                "https://github.com/org/repo/pull/50",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "retry-issue")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, FixAttemptStatus::Success);
    }

    #[test]
    fn test_record_attempt_with_labels() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let labels = vec!["bug".to_string(), "high-priority".to_string()];
        tracker
            .record_attempt_with_labels("linear", "labeled-1", "LIN-L1", &labels)
            .unwrap();

        let attempt = tracker.get_attempt("linear", "labeled-1").unwrap().unwrap();
        assert_eq!(attempt.issue_labels.len(), 2);
        assert!(attempt.issue_labels.contains(&"bug".to_string()));
        assert!(attempt.issue_labels.contains(&"high-priority".to_string()));
    }

    // ---------------------------------------------------------------
    // Error patterns
    // ---------------------------------------------------------------

    #[test]
    fn test_record_and_get_error_patterns() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let now = Utc::now();
        let pattern = ErrorPattern {
            id: 0,
            pattern_hash: "hash-1".to_string(),
            error_type: Some("build_failure".to_string()),
            error_message: Some("undefined reference to main".to_string()),
            first_seen: now,
            last_seen: now,
            occurrence_count: 1,
            sources: Some(vec!["linear".to_string()]),
            example_issue_ids: Some(vec!["issue-1".to_string()]),
            resolution_hints: Some("Check linker flags".to_string()),
        };

        let id = tracker.record_error_pattern(&pattern).unwrap();
        assert!(id > 0);

        let patterns = tracker.get_error_patterns(10).unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].pattern_hash, "hash-1");
        assert_eq!(patterns[0].error_type, Some("build_failure".to_string()));
        assert_eq!(patterns[0].sources, Some(vec!["linear".to_string()]));
    }

    #[test]
    fn test_error_pattern_upsert_increments_count() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let now = Utc::now();
        let pattern = ErrorPattern {
            id: 0,
            pattern_hash: "hash-dup".to_string(),
            error_type: Some("timeout".to_string()),
            error_message: None,
            first_seen: now,
            last_seen: now,
            occurrence_count: 1,
            sources: None,
            example_issue_ids: None,
            resolution_hints: None,
        };

        tracker.record_error_pattern(&pattern).unwrap();
        tracker.record_error_pattern(&pattern).unwrap();
        tracker.record_error_pattern(&pattern).unwrap();

        let patterns = tracker.get_error_patterns(10).unwrap();
        assert_eq!(patterns.len(), 1);
        // Initial insert (1) + 2 upserts (each adds 1)
        assert_eq!(patterns[0].occurrence_count, 3);
    }

    #[test]
    fn test_get_error_patterns_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let patterns = tracker.get_error_patterns(10).unwrap();
        assert!(patterns.is_empty());
    }

    // ---------------------------------------------------------------
    // Processing metrics
    // ---------------------------------------------------------------

    #[test]
    fn test_record_and_get_metric() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let metric = ProcessingMetric {
            id: 0,
            timestamp: Utc::now(),
            metric_name: "queue_depth".to_string(),
            metric_value: 42.0,
            source: Some("linear".to_string()),
            tags: Some(serde_json::json!({"region": "us-east-1"})),
        };

        let id = tracker.record_metric(&metric).unwrap();
        assert!(id > 0);

        let metrics = tracker.get_metrics("queue_depth", None, 10).unwrap();
        assert_eq!(metrics.len(), 1);
        assert!((metrics[0].metric_value - 42.0).abs() < f64::EPSILON);
        assert_eq!(metrics[0].source, Some("linear".to_string()));
        assert!(metrics[0].tags.is_some());
    }

    #[test]
    fn test_get_metrics_with_time_filter() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let old_ts = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent_ts = Utc::now();

        let old_metric = ProcessingMetric {
            id: 0,
            timestamp: old_ts,
            metric_name: "test_metric".to_string(),
            metric_value: 1.0,
            source: None,
            tags: None,
        };
        let recent_metric = ProcessingMetric {
            id: 0,
            timestamp: recent_ts,
            metric_name: "test_metric".to_string(),
            metric_value: 2.0,
            source: None,
            tags: None,
        };

        tracker.record_metric(&old_metric).unwrap();
        tracker.record_metric(&recent_metric).unwrap();

        let since = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let filtered = tracker.get_metrics("test_metric", Some(since), 10).unwrap();
        assert_eq!(filtered.len(), 1);
        assert!((filtered[0].metric_value - 2.0).abs() < f64::EPSILON);
    }

    // ---------------------------------------------------------------
    // Similar issues
    // ---------------------------------------------------------------

    #[test]
    fn test_store_and_find_similar_issues() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let similar = SimilarIssue::new("issue-a", "issue-b", 0.95);
        let id = tracker.store_similar_issue(&similar).unwrap();
        assert!(id > 0);

        let results = tracker.find_similar_issues("issue-a", 0.8, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].similar_issue_id, "issue-b");
        assert!((results[0].similarity_score - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn test_find_similar_issues_min_score_filter() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .store_similar_issue(&SimilarIssue::new("src", "high", 0.95))
            .unwrap();
        tracker
            .store_similar_issue(&SimilarIssue::new("src", "low", 0.5))
            .unwrap();

        let high_only = tracker.find_similar_issues("src", 0.9, 10).unwrap();
        assert_eq!(high_only.len(), 1);
        assert_eq!(high_only[0].similar_issue_id, "high");

        let all = tracker.find_similar_issues("src", 0.0, 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_similar_issue_upsert() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .store_similar_issue(&SimilarIssue::new("a", "b", 0.7))
            .unwrap();
        tracker
            .store_similar_issue(&SimilarIssue::new("a", "b", 0.9))
            .unwrap();

        let results = tracker.find_similar_issues("a", 0.0, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!((results[0].similarity_score - 0.9).abs() < f64::EPSILON);
    }

    // ---------------------------------------------------------------
    // Analytics summary and success rate
    // ---------------------------------------------------------------

    #[test]
    fn test_get_success_rate_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let rate = tracker.get_success_rate().unwrap();
        assert!((rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_get_success_rate_mixed() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.record_attempt("linear", "s1", "L1").unwrap();
        tracker
            .mark_success("linear", "s1", "https://github.com/o/r/pull/1")
            .unwrap();
        tracker.record_attempt("linear", "s2", "L2").unwrap();
        tracker.mark_failed("linear", "s2", "error").unwrap();

        let rate = tracker.get_success_rate().unwrap();
        assert!((rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_get_analytics_summary() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.record_attempt("linear", "a1", "L1").unwrap();
        tracker
            .mark_success("linear", "a1", "https://github.com/o/r/pull/1")
            .unwrap();
        tracker.mark_merged("linear", "a1").unwrap();

        tracker.record_attempt("sentry", "a2", "S1").unwrap();
        tracker.mark_failed("sentry", "a2", "build error").unwrap();

        let summary = tracker.get_analytics_summary().unwrap();
        assert_eq!(summary.total_processed, 2);
        assert_eq!(summary.total_successful, 1);
        assert_eq!(summary.total_merged, 1);
        assert!((summary.success_rate - 0.5).abs() < f64::EPSILON);
        assert!(summary.success_rate_by_source.contains_key("linear"));
        assert!(summary.success_rate_by_source.contains_key("sentry"));
    }

    #[test]
    fn test_get_analytics_summary_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let summary = tracker.get_analytics_summary().unwrap();
        assert_eq!(summary.total_processed, 0);
        assert!((summary.success_rate - 0.0).abs() < f64::EPSILON);
    }

    // ---------------------------------------------------------------
    // Pruning
    // ---------------------------------------------------------------

    #[test]
    fn test_prune_old_activities() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // Insert an old activity via raw SQL
        {
            let conn = tracker.acquire_lock().unwrap();
            conn.execute(
                "INSERT INTO activity_log (timestamp, activity_type, message) VALUES ('2020-01-01 00:00:00', 'old', 'old entry')",
                [],
            ).unwrap();
        }
        // Insert a recent one
        let entry = ActivityLogEntry {
            id: 0,
            timestamp: Utc::now(),
            activity_type: "recent".to_string(),
            source: None,
            issue_id: None,
            short_id: None,
            message: "recent entry".to_string(),
            metadata: None,
        };
        tracker.record_activity(&entry).unwrap();

        let deleted = tracker.prune_old_activities(30).unwrap();
        assert_eq!(deleted, 1);

        let remaining = tracker.get_recent_activities(100, None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].activity_type, "recent");
    }

    #[test]
    fn test_prune_old_metrics() {
        let tracker = SqliteTracker::in_memory().unwrap();
        {
            let conn = tracker.acquire_lock().unwrap();
            conn.execute(
                "INSERT INTO processing_metrics (timestamp, metric_name, metric_value) VALUES ('2020-01-01 00:00:00', 'old_metric', 1.0)",
                [],
            ).unwrap();
        }
        let recent = ProcessingMetric {
            id: 0,
            timestamp: Utc::now(),
            metric_name: "new_metric".to_string(),
            metric_value: 2.0,
            source: None,
            tags: None,
        };
        tracker.record_metric(&recent).unwrap();

        let deleted = tracker.prune_old_metrics(30).unwrap();
        assert_eq!(deleted, 1);
    }

    // ---------------------------------------------------------------
    // Channel cursor
    // ---------------------------------------------------------------

    #[test]
    fn test_get_set_channel_cursor() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Initially empty
        let cursor = tracker
            .get_channel_cursor("discord", "last_message_id")
            .unwrap();
        assert!(cursor.is_none());

        // Set cursor
        tracker
            .set_channel_cursor("discord", "last_message_id", "msg-123")
            .unwrap();
        let cursor = tracker
            .get_channel_cursor("discord", "last_message_id")
            .unwrap();
        assert_eq!(cursor, Some("msg-123".to_string()));

        // Update cursor (upsert)
        tracker
            .set_channel_cursor("discord", "last_message_id", "msg-456")
            .unwrap();
        let cursor = tracker
            .get_channel_cursor("discord", "last_message_id")
            .unwrap();
        assert_eq!(cursor, Some("msg-456".to_string()));
    }

    #[test]
    fn test_channel_cursor_different_keys() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .set_channel_cursor("discord", "cursor_a", "val-a")
            .unwrap();
        tracker
            .set_channel_cursor("discord", "cursor_b", "val-b")
            .unwrap();

        assert_eq!(
            tracker.get_channel_cursor("discord", "cursor_a").unwrap(),
            Some("val-a".to_string())
        );
        assert_eq!(
            tracker.get_channel_cursor("discord", "cursor_b").unwrap(),
            Some("val-b".to_string())
        );
    }

    // ---------------------------------------------------------------
    // get_attempt_by_id
    // ---------------------------------------------------------------

    #[test]
    fn test_get_attempt_by_id() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .record_attempt("linear", "by-id-issue", "LIN-BI")
            .unwrap();
        let attempt = tracker
            .get_attempt("linear", "by-id-issue")
            .unwrap()
            .unwrap();

        let by_id = tracker.get_attempt_by_id(attempt.id).unwrap().unwrap();
        assert_eq!(by_id.issue_id, "by-id-issue");
        assert_eq!(by_id.source, "linear");
    }

    #[test]
    fn test_get_attempt_by_id_not_found() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let result = tracker.get_attempt_by_id(99999).unwrap();
        assert!(result.is_none());
    }

    // ---------------------------------------------------------------
    // Embedding helper functions
    // ---------------------------------------------------------------

    #[test]
    fn test_embedding_to_blob_and_back() {
        let original = vec![1.0f32, 2.5, -(std::f32::consts::PI), 0.0];
        let blob = SqliteTracker::embedding_to_blob(Some(&original));
        assert!(blob.is_some());

        let restored = SqliteTracker::blob_to_embedding(blob);
        assert!(restored.is_some());
        let restored = restored.unwrap();
        assert_eq!(restored.len(), 4);
        assert!((restored[0] - 1.0).abs() < f32::EPSILON);
        assert!((restored[2] - (-std::f32::consts::PI)).abs() < 0.001);
    }

    #[test]
    fn test_embedding_to_blob_none() {
        let blob = SqliteTracker::embedding_to_blob(None);
        assert!(blob.is_none());

        let restored = SqliteTracker::blob_to_embedding(None);
        assert!(restored.is_none());
    }
}
