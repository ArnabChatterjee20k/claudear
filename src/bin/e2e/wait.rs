//! Polling/wait-for utilities with progress logging.

use anyhow::{bail, Result};
use std::future::Future;
use std::time::{Duration, Instant};

/// Poll a check function until it returns `Ok(true)` or timeout is reached.
///
/// Logs progress every 15 seconds.
pub async fn wait_for<F, Fut>(
    description: &str,
    timeout: Duration,
    interval: Duration,
    check: F,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    let start = Instant::now();
    let mut last_log = Instant::now();
    let log_interval = Duration::from_secs(15);

    loop {
        match check().await {
            Ok(true) => {
                tracing::info!(
                    description,
                    elapsed_secs = start.elapsed().as_secs(),
                    "Condition met"
                );
                return Ok(());
            }
            Ok(false) => {}
            Err(e) => {
                tracing::debug!(description, error = %e, "Check returned error");
            }
        }

        if start.elapsed() > timeout {
            bail!(
                "Timed out waiting for '{}' after {:?}",
                description,
                timeout
            );
        }

        if last_log.elapsed() > log_interval {
            tracing::info!(
                description,
                elapsed_secs = start.elapsed().as_secs(),
                timeout_secs = timeout.as_secs(),
                "Still waiting..."
            );
            last_log = Instant::now();
        }

        tokio::time::sleep(interval).await;
    }
}

/// Wait for a fix_attempts row matching the issue_id to have a non-null pr_url.
pub async fn wait_for_pr(
    db: &crate::db::E2eDb,
    issue_id: &str,
    timeout: Duration,
) -> Result<String> {
    let start = Instant::now();
    let interval = Duration::from_secs(5);

    loop {
        let sql = format!(
            "SELECT pr_url FROM fix_attempts WHERE issue_id = '{}' AND pr_url IS NOT NULL AND pr_url != '' LIMIT 1",
            issue_id.replace('\'', "''")
        );

        if let Ok(result) = db.query(&sql) {
            let trimmed = result.trim();
            if !trimmed.is_empty() {
                tracing::info!(
                    issue_id,
                    pr_url = trimmed,
                    elapsed_secs = start.elapsed().as_secs(),
                    "PR created"
                );
                return Ok(trimmed.to_string());
            }
        }

        if start.elapsed() > timeout {
            // Log fix_attempts state for debugging
            let debug_sql = format!(
                "SELECT id, status, error_message FROM fix_attempts WHERE issue_id = '{}'",
                issue_id.replace('\'', "''")
            );
            let debug_info = db.query(&debug_sql).unwrap_or_default();
            bail!(
                "Timed out waiting for PR for issue '{}' after {:?}. fix_attempts state: {}",
                issue_id,
                timeout,
                debug_info
            );
        }

        tokio::time::sleep(interval).await;
    }
}

/// Wait for fix_attempts to detect the issue (row exists with any status).
pub async fn wait_for_detection(
    db: &crate::db::E2eDb,
    issue_id: &str,
    timeout: Duration,
) -> Result<()> {
    wait_for(
        &format!("fix_attempts for {}", issue_id),
        timeout,
        Duration::from_secs(5),
        || async {
            let sql = format!(
                "SELECT COUNT(*) FROM fix_attempts WHERE issue_id = '{}'",
                issue_id.replace('\'', "''")
            );
            match db.count(&sql) {
                Ok(n) => Ok(n > 0),
                Err(e) => {
                    tracing::debug!(error = %e, "DB query failed (will retry)");
                    Ok(false)
                }
            }
        },
    )
    .await
}

/// Wait for a specific substring to appear in a log file.
pub async fn wait_for_log_message(
    log_path: &std::path::Path,
    needle: &str,
    timeout: Duration,
) -> Result<()> {
    let needle = needle.to_string();
    let log_path = log_path.to_path_buf();
    wait_for(
        &format!("log message '{}'", needle),
        timeout,
        Duration::from_secs(1),
        || {
            let needle = needle.clone();
            let log_path = log_path.clone();
            async move {
                let content = tokio::fs::read_to_string(&log_path)
                    .await
                    .unwrap_or_default();
                Ok(content.contains(&needle))
            }
        },
    )
    .await
}
