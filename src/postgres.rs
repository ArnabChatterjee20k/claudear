//! PostgreSQL storage backend with optional Redis cache layer.
//!
//! When a Redis [`ConnectionManager`](redis::aio::ConnectionManager) is provided,
//! single-row reads check the cache first and writes invalidate relevant keys.
//! When `cache` is `None`, all queries go straight to Postgres with zero overhead
//! (just an `Option::is_none()` check).

use crate::error::Result;
use crate::storage::types::UserRow;
use crate::storage::{parse_pr_url, FixAttemptTracker};
use crate::types::{FixAttempt, FixAttemptStats, FixAttemptStatus, PrRecord, SourceStats};

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
#[cfg(feature = "redis")]
use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio_postgres::Row;

/// Shorthand for a type-erased Postgres parameter.
type PgParam<'a> = &'a (dyn tokio_postgres::types::ToSql + Sync);

// ---------------------------------------------------------------------------
// CachedUser – serialisable mirror of UserRow (which skips password_hash)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct CachedUser {
    id: i64,
    email: String,
    password_hash: String,
    name: String,
    role: String,
    avatar_url: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<&UserRow> for CachedUser {
    fn from(u: &UserRow) -> Self {
        Self {
            id: u.id,
            email: u.email.clone(),
            password_hash: u.password_hash.clone(),
            name: u.name.clone(),
            role: u.role.clone(),
            avatar_url: u.avatar_url.clone(),
            created_at: u.created_at.clone(),
            updated_at: u.updated_at.clone(),
        }
    }
}

impl From<CachedUser> for UserRow {
    fn from(c: CachedUser) -> Self {
        Self {
            id: c.id,
            email: c.email,
            password_hash: c.password_hash,
            name: c.name,
            role: c.role,
            avatar_url: c.avatar_url,
            created_at: c.created_at,
            updated_at: c.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// PostgresBackend
// ---------------------------------------------------------------------------

/// PostgreSQL-backed storage with an optional Redis cache sidecar.
pub struct PostgresBackend {
    pool: Pool,
    tenant_id: String,
    #[cfg(feature = "redis")]
    cache: Option<redis::aio::ConnectionManager>,
}

impl PostgresBackend {
    #[cfg(feature = "redis")]
    pub fn new(
        pool: Pool,
        tenant_id: String,
        cache: Option<redis::aio::ConnectionManager>,
    ) -> Self {
        Self {
            pool,
            tenant_id,
            cache,
        }
    }

    #[cfg(not(feature = "redis"))]
    pub fn new(pool: Pool, tenant_id: String) -> Self {
        Self { pool, tenant_id }
    }

    // -----------------------------------------------------------------------
    // Async bridge
    // -----------------------------------------------------------------------

    /// Drive an async future to completion from synchronous trait methods.
    ///
    /// Uses `block_in_place` so the tokio worker thread is freed while we wait.
    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
    }

    /// Get a client from the pool (async, called inside block_on).
    async fn client(
        &self,
    ) -> std::result::Result<deadpool_postgres::Object, deadpool_postgres::PoolError> {
        self.pool.get().await
    }

    // -----------------------------------------------------------------------
    // Cache helpers – best-effort, failures logged + swallowed
    // -----------------------------------------------------------------------

    #[cfg(feature = "redis")]
    fn cache_get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let mut conn = self.cache.as_ref()?.clone();
        match self.block_on(async { conn.get::<_, Option<String>>(key).await }) {
            Ok(Some(json)) => match serde_json::from_str(&json) {
                Ok(val) => Some(val),
                Err(e) => {
                    tracing::warn!(error = %e, key, "redis: cache deserialisation failed");
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, key, "redis: GET failed");
                None
            }
        }
    }

    #[cfg(not(feature = "redis"))]
    fn cache_get<T: DeserializeOwned>(&self, _key: &str) -> Option<T> {
        None
    }

    #[cfg(feature = "redis")]
    fn cache_set<T: Serialize>(&self, key: &str, value: &T) {
        let Some(cache) = &self.cache else { return };
        let mut conn = cache.clone();
        match serde_json::to_string(value) {
            Ok(json) => {
                if let Err(e) = self.block_on(async { conn.set::<_, _, ()>(key, json).await }) {
                    tracing::warn!(error = %e, key, "redis: SET failed");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, key, "redis: serialisation failed");
            }
        }
    }

    #[cfg(not(feature = "redis"))]
    fn cache_set<T: Serialize>(&self, _key: &str, _value: &T) {}

    #[cfg(feature = "redis")]
    fn cache_del(&self, keys: &[&str]) {
        if keys.is_empty() {
            return;
        }
        let Some(cache) = &self.cache else { return };
        let mut conn = cache.clone();
        let owned: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        if let Err(e) = self.block_on(async {
            redis::cmd("DEL")
                .arg(&owned)
                .query_async::<()>(&mut conn)
                .await
        }) {
            tracing::warn!(error = %e, "redis: DEL failed");
        }
    }

    #[cfg(not(feature = "redis"))]
    fn cache_del(&self, _keys: &[&str]) {}

    /// Delete all keys matching `pattern` using SCAN+DEL (never KEYS).
    #[cfg(feature = "redis")]
    fn cache_del_pattern(&self, pattern: &str) {
        let Some(cache) = &self.cache else { return };
        let mut conn = cache.clone();
        if let Err(e) = self.block_on(async {
            let mut cursor: u64 = 0;
            loop {
                let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(pattern)
                    .arg("COUNT")
                    .arg(100)
                    .query_async(&mut conn)
                    .await?;
                if !keys.is_empty() {
                    redis::cmd("DEL")
                        .arg(&keys)
                        .query_async::<()>(&mut conn)
                        .await?;
                }
                cursor = next;
                if cursor == 0 {
                    break;
                }
            }
            Ok::<_, redis::RedisError>(())
        }) {
            tracing::warn!(error = %e, pattern, "redis: pattern DEL failed");
        }
    }

    #[cfg(not(feature = "redis"))]
    fn cache_del_pattern(&self, _pattern: &str) {}

    // -----------------------------------------------------------------------
    // Invalidation helpers
    // -----------------------------------------------------------------------

    /// Invalidate all cache keys that could reference the given attempt.
    #[cfg(feature = "redis")]
    fn invalidate_attempt_keys(
        &self,
        source: &str,
        issue_id: &str,
        id: Option<i64>,
        pr_url: Option<&str>,
    ) {
        if self.cache.is_none() {
            return;
        }
        let t = &self.tenant_id;
        let mut keys = vec![
            format!("{t}:attempted:{source}:{issue_id}"),
            format!("{t}:attempt:{source}:{issue_id}"),
            format!("{t}:stats"),
        ];
        if let Some(id) = id {
            keys.push(format!("{t}:attempt:id:{id}"));
        }
        if let Some(pr) = pr_url {
            keys.push(format!("{t}:attempt:pr:{pr}"));
        }
        let refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        self.cache_del(&refs);
    }

    #[cfg(not(feature = "redis"))]
    fn invalidate_attempt_keys(
        &self,
        _source: &str,
        _issue_id: &str,
        _id: Option<i64>,
        _pr_url: Option<&str>,
    ) {
    }

    // -----------------------------------------------------------------------
    // Row mapping
    // -----------------------------------------------------------------------

    fn row_to_fix_attempt(row: &Row) -> FixAttempt {
        let issue_labels: Vec<String> = row
            .get::<_, Option<String>>("issue_labels")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        FixAttempt {
            id: row.get("id"),
            source: row.get("source"),
            issue_id: row.get("issue_id"),
            short_id: row.get("short_id"),
            attempted_at: row.get("attempted_at"),
            pr_url: row.get("pr_url"),
            scm_repo: row.get("scm_repo"),
            scm_pr_number: row.get("scm_pr_number"),
            status: row
                .get::<_, String>("status")
                .parse()
                .unwrap_or(FixAttemptStatus::Pending),
            error_message: row.get("error_message"),
            merged_at: row.get("merged_at"),
            resolved_at: row.get("resolved_at"),
            retry_count: row.get::<_, Option<i32>>("retry_count").unwrap_or(0) as u32,
            last_retry_at: row.get("last_retry_at"),
            issue_labels,
            parent_attempt_id: row.get("parent_attempt_id"),
            cascade_repo: row.get("cascade_repo"),
        }
    }

    fn row_to_pr_record(row: &Row) -> PrRecord {
        PrRecord {
            id: row.get("id"),
            pr_url: row.get("pr_url"),
            scm_repo: row.get("scm_repo"),
            pr_number: row.get("pr_number"),
            attempt_id: row.get("attempt_id"),
            issue_id: row.get("issue_id"),
            issue_source: row.get("issue_source"),
            title: row.get("title"),
            description: row.get("description"),
            author: row.get("author"),
            head_branch: row.get("head_branch"),
            base_branch: row.get("base_branch"),
            status: row.get("status"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            merged_at: row.get("merged_at"),
            closed_at: row.get("closed_at"),
            approvals_count: row.get("approvals_count"),
            changes_requested_count: row.get("changes_requested_count"),
            comments_count: row.get("comments_count"),
            last_review_at: row.get("last_review_at"),
            time_to_first_review_mins: row.get("time_to_first_review_mins"),
            time_to_merge_mins: row.get("time_to_merge_mins"),
            review_cycles: row.get("review_cycles"),
            files_changed: row.get("files_changed"),
            lines_added: row.get("lines_added"),
            lines_removed: row.get("lines_removed"),
        }
    }

    fn row_to_user(row: &Row) -> UserRow {
        UserRow {
            id: row.get("id"),
            email: row.get("email"),
            password_hash: row.get("password_hash"),
            name: row.get("name"),
            role: row.get("role"),
            avatar_url: row.get("avatar_url"),
            created_at: row.get::<_, DateTime<Utc>>("created_at").to_rfc3339(),
            updated_at: row.get::<_, DateTime<Utc>>("updated_at").to_rfc3339(),
        }
    }
}

/// Generate a cryptographically random session token (64 hex chars = 32 bytes).
fn generate_session_token() -> String {
    use rand::RngExt;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Convert a `tokio_postgres::Error` into the crate error type.
fn db_err(e: impl std::fmt::Display) -> crate::error::Error {
    crate::error::Error::Database(e.to_string())
}

// ===========================================================================
// FixAttemptTracker implementation
// ===========================================================================

impl FixAttemptTracker for PostgresBackend {
    // -----------------------------------------------------------------------
    // Cached reads
    // -----------------------------------------------------------------------

    fn has_attempted(&self, source: &str, issue_id: &str) -> Result<bool> {
        let key = format!("{}:attempted:{}:{}", self.tenant_id, source, issue_id);
        if let Some(cached) = self.cache_get::<bool>(&key) {
            return Ok(cached);
        }

        let exists: bool = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM fix_attempts WHERE source = $1 AND issue_id = $2 AND reset_at IS NULL)",
            ).await.map_err(db_err)?;
            let row = c.query_one(&stmt, &[&source, &issue_id]).await.map_err(db_err)?;
            Ok::<bool, crate::error::Error>(row.get(0))
        })?;

        self.cache_set(&key, &exists);
        Ok(exists)
    }

    fn get_attempt(&self, source: &str, issue_id: &str) -> Result<Option<FixAttempt>> {
        let key = format!("{}:attempt:{}:{}", self.tenant_id, source, issue_id);
        if let Some(cached) = self.cache_get::<FixAttempt>(&key) {
            return Ok(Some(cached));
        }

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"SELECT id, source, issue_id, short_id, attempted_at, pr_url, scm_repo,
                          scm_pr_number, status, error_message, merged_at, resolved_at,
                          retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
                   FROM fix_attempts
                   WHERE source = $1 AND issue_id = $2"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id])
                .await
                .map_err(db_err)
        })?;

        let attempt = row.as_ref().map(Self::row_to_fix_attempt);
        if let Some(ref a) = attempt {
            self.cache_set(&key, a);
        }
        Ok(attempt)
    }

    fn get_attempt_by_pr_url(&self, pr_url: &str) -> Result<Option<FixAttempt>> {
        let key = format!("{}:attempt:pr:{}", self.tenant_id, pr_url);
        if let Some(cached) = self.cache_get::<FixAttempt>(&key) {
            return Ok(Some(cached));
        }

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"SELECT id, source, issue_id, short_id, attempted_at, pr_url, scm_repo,
                          scm_pr_number, status, error_message, merged_at, resolved_at,
                          retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
                   FROM fix_attempts
                   WHERE pr_url = $1
                   ORDER BY attempted_at DESC, id DESC
                   LIMIT 1"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&pr_url]).await.map_err(db_err)
        })?;

        let attempt = row.as_ref().map(Self::row_to_fix_attempt);
        if let Some(ref a) = attempt {
            self.cache_set(&key, a);
        }
        Ok(attempt)
    }

    fn get_attempt_by_id(&self, id: i64) -> Result<Option<FixAttempt>> {
        let key = format!("{}:attempt:id:{}", self.tenant_id, id);
        if let Some(cached) = self.cache_get::<FixAttempt>(&key) {
            return Ok(Some(cached));
        }

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"SELECT id, source, issue_id, short_id, attempted_at, pr_url, scm_repo,
                          scm_pr_number, status, error_message, merged_at, resolved_at,
                          retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
                   FROM fix_attempts
                   WHERE id = $1"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&id]).await.map_err(db_err)
        })?;

        let attempt = row.as_ref().map(Self::row_to_fix_attempt);
        if let Some(ref a) = attempt {
            self.cache_set(&key, a);
        }
        Ok(attempt)
    }

    fn get_pr(&self, pr_url: &str) -> Result<Option<PrRecord>> {
        let key = format!("{}:pr:{}", self.tenant_id, pr_url);
        if let Some(cached) = self.cache_get::<PrRecord>(&key) {
            return Ok(Some(cached));
        }

        let row =
            self.block_on(async {
                let c = self.client().await.map_err(db_err)?;
                let stmt = c.prepare_cached(
                r#"SELECT id, pr_url, scm_repo, pr_number, attempt_id, issue_id, issue_source,
                          title, description, author, head_branch, base_branch, status,
                          created_at, updated_at, merged_at, closed_at,
                          approvals_count, changes_requested_count, comments_count, last_review_at,
                          time_to_first_review_mins, time_to_merge_mins, review_cycles,
                          files_changed, lines_added, lines_removed
                   FROM prs WHERE pr_url = $1"#,
            ).await.map_err(db_err)?;
                c.query_opt(&stmt, &[&pr_url]).await.map_err(db_err)
            })?;

        let pr = row.as_ref().map(Self::row_to_pr_record);
        if let Some(ref p) = pr {
            self.cache_set(&key, p);
        }
        Ok(pr)
    }

    fn get_user_by_id(&self, id: i64) -> Result<Option<UserRow>> {
        let key = format!("{}:user:id:{}", self.tenant_id, id);
        if let Some(cached) = self.cache_get::<CachedUser>(&key) {
            return Ok(Some(cached.into()));
        }

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "SELECT id, email, password_hash, name, role, avatar_url, created_at, updated_at FROM users WHERE id = $1",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&id]).await.map_err(db_err)
        })?;

        let user = row.as_ref().map(Self::row_to_user);
        if let Some(ref u) = user {
            self.cache_set(&key, &CachedUser::from(u));
        }
        Ok(user)
    }

    fn get_user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        let key = format!("{}:user:email:{}", self.tenant_id, email);
        if let Some(cached) = self.cache_get::<CachedUser>(&key) {
            return Ok(Some(cached.into()));
        }

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "SELECT id, email, password_hash, name, role, avatar_url, created_at, updated_at FROM users WHERE email = $1",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&email]).await.map_err(db_err)
        })?;

        let user = row.as_ref().map(Self::row_to_user);
        if let Some(ref u) = user {
            self.cache_set(&key, &CachedUser::from(u));
        }
        Ok(user)
    }

    fn get_session_user(&self, token: &str) -> Result<Option<UserRow>> {
        let key = format!("{}:session:{}", self.tenant_id, token);
        if let Some(cached) = self.cache_get::<CachedUser>(&key) {
            return Ok(Some(cached.into()));
        }

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"SELECT u.id, u.email, u.password_hash, u.name, u.role, u.avatar_url,
                          u.created_at, u.updated_at
                   FROM sessions s
                   JOIN users u ON s.user_id = u.id
                   WHERE s.id = $1 AND s.expires_at > NOW()"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&token]).await.map_err(db_err)
        })?;

        let user = row.as_ref().map(Self::row_to_user);
        if let Some(ref u) = user {
            self.cache_set(&key, &CachedUser::from(u));
        }
        Ok(user)
    }

    fn get_channel_cursor(&self, channel: &str, cursor_key: &str) -> Result<Option<String>> {
        let key = format!("{}:cursor:{}:{}", self.tenant_id, channel, cursor_key);
        if let Some(cached) = self.cache_get::<String>(&key) {
            return Ok(Some(cached));
        }

        let val: Option<String> = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "SELECT cursor_value FROM question_channel_cursor WHERE channel = $1 AND cursor_key = $2",
            ).await.map_err(db_err)?;
            let row = c.query_opt(&stmt, &[&channel, &cursor_key]).await.map_err(db_err)?;
            Ok::<_, crate::error::Error>(row.map(|r| r.get("cursor_value")))
        })?;

        if let Some(ref v) = val {
            self.cache_set(&key, v);
        }
        Ok(val)
    }

    fn get_stats(&self) -> Result<FixAttemptStats> {
        let key = format!("{}:stats", self.tenant_id);
        if let Some(cached) = self.cache_get::<FixAttemptStats>(&key) {
            return Ok(cached);
        }

        // Single query: per-source breakdown; derive overall totals in Rust.
        let rows = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "SELECT source, status, COUNT(*)::bigint AS cnt FROM fix_attempts GROUP BY source, status",
            ).await.map_err(db_err)?;
            c.query(&stmt, &[]).await.map_err(db_err)
        })?;

        let mut stats = FixAttemptStats::default();
        let mut by_source: HashMap<String, SourceStats> = HashMap::new();
        for row in &rows {
            let source: String = row.get("source");
            let status: String = row.get("status");
            let count = row.get::<_, i64>("cnt") as usize;

            // Accumulate overall totals
            stats.total += count;
            match status.as_str() {
                "pending" => stats.pending += count,
                "success" => stats.success += count,
                "failed" => stats.failed += count,
                "merged" => stats.merged += count,
                "closed" => stats.closed += count,
                "cannot_fix" => stats.cannot_fix += count,
                _ => {}
            }

            // Accumulate per-source
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
        stats.by_source = by_source;

        self.cache_set(&key, &stats);
        Ok(stats)
    }

    // -----------------------------------------------------------------------
    // Non-cached reads (multi-row / set)
    // -----------------------------------------------------------------------

    fn get_attempted_issue_ids(&self, source: &str) -> HashSet<String> {
        self.block_on(async {
            let Ok(c) = self.client().await else {
                return HashSet::new();
            };
            let Ok(stmt) = c
                .prepare_cached(
                    "SELECT issue_id FROM fix_attempts WHERE source = $1 AND reset_at IS NULL",
                )
                .await
            else {
                return HashSet::new();
            };
            c.query(&stmt, &[&source])
                .await
                .unwrap_or_default()
                .iter()
                .map(|r| r.get::<_, String>("issue_id"))
                .collect()
        })
    }

    fn get_attempts_by_status(&self, status: FixAttemptStatus) -> Result<Vec<FixAttempt>> {
        let status_str = status.to_string();
        let rows = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"SELECT id, source, issue_id, short_id, attempted_at, pr_url, scm_repo,
                          scm_pr_number, status, error_message, merged_at, resolved_at,
                          retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
                   FROM fix_attempts WHERE status = $1"#,
                )
                .await
                .map_err(db_err)?;
            c.query(&stmt, &[&status_str]).await.map_err(db_err)
        })?;
        Ok(rows.iter().map(Self::row_to_fix_attempt).collect())
    }

    fn get_pending_prs(&self) -> Result<Vec<FixAttempt>> {
        let rows = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"SELECT id, source, issue_id, short_id, attempted_at, pr_url, scm_repo,
                          scm_pr_number, status, error_message, merged_at, resolved_at,
                          retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
                   FROM fix_attempts WHERE status = 'success' AND pr_url IS NOT NULL"#,
                )
                .await
                .map_err(db_err)?;
            c.query(&stmt, &[]).await.map_err(db_err)
        })?;
        Ok(rows.iter().map(Self::row_to_fix_attempt).collect())
    }

    fn get_retryable_issues(&self, max_retries: u32) -> Result<Vec<FixAttempt>> {
        let max = max_retries as i32;
        let rows = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"SELECT id, source, issue_id, short_id, attempted_at, pr_url, scm_repo,
                          scm_pr_number, status, error_message, merged_at, resolved_at,
                          retry_count, last_retry_at, issue_labels, parent_attempt_id, cascade_repo
                   FROM fix_attempts
                   WHERE status IN ('failed', 'closed')
                     AND COALESCE(retry_count, 0) < $1"#,
                )
                .await
                .map_err(db_err)?;
            c.query(&stmt, &[&max]).await.map_err(db_err)
        })?;
        Ok(rows.iter().map(Self::row_to_fix_attempt).collect())
    }

    // -----------------------------------------------------------------------
    // Attempt writes (with cache invalidation)
    // -----------------------------------------------------------------------

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
            source,
            issue_id,
            short_id,
            labels_count = labels.len(),
            "Recording fix attempt"
        );

        let labels_json: Option<String> = if labels.is_empty() {
            None
        } else {
            Some(serde_json::to_string(labels).unwrap_or_default())
        };

        // Atomic upsert using the partial unique index on (source, issue_id) WHERE cascade_repo IS NULL.
        // On conflict with a reset row (reset_at IS NOT NULL): update short_id, attempted_at, labels, clear reset_at.
        // On conflict with a non-reset row: no-op (all fields keep their existing values).
        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                r#"INSERT INTO fix_attempts (source, issue_id, short_id, status, attempted_at, issue_labels)
                   VALUES ($1, $2, $3, 'pending', NOW(), $4)
                   ON CONFLICT (source, issue_id) WHERE cascade_repo IS NULL
                   DO UPDATE SET
                       short_id = CASE WHEN fix_attempts.reset_at IS NOT NULL THEN EXCLUDED.short_id ELSE fix_attempts.short_id END,
                       attempted_at = CASE WHEN fix_attempts.reset_at IS NOT NULL THEN NOW() ELSE fix_attempts.attempted_at END,
                       issue_labels = CASE WHEN fix_attempts.reset_at IS NOT NULL THEN COALESCE(EXCLUDED.issue_labels, fix_attempts.issue_labels) ELSE fix_attempts.issue_labels END,
                       reset_at = CASE WHEN fix_attempts.reset_at IS NOT NULL THEN NULL ELSE fix_attempts.reset_at END
                   RETURNING id, pr_url, (xmax = 0) AS was_inserted"#,
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id, &short_id, &labels_json])
                .await
                .map_err(db_err)
        })?;

        if let Some(ref r) = row {
            let was_inserted: bool = r.get("was_inserted");
            if was_inserted {
                tracing::info!(source, issue_id, "Fix attempt recorded");
            } else {
                tracing::info!(
                    source,
                    issue_id,
                    "Fix attempt upserted (existing row updated)"
                );
            }
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn mark_success(&self, source: &str, issue_id: &str, pr_url: &str) -> Result<()> {
        tracing::info!(source, issue_id, pr_url, "Marking fix attempt as success");

        let (scm_repo, scm_pr_number) = match parse_pr_url(pr_url) {
            Some((repo, num)) => (Some(repo), Some(num)),
            None => {
                tracing::warn!(pr_url, source, issue_id, "Failed to parse PR URL");
                (None, None)
            }
        };

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"UPDATE fix_attempts
                   SET status = 'success', pr_url = $1, scm_repo = $2, scm_pr_number = $3
                   WHERE source = $4 AND issue_id = $5
                   RETURNING id"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(
                &stmt,
                &[&pr_url, &scm_repo, &scm_pr_number, &source, &issue_id],
            )
            .await
            .map_err(db_err)
        })?;

        let id = row.as_ref().map(|r| r.get::<_, i64>("id"));
        self.invalidate_attempt_keys(source, issue_id, id, Some(pr_url));
        Ok(())
    }

    fn mark_failed(&self, source: &str, issue_id: &str, error_message: &str) -> Result<()> {
        tracing::info!(
            source,
            issue_id,
            error_message,
            "Marking fix attempt as failed"
        );

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "UPDATE fix_attempts SET status = 'failed', error_message = $1 WHERE source = $2 AND issue_id = $3 RETURNING id, pr_url",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&error_message, &source, &issue_id])
                .await
                .map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn mark_merged(&self, source: &str, issue_id: &str) -> Result<()> {
        tracing::info!(source, issue_id, "Marking fix attempt as merged");

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "UPDATE fix_attempts SET status = 'merged', merged_at = NOW() WHERE source = $1 AND issue_id = $2 RETURNING id, pr_url",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id]).await.map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn mark_closed(&self, source: &str, issue_id: &str) -> Result<()> {
        tracing::info!(source, issue_id, "Marking fix attempt as closed");

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "UPDATE fix_attempts SET status = 'closed' WHERE source = $1 AND issue_id = $2 RETURNING id, pr_url",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id]).await.map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn mark_resolved(&self, source: &str, issue_id: &str) -> Result<()> {
        tracing::info!(source, issue_id, "Marking fix attempt as resolved");

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "UPDATE fix_attempts SET resolved_at = NOW() WHERE source = $1 AND issue_id = $2 RETURNING id, pr_url",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id]).await.map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn reset_attempt(&self, source: &str, issue_id: &str) -> Result<()> {
        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"UPDATE fix_attempts SET
                     status = 'pending',
                     retry_count = 0,
                     reset_at = NOW(),
                     pr_url = NULL,
                     scm_repo = NULL,
                     scm_pr_number = NULL,
                     error_message = NULL,
                     merged_at = NULL,
                     resolved_at = NULL,
                     attempted_at = NOW()
                   WHERE source = $1 AND issue_id = $2
                   RETURNING id, pr_url"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id])
                .await
                .map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn increment_retry(&self, source: &str, issue_id: &str) -> Result<()> {
        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"UPDATE fix_attempts
                   SET retry_count = COALESCE(retry_count, 0) + 1,
                       last_retry_at = NOW()
                   WHERE source = $1 AND issue_id = $2
                   RETURNING id, pr_url"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id])
                .await
                .map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn mark_cannot_fix(&self, source: &str, issue_id: &str, reason: &str) -> Result<()> {
        tracing::info!(
            source,
            issue_id,
            reason,
            "Marking fix attempt as cannot_fix"
        );

        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "UPDATE fix_attempts SET status = 'cannot_fix', error_message = $1 WHERE source = $2 AND issue_id = $3 RETURNING id, pr_url",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&reason, &source, &issue_id]).await.map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                source,
                issue_id,
                Some(r.get("id")),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    fn prepare_for_retry(&self, source: &str, issue_id: &str) -> Result<()> {
        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"UPDATE fix_attempts SET
                     status = 'pending',
                     retry_count = COALESCE(retry_count, 0) + 1,
                     last_retry_at = NOW(),
                     pr_url = NULL,
                     scm_repo = NULL,
                     scm_pr_number = NULL,
                     error_message = NULL,
                     attempted_at = NOW()
                   WHERE source = $1 AND issue_id = $2
                     AND status IN ('failed', 'closed')
                   RETURNING id, pr_url"#,
                )
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&source, &issue_id])
                .await
                .map_err(db_err)
        })?;

        match row {
            Some(r) => {
                self.invalidate_attempt_keys(
                    source,
                    issue_id,
                    Some(r.get("id")),
                    r.get::<_, Option<String>>("pr_url").as_deref(),
                );
                Ok(())
            }
            None => {
                tracing::warn!(source, issue_id, "prepare_for_retry: no rows updated");
                Err(crate::error::Error::Storage(format!(
                    "Attempt {}/{} not in retryable state (failed/closed)",
                    source, issue_id
                )))
            }
        }
    }

    fn record_cascade_attempt(
        &self,
        source: &str,
        issue_id: &str,
        short_id: &str,
        parent_attempt_id: i64,
        cascade_repo: &str,
    ) -> Result<i64> {
        let (id, was_existing) = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            // Check if there's a pending cascade (no PR yet) for this combo.
            // Completed cascades (with PR) are allowed to be re-triggered (e.g. merge + release).
            let s1 = c.prepare_cached(
                "SELECT id FROM fix_attempts WHERE source = $1 AND issue_id = $2 AND cascade_repo = $3 AND (pr_url IS NULL OR pr_url = '')",
            ).await.map_err(db_err)?;
            let existing = c.query_opt(&s1, &[&source, &issue_id, &cascade_repo]).await.map_err(db_err)?;

            if let Some(row) = existing {
                return Ok::<(i64, bool), crate::error::Error>((row.get::<_, i64>("id"), true));
            }

            let s2 = c.prepare_cached(
                r#"INSERT INTO fix_attempts (source, issue_id, short_id, status, attempted_at, parent_attempt_id, cascade_repo)
                       VALUES ($1, $2, $3, 'pending', NOW(), $4, $5)
                       RETURNING id"#,
            ).await.map_err(db_err)?;
            let row = c
                .query_one(&s2, &[&source, &issue_id, &short_id, &parent_attempt_id, &cascade_repo])
                .await
                .map_err(db_err)?;
            Ok((row.get("id"), false))
        })?;

        if was_existing {
            tracing::info!(
                source,
                issue_id,
                cascade_repo,
                "Cascade attempt already exists, skipping"
            );
        } else {
            tracing::info!(
                source,
                issue_id,
                cascade_repo,
                parent_attempt_id,
                attempt_id = id,
                "Recorded cascade fix attempt"
            );
            self.invalidate_attempt_keys(source, issue_id, Some(id), None);
        }
        Ok(id)
    }

    fn update_attempt_pr(
        &self,
        attempt_id: i64,
        pr_url: &str,
        scm_repo: &str,
        pr_number: i64,
    ) -> Result<()> {
        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                r#"WITH old AS (SELECT source, issue_id, pr_url FROM fix_attempts WHERE id = $4)
                   UPDATE fix_attempts SET pr_url = $1, scm_repo = $2, scm_pr_number = $3, status = 'success'
                   WHERE id = $4
                   RETURNING source, issue_id, (SELECT pr_url FROM old) AS old_pr_url"#,
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&pr_url, &scm_repo, &pr_number, &attempt_id])
                .await
                .map_err(db_err)
        })?;

        if let Some(r) = row {
            let source: String = r.get("source");
            let issue_id: String = r.get("issue_id");
            let old_pr: Option<String> = r.get("old_pr_url");
            self.invalidate_attempt_keys(&source, &issue_id, Some(attempt_id), Some(pr_url));
            if let Some(ref old) = old_pr {
                if old != pr_url {
                    self.cache_del(&[&format!("{}:attempt:pr:{}", self.tenant_id, old)]);
                }
            }
        }
        Ok(())
    }

    fn mark_cascade_failed(&self, attempt_id: i64, error: &str) -> Result<()> {
        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "UPDATE fix_attempts SET status = 'failed', error_message = $1 WHERE id = $2 RETURNING source, issue_id, pr_url",
            ).await.map_err(db_err)?;
            c.query_opt(&stmt, &[&error, &attempt_id]).await.map_err(db_err)
        })?;

        if let Some(r) = row {
            self.invalidate_attempt_keys(
                &r.get::<_, String>("source"),
                &r.get::<_, String>("issue_id"),
                Some(attempt_id),
                r.get::<_, Option<String>>("pr_url").as_deref(),
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // PR writes (with cache invalidation)
    // -----------------------------------------------------------------------

    fn upsert_pr(&self, pr: &PrRecord) -> Result<i64> {
        let id: i64 = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                r#"INSERT INTO prs (
                        pr_url, scm_repo, pr_number, attempt_id, issue_id, issue_source,
                        title, description, author, head_branch, base_branch, status,
                        created_at, updated_at, merged_at, closed_at,
                        approvals_count, changes_requested_count, comments_count, last_review_at,
                        time_to_first_review_mins, time_to_merge_mins, review_cycles,
                        files_changed, lines_added, lines_removed
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                        $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26
                    )
                    ON CONFLICT(pr_url) DO UPDATE SET
                        scm_repo = EXCLUDED.scm_repo,
                        pr_number = EXCLUDED.pr_number,
                        attempt_id = COALESCE(EXCLUDED.attempt_id, prs.attempt_id),
                        issue_id = COALESCE(EXCLUDED.issue_id, prs.issue_id),
                        issue_source = COALESCE(EXCLUDED.issue_source, prs.issue_source),
                        title = COALESCE(EXCLUDED.title, prs.title),
                        description = COALESCE(EXCLUDED.description, prs.description),
                        author = COALESCE(EXCLUDED.author, prs.author),
                        head_branch = COALESCE(EXCLUDED.head_branch, prs.head_branch),
                        base_branch = COALESCE(EXCLUDED.base_branch, prs.base_branch),
                        status = EXCLUDED.status,
                        updated_at = NOW(),
                        merged_at = COALESCE(EXCLUDED.merged_at, prs.merged_at),
                        closed_at = COALESCE(EXCLUDED.closed_at, prs.closed_at),
                        approvals_count = EXCLUDED.approvals_count,
                        changes_requested_count = EXCLUDED.changes_requested_count,
                        comments_count = EXCLUDED.comments_count,
                        last_review_at = COALESCE(EXCLUDED.last_review_at, prs.last_review_at),
                        time_to_first_review_mins = COALESCE(EXCLUDED.time_to_first_review_mins, prs.time_to_first_review_mins),
                        time_to_merge_mins = COALESCE(EXCLUDED.time_to_merge_mins, prs.time_to_merge_mins),
                        review_cycles = EXCLUDED.review_cycles,
                        files_changed = COALESCE(EXCLUDED.files_changed, prs.files_changed),
                        lines_added = COALESCE(EXCLUDED.lines_added, prs.lines_added),
                        lines_removed = COALESCE(EXCLUDED.lines_removed, prs.lines_removed)
                    RETURNING id"#,
            ).await.map_err(db_err)?;
            let row = c
                .query_one(
                    &stmt,
                    &[
                        &pr.pr_url, &pr.scm_repo, &pr.pr_number, &pr.attempt_id,
                        &pr.issue_id, &pr.issue_source, &pr.title, &pr.description,
                        &pr.author, &pr.head_branch, &pr.base_branch, &pr.status,
                        &pr.created_at, &pr.updated_at, &pr.merged_at, &pr.closed_at,
                        &(pr.approvals_count as i64), &(pr.changes_requested_count as i64),
                        &(pr.comments_count as i64), &pr.last_review_at,
                        &pr.time_to_first_review_mins, &pr.time_to_merge_mins,
                        &(pr.review_cycles as i64), &pr.files_changed,
                        &pr.lines_added, &pr.lines_removed,
                    ],
                )
                .await
                .map_err(db_err)?;
            Ok::<i64, crate::error::Error>(row.get("id"))
        })?;

        self.cache_del(&[&format!("{}:pr:{}", self.tenant_id, pr.pr_url)]);
        Ok(id)
    }

    fn update_pr_status(&self, pr_url: &str, status: &str) -> Result<()> {
        let now = Utc::now();
        let (merged_at, closed_at): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = match status {
            "merged" => (Some(now), None),
            "closed" => (None, Some(now)),
            _ => (None, None),
        };

        self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    r#"UPDATE prs SET
                    status = $1,
                    updated_at = $2,
                    merged_at = COALESCE($3, merged_at),
                    closed_at = COALESCE($4, closed_at)
                  WHERE pr_url = $5"#,
                )
                .await
                .map_err(db_err)?;
            c.execute(&stmt, &[&status, &now, &merged_at, &closed_at, &pr_url])
                .await
                .map_err(db_err)
        })?;

        tracing::info!(pr_url, status, "PR status updated");
        self.cache_del(&[&format!("{}:pr:{}", self.tenant_id, pr_url)]);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // User writes (with cache invalidation)
    // -----------------------------------------------------------------------

    fn create_user(&self, email: &str, password_hash: &str, name: &str, role: &str) -> Result<i64> {
        let id: i64 = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                "INSERT INTO users (email, password_hash, name, role) VALUES ($1, $2, $3, $4) RETURNING id",
            ).await.map_err(db_err)?;
            let row = c.query_one(&stmt, &[&email, &password_hash, &name, &role]).await.map_err(db_err)?;
            Ok::<i64, crate::error::Error>(row.get("id"))
        })?;

        let t = &self.tenant_id;
        self.cache_del(&[
            &format!("{t}:user:id:{id}"),
            &format!("{t}:user:email:{email}"),
        ]);
        self.cache_del_pattern(&format!("{t}:session:*"));
        Ok(id)
    }

    fn update_user(
        &self,
        id: i64,
        email: Option<&str>,
        password_hash: Option<&str>,
        name: Option<&str>,
        role: Option<&str>,
        avatar_url: Option<&str>,
    ) -> Result<bool> {
        let mut sets = Vec::new();
        let mut idx = 1u32;

        // Build owned copies so they live long enough for the query
        let e_owned = email.map(|s| s.to_string());
        let p_owned = password_hash.map(|s| s.to_string());
        let n_owned = name.map(|s| s.to_string());
        let r_owned = role.map(|s| s.to_string());
        let a_owned = avatar_url.map(|s| s.to_string());

        if e_owned.is_some() {
            sets.push(format!("email = ${idx}"));
            idx += 1;
        }
        if p_owned.is_some() {
            sets.push(format!("password_hash = ${idx}"));
            idx += 1;
        }
        if n_owned.is_some() {
            sets.push(format!("name = ${idx}"));
            idx += 1;
        }
        if r_owned.is_some() {
            sets.push(format!("role = ${idx}"));
            idx += 1;
        }
        if a_owned.is_some() {
            sets.push(format!("avatar_url = ${idx}"));
            idx += 1;
        }
        if sets.is_empty() {
            return Ok(false);
        }
        sets.push("updated_at = NOW()".to_string());
        let sql = format!("UPDATE users SET {} WHERE id = ${idx}", sets.join(", "));

        // Combine old-email lookup + UPDATE in a single block_on / connection checkout
        let (old_email, rows_affected) = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;

            // Fetch old email for cache invalidation
            let s = c
                .prepare_cached("SELECT email FROM users WHERE id = $1")
                .await
                .map_err(db_err)?;
            let old_email: Option<String> = c
                .query_opt(&s, &[&id])
                .await
                .map_err(db_err)?
                .map(|r| r.get("email"));

            let mut params: Vec<PgParam<'_>> = Vec::new();
            if let Some(ref v) = e_owned {
                params.push(v);
            }
            if let Some(ref v) = p_owned {
                params.push(v);
            }
            if let Some(ref v) = n_owned {
                params.push(v);
            }
            if let Some(ref v) = r_owned {
                params.push(v);
            }
            if let Some(ref v) = a_owned {
                params.push(v);
            }
            params.push(&id);
            let affected = c.execute(&*sql, &params).await.map_err(db_err)?;
            Ok::<_, crate::error::Error>((old_email, affected))
        })?;

        let t = &self.tenant_id;
        let mut del_keys = vec![format!("{t}:user:id:{id}")];
        if let Some(ref old_e) = old_email {
            del_keys.push(format!("{t}:user:email:{old_e}"));
        }
        if let Some(new_e) = email {
            del_keys.push(format!("{t}:user:email:{new_e}"));
        }
        let refs: Vec<&str> = del_keys.iter().map(|s| s.as_str()).collect();
        self.cache_del(&refs);
        self.cache_del_pattern(&format!("{t}:session:*"));

        Ok(rows_affected > 0)
    }

    fn delete_user(&self, id: i64) -> Result<bool> {
        let row = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached("DELETE FROM users WHERE id = $1 RETURNING email")
                .await
                .map_err(db_err)?;
            c.query_opt(&stmt, &[&id]).await.map_err(db_err)
        })?;

        let t = &self.tenant_id;
        let mut del_keys = vec![format!("{t}:user:id:{id}")];
        if let Some(ref r) = row {
            del_keys.push(format!("{t}:user:email:{}", r.get::<_, String>("email")));
        }
        let refs: Vec<&str> = del_keys.iter().map(|s| s.as_str()).collect();
        self.cache_del(&refs);
        self.cache_del_pattern(&format!("{t}:session:*"));

        Ok(row.is_some())
    }

    // -----------------------------------------------------------------------
    // Session writes (with cache invalidation)
    // -----------------------------------------------------------------------

    fn create_session(&self, user_id: i64, expires_at: &str) -> Result<String> {
        let token = generate_session_token();
        self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached(
                    "INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)",
                )
                .await
                .map_err(db_err)?;
            c.execute(&stmt, &[&token, &user_id, &expires_at])
                .await
                .map_err(db_err)
        })?;
        Ok(token)
    }

    fn delete_session(&self, token: &str) -> Result<()> {
        self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached("DELETE FROM sessions WHERE id = $1")
                .await
                .map_err(db_err)?;
            c.execute(&stmt, &[&token]).await.map_err(db_err)
        })?;

        self.cache_del(&[&format!("{}:session:{}", self.tenant_id, token)]);
        Ok(())
    }

    fn cleanup_expired_sessions(&self) -> Result<usize> {
        let rows_affected = self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached("DELETE FROM sessions WHERE expires_at <= NOW()")
                .await
                .map_err(db_err)?;
            c.execute(&stmt, &[]).await.map_err(db_err)
        })?;

        if rows_affected > 0 {
            self.cache_del_pattern(&format!("{}:session:*", self.tenant_id));
        }
        Ok(rows_affected as usize)
    }

    fn delete_user_sessions(&self, user_id: i64) -> Result<()> {
        self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c
                .prepare_cached("DELETE FROM sessions WHERE user_id = $1")
                .await
                .map_err(db_err)?;
            c.execute(&stmt, &[&user_id]).await.map_err(db_err)
        })?;

        self.cache_del_pattern(&format!("{}:session:*", self.tenant_id));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cursor writes (with cache invalidation)
    // -----------------------------------------------------------------------

    fn set_channel_cursor(
        &self,
        channel: &str,
        cursor_key: &str,
        cursor_value: &str,
    ) -> Result<()> {
        self.block_on(async {
            let c = self.client().await.map_err(db_err)?;
            let stmt = c.prepare_cached(
                r#"INSERT INTO question_channel_cursor (channel, cursor_key, cursor_value, updated_at)
                   VALUES ($1, $2, $3, NOW())
                   ON CONFLICT(channel, cursor_key) DO UPDATE SET
                       cursor_value = EXCLUDED.cursor_value,
                       updated_at = EXCLUDED.updated_at"#,
            ).await.map_err(db_err)?;
            c.execute(&stmt, &[&channel, &cursor_key, &cursor_value]).await.map_err(db_err)
        })?;

        self.cache_del(&[&format!(
            "{}:cursor:{}:{}",
            self.tenant_id, channel, cursor_key
        )]);
        Ok(())
    }
}
