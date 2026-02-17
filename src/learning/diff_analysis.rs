//! System 2: Analyze PR diffs to extract structured change information.

use crate::types::{ChangeCategory, DiffAnalysis};
use chrono::Utc;
use std::collections::HashMap;

pub struct DiffAnalyzer;

impl DiffAnalyzer {
    /// Parse a unified diff into structured analysis.
    pub fn analyze_diff(
        raw_diff: &str,
        attempt_id: i64,
        pr_url: &str,
        repo: &str,
        pr_number: i64,
    ) -> DiffAnalysis {
        let mut files = Vec::new();
        let mut categories = std::collections::HashSet::new();

        for line in raw_diff.lines() {
            if let Some(path) = line.strip_prefix("+++ b/") {
                let path = path.trim();
                if path != "/dev/null" {
                    files.push(path.to_string());
                    categories.insert(Self::categorize_file(path));
                }
            }
        }

        let file_types = Self::file_type_stats(&files);
        let file_count = files.len();
        let cat_list: Vec<ChangeCategory> = categories.into_iter().collect();

        let summary = format!(
            "{} files changed across {} categories",
            file_count,
            cat_list.len()
        );

        DiffAnalysis {
            id: 0,
            attempt_id,
            pr_url: pr_url.to_string(),
            github_repo: repo.to_string(),
            pr_number,
            files_changed: files,
            file_types,
            change_categories: cat_list,
            diff_summary: summary,
            created_at: Utc::now(),
        }
    }

    /// Categorize a file path into a ChangeCategory.
    pub fn categorize_file(path: &str) -> ChangeCategory {
        let lower = path.to_lowercase();

        // Tests
        if lower.contains("_test.")
            || lower.contains("_spec.")
            || lower.contains(".test.")
            || lower.contains(".spec.")
            || lower.contains("/tests/")
            || lower.contains("/__tests__/")
            || lower.contains("/test/")
            || lower.starts_with("tests/")
            || lower.starts_with("test/")
            || lower.starts_with("__tests__/")
        {
            return ChangeCategory::Tests;
        }

        // Dependencies (lock files) - must be before Config to catch package-lock.json
        if lower == "cargo.lock"
            || lower == "package-lock.json"
            || lower == "yarn.lock"
            || lower == "composer.lock"
            || lower == "poetry.lock"
            || lower == "gemfile.lock"
            || lower == "go.sum"
        {
            return ChangeCategory::Dependencies;
        }

        // Docs
        if lower.ends_with(".md")
            || lower.ends_with(".txt")
            || lower.ends_with(".rst")
            || lower.contains("/docs/")
            || lower.contains("/doc/")
        {
            return ChangeCategory::Docs;
        }

        // Config
        if lower.ends_with(".toml")
            || lower.ends_with(".yaml")
            || lower.ends_with(".yml")
            || lower.ends_with(".json")
            || lower.ends_with(".env")
            || lower.ends_with(".ini")
            || lower.contains("/config/")
            || lower.contains(".config.")
        {
            return ChangeCategory::Config;
        }

        // Migrations
        if lower.contains("/migrations/") || lower.ends_with(".sql") {
            return ChangeCategory::Migrations;
        }

        // Default to Modification for existing code changes
        ChangeCategory::Modification
    }

    /// Extract file extension statistics.
    pub fn file_type_stats(files: &[String]) -> HashMap<String, usize> {
        let mut stats = HashMap::new();
        for file in files {
            let ext = file.rsplit('.').next().unwrap_or("unknown").to_lowercase();
            *stats.entry(ext).or_insert(0) += 1;
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorize_file() {
        assert_eq!(
            DiffAnalyzer::categorize_file("src/handler_test.rs"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("tests/integration.rs"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("README.md"),
            ChangeCategory::Docs
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("Cargo.toml"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("Cargo.lock"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("migrations/001_init.sql"),
            ChangeCategory::Migrations
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/main.rs"),
            ChangeCategory::Modification
        );
    }

    #[test]
    fn test_analyze_diff() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,5 @@
+use std::io;
 fn main() {
-    println!("hello");
+    println!("hello world");
 }
diff --git a/tests/test_main.rs b/tests/test_main.rs
--- /dev/null
+++ b/tests/test_main.rs
@@ -0,0 +1,5 @@
+#[test]
+fn test_main() {
+    assert!(true);
+}
"#;
        let analysis =
            DiffAnalyzer::analyze_diff(diff, 1, "https://github.com/foo/bar/pull/1", "foo/bar", 1);
        assert_eq!(analysis.files_changed.len(), 2);
        assert!(analysis.change_categories.contains(&ChangeCategory::Tests));
        assert!(analysis
            .change_categories
            .contains(&ChangeCategory::Modification));
    }

    #[test]
    fn test_file_type_stats() {
        let files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "tests/test.py".to_string(),
        ];
        let stats = DiffAnalyzer::file_type_stats(&files);
        assert_eq!(stats.get("rs"), Some(&2));
        assert_eq!(stats.get("py"), Some(&1));
    }

    #[test]
    fn test_analyze_empty_diff() {
        let analysis = DiffAnalyzer::analyze_diff("", 1, "url", "repo", 1);
        assert!(analysis.files_changed.is_empty());
        assert!(analysis.change_categories.is_empty());
        assert!(analysis.file_types.is_empty());
        assert_eq!(analysis.diff_summary, "0 files changed across 0 categories");
    }

    #[test]
    fn test_analyze_diff_skips_dev_null() {
        let diff = "+++ /dev/null\n--- a/src/deleted.rs";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert!(analysis.files_changed.is_empty());
    }

    #[test]
    fn test_categorize_file_all_test_patterns() {
        assert_eq!(
            DiffAnalyzer::categorize_file("src/handler_test.go"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/handler_spec.rb"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/handler.test.ts"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/handler.spec.js"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/tests/handler.rs"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("__tests__/handler.ts"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("test/handler.py"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/test/handler.java"),
            ChangeCategory::Tests
        );
    }

    #[test]
    fn test_categorize_file_all_doc_patterns() {
        assert_eq!(
            DiffAnalyzer::categorize_file("README.md"),
            ChangeCategory::Docs
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("CHANGELOG.txt"),
            ChangeCategory::Docs
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("docs/guide.rst"),
            ChangeCategory::Docs
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("doc/api.md"),
            ChangeCategory::Docs
        );
    }

    #[test]
    fn test_categorize_file_all_config_patterns() {
        assert_eq!(
            DiffAnalyzer::categorize_file("Cargo.toml"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("config.yaml"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("settings.yml"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("tsconfig.json"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file(".env"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("setup.ini"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/config/database.rs"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("jest.config.ts"),
            ChangeCategory::Config
        );
    }

    #[test]
    fn test_categorize_file_all_dependency_patterns() {
        assert_eq!(
            DiffAnalyzer::categorize_file("Cargo.lock"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("package-lock.json"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("yarn.lock"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("composer.lock"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("poetry.lock"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("Gemfile.lock"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("go.sum"),
            ChangeCategory::Dependencies
        );
    }

    #[test]
    fn test_categorize_file_migrations() {
        assert_eq!(
            DiffAnalyzer::categorize_file("migrations/001_init.sql"),
            ChangeCategory::Migrations
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("db/schema.sql"),
            ChangeCategory::Migrations
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/migrations/add_users.sql"),
            ChangeCategory::Migrations
        );
    }

    #[test]
    fn test_analyze_diff_multiple_categories() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
+++ b/src/main.rs
diff --git a/tests/test_main.rs b/tests/test_main.rs
+++ b/tests/test_main.rs
diff --git a/README.md b/README.md
+++ b/README.md
diff --git a/Cargo.toml b/Cargo.toml
+++ b/Cargo.toml
"#;
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 4);
        assert!(analysis
            .change_categories
            .contains(&ChangeCategory::Modification));
        assert!(analysis.change_categories.contains(&ChangeCategory::Tests));
        assert!(analysis.change_categories.contains(&ChangeCategory::Docs));
        assert!(analysis.change_categories.contains(&ChangeCategory::Config));
    }

    #[test]
    fn test_file_type_stats_empty() {
        let stats = DiffAnalyzer::file_type_stats(&[]);
        assert!(stats.is_empty());
    }

    #[test]
    fn test_analyze_diff_preserves_metadata() {
        let diff = "+++ b/src/handler.rs";
        let analysis = DiffAnalyzer::analyze_diff(
            diff,
            42,
            "https://github.com/org/repo/pull/99",
            "org/repo",
            99,
        );
        assert_eq!(analysis.attempt_id, 42);
        assert_eq!(analysis.pr_url, "https://github.com/org/repo/pull/99");
        assert_eq!(analysis.github_repo, "org/repo");
        assert_eq!(analysis.pr_number, 99);
    }

    // ── Edge case tests ──

    #[test]
    fn test_analyze_diff_malformed_no_plus_prefix() {
        // Lines that look like diff headers but don't match "+++ b/" pattern
        let diff = "--- a/src/old.rs\n+++ a/src/new.rs\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        // "+++ a/" doesn't match "+++ b/", so no files extracted
        assert!(analysis.files_changed.is_empty());
    }

    #[test]
    fn test_analyze_diff_whitespace_in_path() {
        let diff = "+++ b/src/my file.rs\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 1);
        assert_eq!(analysis.files_changed[0], "src/my file.rs");
    }

    #[test]
    fn test_analyze_diff_duplicate_files() {
        // Same file appears in diff twice (e.g., multiple hunks)
        let diff = "+++ b/src/main.rs\n+++ b/src/main.rs\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        // Files list will have duplicates, but categories should be deduped
        assert_eq!(analysis.files_changed.len(), 2);
        assert_eq!(analysis.change_categories.len(), 1);
    }

    #[test]
    fn test_file_type_stats_no_extension() {
        let files = vec!["Makefile".to_string(), "Dockerfile".to_string()];
        let stats = DiffAnalyzer::file_type_stats(&files);
        // rsplit('.').next() returns the full name when no dot exists
        assert_eq!(stats.get("makefile"), Some(&1));
        assert_eq!(stats.get("dockerfile"), Some(&1));
    }

    #[test]
    fn test_file_type_stats_multiple_dots() {
        let files = vec!["my.component.test.tsx".to_string()];
        let stats = DiffAnalyzer::file_type_stats(&files);
        // rsplit('.').next() gets the last extension
        assert_eq!(stats.get("tsx"), Some(&1));
    }

    #[test]
    fn test_categorize_file_case_insensitive() {
        assert_eq!(
            DiffAnalyzer::categorize_file("README.MD"),
            ChangeCategory::Docs
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("CARGO.LOCK"),
            ChangeCategory::Dependencies
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("SRC/TESTS/foo.rs"),
            ChangeCategory::Tests
        );
    }

    #[test]
    fn test_categorize_file_empty_string() {
        // Empty string should default to Modification
        assert_eq!(
            DiffAnalyzer::categorize_file(""),
            ChangeCategory::Modification
        );
    }

    #[test]
    fn test_analyze_diff_only_deletions() {
        // Files deleted (target is /dev/null) should not be included
        let diff = "--- a/src/old.rs\n+++ /dev/null\n--- a/src/other.rs\n+++ /dev/null\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert!(analysis.files_changed.is_empty());
    }

    #[test]
    fn test_analyze_diff_mixed_additions_and_deletions() {
        let diff = "+++ /dev/null\n+++ b/src/new.rs\n+++ /dev/null\n+++ b/tests/new_test.rs\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 2);
        assert!(analysis.files_changed.contains(&"src/new.rs".to_string()));
        assert!(analysis
            .files_changed
            .contains(&"tests/new_test.rs".to_string()));
    }

    #[test]
    fn test_categorize_file_test_in_directory_name_not_filename() {
        // "test" in dir path should still trigger Tests
        assert_eq!(
            DiffAnalyzer::categorize_file("src/test/utils.rs"),
            ChangeCategory::Tests
        );
        // But "test" as substring of filename (not matching patterns) shouldn't
        assert_eq!(
            DiffAnalyzer::categorize_file("src/contestant.rs"),
            ChangeCategory::Modification
        );
    }

    #[test]
    fn test_file_type_stats_single_dot_file() {
        let files = vec![".gitignore".to_string()];
        let stats = DiffAnalyzer::file_type_stats(&files);
        assert_eq!(stats.get("gitignore"), Some(&1));
    }
}
