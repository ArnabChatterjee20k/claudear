//! System 2: Analyze PR diffs to extract structured change information.

use chrono::Utc;
use claudear_core::types::{ChangeCategory, DiffAnalysis};
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
            scm_repo: repo.to_string(),
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
        assert_eq!(analysis.scm_repo, "org/repo");
        assert_eq!(analysis.pr_number, 99);
    }

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

    #[test]
    fn test_analyze_diff_summary_format() {
        let diff = "+++ b/src/main.rs\n+++ b/tests/test.rs\n+++ b/README.md\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 3);
        // 3 files across 3 categories (Modification, Tests, Docs)
        assert_eq!(
            analysis.diff_summary,
            format!(
                "{} files changed across {} categories",
                3,
                analysis.change_categories.len()
            )
        );
    }

    #[test]
    fn test_analyze_diff_single_file_summary() {
        let diff = "+++ b/src/lib.rs\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.diff_summary, "1 files changed across 1 categories");
    }

    #[test]
    fn test_analyze_diff_sets_created_at() {
        let before = Utc::now();
        let analysis = DiffAnalyzer::analyze_diff("+++ b/src/main.rs", 1, "url", "repo", 1);
        let after = Utc::now();
        assert!(analysis.created_at >= before);
        assert!(analysis.created_at <= after);
    }

    #[test]
    fn test_analyze_diff_id_is_zero() {
        let analysis = DiffAnalyzer::analyze_diff("+++ b/src/main.rs", 1, "url", "repo", 1);
        assert_eq!(analysis.id, 0, "analyze_diff always sets id=0");
    }

    #[test]
    fn test_analyze_diff_large_number_of_files() {
        let mut diff = String::new();
        for i in 0..100 {
            diff.push_str(&format!("+++ b/src/file_{}.rs\n", i));
        }
        let analysis = DiffAnalyzer::analyze_diff(&diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 100);
        assert_eq!(analysis.file_types.get("rs"), Some(&100));
        // All are Modification category
        assert_eq!(analysis.change_categories.len(), 1);
        assert!(analysis
            .change_categories
            .contains(&ChangeCategory::Modification));
    }

    #[test]
    fn test_analyze_diff_with_real_unified_diff_format() {
        let diff = r#"diff --git a/Cargo.toml b/Cargo.toml
index abc1234..def5678 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1,5 +1,6 @@
 [package]
 name = "my-app"
+version = "0.2.0"
-version = "0.1.0"
 edition = "2021"

diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,5 @@
+pub mod new_module;
 pub fn add(left: u64, right: u64) -> u64 {
     left + right
 }
"#;
        let analysis = DiffAnalyzer::analyze_diff(
            diff,
            5,
            "https://github.com/org/repo/pull/42",
            "org/repo",
            42,
        );
        assert_eq!(analysis.files_changed.len(), 2);
        assert!(analysis.files_changed.contains(&"Cargo.toml".to_string()));
        assert!(analysis.files_changed.contains(&"src/lib.rs".to_string()));
        assert!(analysis.change_categories.contains(&ChangeCategory::Config));
        assert!(analysis
            .change_categories
            .contains(&ChangeCategory::Modification));
        assert_eq!(analysis.attempt_id, 5);
        assert_eq!(analysis.pr_number, 42);
    }

    #[test]
    fn test_categorize_file_sql_without_migrations_dir() {
        // A standalone .sql file (not in migrations dir) should still be Migrations
        assert_eq!(
            DiffAnalyzer::categorize_file("schema.sql"),
            ChangeCategory::Migrations
        );
    }

    #[test]
    fn test_categorize_file_nested_tests_directory() {
        assert_eq!(
            DiffAnalyzer::categorize_file("src/__tests__/component.test.tsx"),
            ChangeCategory::Tests
        );
    }

    #[test]
    fn test_categorize_file_dot_config_pattern() {
        assert_eq!(
            DiffAnalyzer::categorize_file("babel.config.js"),
            ChangeCategory::Config
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("webpack.config.prod.js"),
            ChangeCategory::Config
        );
    }

    #[test]
    fn test_categorize_file_env_file() {
        // .env exactly matches ends_with(".env")
        assert_eq!(
            DiffAnalyzer::categorize_file("app.env"),
            ChangeCategory::Config
        );
        // .env.production does NOT end with ".env" so it falls through to Modification
        assert_eq!(
            DiffAnalyzer::categorize_file(".env.production"),
            ChangeCategory::Modification
        );
    }

    #[test]
    fn test_categorize_file_docs_in_path() {
        assert_eq!(
            DiffAnalyzer::categorize_file("project/docs/setup.html"),
            ChangeCategory::Docs
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("project/doc/api/endpoints.html"),
            ChangeCategory::Docs
        );
    }

    #[test]
    fn test_categorize_file_rst_extension() {
        assert_eq!(
            DiffAnalyzer::categorize_file("docs/index.rst"),
            ChangeCategory::Docs
        );
    }

    #[test]
    fn test_categorize_file_spec_pattern() {
        assert_eq!(
            DiffAnalyzer::categorize_file("src/handler.spec.ts"),
            ChangeCategory::Tests
        );
        assert_eq!(
            DiffAnalyzer::categorize_file("src/handler_spec.rb"),
            ChangeCategory::Tests
        );
    }

    #[test]
    fn test_file_type_stats_many_extensions() {
        let files = vec![
            "a.rs".to_string(),
            "b.rs".to_string(),
            "c.ts".to_string(),
            "d.ts".to_string(),
            "e.ts".to_string(),
            "f.py".to_string(),
            "g.go".to_string(),
            "h.go".to_string(),
        ];
        let stats = DiffAnalyzer::file_type_stats(&files);
        assert_eq!(stats.get("rs"), Some(&2));
        assert_eq!(stats.get("ts"), Some(&3));
        assert_eq!(stats.get("py"), Some(&1));
        assert_eq!(stats.get("go"), Some(&2));
        assert_eq!(stats.len(), 4);
    }

    #[test]
    fn test_file_type_stats_uppercase_extensions() {
        let files = vec!["Image.PNG".to_string(), "Photo.JPG".to_string()];
        let stats = DiffAnalyzer::file_type_stats(&files);
        // Extensions should be lowercased
        assert_eq!(stats.get("png"), Some(&1));
        assert_eq!(stats.get("jpg"), Some(&1));
    }

    #[test]
    fn test_analyze_diff_file_types_populated() {
        let diff = "+++ b/src/main.rs\n+++ b/src/lib.rs\n+++ b/app.ts\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.file_types.get("rs"), Some(&2));
        assert_eq!(analysis.file_types.get("ts"), Some(&1));
    }

    #[test]
    fn test_analyze_diff_categories_deduplication() {
        // Multiple test files should only produce one Tests category
        let diff = "+++ b/tests/a.rs\n+++ b/tests/b.rs\n+++ b/tests/c.rs\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 3);
        assert_eq!(analysis.change_categories.len(), 1);
        assert_eq!(analysis.change_categories[0], ChangeCategory::Tests);
    }

    #[test]
    fn test_categorize_file_new_code_not_used_by_categorize() {
        // categorize_file never returns NewCode -- it defaults to Modification
        // for any source file that doesn't match other patterns
        assert_eq!(
            DiffAnalyzer::categorize_file("src/brand_new_module.rs"),
            ChangeCategory::Modification
        );
    }

    #[test]
    fn test_analyze_diff_trailing_whitespace_in_path() {
        let diff = "+++ b/src/main.rs   \n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 1);
        // The path should be trimmed
        assert_eq!(analysis.files_changed[0], "src/main.rs");
    }

    #[test]
    fn test_analyze_diff_only_context_lines_no_files() {
        let diff =
            "@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n }\n";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert!(analysis.files_changed.is_empty());
    }

    #[test]
    fn test_categorize_file_config_directory_pattern() {
        assert_eq!(
            DiffAnalyzer::categorize_file("src/config/settings.rs"),
            ChangeCategory::Config
        );
    }

    #[test]
    fn test_categorize_file_deeply_nested_test() {
        assert_eq!(
            DiffAnalyzer::categorize_file("packages/core/src/tests/integration/test_api.rs"),
            ChangeCategory::Tests
        );
    }

    #[test]
    fn test_analyze_diff_all_six_categories() {
        let diff = "\
+++ b/src/main.rs
+++ b/tests/test.rs
+++ b/README.md
+++ b/Cargo.toml
+++ b/Cargo.lock
+++ b/db/migrations/001.sql
";
        let analysis = DiffAnalyzer::analyze_diff(diff, 1, "url", "repo", 1);
        assert_eq!(analysis.files_changed.len(), 6);
        // Should have 6 distinct categories: Modification, Tests, Docs, Config, Dependencies, Migrations
        assert_eq!(
            analysis.change_categories.len(),
            6,
            "Should have 6 distinct categories, got: {:?}",
            analysis.change_categories
        );
    }
}
