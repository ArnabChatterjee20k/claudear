//! System 5: Classify review feedback into categories and detect patterns.

use crate::error::Result;
use crate::github::PrReviewComment;
use crate::storage::FixAttemptTracker;
use crate::types::{ReviewCategory, ReviewPattern};
use chrono::Utc;

pub struct ReviewClassifier;

impl ReviewClassifier {
    /// Classify a review comment into a category via keyword matching.
    pub fn classify(comment_body: &str) -> ReviewCategory {
        let lower = comment_body.to_lowercase();

        // Check each category by keyword presence
        let categories = [
            (
                ReviewCategory::Security,
                &[
                    "security",
                    "vulnerability",
                    "sanitize",
                    "escape",
                    "inject",
                    "xss",
                    "csrf",
                    "auth",
                ][..],
            ),
            (
                ReviewCategory::MissingTests,
                &[
                    "test",
                    "coverage",
                    "spec",
                    "assert",
                    "verify",
                    "unit test",
                    "integration test",
                ],
            ),
            (
                ReviewCategory::WrongApproach,
                &[
                    "approach",
                    "instead",
                    "should use",
                    "better to",
                    "don't",
                    "shouldn't",
                    "wrong way",
                    "not the right",
                ],
            ),
            (
                ReviewCategory::StyleIssue,
                &[
                    "style",
                    "format",
                    "naming",
                    "convention",
                    "lint",
                    "indentation",
                    "whitespace",
                ],
            ),
            (
                ReviewCategory::Incomplete,
                &[
                    "incomplete",
                    "missing",
                    "also need",
                    "forgot",
                    "what about",
                    "still need",
                    "not handling",
                ],
            ),
            (
                ReviewCategory::Performance,
                &[
                    "performance",
                    "slow",
                    "optimize",
                    "efficient",
                    "cache",
                    "n+1",
                    "complexity",
                ],
            ),
            (
                ReviewCategory::Documentation,
                &[
                    "document",
                    "comment",
                    "explain",
                    "readme",
                    "jsdoc",
                    "rustdoc",
                    "docstring",
                ],
            ),
        ];

        for (category, keywords) in &categories {
            if keywords.iter().any(|kw| lower.contains(kw)) {
                return *category;
            }
        }

        ReviewCategory::Other
    }

    /// Process review comments for a PR, storing/updating patterns.
    pub fn process_review_comments(
        tracker: &dyn FixAttemptTracker,
        repo: &str,
        comments: &[PrReviewComment],
        review_body: Option<&str>,
    ) -> Result<Vec<ReviewPattern>> {
        let mut patterns = Vec::new();

        // Classify the review body if present
        if let Some(body) = review_body {
            if !body.trim().is_empty() {
                let category = Self::classify(body);
                let pattern = ReviewPattern {
                    id: 0,
                    github_repo: repo.to_string(),
                    category,
                    pattern_text: truncate(body, 200),
                    example_comments: vec![truncate(body, 500)],
                    occurrence_count: 1,
                    promoted_to_instruction: false,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                };
                tracker.upsert_review_pattern(&pattern)?;
                patterns.push(pattern);
            }
        }

        // Classify each inline comment
        for comment in comments {
            let category = Self::classify(&comment.body);
            let pattern = ReviewPattern {
                id: 0,
                github_repo: repo.to_string(),
                category,
                pattern_text: truncate(&comment.body, 200),
                example_comments: vec![truncate(&comment.body, 500)],
                occurrence_count: 1,
                promoted_to_instruction: false,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            tracker.upsert_review_pattern(&pattern)?;
            patterns.push(pattern);
        }

        Ok(patterns)
    }

    /// Check if any patterns have crossed the promotion threshold.
    pub fn check_promotion_threshold(
        tracker: &dyn FixAttemptTracker,
        repo: &str,
        threshold: usize,
    ) -> Result<Vec<ReviewPattern>> {
        let patterns = tracker.get_review_patterns(repo, 100)?;
        let promoted: Vec<ReviewPattern> = patterns
            .into_iter()
            .filter(|p| p.occurrence_count >= threshold as i64 && !p.promoted_to_instruction)
            .collect();
        Ok(promoted)
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len < 4 {
        // Too short for meaningful "X..." — just take first max_len bytes
        // (find a safe char boundary)
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i < max_len)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        s[..end].to_string()
    } else {
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i <= max_len.saturating_sub(3))
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_missing_tests() {
        assert_eq!(
            ReviewClassifier::classify("Please add tests for this change"),
            ReviewCategory::MissingTests
        );
    }

    #[test]
    fn test_classify_security() {
        assert_eq!(
            ReviewClassifier::classify("This could be a security vulnerability"),
            ReviewCategory::Security
        );
    }

    #[test]
    fn test_classify_style() {
        assert_eq!(
            ReviewClassifier::classify("The naming convention doesn't match our style guide"),
            ReviewCategory::StyleIssue
        );
    }

    #[test]
    fn test_classify_wrong_approach() {
        assert_eq!(
            ReviewClassifier::classify("You should use a different approach instead"),
            ReviewCategory::WrongApproach
        );
    }

    #[test]
    fn test_classify_incomplete() {
        assert_eq!(
            ReviewClassifier::classify("This is incomplete, you're also missing error handling"),
            ReviewCategory::Incomplete
        );
    }

    #[test]
    fn test_classify_performance() {
        assert_eq!(
            ReviewClassifier::classify("This could be slow, consider adding a cache"),
            ReviewCategory::Performance
        );
    }

    #[test]
    fn test_classify_documentation() {
        assert_eq!(
            ReviewClassifier::classify("Please add a comment explaining this logic"),
            ReviewCategory::Documentation
        );
    }

    #[test]
    fn test_classify_other() {
        assert_eq!(
            ReviewClassifier::classify("Looks good to me"),
            ReviewCategory::Other
        );
    }

    #[test]
    fn test_classify_case_insensitive() {
        assert_eq!(
            ReviewClassifier::classify("PLEASE ADD TESTS"),
            ReviewCategory::MissingTests
        );
        assert_eq!(
            ReviewClassifier::classify("SECURITY vulnerability"),
            ReviewCategory::Security
        );
    }

    #[test]
    fn test_classify_security_keywords_comprehensive() {
        assert_eq!(
            ReviewClassifier::classify("potential xss issue"),
            ReviewCategory::Security
        );
        assert_eq!(
            ReviewClassifier::classify("csrf token missing"),
            ReviewCategory::Security
        );
        assert_eq!(
            ReviewClassifier::classify("auth not verified"),
            ReviewCategory::Security
        );
        assert_eq!(
            ReviewClassifier::classify("need to sanitize input"),
            ReviewCategory::Security
        );
    }

    #[test]
    fn test_classify_performance_keywords() {
        assert_eq!(
            ReviewClassifier::classify("this is an n+1 query"),
            ReviewCategory::Performance
        );
        assert_eq!(
            ReviewClassifier::classify("time complexity is O(n^2)"),
            ReviewCategory::Performance
        );
        assert_eq!(
            ReviewClassifier::classify("we should optimize this"),
            ReviewCategory::Performance
        );
    }

    #[test]
    fn test_classify_incomplete_keywords() {
        assert_eq!(
            ReviewClassifier::classify("what about error handling?"),
            ReviewCategory::Incomplete
        );
        assert_eq!(
            ReviewClassifier::classify("still need to handle the edge case"),
            ReviewCategory::Incomplete
        );
        assert_eq!(
            ReviewClassifier::classify("you're not handling null values"),
            ReviewCategory::Incomplete
        );
    }

    #[test]
    fn test_security_takes_precedence_over_tests() {
        // "security" appears before "test" in the priority list
        assert_eq!(
            ReviewClassifier::classify("add security tests"),
            ReviewCategory::Security
        );
    }

    #[test]
    fn test_truncate_function() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("hello world", 8), "hello...");
        assert_eq!(truncate("", 10), "");
        // Exact length should not truncate
        assert_eq!(truncate("exact", 5), "exact");
    }

    #[test]
    fn test_truncate_unicode() {
        let emoji_text = "Hello 🌍 world";
        let truncated = truncate(emoji_text, 8);
        assert!(truncated.len() <= 12); // Should be safe with unicode
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_classify_empty_string() {
        assert_eq!(ReviewClassifier::classify(""), ReviewCategory::Other);
    }

    #[test]
    fn test_classify_wrong_approach_keywords() {
        assert_eq!(
            ReviewClassifier::classify("this is the wrong way to do it"),
            ReviewCategory::WrongApproach
        );
        assert_eq!(
            ReviewClassifier::classify("not the right pattern here"),
            ReviewCategory::WrongApproach
        );
        assert_eq!(
            ReviewClassifier::classify("you should use a hashmap instead"),
            ReviewCategory::WrongApproach
        );
        assert_eq!(
            ReviewClassifier::classify("it would be better to use async"),
            ReviewCategory::WrongApproach
        );
    }

    #[test]
    fn test_classify_documentation_keywords() {
        assert_eq!(
            ReviewClassifier::classify("add a rustdoc comment"),
            ReviewCategory::Documentation
        );
        assert_eq!(
            ReviewClassifier::classify("need a docstring here"),
            ReviewCategory::Documentation
        );
        assert_eq!(
            ReviewClassifier::classify("please explain this logic"),
            ReviewCategory::Documentation
        );
    }

    // ── Integration tests using SqliteTracker ──

    #[test]
    fn test_process_review_comments_with_body_only() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let patterns = ReviewClassifier::process_review_comments(
            &tracker,
            "org/repo",
            &[],
            Some("Please add tests for this change"),
        )
        .unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].category, ReviewCategory::MissingTests);
        assert!(patterns[0].pattern_text.contains("add tests"));

        // Verify it was stored
        let stored = tracker.get_review_patterns("org/repo", 10).unwrap();
        assert_eq!(stored.len(), 1);
    }

    #[test]
    fn test_process_review_comments_with_inline_comments() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let comments = vec![
            PrReviewComment {
                id: 1,
                path: "src/main.rs".to_string(),
                position: Some(10),
                original_position: None,
                body: "This could be a security vulnerability with user input".to_string(),
                user: crate::github::GitHubUser {
                    login: "reviewer".to_string(),
                    id: 1,
                    user_type: None,
                },
                created_at: "2024-01-01T00:00:00Z".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
                html_url: "https://github.com/org/repo/pull/1".to_string(),
                pull_request_review_id: None,
                start_line: None,
                line: None,
                side: None,
            },
            PrReviewComment {
                id: 2,
                path: "src/api.rs".to_string(),
                position: Some(20),
                original_position: None,
                body: "This is slow, consider adding a cache layer".to_string(),
                user: crate::github::GitHubUser {
                    login: "reviewer".to_string(),
                    id: 1,
                    user_type: None,
                },
                created_at: "2024-01-01T00:00:00Z".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
                html_url: "https://github.com/org/repo/pull/1".to_string(),
                pull_request_review_id: None,
                start_line: None,
                line: None,
                side: None,
            },
        ];

        let patterns =
            ReviewClassifier::process_review_comments(&tracker, "org/repo", &comments, None)
                .unwrap();
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0].category, ReviewCategory::Security);
        assert_eq!(patterns[1].category, ReviewCategory::Performance);

        let stored = tracker.get_review_patterns("org/repo", 10).unwrap();
        assert_eq!(stored.len(), 2);
    }

    #[test]
    fn test_process_review_comments_with_body_and_inline() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let comments = vec![PrReviewComment {
            id: 1,
            path: "src/main.rs".to_string(),
            position: Some(10),
            original_position: None,
            body: "This naming convention doesn't match our style guide".to_string(),
            user: crate::github::GitHubUser {
                login: "reviewer".to_string(),
                id: 1,
                user_type: None,
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            html_url: "url".to_string(),
            pull_request_review_id: None,
            start_line: None,
            line: None,
            side: None,
        }];

        let patterns = ReviewClassifier::process_review_comments(
            &tracker,
            "org/repo",
            &comments,
            Some("Please add unit tests"),
        )
        .unwrap();
        assert_eq!(patterns.len(), 2);
        // Body: "unit tests" -> MissingTests, Inline: "naming convention" -> StyleIssue
        let categories: Vec<_> = patterns.iter().map(|p| p.category).collect();
        assert!(categories.contains(&ReviewCategory::MissingTests));
        assert!(categories.contains(&ReviewCategory::StyleIssue));
    }

    #[test]
    fn test_process_review_comments_empty_body_skipped() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let patterns =
            ReviewClassifier::process_review_comments(&tracker, "org/repo", &[], Some("  "))
                .unwrap();
        assert!(patterns.is_empty());

        let stored = tracker.get_review_patterns("org/repo", 10).unwrap();
        assert!(stored.is_empty());
    }

    #[test]
    fn test_check_promotion_threshold_below() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        // Store a pattern with occurrence_count=1
        ReviewClassifier::process_review_comments(
            &tracker,
            "org/repo",
            &[],
            Some("Add tests please"),
        )
        .unwrap();

        let promotable =
            ReviewClassifier::check_promotion_threshold(&tracker, "org/repo", 3).unwrap();
        assert!(promotable.is_empty(), "count=1 should be below threshold=3");
    }

    #[test]
    fn test_check_promotion_threshold_reached() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // Upsert the same pattern 3 times to reach threshold
        for _ in 0..3 {
            ReviewClassifier::process_review_comments(
                &tracker,
                "org/repo",
                &[],
                Some("Add tests for new code"),
            )
            .unwrap();
        }

        let promotable =
            ReviewClassifier::check_promotion_threshold(&tracker, "org/repo", 3).unwrap();
        assert_eq!(promotable.len(), 1, "count=3 should meet threshold=3");
        assert_eq!(promotable[0].category, ReviewCategory::MissingTests);
        assert!(!promotable[0].promoted_to_instruction);
    }

    #[test]
    fn test_classify_whitespace_only() {
        assert_eq!(
            ReviewClassifier::classify("   \t\n  "),
            ReviewCategory::Other
        );
    }

    #[test]
    fn test_classify_very_long_comment() {
        let long = "a ".repeat(10_000) + "test";
        assert_eq!(
            ReviewClassifier::classify(&long),
            ReviewCategory::MissingTests
        );
    }

    #[test]
    fn test_classify_keyword_as_substring() {
        // "escape" is a Security keyword — should match even embedded
        assert_eq!(
            ReviewClassifier::classify("please escape user input"),
            ReviewCategory::Security
        );
    }

    #[test]
    fn test_classify_multiple_categories_first_wins() {
        // Security is checked before MissingTests, so security wins
        assert_eq!(
            ReviewClassifier::classify("security test coverage needed"),
            ReviewCategory::Security
        );
        // MissingTests is checked before StyleIssue
        assert_eq!(
            ReviewClassifier::classify("add test for naming convention"),
            ReviewCategory::MissingTests
        );
    }

    #[test]
    fn test_truncate_zero_max_len() {
        let result = truncate("hello world", 0);
        // max_len=0: output must not exceed 0 bytes
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_max_len_one() {
        let result = truncate("hello", 1);
        // max_len=1: too small for ellipsis, just take 1 char
        assert_eq!(result, "h");
        assert!(result.len() <= 1);
    }

    #[test]
    fn test_truncate_max_len_three() {
        let result = truncate("hello", 3);
        // max_len=3: too small for "X...", take first 3 bytes
        assert_eq!(result, "hel");
        assert!(result.len() <= 3);
    }

    #[test]
    fn test_truncate_max_len_four() {
        let result = truncate("hello world", 4);
        // max_len=4: room for "h..."
        assert_eq!(result, "h...");
        assert!(result.len() <= 4);
    }

    #[test]
    fn test_truncate_never_exceeds_max_len() {
        for max_len in 0..20 {
            let result = truncate("hello world, this is a longer string", max_len);
            assert!(
                result.len() <= max_len,
                "truncate with max_len={} produced '{}' ({} bytes)",
                max_len,
                result,
                result.len()
            );
        }
    }

    #[test]
    fn test_process_review_comments_no_body_no_comments() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let patterns =
            ReviewClassifier::process_review_comments(&tracker, "org/repo", &[], None).unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_process_review_comments_none_body() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let patterns =
            ReviewClassifier::process_review_comments(&tracker, "org/repo", &[], None).unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_check_promotion_threshold_zero() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        // With threshold=0, everything should promote
        ReviewClassifier::process_review_comments(&tracker, "org/repo", &[], Some("Add tests"))
            .unwrap();
        let promotable =
            ReviewClassifier::check_promotion_threshold(&tracker, "org/repo", 0).unwrap();
        // occurrence_count=1 >= threshold=0, so it should promote
        assert_eq!(promotable.len(), 1);
    }

    #[test]
    fn test_check_promotion_threshold_different_repos() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        for _ in 0..3 {
            ReviewClassifier::process_review_comments(
                &tracker,
                "org/repo-a",
                &[],
                Some("Add tests"),
            )
            .unwrap();
        }
        ReviewClassifier::process_review_comments(&tracker, "org/repo-b", &[], Some("Add tests"))
            .unwrap();

        let promotable_a =
            ReviewClassifier::check_promotion_threshold(&tracker, "org/repo-a", 3).unwrap();
        let promotable_b =
            ReviewClassifier::check_promotion_threshold(&tracker, "org/repo-b", 3).unwrap();
        assert_eq!(promotable_a.len(), 1);
        assert!(promotable_b.is_empty());
    }
}
