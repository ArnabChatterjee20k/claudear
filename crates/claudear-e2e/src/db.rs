//! SQLite assertion helpers for E2E tests.
//!
//! Supports direct file access (native) and Docker volume access.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// Database access mode.
pub enum DbAccess {
    /// Direct file system access to the SQLite database.
    Direct(PathBuf),
    /// Access via `docker exec` on the running daemon container.
    Docker {
        container_name: String,
        volume: String,
    },
}

impl DbAccess {
    /// Create direct access to a database file.
    pub fn direct(path: impl Into<PathBuf>) -> Self {
        DbAccess::Direct(path.into())
    }

    /// Create Docker access using `docker exec` on the running container.
    pub fn docker(container_name: impl Into<String>, volume: impl Into<String>) -> Self {
        DbAccess::Docker {
            container_name: container_name.into(),
            volume: volume.into(),
        }
    }
}

/// E2E database client for assertions.
pub struct E2eDb {
    access: DbAccess,
}

impl E2eDb {
    pub fn new(access: DbAccess) -> Self {
        Self { access }
    }

    /// Execute a SQL query and return the output as a string.
    pub fn query(&self, sql: &str) -> Result<String> {
        match &self.access {
            DbAccess::Direct(path) => {
                // Use read-write mode to support WAL (read-only can't access -shm).
                // Set busy_timeout to wait for the daemon's write lock.
                let conn = rusqlite::Connection::open(path).context("open db")?;
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .context("set busy_timeout")?;

                let mut stmt = conn.prepare(sql).context("prepare SQL")?;
                let col_count = stmt.column_count();
                let mut rows_out = Vec::new();

                let mut rows = stmt.query([]).context("execute query")?;
                while let Some(row) = rows.next().context("fetch row")? {
                    let mut cols = Vec::new();
                    for i in 0..col_count {
                        // Use Value to handle both integer and text columns
                        let val = row
                            .get_ref(i)
                            .map(|v| match v {
                                rusqlite::types::ValueRef::Null => String::new(),
                                rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                                rusqlite::types::ValueRef::Real(f) => f.to_string(),
                                rusqlite::types::ValueRef::Text(s) => {
                                    String::from_utf8_lossy(s).to_string()
                                }
                                rusqlite::types::ValueRef::Blob(b) => {
                                    format!("<blob {}B>", b.len())
                                }
                            })
                            .unwrap_or_default();
                        cols.push(val);
                    }
                    rows_out.push(cols.join("\t"));
                }

                Ok(rows_out.join("\n"))
            }
            DbAccess::Docker {
                container_name,
                volume,
            } => {
                // Try `docker exec` on the running daemon container first to avoid
                // WAL lock conflicts. If the container is stopped (e.g., during
                // regression simulation), fall back to `docker run` with the volume.
                let output = Command::new("docker")
                    .args([
                        "exec",
                        container_name,
                        "sqlite3",
                        "-separator",
                        "\t",
                        "/app/data/claudear.db",
                        sql,
                    ])
                    .output()
                    .context("docker exec sqlite3")?;

                if output.status.success() {
                    return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
                }

                // Container not running — use `docker run` with the volume
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::debug!(
                    stderr = %stderr,
                    "docker exec failed, falling back to docker run"
                );

                let output = Command::new("docker")
                    .args([
                        "run",
                        "--rm",
                        "-v",
                        &format!("{}:/app/data", volume),
                        "--entrypoint",
                        "sqlite3",
                        "claudear-app:latest",
                        "-separator",
                        "\t",
                        "/app/data/claudear.db",
                        sql,
                    ])
                    .output()
                    .context("docker run sqlite3")?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if !stderr.trim().is_empty() {
                        tracing::warn!(sql, stderr = %stderr, "SQLite query warning");
                    }
                }

                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            }
        }
    }

    /// Count rows matching a SQL COUNT query.
    pub fn count(&self, sql: &str) -> Result<i64> {
        let result = self.query(sql)?;
        result
            .trim()
            .parse::<i64>()
            .context(format!("parse count from '{}' (sql: {})", result, sql))
    }

    /// Execute a SQL statement (INSERT/UPDATE/DELETE).
    pub fn exec(&self, sql: &str) -> Result<()> {
        match &self.access {
            DbAccess::Direct(path) => {
                let conn = rusqlite::Connection::open(path).context("open db for write")?;
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .context("set busy_timeout")?;
                conn.execute_batch(sql).context("execute SQL")?;
                Ok(())
            }
            DbAccess::Docker {
                container_name,
                volume,
            } => {
                // Try `docker exec` first; fall back to `docker run` if container stopped.
                let output = Command::new("docker")
                    .args([
                        "exec",
                        container_name,
                        "sqlite3",
                        "/app/data/claudear.db",
                        sql,
                    ])
                    .output()
                    .context("docker exec sqlite3")?;

                if output.status.success() {
                    return Ok(());
                }

                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::debug!(
                    stderr = %stderr,
                    "docker exec failed for write, falling back to docker run"
                );

                let output = Command::new("docker")
                    .args([
                        "run",
                        "--rm",
                        "-v",
                        &format!("{}:/app/data", volume),
                        "--entrypoint",
                        "sqlite3",
                        "claudear-app:latest",
                        "/app/data/claudear.db",
                        sql,
                    ])
                    .output()
                    .context("docker run sqlite3")?;

                if !output.status.success() {
                    bail!(
                        "SQL exec failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Ok(())
            }
        }
    }

    /// Assert that a count query returns at least `min` rows.
    pub fn assert_min_count(&self, label: &str, sql: &str, min: i64) -> Result<()> {
        let count = self.count(sql)?;
        if count < min {
            bail!(
                "Assertion failed [{}]: expected >= {} rows, got {} (sql: {})",
                label,
                min,
                count,
                sql
            );
        }
        tracing::info!(label, count, min, "DB assertion passed");
        Ok(())
    }
}
