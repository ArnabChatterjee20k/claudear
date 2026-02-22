//! Scenario 3: Multi-repo cascade.
//!
//! Create issue for repo-1 -> detect fix_attempts -> PR on repo-1 -> merge ->
//! cascade trigger for repo-2 -> PR on repo-2 -> merge -> assert both repos.

use super::ScenarioContext;
use crate::cleanup::CleanupTracker;
use crate::config::ConfigBuilder;
use crate::db::{DbAccess, E2eDb};
use crate::{daemon, wait};
use anyhow::{bail, Context, Result};
use std::time::Duration;

const PORT: u16 = 3152;

pub async fn run(ctx: &ScenarioContext<'_>) -> Result<()> {
    let mut cleanup = CleanupTracker::new(ctx.scm.clone(), ctx.source.clone());
    let result = run_inner(ctx, &mut cleanup).await;
    cleanup.cleanup().await;
    result
}

async fn run_inner(ctx: &ScenarioContext<'_>, cleanup: &mut CleanupTracker) -> Result<()> {
    let repo2 = ctx.repo2.context("S3 requires a second repo (repo2)")?;

    let tmp_dir = tempfile::tempdir().context("create temp dir")?;
    let work_dir = tmp_dir.path().join("workdir");
    let repos_dir = tmp_dir.path().join("repos");
    let log_dir = tmp_dir.path().join("logs");
    let db_path = tmp_dir.path().join("s3.db");

    std::fs::create_dir_all(&repos_dir).context("create repos dir")?;

    // Clone both repos locally so the daemon's inferrer can discover them
    tracing::info!("Cloning repos for daemon discovery");
    ctx.clone_repo(ctx.repo, &repos_dir)?;
    ctx.clone_repo(repo2, &repos_dir)?;

    tracing::info!("S3-A: Creating test issue for cascade");

    let issue = ctx
        .source
        .create_issue(
            "[E2E-S3] Cascade upstream fix",
            "This issue was created by the claudear E2E cascade test.\n\nPlease add a comment to README.md.",
            &["claudear-e2e".to_string()],
        )
        .await
        .context("create cascade issue")?;

    let issue_id = issue.id.clone();
    cleanup.track_issue(&issue_id);
    tracing::info!(issue_id = %issue_id, short_id = %issue.short_id, "Created cascade issue");

    tracing::info!("S3-B: Starting daemon with cascade");

    let mut builder = ConfigBuilder::new(&work_dir, &db_path, PORT)
        .claude_timeout(ctx.claude_timeout)
        .skip_permissions()
        .retry(0)
        .regression(false)
        .cascade_rule(ctx.repo, repo2, "merge");

    if ctx.use_docker {
        builder = builder
            .docker_paths()
            .auto_discover_paths(vec!["/app/repos".to_string()]);
    } else {
        builder = builder.auto_discover_paths(vec![repos_dir.to_string_lossy().to_string()]);
    }

    // Configure SCM backend
    match ctx.scm_name {
        "github" => {
            let token =
                std::env::var("CLAUDEAR_E2E_GITHUB_TOKEN").context("GITHUB_TOKEN required")?;
            builder = builder.github(&token, ctx.repo);
        }
        "gitlab" => {
            let token =
                std::env::var("CLAUDEAR_E2E_GITLAB_TOKEN").context("GITLAB_TOKEN required")?;
            let base_url = std::env::var("CLAUDEAR_E2E_GITLAB_URL")
                .unwrap_or_else(|_| "https://gitlab.com".to_string());
            let group = ctx.repo.split('/').next().unwrap_or(ctx.repo);
            builder = builder.gitlab(&token, &base_url, group);
        }
        _ => {}
    }

    // Configure issue source
    match ctx.source_name {
        "linear" => {
            let api_key = std::env::var("CLAUDEAR_E2E_LINEAR_API_KEY").context("LINEAR_API_KEY")?;
            let team_id = std::env::var("CLAUDEAR_E2E_LINEAR_TEAM_ID").context("LINEAR_TEAM_ID")?;
            builder = builder.linear(&api_key, &team_id);
        }
        "jira" => {
            let base_url = std::env::var("CLAUDEAR_E2E_JIRA_URL").context("JIRA_URL")?;
            let email = std::env::var("CLAUDEAR_E2E_JIRA_EMAIL").context("JIRA_EMAIL")?;
            let token = std::env::var("CLAUDEAR_E2E_JIRA_API_TOKEN").context("JIRA_TOKEN")?;
            let key = std::env::var("CLAUDEAR_E2E_JIRA_PROJECT_KEY").context("JIRA_KEY")?;
            builder = builder.jira(&base_url, &email, &token, &key);
        }
        _ => {}
    }

    let config_path = builder.write_to(tmp_dir.path(), "s3")?;

    let mut handle = if ctx.use_docker {
        daemon::start_docker(
            ctx.docker_image,
            &config_path,
            PORT,
            &log_dir,
            "s3",
            None,
            Some(&repos_dir),
        )?
    } else {
        let binary = ctx.binary_path()?;
        daemon::start_process(&binary, &config_path, PORT, &log_dir, "s3")?
    };

    daemon::wait_healthy(PORT, Duration::from_secs(30)).await?;

    let db = E2eDb::new(if ctx.use_docker {
        DbAccess::docker(
            "claudear-e2e-s3",
            handle.volume_name().unwrap_or("claudear-e2e-db-3152"),
        )
    } else {
        DbAccess::direct(&db_path)
    });

    tracing::info!("S3-C: Waiting for issue detection");
    wait::wait_for_detection(&db, &issue_id, ctx.timeout()).await?;

    tracing::info!("S3-D: Waiting for PR on repo-1");
    let pr_url_1 = wait::wait_for_pr(&db, &issue_id, ctx.timeout()).await?;

    let pr_number_1 = ctx
        .scm
        .parse_pr_number(&pr_url_1)
        .context("parse PR number for repo-1")?;

    cleanup.track_pr(ctx.repo, pr_number_1);

    if let Ok(branch) = ctx.scm.get_pr_branch(ctx.repo, pr_number_1).await {
        cleanup.track_branch(ctx.repo, &branch);
    }

    tracing::info!(pr_url = %pr_url_1, pr_number = pr_number_1, "PR created on repo-1");

    tracing::info!("S3-E: Merging PR on repo-1");
    ctx.scm
        .merge_pr(ctx.repo, pr_number_1)
        .await
        .context("merge PR on repo-1")?;

    tracing::info!("S3-F: Waiting for cascade PR on repo-2");

    // Wait for a cascade fix_attempts row for repo-2 (cascade_repo IS NOT NULL)
    wait::wait_for(
        "fix_attempts for repo-2",
        ctx.timeout(),
        ctx.poll_interval(),
        || async {
            let sql = format!(
                "SELECT COUNT(*) FROM fix_attempts WHERE pr_url LIKE '%{}%' AND pr_url IS NOT NULL AND pr_url != ''",
                repo2.replace('\'', "''")
            );
            Ok(db.count(&sql).unwrap_or(0) > 0)
        },
    )
    .await?;

    // Get the cascade PR URL
    let cascade_pr_url = db.query(&format!(
        "SELECT pr_url FROM fix_attempts WHERE pr_url LIKE '%{}%' AND pr_url IS NOT NULL LIMIT 1",
        repo2.replace('\'', "''")
    ))?;
    let cascade_pr_url = cascade_pr_url.trim().to_string();

    if cascade_pr_url.is_empty() {
        bail!("Cascade PR URL is empty for repo-2");
    }

    let cascade_pr_number = ctx
        .scm
        .parse_pr_number(&cascade_pr_url)
        .context("parse cascade PR number")?;

    cleanup.track_pr(repo2, cascade_pr_number);

    if let Ok(branch) = ctx.scm.get_pr_branch(repo2, cascade_pr_number).await {
        cleanup.track_branch(repo2, &branch);
    }

    tracing::info!(
        pr_url = %cascade_pr_url,
        pr_number = cascade_pr_number,
        "Cascade PR created on repo-2"
    );

    // Merge the cascade PR
    tracing::info!("S3-F: Merging cascade PR on repo-2");
    ctx.scm
        .merge_pr(repo2, cascade_pr_number)
        .await
        .context("merge cascade PR")?;

    tracing::info!("S3: Verifying assertions");

    db.assert_min_count("fix_attempts total", "SELECT COUNT(*) FROM fix_attempts", 2)?;

    db.assert_min_count(
        "cascade_fix_attempts",
        "SELECT COUNT(*) FROM fix_attempts WHERE cascade_repo IS NOT NULL",
        1,
    )?;

    daemon::stop(&mut handle);

    tracing::info!("S3: All checkpoints passed!");
    Ok(())
}
