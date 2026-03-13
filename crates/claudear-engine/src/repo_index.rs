//! Repository index building functions that bridge analysis and integrations.
//!
//! These functions use SCM providers (GitHub, GitLab) from the integrations crate
//! to build repository indexes.

use claudear_analysis::repo::{build_repo_index, IndexedRepo, RepoIndex};
use claudear_core::error::Result;
use claudear_integrations::github::GitHubClient;
use claudear_integrations::scm::ScmProvider;
use std::path::Path;

/// Build a repo index from GitHub API.
pub async fn build_repo_index_from_github(
    known_orgs: &[String],
    client: &GitHubClient,
    workspace: &Path,
    use_ssh: bool,
) -> Result<RepoIndex> {
    let mut index = RepoIndex::new();

    for org in known_orgs {
        tracing::info!(org = %org, "Fetching repositories from GitHub API");

        match client.list_org_repos(org).await {
            Ok(repos) => {
                for repo in repos {
                    let clone_url = if use_ssh {
                        &repo.ssh_url
                    } else {
                        &repo.clone_url
                    };
                    let indexed = IndexedRepo::from_api(
                        &repo.full_name,
                        clone_url,
                        &repo.default_branch,
                        workspace,
                    );

                    tracing::debug!(
                        repo = %repo.full_name,
                        path = %indexed.path.display(),
                        "Added API-discovered repository to index"
                    );

                    index.add_repo(indexed);
                }
            }
            Err(e) => {
                tracing::warn!(org = %org, error = %e, "Failed to fetch repos from org");
            }
        }
    }

    tracing::info!(
        count = index.len(),
        "Repository index built from GitHub API"
    );

    Ok(index)
}

/// Build a repo index from GitLab groups using any ScmProvider.
pub async fn build_repo_index_from_gitlab(
    groups: &[String],
    provider: &dyn ScmProvider,
    workspace: &Path,
    use_ssh: bool,
) -> Result<RepoIndex> {
    let mut index = RepoIndex::new();

    for group in groups {
        tracing::info!(group = %group, "Fetching repositories from GitLab API");

        match provider.list_repos(group).await {
            Ok(repos) => {
                for repo in repos {
                    let clone_url = if use_ssh {
                        &repo.ssh_url
                    } else {
                        &repo.clone_url
                    };
                    let indexed = IndexedRepo::from_api(
                        &repo.full_name,
                        clone_url,
                        &repo.default_branch,
                        workspace,
                    );

                    tracing::debug!(
                        repo = %repo.full_name,
                        path = %indexed.path.display(),
                        "Added GitLab-discovered repository to index"
                    );

                    index.add_repo(indexed);
                }
            }
            Err(e) => {
                tracing::warn!(group = %group, error = %e, "Failed to fetch repos from group");
            }
        }
    }

    tracing::info!(
        count = index.len(),
        "Repository index built from GitLab API"
    );

    Ok(index)
}

/// Build a repo index using the best available method (filesystem, GitHub, GitLab).
pub async fn build_repo_index_with_fallback(
    known_orgs: &[String],
    auto_discover_paths: &[String],
    github_client: Option<&GitHubClient>,
    gitlab_provider: Option<&dyn ScmProvider>,
    gitlab_groups: &[String],
    workspace: &Path,
    use_ssh: bool,
) -> Result<RepoIndex> {
    if !auto_discover_paths.is_empty() {
        tracing::info!("Building repo index from local filesystem");
        return build_repo_index(known_orgs, auto_discover_paths);
    }

    if let Some(client) = github_client {
        if client.is_enabled() && !known_orgs.is_empty() {
            tracing::info!(
                "Building repo index from GitHub API (no auto_discover_paths configured)"
            );
            let mut index =
                build_repo_index_from_github(known_orgs, client, workspace, use_ssh).await?;

            if let Some(gl) = gitlab_provider {
                if gl.is_enabled() && !gitlab_groups.is_empty() {
                    let gl_index =
                        build_repo_index_from_gitlab(gitlab_groups, gl, workspace, use_ssh).await?;
                    index.merge(gl_index);
                }
            }

            return Ok(index);
        }
    }

    if let Some(gl) = gitlab_provider {
        if gl.is_enabled() && !gitlab_groups.is_empty() {
            tracing::info!("Building repo index from GitLab API");
            return build_repo_index_from_gitlab(gitlab_groups, gl, workspace, use_ssh).await;
        }
    }

    tracing::info!("No discovery method available, returning empty index");
    Ok(RepoIndex::new())
}
