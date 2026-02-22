//! Auto-discovery of repository dependencies from package manifests.

use crate::error::Result;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Discovered dependency information.
#[derive(Debug, Clone)]
pub struct DiscoveredDependency {
    /// The repository that has the dependency.
    pub repo: String,
    /// The dependency name (org/package format).
    pub depends_on: String,
    /// Type of dependency (composer, npm).
    pub dep_type: String,
    /// Local path where the repo was found.
    pub repo_path: String,
}

/// Scans directories for dependencies from known organizations.
pub struct DependencyDiscovery {
    known_orgs: HashSet<String>,
}

impl DependencyDiscovery {
    /// Create a new discovery scanner with the given known organizations.
    pub fn new(known_orgs: Vec<String>) -> Self {
        Self {
            known_orgs: known_orgs.into_iter().collect(),
        }
    }

    /// Check if a package name belongs to a known organization.
    fn is_known_org(&self, package_name: &str) -> bool {
        if let Some(org) = package_name.split('/').next() {
            self.known_orgs.contains(org)
        } else {
            false
        }
    }

    /// Scan a single directory for dependencies.
    pub fn scan_directory(&self, path: &Path) -> Result<Vec<DiscoveredDependency>> {
        let mut deps = Vec::new();

        // Try to determine repo name from composer.json or package.json
        let repo_name = self.get_repo_name(path);

        if let Some(ref name) = repo_name {
            // Scan composer.json
            let composer_path = path.join("composer.json");
            if composer_path.exists() {
                if let Ok(composer_deps) = self.scan_composer(&composer_path, name) {
                    deps.extend(composer_deps);
                }
            }

            // Scan package.json
            let package_path = path.join("package.json");
            if package_path.exists() {
                if let Ok(npm_deps) = self.scan_package_json(&package_path, name) {
                    deps.extend(npm_deps);
                }
            }
        }

        Ok(deps)
    }

    /// Scan multiple directories and return all discovered dependencies.
    pub fn scan_directories(&self, paths: &[String]) -> Result<Vec<DiscoveredDependency>> {
        let mut all_deps = Vec::new();

        for path_str in paths {
            let path = Path::new(path_str);

            // If path is a directory containing multiple repos (like ~/Local)
            // scan each subdirectory
            if path.is_dir() {
                // First try scanning the path itself
                if let Ok(deps) = self.scan_directory(path) {
                    if !deps.is_empty() {
                        all_deps.extend(deps);
                        continue;
                    }
                }

                // Otherwise scan subdirectories
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        let entry_path = entry.path();
                        if entry_path.is_dir() {
                            if let Ok(deps) = self.scan_directory(&entry_path) {
                                all_deps.extend(deps);
                            }
                        }
                    }
                }
            }
        }

        Ok(all_deps)
    }

    /// Get the repository name from package manifests.
    fn get_repo_name(&self, path: &Path) -> Option<String> {
        // Try composer.json first
        let composer_path = path.join("composer.json");
        if composer_path.exists() {
            if let Ok(content) = fs::read_to_string(&composer_path) {
                if let Ok(composer) = serde_json::from_str::<ComposerJson>(&content) {
                    if let Some(name) = composer.name {
                        return Some(name);
                    }
                }
            }
        }

        // Try package.json
        let package_path = path.join("package.json");
        if package_path.exists() {
            if let Ok(content) = fs::read_to_string(&package_path) {
                if let Ok(package) = serde_json::from_str::<PackageJson>(&content) {
                    if let Some(name) = package.name {
                        // npm packages might be @org/package, convert to org/package
                        return Some(name.trim_start_matches('@').to_string());
                    }
                }
            }
        }

        // Fall back to directory name
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }

    /// Scan composer.json for dependencies.
    fn scan_composer(&self, path: &Path, repo_name: &str) -> Result<Vec<DiscoveredDependency>> {
        let content = fs::read_to_string(path)?;
        let composer: ComposerJson = serde_json::from_str(&content)?;

        let mut deps = Vec::new();
        let repo_path = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Process require
        if let Some(require) = composer.require {
            for (package, _version) in require {
                if self.is_known_org(&package) {
                    deps.push(DiscoveredDependency {
                        repo: repo_name.to_string(),
                        depends_on: package,
                        dep_type: "composer".to_string(),
                        repo_path: repo_path.clone(),
                    });
                }
            }
        }

        // Process require-dev
        if let Some(require_dev) = composer.require_dev {
            for (package, _version) in require_dev {
                if self.is_known_org(&package) {
                    deps.push(DiscoveredDependency {
                        repo: repo_name.to_string(),
                        depends_on: package,
                        dep_type: "composer".to_string(),
                        repo_path: repo_path.clone(),
                    });
                }
            }
        }

        Ok(deps)
    }

    /// Scan package.json for dependencies.
    fn scan_package_json(&self, path: &Path, repo_name: &str) -> Result<Vec<DiscoveredDependency>> {
        let content = fs::read_to_string(path)?;
        let package: PackageJson = serde_json::from_str(&content)?;

        let mut deps = Vec::new();
        let repo_path = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Process dependencies
        if let Some(dependencies) = package.dependencies {
            for (package_name, _version) in dependencies {
                // npm scoped packages: @org/package -> org/package
                let normalized = package_name.trim_start_matches('@').to_string();
                if self.is_known_org(&normalized) {
                    deps.push(DiscoveredDependency {
                        repo: repo_name.to_string(),
                        depends_on: normalized,
                        dep_type: "npm".to_string(),
                        repo_path: repo_path.clone(),
                    });
                }
            }
        }

        // Process devDependencies
        if let Some(dev_dependencies) = package.dev_dependencies {
            for (package_name, _version) in dev_dependencies {
                let normalized = package_name.trim_start_matches('@').to_string();
                if self.is_known_org(&normalized) {
                    deps.push(DiscoveredDependency {
                        repo: repo_name.to_string(),
                        depends_on: normalized,
                        dep_type: "npm".to_string(),
                        repo_path: repo_path.clone(),
                    });
                }
            }
        }

        Ok(deps)
    }
}

#[derive(Deserialize)]
struct ComposerJson {
    name: Option<String>,
    require: Option<std::collections::HashMap<String, serde_json::Value>>,
    #[serde(rename = "require-dev")]
    require_dev: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize)]
struct PackageJson {
    name: Option<String>,
    dependencies: Option<std::collections::HashMap<String, serde_json::Value>>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_known_org() {
        let discovery =
            DependencyDiscovery::new(vec!["utopia-php".to_string(), "appwrite".to_string()]);

        assert!(discovery.is_known_org("utopia-php/database"));
        assert!(discovery.is_known_org("appwrite/sdk"));
        assert!(!discovery.is_known_org("symfony/console"));
        assert!(!discovery.is_known_org("laravel/framework"));
    }

    #[test]
    fn test_normalize_npm_scope() {
        let name = "@appwrite/sdk";
        let normalized = name.trim_start_matches('@').to_string();
        assert_eq!(normalized, "appwrite/sdk");
    }

    #[test]
    fn test_is_known_org_no_slash() {
        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        // Package without slash: split('/').next() returns the whole string,
        // which matches "utopia-php" in known_orgs
        assert!(discovery.is_known_org("utopia-php"));
    }

    #[test]
    fn test_is_known_org_empty_string() {
        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        assert!(!discovery.is_known_org(""));
    }

    #[test]
    fn test_is_known_org_empty_orgs() {
        let discovery = DependencyDiscovery::new(vec![]);
        assert!(!discovery.is_known_org("utopia-php/database"));
    }

    #[test]
    fn test_is_known_org_case_sensitive() {
        let discovery = DependencyDiscovery::new(vec!["Appwrite".to_string()]);
        assert!(discovery.is_known_org("Appwrite/sdk"));
        assert!(!discovery.is_known_org("appwrite/sdk"));
    }

    #[test]
    fn test_scan_directory_no_manifests() {
        let temp = tempfile::TempDir::new().unwrap();
        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery.scan_directory(temp.path()).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_scan_directory_composer_json() {
        let temp = tempfile::TempDir::new().unwrap();
        let composer = serde_json::json!({
            "name": "appwrite/cloud",
            "require": {
                "utopia-php/database": "^1.0",
                "symfony/console": "^5.0"
            }
        });
        std::fs::write(
            temp.path().join("composer.json"),
            serde_json::to_string(&composer).unwrap(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery.scan_directory(temp.path()).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, "utopia-php/database");
        assert_eq!(deps[0].repo, "appwrite/cloud");
        assert_eq!(deps[0].dep_type, "composer");
    }

    #[test]
    fn test_scan_directory_composer_require_dev() {
        let temp = tempfile::TempDir::new().unwrap();
        let composer = serde_json::json!({
            "name": "appwrite/cloud",
            "require-dev": {
                "utopia-php/testing": "^1.0"
            }
        });
        std::fs::write(
            temp.path().join("composer.json"),
            serde_json::to_string(&composer).unwrap(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery.scan_directory(temp.path()).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, "utopia-php/testing");
    }

    #[test]
    fn test_scan_directory_package_json() {
        let temp = tempfile::TempDir::new().unwrap();
        let package = serde_json::json!({
            "name": "@appwrite/console",
            "dependencies": {
                "@appwrite/sdk": "^1.0",
                "react": "^18.0"
            }
        });
        std::fs::write(
            temp.path().join("package.json"),
            serde_json::to_string(&package).unwrap(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec!["appwrite".to_string()]);
        let deps = discovery.scan_directory(temp.path()).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, "appwrite/sdk");
        assert_eq!(deps[0].dep_type, "npm");
    }

    #[test]
    fn test_scan_directory_package_json_dev_deps() {
        let temp = tempfile::TempDir::new().unwrap();
        let package = serde_json::json!({
            "name": "my-app",
            "devDependencies": {
                "@appwrite/testing": "^1.0"
            }
        });
        std::fs::write(
            temp.path().join("package.json"),
            serde_json::to_string(&package).unwrap(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec!["appwrite".to_string()]);
        let deps = discovery.scan_directory(temp.path()).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, "appwrite/testing");
    }

    #[test]
    fn test_scan_directory_malformed_json() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("composer.json"), "{ invalid json }").unwrap();

        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        // Should not error out - gracefully handles malformed JSON
        let deps = discovery.scan_directory(temp.path()).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_scan_directory_composer_no_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let composer = serde_json::json!({
            "require": {
                "utopia-php/database": "^1.0"
            }
        });
        std::fs::write(
            temp.path().join("composer.json"),
            serde_json::to_string(&composer).unwrap(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery.scan_directory(temp.path()).unwrap();
        // Falls back to directory name for repo name
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, "utopia-php/database");
    }

    #[test]
    fn test_scan_directory_empty_require() {
        let temp = tempfile::TempDir::new().unwrap();
        let composer = serde_json::json!({
            "name": "myapp",
            "require": {}
        });
        std::fs::write(
            temp.path().join("composer.json"),
            serde_json::to_string(&composer).unwrap(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery.scan_directory(temp.path()).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_scan_directories_empty_paths() {
        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery.scan_directories(&[]).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_scan_directories_nonexistent_path() {
        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery
            .scan_directories(&["/nonexistent/path".to_string()])
            .unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_scan_directories_scans_subdirectories() {
        let temp = tempfile::TempDir::new().unwrap();

        // Create a subdirectory with a composer.json
        let sub = temp.path().join("my-project");
        std::fs::create_dir(&sub).unwrap();
        let composer = serde_json::json!({
            "name": "my-project",
            "require": {
                "utopia-php/database": "^1.0"
            }
        });
        std::fs::write(
            sub.join("composer.json"),
            serde_json::to_string(&composer).unwrap(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec!["utopia-php".to_string()]);
        let deps = discovery
            .scan_directories(&[temp.path().to_string_lossy().to_string()])
            .unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on, "utopia-php/database");
    }

    #[test]
    fn test_get_repo_name_fallback_to_dir_name() {
        let temp = tempfile::TempDir::new().unwrap();
        // No manifests - falls back to directory name
        let discovery = DependencyDiscovery::new(vec![]);
        let name = discovery.get_repo_name(temp.path());
        assert!(name.is_some());
        // Should be the temp dir's basename
    }

    #[test]
    fn test_get_repo_name_prefers_composer_over_package() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("composer.json"),
            serde_json::json!({"name": "composer-name"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            serde_json::json!({"name": "package-name"}).to_string(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec![]);
        let name = discovery.get_repo_name(temp.path());
        assert_eq!(name, Some("composer-name".to_string()));
    }

    #[test]
    fn test_get_repo_name_npm_scope_stripped() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            serde_json::json!({"name": "@org/package"}).to_string(),
        )
        .unwrap();

        let discovery = DependencyDiscovery::new(vec![]);
        let name = discovery.get_repo_name(temp.path());
        assert_eq!(name, Some("org/package".to_string()));
    }

    #[test]
    fn test_discovered_dependency_fields() {
        let dep = DiscoveredDependency {
            repo: "my-app".to_string(),
            depends_on: "utopia-php/database".to_string(),
            dep_type: "composer".to_string(),
            repo_path: "/path/to/app".to_string(),
        };
        assert_eq!(dep.repo, "my-app");
        assert_eq!(dep.depends_on, "utopia-php/database");
        assert_eq!(dep.dep_type, "composer");
        assert_eq!(dep.repo_path, "/path/to/app");
    }

    #[test]
    fn test_discovered_dependency_clone() {
        let dep = DiscoveredDependency {
            repo: "app".to_string(),
            depends_on: "lib".to_string(),
            dep_type: "npm".to_string(),
            repo_path: "/p".to_string(),
        };
        let cloned = dep.clone();
        assert_eq!(cloned.repo, "app");
    }
}
