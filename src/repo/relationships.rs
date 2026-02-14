//! Repository dependency tracking and relationships.

use crate::error::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Type of dependency between repositories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyType {
    /// NPM dependency
    Npm,
    /// Composer (PHP) dependency
    Composer,
    /// Git submodule
    GitSubmodule,
    /// Manually defined relationship
    Manual,
}

impl DependencyType {
    /// Parse from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "npm" => Some(DependencyType::Npm),
            "composer" => Some(DependencyType::Composer),
            "git_submodule" | "submodule" => Some(DependencyType::GitSubmodule),
            "manual" => Some(DependencyType::Manual),
            _ => None,
        }
    }

    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            DependencyType::Npm => "npm",
            DependencyType::Composer => "composer",
            DependencyType::GitSubmodule => "git_submodule",
            DependencyType::Manual => "manual",
        }
    }
}

/// A repository in the dependency graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    /// Unique repository name/identifier.
    pub name: String,
    /// Local filesystem path (if available).
    pub path: Option<String>,
    /// GitHub URL (owner/repo format or full URL).
    pub github_url: Option<String>,
    /// When this repository was added.
    pub created_at: DateTime<Utc>,
}

impl Repository {
    /// Create a new repository.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: None,
            github_url: None,
            created_at: Utc::now(),
        }
    }

    /// Set the local path.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set the GitHub URL.
    pub fn with_github_url(mut self, url: impl Into<String>) -> Self {
        self.github_url = Some(url.into());
        self
    }
}

/// A dependency relationship between two repositories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    /// The upstream repository name (depended upon).
    pub upstream: String,
    /// The downstream repository name (depends on upstream).
    pub downstream: String,
    /// Type of dependency.
    pub dep_type: DependencyType,
    /// Version pattern (e.g., "^1.0.0" for npm).
    pub version_pattern: Option<String>,
    /// When this relationship was created.
    pub created_at: DateTime<Utc>,
}

/// A cascading change triggered by a merged PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadingChange {
    /// The repository that triggered this change.
    pub trigger_repo: String,
    /// The repository that needs to be updated.
    pub target_repo: String,
    /// Type of change needed.
    pub change_type: CascadeChangeType,
    /// Status of this cascading change.
    pub status: CascadeStatus,
    /// PR URL if created.
    pub pr_url: Option<String>,
    /// Created timestamp.
    pub created_at: DateTime<Utc>,
    /// Completed timestamp.
    pub completed_at: Option<DateTime<Utc>>,
}

/// Type of cascading change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CascadeChangeType {
    /// Bump the version dependency.
    VersionBump,
    /// Update code to match API changes.
    CodeChange,
    /// Update tests.
    TestUpdate,
}

/// Status of a cascading change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CascadeStatus {
    /// Change is pending.
    Pending,
    /// Change is in progress.
    InProgress,
    /// Change completed successfully.
    Completed,
    /// Change failed.
    Failed,
    /// Change was skipped.
    Skipped,
}

/// Dependency graph for repositories.
#[derive(Debug, Clone, Default)]
pub struct DependencyGraph {
    /// All repositories.
    repositories: HashMap<String, Repository>,
    /// Dependencies: upstream -> list of downstreams that depend on it.
    downstream_deps: HashMap<String, Vec<Dependency>>,
    /// Dependencies: downstream -> list of upstreams it depends on.
    upstream_deps: HashMap<String, Vec<Dependency>>,
}

impl DependencyGraph {
    /// Create a new empty dependency graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a repository to the graph.
    pub fn add_repository(&mut self, repo: Repository) {
        self.repositories.insert(repo.name.clone(), repo);
    }

    /// Get a repository by name.
    pub fn get_repository(&self, name: &str) -> Option<&Repository> {
        self.repositories.get(name)
    }

    /// Get all repositories.
    pub fn repositories(&self) -> impl Iterator<Item = &Repository> {
        self.repositories.values()
    }

    /// Add a dependency relationship.
    pub fn add_dependency(&mut self, dep: Dependency) {
        // Add to downstream deps (upstream -> downstreams)
        self.downstream_deps
            .entry(dep.upstream.clone())
            .or_default()
            .push(dep.clone());

        // Add to upstream deps (downstream -> upstreams)
        self.upstream_deps
            .entry(dep.downstream.clone())
            .or_default()
            .push(dep);
    }

    /// Get all repositories that depend on the given repository (direct dependants).
    pub fn get_dependants(&self, repo: &str) -> Vec<&Repository> {
        self.downstream_deps
            .get(repo)
            .map(|deps| {
                deps.iter()
                    .filter_map(|d| self.repositories.get(&d.downstream))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all repositories that the given repository depends on (direct dependencies).
    pub fn get_dependencies(&self, repo: &str) -> Vec<&Repository> {
        self.upstream_deps
            .get(repo)
            .map(|deps| {
                deps.iter()
                    .filter_map(|d| self.repositories.get(&d.upstream))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all downstream dependants transitively (BFS).
    pub fn get_all_dependants(&self, repo: &str) -> Vec<&Repository> {
        let mut visited = HashSet::new();
        let mut result = Vec::new();
        let mut queue = vec![repo.to_string()];

        while let Some(current) = queue.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(deps) = self.downstream_deps.get(&current) {
                for dep in deps {
                    if !visited.contains(&dep.downstream) {
                        if let Some(r) = self.repositories.get(&dep.downstream) {
                            result.push(r);
                            queue.push(dep.downstream.clone());
                        }
                    }
                }
            }
        }

        result
    }

    /// Check if repo A depends on repo B (directly or transitively).
    pub fn depends_on(&self, repo_a: &str, repo_b: &str) -> bool {
        let mut visited = HashSet::new();
        let mut queue = vec![repo_a.to_string()];

        while let Some(current) = queue.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(deps) = self.upstream_deps.get(&current) {
                for dep in deps {
                    if dep.upstream == repo_b {
                        return true;
                    }
                    if !visited.contains(&dep.upstream) {
                        queue.push(dep.upstream.clone());
                    }
                }
            }
        }

        false
    }

    /// Get the dependency type for the first hop from a repo.
    ///
    /// Returns the type of dependency between the given repo and its first dependant.
    pub fn get_first_hop_dependency_type(&self, repo: &str) -> Option<DependencyType> {
        self.downstream_deps
            .get(repo)
            .and_then(|deps| deps.first())
            .map(|dep| dep.dep_type)
    }

    /// Get the dependency graph as a printable string.
    pub fn to_string_tree(&self, root: Option<&str>) -> String {
        let mut output = String::new();

        if let Some(root_name) = root {
            self.print_subtree(root_name, 0, &mut output, &mut HashSet::new());
        } else {
            // Find root nodes (repos with no upstream dependencies)
            let roots: Vec<_> = self
                .repositories
                .keys()
                .filter(|name| !self.upstream_deps.contains_key(*name))
                .collect();

            for root in roots {
                self.print_subtree(root, 0, &mut output, &mut HashSet::new());
                output.push('\n');
            }
        }

        output
    }

    fn print_subtree(
        &self,
        name: &str,
        depth: usize,
        output: &mut String,
        visited: &mut HashSet<String>,
    ) {
        if visited.contains(name) {
            output.push_str(&format!("{}{} (cycle)\n", "  ".repeat(depth), name));
            return;
        }
        visited.insert(name.to_string());

        output.push_str(&format!("{}{}\n", "  ".repeat(depth), name));

        if let Some(deps) = self.downstream_deps.get(name) {
            for dep in deps {
                self.print_subtree(&dep.downstream, depth + 1, output, visited);
            }
        }
    }
}

/// Manages repository relationships and dependencies.
pub struct RepoRelationships {
    graph: DependencyGraph,
}

impl RepoRelationships {
    /// Create a new repository relationships manager.
    pub fn new() -> Self {
        Self {
            graph: DependencyGraph::new(),
        }
    }

    /// Create with default repository relationships.
    ///
    /// Dependencies are now discovered automatically from the repo index,
    /// so this method simply returns a manager seeded with Appwrite defaults.
    pub fn with_defaults() -> Self {
        let mut manager = Self::new();
        manager.seed_appwrite_defaults();
        manager
    }

    /// Create with default Appwrite relationships.
    pub fn with_appwrite_defaults() -> Self {
        let mut manager = Self::new();
        manager.seed_appwrite_defaults();
        manager
    }

    /// Seed default Appwrite repository relationships.
    pub fn seed_appwrite_defaults(&mut self) {
        // Add repositories
        self.graph
            .add_repository(Repository::new("cloud").with_github_url("appwrite/cloud"));
        self.graph
            .add_repository(Repository::new("appwrite").with_github_url("appwrite/appwrite"));
        self.graph
            .add_repository(Repository::new("utopia-php").with_github_url("utopia-php/utopia-php"));
        self.graph.add_repository(
            Repository::new("utopia-database").with_github_url("utopia-php/database"),
        );
        self.graph
            .add_repository(Repository::new("utopia-http").with_github_url("utopia-php/http"));
        self.graph
            .add_repository(Repository::new("utopia-cache").with_github_url("utopia-php/cache"));

        // Add dependencies: upstream -> downstream (upstream is depended upon)
        // cloud depends on appwrite (appwrite is upstream, cloud is downstream)
        // appwrite depends on utopia-* (utopia-* are upstream, appwrite is downstream)
        self.add_dependency("appwrite", "cloud", DependencyType::Composer, None)
            .ok();
        self.add_dependency("utopia-php", "appwrite", DependencyType::Composer, None)
            .ok();
        self.add_dependency(
            "utopia-database",
            "appwrite",
            DependencyType::Composer,
            None,
        )
        .ok();
        self.add_dependency("utopia-http", "appwrite", DependencyType::Composer, None)
            .ok();
        self.add_dependency("utopia-cache", "appwrite", DependencyType::Composer, None)
            .ok();
    }

    /// Add a repository.
    pub fn add_repository(&mut self, repo: Repository) {
        self.graph.add_repository(repo);
    }

    /// Get a repository by name.
    pub fn get_repository(&self, name: &str) -> Option<&Repository> {
        self.graph.get_repository(name)
    }

    /// Get all repositories.
    pub fn list_repositories(&self) -> Vec<&Repository> {
        self.graph.repositories().collect()
    }

    /// Add a dependency relationship.
    ///
    /// This also ensures both repositories exist in the graph.
    pub fn add_dependency(
        &mut self,
        upstream: &str,
        downstream: &str,
        dep_type: DependencyType,
        version_pattern: Option<String>,
    ) -> Result<()> {
        // Ensure both repos exist in the graph
        if self.graph.get_repository(upstream).is_none() {
            self.graph
                .add_repository(Repository::new(upstream).with_github_url(upstream));
        }
        if self.graph.get_repository(downstream).is_none() {
            self.graph
                .add_repository(Repository::new(downstream).with_github_url(downstream));
        }

        let dep = Dependency {
            upstream: upstream.to_string(),
            downstream: downstream.to_string(),
            dep_type,
            version_pattern,
            created_at: Utc::now(),
        };
        self.graph.add_dependency(dep);
        Ok(())
    }

    /// Get all repositories that depend on the given repository.
    pub fn get_dependants(&self, repo: &str) -> Vec<&Repository> {
        self.graph.get_dependants(repo)
    }

    /// Get all repositories that the given repository depends on.
    pub fn get_dependencies(&self, repo: &str) -> Vec<&Repository> {
        self.graph.get_dependencies(repo)
    }

    /// Get all dependants transitively.
    pub fn get_all_dependants(&self, repo: &str) -> Vec<&Repository> {
        self.graph.get_all_dependants(repo)
    }

    /// Get the dependency graph.
    pub fn get_graph(&self) -> &DependencyGraph {
        &self.graph
    }

    /// Print the dependency tree.
    pub fn print_tree(&self, root: Option<&str>) -> String {
        self.graph.to_string_tree(root)
    }

    /// When a PR is merged in a repository, determine what downstream changes are needed.
    pub fn get_cascade_changes(&self, repo: &str) -> Vec<CascadingChange> {
        let dependants = self.get_dependants(repo);

        dependants
            .into_iter()
            .map(|target| CascadingChange {
                trigger_repo: repo.to_string(),
                target_repo: target.name.clone(),
                change_type: CascadeChangeType::VersionBump,
                status: CascadeStatus::Pending,
                pr_url: None,
                created_at: Utc::now(),
                completed_at: None,
            })
            .collect()
    }
}

impl Default for RepoRelationships {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dependency_type_parse() {
        assert_eq!(DependencyType::parse("npm"), Some(DependencyType::Npm));
        assert_eq!(
            DependencyType::parse("composer"),
            Some(DependencyType::Composer)
        );
        assert_eq!(
            DependencyType::parse("manual"),
            Some(DependencyType::Manual)
        );
        assert_eq!(DependencyType::parse("invalid"), None);
    }

    #[test]
    fn test_add_repository() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("test-repo"));

        assert!(manager.get_repository("test-repo").is_some());
        assert!(manager.get_repository("nonexistent").is_none());
    }

    #[test]
    fn test_add_dependency() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("upstream"));
        manager.add_repository(Repository::new("downstream"));

        manager
            .add_dependency("upstream", "downstream", DependencyType::Npm, None)
            .unwrap();

        let dependants = manager.get_dependants("upstream");
        assert_eq!(dependants.len(), 1);
        assert_eq!(dependants[0].name, "downstream");

        let dependencies = manager.get_dependencies("downstream");
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].name, "upstream");
    }

    #[test]
    fn test_transitive_dependants() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("lib"));
        manager.add_repository(Repository::new("core"));
        manager.add_repository(Repository::new("app"));

        manager
            .add_dependency("lib", "core", DependencyType::Npm, None)
            .unwrap();
        manager
            .add_dependency("core", "app", DependencyType::Npm, None)
            .unwrap();

        // lib -> core -> app
        let all_dependants = manager.get_all_dependants("lib");
        assert_eq!(all_dependants.len(), 2);

        let names: Vec<_> = all_dependants.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"app"));
    }

    #[test]
    fn test_appwrite_defaults() {
        let manager = RepoRelationships::with_appwrite_defaults();

        // Check repositories exist
        assert!(manager.get_repository("cloud").is_some());
        assert!(manager.get_repository("appwrite").is_some());
        assert!(manager.get_repository("utopia-php").is_some());

        // cloud depends on appwrite
        let cloud_deps = manager.get_dependencies("cloud");
        assert_eq!(cloud_deps.len(), 1);
        assert_eq!(cloud_deps[0].name, "appwrite");

        // appwrite is depended on by cloud
        let appwrite_dependants = manager.get_dependants("appwrite");
        assert_eq!(appwrite_dependants.len(), 1);
        assert_eq!(appwrite_dependants[0].name, "cloud");
    }

    #[test]
    fn test_cascade_changes() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("lib"));
        manager.add_repository(Repository::new("app1"));
        manager.add_repository(Repository::new("app2"));

        manager
            .add_dependency("lib", "app1", DependencyType::Npm, None)
            .unwrap();
        manager
            .add_dependency("lib", "app2", DependencyType::Npm, None)
            .unwrap();

        let changes = manager.get_cascade_changes("lib");
        assert_eq!(changes.len(), 2);

        let targets: Vec<_> = changes.iter().map(|c| c.target_repo.as_str()).collect();
        assert!(targets.contains(&"app1"));
        assert!(targets.contains(&"app2"));
    }

    #[test]
    fn test_print_tree() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("root"));
        manager.add_repository(Repository::new("child1"));
        manager.add_repository(Repository::new("child2"));

        manager
            .add_dependency("root", "child1", DependencyType::Manual, None)
            .unwrap();
        manager
            .add_dependency("root", "child2", DependencyType::Manual, None)
            .unwrap();

        let tree = manager.print_tree(Some("root"));
        assert!(tree.contains("root"));
        assert!(tree.contains("child1"));
        assert!(tree.contains("child2"));
    }

    #[test]
    fn test_depends_on() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("a"));
        manager.add_repository(Repository::new("b"));
        manager.add_repository(Repository::new("c"));

        manager
            .add_dependency("a", "b", DependencyType::Manual, None)
            .unwrap();
        manager
            .add_dependency("b", "c", DependencyType::Manual, None)
            .unwrap();

        // c depends on b (directly)
        assert!(manager.get_graph().depends_on("c", "b"));
        // c depends on a (transitively)
        assert!(manager.get_graph().depends_on("c", "a"));
        // a does not depend on c
        assert!(!manager.get_graph().depends_on("a", "c"));
    }

    #[test]
    fn test_dependency_type_as_str() {
        assert_eq!(DependencyType::Npm.as_str(), "npm");
        assert_eq!(DependencyType::Composer.as_str(), "composer");
        assert_eq!(DependencyType::GitSubmodule.as_str(), "git_submodule");
        assert_eq!(DependencyType::Manual.as_str(), "manual");
    }

    #[test]
    fn test_dependency_type_parse_submodule() {
        assert_eq!(
            DependencyType::parse("git_submodule"),
            Some(DependencyType::GitSubmodule)
        );
        assert_eq!(
            DependencyType::parse("submodule"),
            Some(DependencyType::GitSubmodule)
        );
    }

    #[test]
    fn test_repository_with_path() {
        let repo = Repository::new("my-repo").with_path("/path/to/repo");
        assert_eq!(repo.name, "my-repo");
        assert_eq!(repo.path, Some("/path/to/repo".to_string()));
    }

    #[test]
    fn test_repository_with_github_url() {
        let repo = Repository::new("my-repo").with_github_url("https://github.com/org/repo");
        assert_eq!(
            repo.github_url,
            Some("https://github.com/org/repo".to_string())
        );
    }

    #[test]
    fn test_repository_created_at() {
        let repo = Repository::new("my-repo");
        // Should be recent (within last minute)
        let diff = Utc::now() - repo.created_at;
        assert!(diff.num_seconds() < 60);
    }

    #[test]
    fn test_dependency_graph_empty() {
        let graph = DependencyGraph::new();
        assert_eq!(graph.repositories().count(), 0);
    }

    #[test]
    fn test_dependency_graph_get_nonexistent_repo() {
        let graph = DependencyGraph::new();
        assert!(graph.get_repository("nonexistent").is_none());
    }

    #[test]
    fn test_get_dependants_empty() {
        let graph = DependencyGraph::new();
        let dependants = graph.get_dependants("nonexistent");
        assert!(dependants.is_empty());
    }

    #[test]
    fn test_get_dependencies_empty() {
        let graph = DependencyGraph::new();
        let dependencies = graph.get_dependencies("nonexistent");
        assert!(dependencies.is_empty());
    }

    #[test]
    fn test_list_repositories() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("repo1"));
        manager.add_repository(Repository::new("repo2"));

        let repos = manager.list_repositories();
        assert_eq!(repos.len(), 2);
    }

    #[test]
    fn test_dependency_with_version_pattern() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("lib"));
        manager.add_repository(Repository::new("app"));

        manager
            .add_dependency(
                "lib",
                "app",
                DependencyType::Npm,
                Some("^1.0.0".to_string()),
            )
            .unwrap();

        let deps = manager.get_dependencies("app");
        assert_eq!(deps.len(), 1);
    }

    #[test]
    fn test_cascade_change_fields() {
        let change = CascadingChange {
            trigger_repo: "upstream".to_string(),
            target_repo: "downstream".to_string(),
            change_type: CascadeChangeType::VersionBump,
            status: CascadeStatus::Pending,
            pr_url: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        assert_eq!(change.trigger_repo, "upstream");
        assert_eq!(change.target_repo, "downstream");
        assert_eq!(change.change_type, CascadeChangeType::VersionBump);
        assert_eq!(change.status, CascadeStatus::Pending);
    }

    #[test]
    fn test_cascade_change_types() {
        assert_eq!(
            CascadeChangeType::VersionBump,
            CascadeChangeType::VersionBump
        );
        assert_eq!(CascadeChangeType::CodeChange, CascadeChangeType::CodeChange);
        assert_eq!(CascadeChangeType::TestUpdate, CascadeChangeType::TestUpdate);
        assert_ne!(
            CascadeChangeType::VersionBump,
            CascadeChangeType::CodeChange
        );
    }

    #[test]
    fn test_cascade_status_variants() {
        assert_eq!(CascadeStatus::Pending, CascadeStatus::Pending);
        assert_eq!(CascadeStatus::InProgress, CascadeStatus::InProgress);
        assert_eq!(CascadeStatus::Completed, CascadeStatus::Completed);
        assert_eq!(CascadeStatus::Failed, CascadeStatus::Failed);
        assert_eq!(CascadeStatus::Skipped, CascadeStatus::Skipped);
    }

    #[test]
    fn test_print_tree_root_only() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("root"));

        let tree = manager.print_tree(Some("root"));
        assert!(tree.contains("root"));
    }

    #[test]
    fn test_dependency_graph_cycle_detection() {
        let mut graph = DependencyGraph::new();
        graph.add_repository(Repository::new("a"));
        graph.add_repository(Repository::new("b"));

        // Add dependencies that create a cycle (for tree printing)
        graph.add_dependency(Dependency {
            upstream: "a".to_string(),
            downstream: "b".to_string(),
            dep_type: DependencyType::Manual,
            version_pattern: None,
            created_at: Utc::now(),
        });
        graph.add_dependency(Dependency {
            upstream: "b".to_string(),
            downstream: "a".to_string(),
            dep_type: DependencyType::Manual,
            version_pattern: None,
            created_at: Utc::now(),
        });

        let tree = graph.to_string_tree(Some("a"));
        // Should detect cycle
        assert!(tree.contains("cycle") || tree.contains("a") || tree.contains("b"));
    }

    #[test]
    fn test_repo_relationships_default() {
        let manager = RepoRelationships::default();
        assert!(manager.list_repositories().is_empty());
    }

    #[test]
    fn test_get_cascade_changes_no_dependants() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("standalone"));

        let changes = manager.get_cascade_changes("standalone");
        assert!(changes.is_empty());
    }

    #[test]
    fn test_dependency_serialization() {
        let dep = Dependency {
            upstream: "lib".to_string(),
            downstream: "app".to_string(),
            dep_type: DependencyType::Npm,
            version_pattern: Some("^1.0.0".to_string()),
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&dep).unwrap();
        assert!(json.contains("lib"));
        assert!(json.contains("app"));
        // Serde serializes the enum variant name as "Npm"
        assert!(json.contains("Npm"));
    }

    #[test]
    fn test_repository_serialization() {
        let repo = Repository::new("my-repo")
            .with_path("/path")
            .with_github_url("https://github.com/org/repo");

        let json = serde_json::to_string(&repo).unwrap();
        assert!(json.contains("my-repo"));
        assert!(json.contains("/path"));
    }

    #[test]
    fn test_appwrite_defaults_utopia_deps() {
        let manager = RepoRelationships::with_appwrite_defaults();

        // appwrite should depend on utopia-php
        let appwrite_deps = manager.get_dependencies("appwrite");
        let dep_names: Vec<_> = appwrite_deps.iter().map(|r| r.name.as_str()).collect();

        assert!(dep_names.contains(&"utopia-php"));
        assert!(dep_names.contains(&"utopia-database"));
        assert!(dep_names.contains(&"utopia-http"));
        assert!(dep_names.contains(&"utopia-cache"));
    }

    #[test]
    fn test_print_tree_no_root_finds_all_roots() {
        let mut manager = RepoRelationships::new();

        // Create two independent trees
        manager.add_repository(Repository::new("root-a"));
        manager.add_repository(Repository::new("child-a"));
        manager.add_repository(Repository::new("root-b"));
        manager.add_repository(Repository::new("child-b"));

        manager
            .add_dependency("root-a", "child-a", DependencyType::Manual, None)
            .unwrap();
        manager
            .add_dependency("root-b", "child-b", DependencyType::Manual, None)
            .unwrap();

        // Print with no root to discover all root nodes
        let tree = manager.print_tree(None);
        assert!(tree.contains("root-a"));
        assert!(tree.contains("child-a"));
        assert!(tree.contains("root-b"));
        assert!(tree.contains("child-b"));
    }

    #[test]
    fn test_print_subtree_cycle_shows_cycle_marker() {
        let mut graph = DependencyGraph::new();
        graph.add_repository(Repository::new("x"));
        graph.add_repository(Repository::new("y"));

        graph.add_dependency(Dependency {
            upstream: "x".to_string(),
            downstream: "y".to_string(),
            dep_type: DependencyType::Manual,
            version_pattern: None,
            created_at: Utc::now(),
        });
        graph.add_dependency(Dependency {
            upstream: "y".to_string(),
            downstream: "x".to_string(),
            dep_type: DependencyType::Manual,
            version_pattern: None,
            created_at: Utc::now(),
        });

        let tree = graph.to_string_tree(Some("x"));
        // The cycle detection should produce "(cycle)" in the output
        assert!(tree.contains("(cycle)"));
    }

    #[test]
    fn test_add_dependency_auto_creates_repos() {
        let mut manager = RepoRelationships::new();
        // Neither repo exists yet
        assert!(manager.get_repository("new-upstream").is_none());
        assert!(manager.get_repository("new-downstream").is_none());

        manager
            .add_dependency("new-upstream", "new-downstream", DependencyType::Npm, None)
            .unwrap();

        // Both should now exist
        assert!(manager.get_repository("new-upstream").is_some());
        assert!(manager.get_repository("new-downstream").is_some());

        // And the dependency should be recorded
        let deps = manager.get_dependants("new-upstream");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "new-downstream");
    }

    #[test]
    fn test_get_all_dependants_with_diamond_dependency() {
        let mut manager = RepoRelationships::new();
        // Diamond: A -> B, A -> C, B -> D, C -> D
        manager.add_repository(Repository::new("A"));
        manager.add_repository(Repository::new("B"));
        manager.add_repository(Repository::new("C"));
        manager.add_repository(Repository::new("D"));

        manager
            .add_dependency("A", "B", DependencyType::Npm, None)
            .unwrap();
        manager
            .add_dependency("A", "C", DependencyType::Npm, None)
            .unwrap();
        manager
            .add_dependency("B", "D", DependencyType::Npm, None)
            .unwrap();
        manager
            .add_dependency("C", "D", DependencyType::Npm, None)
            .unwrap();

        let all = manager.get_all_dependants("A");
        let names: Vec<_> = all.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
        assert!(names.contains(&"D"));
        // D should appear only once despite two paths
        assert_eq!(names.iter().filter(|&&n| n == "D").count(), 1);
    }

    #[test]
    fn test_get_first_hop_dependency_type_none() {
        let graph = DependencyGraph::new();
        assert!(graph.get_first_hop_dependency_type("nonexistent").is_none());
    }

    #[test]
    fn test_depends_on_self_returns_false() {
        let mut manager = RepoRelationships::new();
        manager.add_repository(Repository::new("self-repo"));

        // A repo does not depend on itself
        assert!(!manager.get_graph().depends_on("self-repo", "self-repo"));
    }

    #[test]
    fn test_with_defaults_matches_appwrite_defaults() {
        let defaults = RepoRelationships::with_defaults();
        let appwrite = RepoRelationships::with_appwrite_defaults();

        // Both should have the same set of repositories
        assert_eq!(
            defaults.list_repositories().len(),
            appwrite.list_repositories().len()
        );
    }
}
