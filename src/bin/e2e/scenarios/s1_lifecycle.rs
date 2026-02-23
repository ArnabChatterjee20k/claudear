//! Scenario 1: Full lifecycle.
//!
//! Linear/Jira source -> daemon poll -> PR -> review comments ->
//! request_changes review -> approve -> merge -> regression watch -> resolve.

use super::ScenarioContext;
use crate::cleanup::CleanupTracker;
use crate::config::ConfigBuilder;
use crate::db::{DbAccess, E2eDb};
use crate::{daemon, wait};
use anyhow::{Context, Result};
use claudear::scm::PostReviewAction;
use std::time::Duration;

const PORT: u16 = 3150;

pub async fn run(ctx: &ScenarioContext<'_>) -> Result<()> {
    let mut cleanup = CleanupTracker::new(ctx.scm.clone(), ctx.source.clone());
    let result = run_inner(ctx, &mut cleanup).await;

    // Always clean up regardless of outcome
    cleanup.cleanup().await;

    result
}

async fn run_inner(ctx: &ScenarioContext<'_>, cleanup: &mut CleanupTracker) -> Result<()> {
    let tmp_dir = tempfile::tempdir().context("create temp dir")?;
    let workspace = tmp_dir.path().join("workdir");
    let repos_dir = tmp_dir.path().join("repos");
    let log_dir = tmp_dir.path().join("logs");
    let db_path = tmp_dir.path().join("s1.db");

    std::fs::create_dir_all(&repos_dir).context("create repos dir")?;

    // Clone the repo locally so the daemon's inferrer can discover it
    tracing::info!("Cloning repo for daemon discovery");
    ctx.clone_repo(ctx.repo, &repos_dir)?;

    // Discord/Slack use cursor-based polling: the first poll seeds the cursor
    // at the latest message and returns nothing. We must start the daemon and
    // wait for the cursor to seed BEFORE posting the issue message, otherwise
    // the message will be permanently skipped.
    let cursor_based_source = matches!(ctx.source_name, "discord" | "slack");

    let mut builder = ConfigBuilder::new(&workspace, &db_path, PORT)
        .claude_timeout(ctx.claude_timeout)
        .skip_permissions()
        .retry(1)
        .regression(true);

    // Docker: override paths to container layout, auto-discover /app/repos
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
        "discord" => {
            let bot_token =
                std::env::var("CLAUDEAR_E2E_DISCORD_BOT_TOKEN").context("DISCORD_BOT_TOKEN")?;
            let channel_id =
                std::env::var("CLAUDEAR_E2E_DISCORD_CHANNEL_ID").context("DISCORD_CHANNEL_ID")?;
            builder = builder.discord_source(&bot_token, &channel_id);
        }
        "slack" => {
            let bot_token =
                std::env::var("CLAUDEAR_E2E_SLACK_BOT_TOKEN").context("SLACK_BOT_TOKEN")?;
            let channel_id =
                std::env::var("CLAUDEAR_E2E_SLACK_CHANNEL_ID").context("SLACK_CHANNEL_ID")?;
            builder = builder.slack_source(&bot_token, &channel_id);
        }
        _ => {}
    }

    // Configure notifier for the ask backend
    match ctx.ask_name {
        "slack" => {
            // Slack: bot_token + channel_id for proper bot identity
            if let (Ok(bot_token), Ok(channel_id)) = (
                std::env::var("CLAUDEAR_E2E_SLACK_BOT_TOKEN"),
                std::env::var("CLAUDEAR_E2E_SLACK_CHANNEL_ID"),
            ) {
                builder = builder.slack_notifier(&bot_token, &channel_id);
            }
        }
        "discord" => {
            // Discord: webhook_url for rich embed notifications
            if let Ok(webhook_url) = std::env::var("CLAUDEAR_E2E_DISCORD_WEBHOOK_URL") {
                builder = builder.discord_notifier(&webhook_url);
            }
        }
        _ => {}
    }

    let config_path = builder.write_to(tmp_dir.path(), "s1")?;

    // For label/state-based sources (Linear, Jira): create issue first, then start daemon.
    // For cursor-based sources (Discord, Slack): start daemon first, wait for cursor seed,
    // then create issue.
    let issue_id;
    let mut handle;
    let db;

    if cursor_based_source {
        tracing::info!(
            "S1-A: Starting daemon (cursor-based source — daemon must start before issue creation)"
        );

        handle = if ctx.use_docker {
            daemon::start_docker(
                ctx.docker_image,
                &config_path,
                PORT,
                &log_dir,
                "s1",
                None,
                Some(&repos_dir),
                true, // reset volume for fresh start
            )?
        } else {
            let binary = ctx.binary_path()?;
            daemon::start_process(&binary, &config_path, PORT, &log_dir, "s1")?
        };

        daemon::wait_healthy(&handle, PORT, Duration::from_secs(30)).await?;

        db = E2eDb::new(if ctx.use_docker {
            DbAccess::docker(
                "claudear-e2e-s1",
                handle.volume_name().unwrap_or("claudear-e2e-db-3150"),
            )
        } else {
            DbAccess::direct(&db_path)
        });

        // Wait for the source cursor to be seeded
        tracing::info!("S1-A: Waiting for cursor seed");
        wait::wait_for_log_message(handle.log_path(), "seeded cursor", Duration::from_secs(30))
            .await
            .context("wait for cursor seed in daemon log")?;

        tracing::info!("S1-B: Creating test issue (after cursor seed)");

        let issue = ctx
            .source
            .create_issue(
                "[E2E-S1] Auto-generated lifecycle test",
                "This issue was created by the claudear E2E smoke test.\n\nPlease add a comment to README.md.",
                &["claudear-e2e".to_string(), "bug".to_string()],
            )
            .await
            .context("create test issue")?;

        issue_id = issue.id.clone();
        cleanup.track_issue(&issue_id);
        tracing::info!(issue_id = %issue_id, short_id = %issue.short_id, "Created issue");
    } else {
        tracing::info!("S1-A: Creating test issue");

        let issue = ctx
            .source
            .create_issue(
                "[E2E-S1] Auto-generated lifecycle test",
                "This issue was created by the claudear E2E smoke test.\n\nPlease add a comment to README.md.",
                &["claudear-e2e".to_string(), "bug".to_string()],
            )
            .await
            .context("create test issue")?;

        issue_id = issue.id.clone();
        cleanup.track_issue(&issue_id);
        tracing::info!(issue_id = %issue_id, short_id = %issue.short_id, "Created issue");

        tracing::info!("S1-B: Starting daemon");

        handle = if ctx.use_docker {
            daemon::start_docker(
                ctx.docker_image,
                &config_path,
                PORT,
                &log_dir,
                "s1",
                None,
                Some(&repos_dir),
                true, // reset volume for fresh start
            )?
        } else {
            let binary = ctx.binary_path()?;
            daemon::start_process(&binary, &config_path, PORT, &log_dir, "s1")?
        };

        daemon::wait_healthy(&handle, PORT, Duration::from_secs(30)).await?;

        db = E2eDb::new(if ctx.use_docker {
            DbAccess::docker(
                "claudear-e2e-s1",
                handle.volume_name().unwrap_or("claudear-e2e-db-3150"),
            )
        } else {
            DbAccess::direct(&db_path)
        });
    }

    tracing::info!("S1-C: Waiting for issue detection");
    wait::wait_for_detection(&db, &issue_id, ctx.timeout()).await?;

    // For sources without native label support (Discord, Slack), inject "bug"
    // label into fix_attempts so is_bug() returns true (needed for regression watch).
    if matches!(ctx.source_name, "discord" | "slack") {
        let sql = format!(
            "UPDATE fix_attempts SET issue_labels = '{}' WHERE issue_id = '{}'",
            r#"["bug","claudear-e2e"]"#,
            issue_id.replace('\'', "''")
        );
        db.exec(&sql)?;
        tracing::info!("Injected bug label for non-label source");
    }

    tracing::info!("S1-D: Waiting for PR creation");
    let pr_url = wait::wait_for_pr(&db, &issue_id, ctx.timeout()).await?;

    let pr_number = ctx
        .scm
        .parse_pr_number(&pr_url)
        .context("parse PR number from URL")?;

    cleanup.track_pr(ctx.repo, pr_number);

    // Get the PR branch for cleanup
    if let Ok(branch) = ctx.scm.get_pr_branch(ctx.repo, pr_number).await {
        cleanup.track_branch(ctx.repo, &branch);
    }

    tracing::info!(pr_url = %pr_url, pr_number, "PR created");

    tracing::info!("S1-E: Posting review comment");

    if let Some(reviewer_token) = ctx.reviewer_token {
        // Build a reviewer SCM client
        let reviewer_scm = build_reviewer_scm(ctx.scm_name, reviewer_token)?;

        reviewer_scm
            .post_review(
                ctx.repo,
                pr_number,
                PostReviewAction::Comment,
                "E2E test: Please also add the current date to the comment.",
            )
            .await
            .context("post review comment")?;

        tracing::info!("S1-F: Posting request_changes review");
        tokio::time::sleep(Duration::from_secs(5)).await;

        match reviewer_scm
            .post_review(
                ctx.repo,
                pr_number,
                PostReviewAction::RequestChanges,
                "E2E test: Changes requested - please include the year.",
            )
            .await
        {
            Ok(()) => {
                // Wait for the agent to process the review feedback
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Err(e) => {
                // GitHub returns 422 when requesting changes on your own PR
                tracing::warn!(error = %e, "request_changes failed (reviewer may be PR author), skipping");
            }
        }

        tracing::info!("S1-G: Approving PR");
        match reviewer_scm
            .post_review(ctx.repo, pr_number, PostReviewAction::Approve, "LGTM")
            .await
        {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(error = %e, "approve failed (reviewer may be PR author), skipping");
            }
        }
    }

    tracing::info!("S1-H: Merging PR");
    ctx.scm
        .merge_pr(ctx.repo, pr_number)
        .await
        .context("merge PR")?;

    tracing::info!("S1-I: Waiting for regression watch");
    wait::wait_for(
        "regression_watches row",
        ctx.timeout(),
        ctx.poll_interval(),
        || async {
            let sql = format!(
                "SELECT COUNT(*) FROM regression_watches WHERE issue_id = '{}'",
                issue_id.replace('\'', "''")
            );
            Ok(db.count(&sql).unwrap_or(0) > 0)
        },
    )
    .await?;

    tracing::info!("S1-J: Verifying learning tables");
    db.assert_min_count(
        "fix_attempts",
        &format!(
            "SELECT COUNT(*) FROM fix_attempts WHERE issue_id = '{}'",
            issue_id.replace('\'', "''")
        ),
        1,
    )?;

    db.assert_min_count(
        "claude_executions",
        "SELECT COUNT(*) FROM claude_executions",
        1,
    )?;

    // Stop daemon
    daemon::stop(&mut handle);

    tracing::info!("S1: All checkpoints passed!");
    Ok(())
}

fn build_reviewer_scm(
    scm_name: &str,
    reviewer_token: &str,
) -> Result<std::sync::Arc<dyn claudear::ScmProvider>> {
    match scm_name {
        "github" => {
            let config = claudear::config::GitHubConfig {
                token: Some(reviewer_token.into()),
                review_trigger: "@claudear".to_string(),
                ..Default::default()
            };
            Ok(std::sync::Arc::new(claudear::GitHubClient::new(config)))
        }
        "gitlab" => {
            let base_url = std::env::var("CLAUDEAR_E2E_GITLAB_URL")
                .unwrap_or_else(|_| "https://gitlab.com".to_string());
            let config = claudear::config::GitLabConfig {
                enabled: true,
                token: Some(reviewer_token.into()),
                base_url,
                review_trigger: "@claudear".to_string(),
                ..Default::default()
            };
            Ok(std::sync::Arc::new(claudear::GitLabClient::new(config)))
        }
        other => anyhow::bail!("Unknown SCM for reviewer: {}", other),
    }
}
