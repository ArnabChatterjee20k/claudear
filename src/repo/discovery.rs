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
}
