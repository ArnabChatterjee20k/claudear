//! Scenario 3: Multi-repo cascade.
//!
//! Phase 1 (merge trigger):
//!   Create issue for repo-1 -> detect fix_attempts -> PR on repo-1 -> merge ->
//!   merge-triggered cascade for repo-2 -> PR on repo-2 -> merge.
//!
//! Phase 2 (release trigger):
//!   Create release on repo-1 -> release-triggered cascade for repo-2 ->
//!   PR on repo-2 -> verify.

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
    let workspace = tmp_dir.path().join("workdir");
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

    tracing::info!("S3-B: Starting daemon with merge + release cascade rules");

    // Configure BOTH merge and release cascade rules for the same upstream->downstream pair.
    let mut builder = ConfigBuilder::new(&workspace, &db_path, PORT)
        .claude_timeout(ctx.claude_timeout)
        .skip_permissions()
        .retry(0)
        .regression(false)
        .cascade_rule(ctx.repo, repo2, "merge")
        .cascade_rule(ctx.repo, repo2, "release");

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
            true, // reset volume for fresh start
        )?
    } else {
        let binary = ctx.binary_path()?;
        daemon::start_process(&binary, &config_path, PORT, &log_dir, "s3")?
    };

    daemon::wait_healthy(&handle, PORT, Duration::from_secs(30)).await?;

    let db = E2eDb::new(if ctx.use_docker {
        DbAccess::docker(
            "claudear-e2e-s3",
            handle.volume_name().unwrap_or("claudear-e2e-db-3152"),
        )
    } else {
        DbAccess::direct(&db_path)
    });

    // ─── Phase 1: Merge-triggered cascade ───────────────────────────

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

    // Verify PR exists on the SCM
    {
        let branch = ctx
            .scm
            .get_pr_branch(ctx.repo, pr_number_1)
            .await
            .unwrap_or_default();
        if branch.is_empty() {
            tracing::warn!(pr_number = pr_number_1, "PR branch is empty — repo-1 PR may not exist on SCM");
        } else {
            tracing::info!(pr_number = pr_number_1, branch = %branch, "Repo-1 PR verified on SCM");
        }
    }

    tracing::info!("S3-E: Merging PR on repo-1");
    ctx.scm
        .merge_pr(ctx.repo, pr_number_1)
        .await
        .context("merge PR on repo-1")?;

    tracing::info!("S3-F: Waiting for merge-triggered cascade PR on repo-2");

    // Wait for a cascade fix_attempts row for repo-2
    wait::wait_for(
        "merge-cascade fix_attempts for repo-2",
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

    // Get the merge-cascade PR URL
    let merge_cascade_pr_url = db.query(&format!(
        "SELECT pr_url FROM fix_attempts WHERE pr_url LIKE '%{}%' AND pr_url IS NOT NULL LIMIT 1",
        repo2.replace('\'', "''")
    ))?;
    let merge_cascade_pr_url = merge_cascade_pr_url.trim().to_string();

    if merge_cascade_pr_url.is_empty() {
        bail!("Merge-cascade PR URL is empty for repo-2");
    }

    let merge_cascade_pr_number = ctx
        .scm
        .parse_pr_number(&merge_cascade_pr_url)
        .context("parse merge-cascade PR number")?;

    cleanup.track_pr(repo2, merge_cascade_pr_number);

    if let Ok(branch) = ctx.scm.get_pr_branch(repo2, merge_cascade_pr_number).await {
        cleanup.track_branch(repo2, &branch);
    }

    tracing::info!(
        pr_url = %merge_cascade_pr_url,
        pr_number = merge_cascade_pr_number,
        "Merge-cascade PR created on repo-2"
    );

    // Verify merge-cascade PR exists on the SCM
    {
        let branch = ctx
            .scm
            .get_pr_branch(repo2, merge_cascade_pr_number)
            .await
            .unwrap_or_default();
        if branch.is_empty() {
            tracing::warn!(pr_number = merge_cascade_pr_number, "PR branch is empty — merge-cascade PR may not exist on SCM");
        } else {
            tracing::info!(pr_number = merge_cascade_pr_number, branch = %branch, "Merge-cascade PR verified on SCM");
        }
    }

    // Merge the cascade PR
    tracing::info!("S3-G: Merging merge-cascade PR on repo-2");
    ctx.scm
        .merge_pr(repo2, merge_cascade_pr_number)
        .await
        .context("merge cascade PR")?;

    tracing::info!("S3: Verifying merge-cascade assertions");

    db.assert_min_count(
        "fix_attempts total after merge-cascade",
        "SELECT COUNT(*) FROM fix_attempts",
        2,
    )?;

    db.assert_min_count(
        "cascade_fix_attempts after merge-cascade",
        "SELECT COUNT(*) FROM fix_attempts WHERE cascade_repo IS NOT NULL",
        1,
    )?;

    // ─── Phase 2: Release-triggered cascade ─────────────────────────

    // Record cascade count before creating the release
    let cascade_count_before = db
        .count("SELECT COUNT(*) FROM fix_attempts WHERE cascade_repo IS NOT NULL")
        .unwrap_or(0);

    tracing::info!(
        cascade_count_before,
        "S3-H: Creating release on repo-1 to trigger release cascade"
    );

    let release_tag = format!(
        "e2e-s3-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    ctx.scm
        .create_release(
            ctx.repo,
            &release_tag,
            &format!("E2E S3 Release {}", release_tag),
            "Automated release for E2E cascade test",
        )
        .await
        .context("create release on repo-1")?;

    tracing::info!(tag = %release_tag, "Release created on repo-1");

    tracing::info!("S3-I: Waiting for release-triggered cascade PR on repo-2");

    // Wait for a new cascade attempt (count should increase)
    wait::wait_for(
        "release-cascade fix_attempts for repo-2",
        ctx.timeout(),
        ctx.poll_interval(),
        || async {
            let count = db
                .count("SELECT COUNT(*) FROM fix_attempts WHERE cascade_repo IS NOT NULL")
                .unwrap_or(0);
            Ok(count > cascade_count_before)
        },
    )
    .await?;

    // Get the release-cascade PR URL (the newest one for repo-2)
    let release_cascade_pr_url = db.query(&format!(
        "SELECT pr_url FROM fix_attempts WHERE pr_url LIKE '%{}%' AND pr_url IS NOT NULL ORDER BY id DESC LIMIT 1",
        repo2.replace('\'', "''")
    ))?;
    let release_cascade_pr_url = release_cascade_pr_url.trim().to_string();

    if !release_cascade_pr_url.is_empty() {
        let release_cascade_pr_number = ctx
            .scm
            .parse_pr_number(&release_cascade_pr_url)
            .context("parse release-cascade PR number")?;

        cleanup.track_pr(repo2, release_cascade_pr_number);

        if let Ok(branch) = ctx
            .scm
            .get_pr_branch(repo2, release_cascade_pr_number)
            .await
        {
            cleanup.track_branch(repo2, &branch);
        }

        tracing::info!(
            pr_url = %release_cascade_pr_url,
            pr_number = release_cascade_pr_number,
            "Release-cascade PR created on repo-2"
        );
    }

    tracing::info!("S3: Verifying final assertions");

    // After both phases we expect at least 3 attempts total:
    // 1 original + 1 merge-cascade + 1 release-cascade
    db.assert_min_count(
        "fix_attempts total after both cascades",
        "SELECT COUNT(*) FROM fix_attempts",
        3,
    )?;

    // At least 2 cascade attempts
    db.assert_min_count(
        "cascade_fix_attempts after both cascades",
        "SELECT COUNT(*) FROM fix_attempts WHERE cascade_repo IS NOT NULL",
        2,
    )?;

    daemon::stop(&mut handle);

    tracing::info!("S3: All checkpoints passed (merge + release cascade)!");
    Ok(())
}
