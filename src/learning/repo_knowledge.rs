//! System 4: Per-repo knowledge accumulation manager.

use crate::error::Result;
use crate::storage::FixAttemptTracker;
use crate::types::{DiffAnalysis, PromotedInstruction, RepoKnowledge, ReviewPattern};
use chrono::Utc;

pub struct RepoKnowledgeManager;

impl RepoKnowledgeManager {
    /// Learn from a successful diff: extract common file patterns, directories.
    pub fn learn_from_diff(
        tracker: &dyn FixAttemptTracker,
        repo: &str,
        analysis: &DiffAnalysis,
    ) -> Result<()> {
        // Extract common fix directories
        let mut dirs = std::collections::HashSet::new();
        for file in &analysis.files_changed {
            if let Some(dir) = file.rsplit_once('/').map(|(d, _)| d) {
                dirs.insert(dir.to_string());
            }
        }

        for dir in dirs {
            let entry = RepoKnowledge {
                id: 0,
                repo: repo.to_string(),
                knowledge_key: "common_fix_dirs".to_string(),
                knowledge_value: dir,
                source_type: "diff_analysis".to_string(),
                confidence: 0.6,
                occurrence_count: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            tracker.upsert_repo_knowledge(&entry)?;
        }

        // Store file type conventions
        for (ext, count) in &analysis.file_types {
            if *count > 0 {
                let entry = RepoKnowledge {
                    id: 0,
                    repo: repo.to_string(),
                    knowledge_key: "file_conventions".to_string(),
                    knowledge_value: format!("Uses .{} files (seen {} in diffs)", ext, count),
                    source_type: "diff_analysis".to_string(),
                    confidence: 0.5,
                    occurrence_count: 1,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                };
                tracker.upsert_repo_knowledge(&entry)?;
            }
        }

        Ok(())
    }

    /// Learn from a promoted Q&A instruction.
    pub fn learn_from_promotion(
        tracker: &dyn FixAttemptTracker,
        repo: &str,
        instruction: &PromotedInstruction,
    ) -> Result<()> {
        let entry = RepoKnowledge {
            id: 0,
            repo: repo.to_string(),
            knowledge_key: "promoted_qa".to_string(),
            knowledge_value: instruction.instruction_text.clone(),
            source_type: "qa_promotion".to_string(),
            confidence: instruction.confidence,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        tracker.upsert_repo_knowledge(&entry)?;
        Ok(())
    }

    /// Learn from recurring review patterns.
    pub fn learn_from_review_pattern(
        tracker: &dyn FixAttemptTracker,
        repo: &str,
        pattern: &ReviewPattern,
    ) -> Result<()> {
        let entry = RepoKnowledge {
            id: 0,
            repo: repo.to_string(),
            knowledge_key: "review_preferences".to_string(),
            knowledge_value: format!("[{}] {}", pattern.category, pattern.pattern_text),
            source_type: "review_pattern".to_string(),
            confidence: 0.7,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        tracker.upsert_repo_knowledge(&entry)?;
        Ok(())
    }

    /// Format all repo knowledge as prompt context.
    pub fn format_knowledge_context(knowledge: &[RepoKnowledge]) -> String {
        if knowledge.is_empty() {
            return String::new();
        }

        let mut output = String::from("# Repo Knowledge\n\n");

        // Group by key
        let mut grouped: std::collections::HashMap<&str, Vec<&RepoKnowledge>> =
            std::collections::HashMap::new();
        for entry in knowledge {
            grouped.entry(&entry.knowledge_key).or_default().push(entry);
        }

        for (key, entries) in &grouped {
            let label = match *key {
                "common_fix_dirs" => "Common fix directories",
                "test_pattern" => "Test patterns",
                "file_conventions" => "File conventions",
                "review_preferences" => "Review preferences",
                "common_root_causes" => "Common root causes",
                "promoted_qa" => "Standing instructions",
                other => other,
            };

            output.push_str(&format!("## {}\n", label));
            for entry in entries.iter().take(5) {
                output.push_str(&format!("- {}\n", entry.knowledge_value));
            }
            output.push('\n');
        }

        output
    }

    /// Generate AGENT.md content from accumulated knowledge.
    pub fn generate_agent_md(
        knowledge: &[RepoKnowledge],
        instructions: &[PromotedInstruction],
    ) -> String {
        let mut output = String::from("# AGENT.md - Auto-generated by Claudear\n\n");
        output
            .push_str("This file contains accumulated knowledge from successful fix attempts.\n\n");

        if !instructions.is_empty() {
            output.push_str("## Standing Instructions\n\n");
            for instruction in instructions {
                output.push_str(&format!("- {}\n", instruction.instruction_text));
            }
            output.push('\n');
        }

        if !knowledge.is_empty() {
            output.push_str(&Self::format_knowledge_context(knowledge));
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_format_knowledge_context_empty() {
        assert!(RepoKnowledgeManager::format_knowledge_context(&[]).is_empty());
    }

    #[test]
    fn test_format_knowledge_context() {
        let knowledge = vec![
            RepoKnowledge {
                id: 1,
                repo: "foo/bar".to_string(),
                knowledge_key: "common_fix_dirs".to_string(),
                knowledge_value: "src/handlers".to_string(),
                source_type: "diff_analysis".to_string(),
                confidence: 0.8,
                occurrence_count: 3,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            RepoKnowledge {
                id: 2,
                repo: "foo/bar".to_string(),
                knowledge_key: "review_preferences".to_string(),
                knowledge_value: "[missing_tests] Always add tests".to_string(),
                source_type: "review_pattern".to_string(),
                confidence: 0.7,
                occurrence_count: 5,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];
        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);
        assert!(ctx.contains("Common fix directories"));
        assert!(ctx.contains("src/handlers"));
        assert!(ctx.contains("Review preferences"));
    }

    #[test]
    fn test_generate_agent_md() {
        let knowledge = vec![RepoKnowledge {
            id: 1,
            repo: "foo/bar".to_string(),
            knowledge_key: "test_pattern".to_string(),
            knowledge_value: "Run cargo test".to_string(),
            source_type: "strategy".to_string(),
            confidence: 0.9,
            occurrence_count: 5,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let instructions = vec![PromotedInstruction {
            id: 1,
            repo: "foo/bar".to_string(),
            source_type: "qa_promotion".to_string(),
            instruction_text: "Always use the async API".to_string(),
            occurrence_count: 3,
            confidence: 0.8,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let md = RepoKnowledgeManager::generate_agent_md(&knowledge, &instructions);
        assert!(md.contains("AGENT.md"));
        assert!(md.contains("Always use the async API"));
        assert!(md.contains("cargo test"));
    }

    #[test]
    fn test_generate_agent_md_no_instructions() {
        let knowledge = vec![RepoKnowledge {
            id: 1,
            repo: "foo/bar".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.7,
            occurrence_count: 3,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let md = RepoKnowledgeManager::generate_agent_md(&knowledge, &[]);
        assert!(md.contains("AGENT.md"));
        assert!(!md.contains("Standing Instructions"));
        assert!(md.contains("src/handlers"));
    }

    #[test]
    fn test_generate_agent_md_empty() {
        let md = RepoKnowledgeManager::generate_agent_md(&[], &[]);
        assert!(md.contains("AGENT.md"));
        // Should still have the header
        assert!(md.contains("Auto-generated by Claudear"));
    }

    #[test]
    fn test_format_knowledge_context_groups_by_key() {
        let knowledge = vec![
            RepoKnowledge {
                id: 1,
                repo: "foo/bar".to_string(),
                knowledge_key: "common_fix_dirs".to_string(),
                knowledge_value: "src/handlers".to_string(),
                source_type: "diff".to_string(),
                confidence: 0.7,
                occurrence_count: 3,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            RepoKnowledge {
                id: 2,
                repo: "foo/bar".to_string(),
                knowledge_key: "common_fix_dirs".to_string(),
                knowledge_value: "src/models".to_string(),
                source_type: "diff".to_string(),
                confidence: 0.6,
                occurrence_count: 2,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];
        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);
        assert!(ctx.contains("Common fix directories"));
        assert!(ctx.contains("src/handlers"));
        assert!(ctx.contains("src/models"));
        // Should be in the same section, only one header
        let header_count = ctx.matches("Common fix directories").count();
        assert_eq!(header_count, 1);
    }

    #[test]
    fn test_format_knowledge_context_limits_entries_per_key() {
        let knowledge: Vec<RepoKnowledge> = (0..10)
            .map(|i| RepoKnowledge {
                id: i,
                repo: "foo/bar".to_string(),
                knowledge_key: "common_fix_dirs".to_string(),
                knowledge_value: format!("src/dir{}", i),
                source_type: "diff".to_string(),
                confidence: 0.5,
                occurrence_count: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .collect();
        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);
        // Should show at most 5 entries per key
        assert!(ctx.contains("src/dir4"));
        assert!(!ctx.contains("src/dir5"));
    }

    #[test]
    fn test_format_knowledge_context_all_key_labels() {
        let keys = vec![
            ("common_fix_dirs", "Common fix directories"),
            ("test_pattern", "Test patterns"),
            ("file_conventions", "File conventions"),
            ("review_preferences", "Review preferences"),
            ("common_root_causes", "Common root causes"),
            ("promoted_qa", "Standing instructions"),
            ("custom_key", "custom_key"), // Fallthrough
        ];
        for (key, expected_label) in keys {
            let knowledge = vec![RepoKnowledge {
                id: 1,
                repo: "foo/bar".to_string(),
                knowledge_key: key.to_string(),
                knowledge_value: "value".to_string(),
                source_type: "test".to_string(),
                confidence: 0.5,
                occurrence_count: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }];
            let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);
            assert!(
                ctx.contains(expected_label),
                "Expected label '{}' for key '{}', got:\n{}",
                expected_label,
                key,
                ctx
            );
        }
    }

    // ── Integration tests with SqliteTracker ──

    #[test]
    fn test_learn_from_diff_stores_dirs_and_types() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec![
                "src/handlers/auth.rs".to_string(),
                "src/handlers/api.rs".to_string(),
                "src/models/user.rs".to_string(),
            ],
            file_types: {
                let mut m = std::collections::HashMap::new();
                m.insert("rs".to_string(), 3);
                m
            },
            change_categories: vec![],
            diff_summary: "test".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        // Should have stored common_fix_dirs entries
        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert!(!dirs.is_empty());
        let dir_values: Vec<&str> = dirs.iter().map(|d| d.knowledge_value.as_str()).collect();
        assert!(dir_values.contains(&"src/handlers"));
        assert!(dir_values.contains(&"src/models"));

        // Should have stored file_conventions
        let conventions = tracker
            .get_repo_knowledge_by_key("org/repo", "file_conventions")
            .unwrap();
        assert!(!conventions.is_empty());
        assert!(conventions[0].knowledge_value.contains(".rs"));
    }

    #[test]
    fn test_learn_from_diff_empty_analysis() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec![],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "empty".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let all = tracker.get_repo_knowledge("org/repo").unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn test_learn_from_diff_repeated_builds_occurrence_count() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let make_analysis = || DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["src/handlers/auth.rs".to_string()],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "test".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &make_analysis()).unwrap();
        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &make_analysis()).unwrap();
        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &make_analysis()).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].occurrence_count, 3);
    }

    #[test]
    fn test_learn_from_promotion_stores_knowledge() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let instruction = PromotedInstruction {
            id: 1,
            repo: "org/repo".to_string(),
            source_type: "qa_promotion".to_string(),
            instruction_text: "Always run tests before committing".to_string(),
            occurrence_count: 5,
            confidence: 0.9,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_promotion(&tracker, "org/repo", &instruction).unwrap();

        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "promoted_qa")
            .unwrap();
        assert_eq!(knowledge.len(), 1);
        assert_eq!(
            knowledge[0].knowledge_value,
            "Always run tests before committing"
        );
        assert_eq!(knowledge[0].source_type, "qa_promotion");
        assert!((knowledge[0].confidence - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn test_learn_from_review_pattern_stores_knowledge() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let pattern = crate::types::ReviewPattern {
            id: 1,
            github_repo: "org/repo".to_string(),
            category: crate::types::ReviewCategory::MissingTests,
            pattern_text: "Always add tests for new endpoints".to_string(),
            example_comments: vec!["Add tests!".to_string()],
            occurrence_count: 5,
            promoted_to_instruction: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_review_pattern(&tracker, "org/repo", &pattern).unwrap();

        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "review_preferences")
            .unwrap();
        assert_eq!(knowledge.len(), 1);
        assert!(knowledge[0].knowledge_value.contains("missing_tests"));
        assert!(knowledge[0].knowledge_value.contains("Always add tests"));
    }

    #[test]
    fn test_get_repo_knowledge_returns_all_keys() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // Store different types of knowledge
        let dir_entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.6,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        tracker.upsert_repo_knowledge(&dir_entry).unwrap();

        let test_entry = RepoKnowledge {
            knowledge_key: "test_pattern".to_string(),
            knowledge_value: "cargo test".to_string(),
            ..dir_entry.clone()
        };
        tracker.upsert_repo_knowledge(&test_entry).unwrap();

        let review_entry = RepoKnowledge {
            knowledge_key: "review_preferences".to_string(),
            knowledge_value: "always add tests".to_string(),
            ..dir_entry.clone()
        };
        tracker.upsert_repo_knowledge(&review_entry).unwrap();

        // get_repo_knowledge should return ALL keys
        let all = tracker.get_repo_knowledge("org/repo").unwrap();
        assert_eq!(all.len(), 3);
        let keys: Vec<&str> = all.iter().map(|k| k.knowledge_key.as_str()).collect();
        assert!(keys.contains(&"common_fix_dirs"));
        assert!(keys.contains(&"test_pattern"));
        assert!(keys.contains(&"review_preferences"));
    }

    #[test]
    fn test_full_pipeline_diff_to_knowledge_to_context() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // Step 1: Analyze a diff
        let diff = r#"diff --git a/src/handlers/auth.rs b/src/handlers/auth.rs
+++ b/src/handlers/auth.rs
diff --git a/src/handlers/api.rs b/src/handlers/api.rs
+++ b/src/handlers/api.rs
diff --git a/tests/test_auth.rs b/tests/test_auth.rs
+++ b/tests/test_auth.rs
"#;
        let analysis = crate::learning::DiffAnalyzer::analyze_diff(diff, 1, "url", "org/repo", 1);
        assert_eq!(analysis.files_changed.len(), 3);

        // Step 2: Learn from the diff
        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        // Step 3: Retrieve and format knowledge
        let knowledge = tracker.get_repo_knowledge("org/repo").unwrap();
        assert!(!knowledge.is_empty());

        let context = RepoKnowledgeManager::format_knowledge_context(&knowledge);
        assert!(context.contains("Repo Knowledge"));
        assert!(context.contains("src/handlers"));
    }

    #[test]
    fn test_full_pipeline_review_to_knowledge() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // Step 1: Process review comments multiple times to reach promotion threshold
        for _ in 0..3 {
            crate::learning::ReviewClassifier::process_review_comments(
                &tracker,
                "org/repo",
                &[],
                Some("Please add unit tests for this endpoint"),
            )
            .unwrap();
        }

        // Step 2: Check if pattern crossed threshold
        let promotable =
            crate::learning::ReviewClassifier::check_promotion_threshold(&tracker, "org/repo", 3)
                .unwrap();
        assert_eq!(promotable.len(), 1);

        // Step 3: Learn from promoted pattern
        for pattern in &promotable {
            RepoKnowledgeManager::learn_from_review_pattern(&tracker, "org/repo", pattern).unwrap();
        }

        // Step 4: Verify knowledge was stored
        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "review_preferences")
            .unwrap();
        assert_eq!(knowledge.len(), 1);
        assert!(knowledge[0].knowledge_value.contains("unit tests"));

        // Step 5: Generate AGENT.md
        let all_knowledge = tracker.get_repo_knowledge("org/repo").unwrap();
        let instructions = tracker.get_promoted_instructions("org/repo").unwrap();
        let agent_md = RepoKnowledgeManager::generate_agent_md(&all_knowledge, &instructions);
        assert!(agent_md.contains("AGENT.md"));
        assert!(agent_md.contains("Review preferences"));
    }

    // ── Bug-hunting: Knowledge storage and retrieval edge cases ──

    #[test]
    fn test_learn_from_diff_file_without_directory() {
        // Files at the repo root (no '/') should NOT produce a common_fix_dirs entry.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["Makefile".to_string(), "README.md".to_string()],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "root-only files".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert!(
            dirs.is_empty(),
            "Root-level files should not create directory entries, got: {:?}",
            dirs.iter().map(|d| &d.knowledge_value).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_learn_from_diff_deeply_nested_paths() {
        // Deeply nested files should only produce their immediate parent dir,
        // not intermediate components.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["a/b/c/d/e/deep.rs".to_string()],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "deep path".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].knowledge_value, "a/b/c/d/e");
    }

    #[test]
    fn test_learn_from_diff_file_type_zero_count_skipped() {
        // File types with count == 0 should NOT be stored.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec![],
            file_types: {
                let mut m = std::collections::HashMap::new();
                m.insert("rs".to_string(), 0);
                m.insert("py".to_string(), 2);
                m
            },
            change_categories: vec![],
            diff_summary: "test".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let conventions = tracker
            .get_repo_knowledge_by_key("org/repo", "file_conventions")
            .unwrap();
        assert_eq!(conventions.len(), 1);
        assert!(conventions[0].knowledge_value.contains(".py"));
        assert!(
            !conventions[0].knowledge_value.contains(".rs"),
            "Zero-count file type should not be stored"
        );
    }

    #[test]
    fn test_knowledge_isolation_between_repos() {
        // Knowledge for repo A must not leak into repo B.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let make_entry = |repo: &str, val: &str| RepoKnowledge {
            id: 0,
            repo: repo.to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: val.to_string(),
            source_type: "diff".to_string(),
            confidence: 0.6,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker
            .upsert_repo_knowledge(&make_entry("org/alpha", "src/alpha"))
            .unwrap();
        tracker
            .upsert_repo_knowledge(&make_entry("org/beta", "src/beta"))
            .unwrap();

        let alpha_k = tracker.get_repo_knowledge("org/alpha").unwrap();
        let beta_k = tracker.get_repo_knowledge("org/beta").unwrap();

        assert_eq!(alpha_k.len(), 1);
        assert_eq!(alpha_k[0].knowledge_value, "src/alpha");
        assert_eq!(beta_k.len(), 1);
        assert_eq!(beta_k[0].knowledge_value, "src/beta");
    }

    #[test]
    fn test_get_repo_knowledge_for_nonexistent_repo() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let knowledge = tracker.get_repo_knowledge("nonexistent/repo").unwrap();
        assert!(knowledge.is_empty());
    }

    #[test]
    fn test_get_repo_knowledge_by_key_nonexistent_key() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.6,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        tracker.upsert_repo_knowledge(&entry).unwrap();

        let result = tracker
            .get_repo_knowledge_by_key("org/repo", "nonexistent_key")
            .unwrap();
        assert!(result.is_empty());
    }

    // ── Bug-hunting: Deduplication of knowledge entries ──

    #[test]
    fn test_upsert_deduplicates_by_repo_key_value_triple() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.5,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker.upsert_repo_knowledge(&entry).unwrap();
        tracker.upsert_repo_knowledge(&entry).unwrap();
        tracker.upsert_repo_knowledge(&entry).unwrap();

        let all = tracker.get_repo_knowledge("org/repo").unwrap();
        assert_eq!(
            all.len(),
            1,
            "Duplicate entries should be merged, not duplicated"
        );
        assert_eq!(
            all[0].occurrence_count, 3,
            "Occurrence count should reflect all upserts"
        );
    }

    #[test]
    fn test_upsert_same_key_different_values_are_separate() {
        // Same knowledge_key but different knowledge_value -> distinct entries.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry_a = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.6,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let entry_b = RepoKnowledge {
            knowledge_value: "src/models".to_string(),
            ..entry_a.clone()
        };

        tracker.upsert_repo_knowledge(&entry_a).unwrap();
        tracker.upsert_repo_knowledge(&entry_b).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn test_upsert_confidence_updated_on_duplicate() {
        // When a duplicate entry is upserted with higher confidence, the
        // stored confidence should be updated.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.5,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker.upsert_repo_knowledge(&entry).unwrap();

        let updated_entry = RepoKnowledge {
            confidence: 0.9,
            ..entry.clone()
        };
        tracker.upsert_repo_knowledge(&updated_entry).unwrap();

        let stored = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert!(
            (stored[0].confidence - 0.9).abs() < f64::EPSILON,
            "Confidence should be updated to 0.9 on upsert, got {}",
            stored[0].confidence
        );
    }

    #[test]
    fn test_learn_from_diff_deduplicates_directories() {
        // Multiple files in the same directory should only produce ONE dir entry.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec![
                "src/handlers/auth.rs".to_string(),
                "src/handlers/api.rs".to_string(),
                "src/handlers/user.rs".to_string(),
            ],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "test".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert_eq!(
            dirs.len(),
            1,
            "3 files in same directory should produce only 1 directory entry"
        );
        assert_eq!(dirs[0].knowledge_value, "src/handlers");
    }

    // ── Bug-hunting: Confidence scoring and thresholds ──

    #[test]
    fn test_learn_from_diff_initial_confidence_values() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["src/main.rs".to_string()],
            file_types: {
                let mut m = std::collections::HashMap::new();
                m.insert("rs".to_string(), 1);
                m
            },
            change_categories: vec![],
            diff_summary: "test".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert_eq!(dirs.len(), 1);
        assert!(
            (dirs[0].confidence - 0.6).abs() < f64::EPSILON,
            "common_fix_dirs initial confidence should be 0.6, got {}",
            dirs[0].confidence
        );

        let conventions = tracker
            .get_repo_knowledge_by_key("org/repo", "file_conventions")
            .unwrap();
        assert_eq!(conventions.len(), 1);
        assert!(
            (conventions[0].confidence - 0.5).abs() < f64::EPSILON,
            "file_conventions initial confidence should be 0.5, got {}",
            conventions[0].confidence
        );
    }

    #[test]
    fn test_learn_from_promotion_preserves_instruction_confidence() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        for confidence in [0.0, 0.1, 0.5, 0.99, 1.0] {
            let instruction = PromotedInstruction {
                id: 1,
                repo: "org/repo".to_string(),
                source_type: "qa_promotion".to_string(),
                instruction_text: format!("instruction at {}", confidence),
                occurrence_count: 1,
                confidence,
                is_active: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };

            RepoKnowledgeManager::learn_from_promotion(&tracker, "org/repo", &instruction).unwrap();
        }

        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "promoted_qa")
            .unwrap();
        assert_eq!(
            knowledge.len(),
            5,
            "Each instruction text is unique, so 5 entries expected"
        );

        for entry in &knowledge {
            let expected: f64 = entry
                .knowledge_value
                .rsplit(' ')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(
                (entry.confidence - expected).abs() < f64::EPSILON,
                "Stored confidence {} should match instruction confidence {}",
                entry.confidence,
                expected
            );
        }
    }

    #[test]
    fn test_learn_from_review_pattern_confidence_is_fixed_at_0_7() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let pattern = crate::types::ReviewPattern {
            id: 1,
            github_repo: "org/repo".to_string(),
            category: crate::types::ReviewCategory::Security,
            pattern_text: "Never store secrets in code".to_string(),
            example_comments: vec![],
            occurrence_count: 100,
            promoted_to_instruction: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_review_pattern(&tracker, "org/repo", &pattern).unwrap();

        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "review_preferences")
            .unwrap();
        assert_eq!(knowledge.len(), 1);
        assert!(
            (knowledge[0].confidence - 0.7).abs() < f64::EPSILON,
            "Review pattern confidence should always be 0.7, got {}",
            knowledge[0].confidence
        );
    }

    // ── Bug-hunting: Format of stored knowledge (valid markdown, proper escaping) ──

    #[test]
    fn test_format_knowledge_context_produces_valid_markdown_headers() {
        let knowledge = vec![RepoKnowledge {
            id: 1,
            repo: "foo/bar".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.8,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);

        // Should start with a level-1 heading
        assert!(ctx.starts_with("# Repo Knowledge\n\n"));
        // Each section should have a level-2 heading
        assert!(ctx.contains("## Common fix directories\n"));
        // Each value should be a markdown list item
        assert!(ctx.contains("- src/handlers\n"));
    }

    #[test]
    fn test_format_knowledge_context_with_markdown_special_chars_in_values() {
        // Knowledge values containing markdown special characters should be
        // rendered as-is (no double-escaping or corruption).
        let knowledge = vec![
            RepoKnowledge {
                id: 1,
                repo: "foo/bar".to_string(),
                knowledge_key: "review_preferences".to_string(),
                knowledge_value: "[style] Use `async fn` instead of `fn` + `.await`".to_string(),
                source_type: "review".to_string(),
                confidence: 0.7,
                occurrence_count: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            RepoKnowledge {
                id: 2,
                repo: "foo/bar".to_string(),
                knowledge_key: "common_root_causes".to_string(),
                knowledge_value: "Missing `#[derive(Debug)]` on **error** types".to_string(),
                source_type: "analysis".to_string(),
                confidence: 0.6,
                occurrence_count: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];

        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);
        assert!(ctx.contains("Use `async fn` instead of `fn` + `.await`"));
        assert!(ctx.contains("Missing `#[derive(Debug)]` on **error** types"));
    }

    #[test]
    fn test_format_knowledge_context_newline_in_value() {
        // A knowledge_value that contains a newline should still produce
        // coherent output (even if imperfect, it must not panic).
        let knowledge = vec![RepoKnowledge {
            id: 1,
            repo: "foo/bar".to_string(),
            knowledge_key: "promoted_qa".to_string(),
            knowledge_value: "Line one\nLine two".to_string(),
            source_type: "qa".to_string(),
            confidence: 0.8,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];

        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);
        // Should at least contain both lines, not panic or truncate
        assert!(ctx.contains("Line one"));
        assert!(ctx.contains("Line two"));
    }

    #[test]
    fn test_generate_agent_md_header_format() {
        let md = RepoKnowledgeManager::generate_agent_md(&[], &[]);
        // Must start with an H1
        assert!(md.starts_with("# AGENT.md - Auto-generated by Claudear\n\n"));
        // Must contain the description line
        assert!(md.contains("accumulated knowledge from successful fix attempts"));
    }

    #[test]
    fn test_generate_agent_md_standing_instructions_section_only_with_instructions() {
        let instructions = vec![PromotedInstruction {
            id: 1,
            repo: "foo/bar".to_string(),
            source_type: "qa_promotion".to_string(),
            instruction_text: "Always run clippy".to_string(),
            occurrence_count: 5,
            confidence: 0.9,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];

        let md_with = RepoKnowledgeManager::generate_agent_md(&[], &instructions);
        assert!(md_with.contains("## Standing Instructions\n\n"));
        assert!(md_with.contains("- Always run clippy\n"));

        let md_without = RepoKnowledgeManager::generate_agent_md(&[], &[]);
        assert!(!md_without.contains("Standing Instructions"));
    }

    #[test]
    fn test_learn_from_review_pattern_format_bracket_category() {
        // Verify the stored value has the format "[category] text".
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let categories = vec![
            (crate::types::ReviewCategory::MissingTests, "missing_tests"),
            (crate::types::ReviewCategory::StyleIssue, "style_issue"),
            (
                crate::types::ReviewCategory::WrongApproach,
                "wrong_approach",
            ),
            (crate::types::ReviewCategory::Incomplete, "incomplete"),
            (crate::types::ReviewCategory::Security, "security"),
            (crate::types::ReviewCategory::Performance, "performance"),
            (crate::types::ReviewCategory::Documentation, "documentation"),
            (crate::types::ReviewCategory::Other, "other"),
        ];

        for (cat, expected_label) in categories {
            let pattern = crate::types::ReviewPattern {
                id: 1,
                github_repo: "org/repo".to_string(),
                category: cat,
                pattern_text: format!("Pattern for {}", expected_label),
                example_comments: vec![],
                occurrence_count: 1,
                promoted_to_instruction: false,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };

            RepoKnowledgeManager::learn_from_review_pattern(&tracker, "org/repo", &pattern)
                .unwrap();
        }

        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "review_preferences")
            .unwrap();
        assert_eq!(
            knowledge.len(),
            8,
            "Each category should produce a distinct entry"
        );

        for entry in &knowledge {
            let val = &entry.knowledge_value;
            assert!(
                val.starts_with('[') && val.contains("] "),
                "Expected format '[category] text', got: {}",
                val
            );
        }
    }

    // ── Bug-hunting: Empty/null handling in all fields ──

    #[test]
    fn test_learn_from_diff_empty_file_path() {
        // An empty string file path: no slash, so no directory entry.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["".to_string()],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "".to_string(),
            created_at: Utc::now(),
        };

        // Should not panic
        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        assert!(
            dirs.is_empty(),
            "Empty file path should not create a directory entry"
        );
    }

    #[test]
    fn test_learn_from_promotion_empty_instruction_text() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let instruction = PromotedInstruction {
            id: 1,
            repo: "org/repo".to_string(),
            source_type: "qa_promotion".to_string(),
            instruction_text: "".to_string(),
            occurrence_count: 1,
            confidence: 0.5,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Should not panic; empty text is stored as-is
        RepoKnowledgeManager::learn_from_promotion(&tracker, "org/repo", &instruction).unwrap();

        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "promoted_qa")
            .unwrap();
        assert_eq!(knowledge.len(), 1);
        assert_eq!(knowledge[0].knowledge_value, "");
    }

    #[test]
    fn test_learn_from_review_pattern_empty_pattern_text() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let pattern = crate::types::ReviewPattern {
            id: 1,
            github_repo: "org/repo".to_string(),
            category: crate::types::ReviewCategory::Other,
            pattern_text: "".to_string(),
            example_comments: vec![],
            occurrence_count: 1,
            promoted_to_instruction: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_review_pattern(&tracker, "org/repo", &pattern).unwrap();

        let knowledge = tracker
            .get_repo_knowledge_by_key("org/repo", "review_preferences")
            .unwrap();
        assert_eq!(knowledge.len(), 1);
        assert_eq!(knowledge[0].knowledge_value, "[other] ");
    }

    #[test]
    fn test_learn_from_diff_empty_repo_name() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "".to_string(),
            pr_number: 1,
            files_changed: vec!["src/main.rs".to_string()],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "test".to_string(),
            created_at: Utc::now(),
        };

        // Should succeed (empty repo is a valid string, just unusual)
        RepoKnowledgeManager::learn_from_diff(&tracker, "", &analysis).unwrap();

        let knowledge = tracker.get_repo_knowledge("").unwrap();
        assert!(
            !knowledge.is_empty(),
            "Empty repo name should still store entries"
        );
    }

    #[test]
    fn test_format_knowledge_context_empty_values() {
        let knowledge = vec![RepoKnowledge {
            id: 1,
            repo: "foo/bar".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.5,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];

        // Should not panic; empty value renders as "- \n"
        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);
        assert!(ctx.contains("# Repo Knowledge"));
        assert!(ctx.contains("## Common fix directories"));
    }

    #[test]
    fn test_generate_agent_md_with_empty_instruction_text() {
        let instructions = vec![PromotedInstruction {
            id: 1,
            repo: "foo/bar".to_string(),
            source_type: "qa_promotion".to_string(),
            instruction_text: "".to_string(),
            occurrence_count: 1,
            confidence: 0.5,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];

        let md = RepoKnowledgeManager::generate_agent_md(&[], &instructions);
        assert!(md.contains("Standing Instructions"));
        // Should have an empty list item but not panic
        assert!(md.contains("- \n"));
    }

    // ── Bug-hunting: Ordering and retrieval semantics ──

    #[test]
    fn test_get_repo_knowledge_ordered_by_occurrence_count_desc() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let make_entry = |val: &str, occ: i64| RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: val.to_string(),
            source_type: "diff".to_string(),
            confidence: 0.6,
            occurrence_count: occ,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Insert in random order
        tracker
            .upsert_repo_knowledge(&make_entry("src/rare", 1))
            .unwrap();
        tracker
            .upsert_repo_knowledge(&make_entry("src/common", 1))
            .unwrap();
        tracker
            .upsert_repo_knowledge(&make_entry("src/medium", 1))
            .unwrap();

        // Bump occurrence counts to desired levels via repeated upserts
        for _ in 0..9 {
            tracker
                .upsert_repo_knowledge(&make_entry("src/common", 1))
                .unwrap();
        }
        for _ in 0..4 {
            tracker
                .upsert_repo_knowledge(&make_entry("src/medium", 1))
                .unwrap();
        }

        let all = tracker.get_repo_knowledge("org/repo").unwrap();
        assert_eq!(all.len(), 3);
        // Results should be ordered by occurrence_count DESC
        assert!(
            all[0].occurrence_count >= all[1].occurrence_count,
            "Expected descending order: {} >= {}",
            all[0].occurrence_count,
            all[1].occurrence_count
        );
        assert!(
            all[1].occurrence_count >= all[2].occurrence_count,
            "Expected descending order: {} >= {}",
            all[1].occurrence_count,
            all[2].occurrence_count
        );
        assert_eq!(all[0].knowledge_value, "src/common");
    }

    #[test]
    fn test_format_knowledge_context_takes_only_first_5_per_key() {
        // Create 8 entries with two different keys (8 each)
        let mut knowledge = Vec::new();
        for i in 0..8 {
            knowledge.push(RepoKnowledge {
                id: i,
                repo: "foo/bar".to_string(),
                knowledge_key: "common_fix_dirs".to_string(),
                knowledge_value: format!("dir_{}", i),
                source_type: "diff".to_string(),
                confidence: 0.5,
                occurrence_count: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            });
        }
        for i in 0..8 {
            knowledge.push(RepoKnowledge {
                id: 100 + i,
                repo: "foo/bar".to_string(),
                knowledge_key: "test_pattern".to_string(),
                knowledge_value: format!("test_{}", i),
                source_type: "analysis".to_string(),
                confidence: 0.5,
                occurrence_count: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            });
        }

        let ctx = RepoKnowledgeManager::format_knowledge_context(&knowledge);

        // Each key should show at most 5 entries
        let dir_count = ctx.matches("dir_").count();
        let test_count = ctx.matches("test_").count();
        assert_eq!(
            dir_count, 5,
            "Should limit to 5 entries per key for common_fix_dirs"
        );
        assert_eq!(
            test_count, 5,
            "Should limit to 5 entries per key for test_pattern"
        );
    }

    // ── Bug-hunting: Unicode and special characters ──

    #[test]
    fn test_knowledge_with_unicode_values() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "review_preferences".to_string(),
            knowledge_value: "Use proper i18n: \u{00e9}\u{00e8}\u{00ea} and CJK: \u{4f60}\u{597d}"
                .to_string(),
            source_type: "review".to_string(),
            confidence: 0.7,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker.upsert_repo_knowledge(&entry).unwrap();

        let stored = tracker
            .get_repo_knowledge_by_key("org/repo", "review_preferences")
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].knowledge_value,
            "Use proper i18n: \u{00e9}\u{00e8}\u{00ea} and CJK: \u{4f60}\u{597d}"
        );
    }

    #[test]
    fn test_knowledge_with_very_long_value() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let long_value = "x".repeat(10_000);
        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_root_causes".to_string(),
            knowledge_value: long_value.clone(),
            source_type: "analysis".to_string(),
            confidence: 0.5,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker.upsert_repo_knowledge(&entry).unwrap();

        let stored = tracker
            .get_repo_knowledge_by_key("org/repo", "common_root_causes")
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].knowledge_value.len(), 10_000);
    }

    #[test]
    fn test_format_knowledge_context_with_sql_injection_attempt_in_value() {
        // This tests that stored values with SQL-like content don't
        // cause issues when retrieved and formatted.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "promoted_qa".to_string(),
            knowledge_value: "Robert'); DROP TABLE repo_knowledge;--".to_string(),
            source_type: "test".to_string(),
            confidence: 0.5,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker.upsert_repo_knowledge(&entry).unwrap();

        // Table should still be intact
        let stored = tracker.get_repo_knowledge("org/repo").unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0]
            .knowledge_value
            .contains("DROP TABLE repo_knowledge"));

        // Formatting should also work without panic
        let ctx = RepoKnowledgeManager::format_knowledge_context(&stored);
        assert!(ctx.contains("DROP TABLE"));
    }

    // ── Bug-hunting: Multiple learn calls from different sources ──

    #[test]
    fn test_mixed_learning_sources_all_coexist() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // learn_from_diff
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["src/api/handler.rs".to_string()],
            file_types: {
                let mut m = std::collections::HashMap::new();
                m.insert("rs".to_string(), 1);
                m
            },
            change_categories: vec![],
            diff_summary: "fix handler".to_string(),
            created_at: Utc::now(),
        };
        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        // learn_from_promotion
        let instruction = PromotedInstruction {
            id: 1,
            repo: "org/repo".to_string(),
            source_type: "qa_promotion".to_string(),
            instruction_text: "Always check error handling".to_string(),
            occurrence_count: 3,
            confidence: 0.85,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        RepoKnowledgeManager::learn_from_promotion(&tracker, "org/repo", &instruction).unwrap();

        // learn_from_review_pattern
        let pattern = crate::types::ReviewPattern {
            id: 1,
            github_repo: "org/repo".to_string(),
            category: crate::types::ReviewCategory::MissingTests,
            pattern_text: "Add integration tests".to_string(),
            example_comments: vec!["Need tests".to_string()],
            occurrence_count: 5,
            promoted_to_instruction: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        RepoKnowledgeManager::learn_from_review_pattern(&tracker, "org/repo", &pattern).unwrap();

        // All should coexist
        let all = tracker.get_repo_knowledge("org/repo").unwrap();
        let keys: std::collections::HashSet<&str> =
            all.iter().map(|k| k.knowledge_key.as_str()).collect();
        assert!(keys.contains("common_fix_dirs"));
        assert!(keys.contains("file_conventions"));
        assert!(keys.contains("promoted_qa"));
        assert!(keys.contains("review_preferences"));

        // Generate AGENT.md with all knowledge
        let instructions = vec![instruction];
        let md = RepoKnowledgeManager::generate_agent_md(&all, &instructions);
        assert!(md.contains("Standing Instructions"));
        assert!(md.contains("Always check error handling"));
        assert!(md.contains("Common fix directories"));
        assert!(md.contains("Standing instructions")); // promoted_qa section label
        assert!(md.contains("File conventions"));
        assert!(md.contains("Review preferences"));
    }

    #[test]
    fn test_upsert_returns_valid_id() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.6,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let id1 = tracker.upsert_repo_knowledge(&entry).unwrap();
        assert!(id1 > 0, "First insert should return a positive id");

        // Second upsert of same entry should still return a valid id
        let id2 = tracker.upsert_repo_knowledge(&entry).unwrap();
        assert!(
            id2 > 0,
            "Upsert of existing entry should return a positive id"
        );
        assert_eq!(
            id1, id2,
            "Upserting the same entry should return the same id"
        );
    }

    #[test]
    fn test_learn_from_diff_with_trailing_slash_in_path() {
        // A file path ending with a slash is unusual but should not panic.
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["src/handlers/".to_string()],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "trailing slash".to_string(),
            created_at: Utc::now(),
        };

        // Should not panic
        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let dirs = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        // The rsplit_once('/') on "src/handlers/" yields ("src/handlers", "")
        // so we get "src/handlers" as a directory, which is reasonable.
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].knowledge_value, "src/handlers");
    }

    #[test]
    fn test_learn_from_diff_multiple_file_types() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let analysis = DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "url".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec![],
            file_types: {
                let mut m = std::collections::HashMap::new();
                m.insert("rs".to_string(), 5);
                m.insert("toml".to_string(), 2);
                m.insert("md".to_string(), 1);
                m
            },
            change_categories: vec![],
            diff_summary: "multiple types".to_string(),
            created_at: Utc::now(),
        };

        RepoKnowledgeManager::learn_from_diff(&tracker, "org/repo", &analysis).unwrap();

        let conventions = tracker
            .get_repo_knowledge_by_key("org/repo", "file_conventions")
            .unwrap();
        assert_eq!(conventions.len(), 3);

        let values: Vec<&str> = conventions
            .iter()
            .map(|c| c.knowledge_value.as_str())
            .collect();

        // Check each file type is mentioned
        assert!(values.iter().any(|v| v.contains(".rs")));
        assert!(values.iter().any(|v| v.contains(".toml")));
        assert!(values.iter().any(|v| v.contains(".md")));

        // Check counts are in the formatted string
        assert!(values.iter().any(|v| v.contains("seen 5 in diffs")));
        assert!(values.iter().any(|v| v.contains("seen 2 in diffs")));
        assert!(values.iter().any(|v| v.contains("seen 1 in diffs")));
    }

    #[test]
    fn test_updated_at_advances_on_upsert() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        let entry = RepoKnowledge {
            id: 0,
            repo: "org/repo".to_string(),
            knowledge_key: "common_fix_dirs".to_string(),
            knowledge_value: "src/handlers".to_string(),
            source_type: "diff".to_string(),
            confidence: 0.6,
            occurrence_count: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker.upsert_repo_knowledge(&entry).unwrap();
        let first = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        let first_updated = first[0].updated_at;

        // Small delay to ensure time advances
        std::thread::sleep(std::time::Duration::from_millis(10));

        tracker.upsert_repo_knowledge(&entry).unwrap();
        let second = tracker
            .get_repo_knowledge_by_key("org/repo", "common_fix_dirs")
            .unwrap();
        let second_updated = second[0].updated_at;

        assert!(
            second_updated >= first_updated,
            "updated_at should advance on upsert: {:?} >= {:?}",
            second_updated,
            first_updated
        );
    }
}
