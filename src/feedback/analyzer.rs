//! Feedback analyzer for improving prompts based on past outcomes.

use super::outcomes::{FixOutcome, Outcome, OutcomeTracker};
use crate::error::Result;
use crate::storage::SqliteTracker;
use crate::types::{FixAttempt, Issue};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A similar issue found in past outcomes.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarIssue {
    /// The similar past outcome.
    pub outcome: FixOutcome,
    /// Similarity score (0.0 to 1.0).
    pub similarity: f64,
}

/// A suggestion for improving prompts.
#[derive(Debug, Clone, Serialize)]
pub struct PromptSuggestion {
    /// Type of suggestion.
    pub suggestion_type: SuggestionType,
    /// The suggestion text.
    pub text: String,
    /// Confidence level (0.0 to 1.0).
    pub confidence: f64,
    /// Source of this suggestion (which outcomes it's based on).
    pub based_on: Vec<i64>,
}

/// Types of prompt suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestionType {
    /// Add context about what worked before.
    AddContext,
    /// Avoid a pattern that failed before.
    AvoidPattern,
    /// Include specific instructions based on past success.
    IncludeInstruction,
    /// Warning about a common failure mode.
    Warning,
}

/// Analyzes past fix outcomes to improve future prompts.
pub struct FeedbackAnalyzer {
    tracker: OutcomeTracker,
    sqlite_tracker: Option<Arc<SqliteTracker>>,
    min_similarity: f64,
    max_similar_results: usize,
}

impl FeedbackAnalyzer {
    /// Create a new feedback analyzer.
    pub fn new() -> Self {
        Self {
            tracker: OutcomeTracker::new(),
            sqlite_tracker: None,
            min_similarity: 0.1,
            max_similar_results: 5,
        }
    }

    /// Create with custom settings.
    pub fn with_settings(min_similarity: f64, max_similar_results: usize) -> Self {
        Self {
            tracker: OutcomeTracker::new(),
            sqlite_tracker: None,
            min_similarity,
            max_similar_results,
        }
    }

    /// Attach a SqliteTracker for HNSW-based semantic search on outcomes.
    pub fn with_sqlite_tracker(mut self, sqlite_tracker: Arc<SqliteTracker>) -> Self {
        self.sqlite_tracker = Some(sqlite_tracker);
        self
    }

    /// Load outcomes from persistent storage (e.g. DB hydration on startup).
    pub fn load_outcomes(&mut self, outcomes: Vec<FixOutcome>) {
        self.tracker.load(outcomes);
    }

    /// Record an outcome.
    pub fn record_outcome(
        &mut self,
        attempt: &FixAttempt,
        issue: &Issue,
        prompt: &str,
        outcome: Outcome,
    ) -> Result<i64> {
        let fix_outcome = FixOutcome::from_attempt(attempt, issue, prompt, outcome);
        self.tracker.record(fix_outcome)
    }

    /// Find similar outcomes using semantic search with a pre-computed embedding.
    ///
    /// Uses HNSW via vectorlite. Returns empty if no results found.
    pub fn find_similar(&self, query_embedding: &[f32]) -> Vec<SimilarIssue> {
        if let Some(ref sqlite) = self.sqlite_tracker {
            match sqlite.find_similar_outcomes_vector(
                query_embedding,
                self.min_similarity,
                self.max_similar_results,
            ) {
                Ok(Some(results)) => {
                    tracing::debug!(
                        count = results.len(),
                        "Outcome similarity search used HNSW index"
                    );
                    return results
                        .into_iter()
                        .map(|(outcome, similarity)| SimilarIssue {
                            outcome,
                            similarity,
                        })
                        .collect();
                }
                Ok(None) => {
                    tracing::warn!("vectorlite unavailable for outcome search");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "HNSW outcome search failed");
                }
            }
        }

        Vec::new()
    }

    /// Build an enhanced prompt using semantic search with a pre-computed embedding.
    pub fn enhance_prompt(
        &self,
        base_prompt: &str,
        _issue: &Issue,
        query_embedding: &[f32],
    ) -> String {
        let similar = self.find_similar(query_embedding);
        let suggestions = Self::suggestions_from_similar(&similar);

        if suggestions.is_empty() {
            return base_prompt.to_string();
        }

        let mut enhanced = String::new();
        enhanced.push_str("# Learnings from Similar Issues\n\n");

        for suggestion in suggestions.iter().take(3) {
            let prefix = match suggestion.suggestion_type {
                SuggestionType::AddContext => "Context:",
                SuggestionType::AvoidPattern => "Avoid:",
                SuggestionType::IncludeInstruction => "Instruction:",
                SuggestionType::Warning => "Warning:",
            };
            enhanced.push_str(&format!("- {} {}\n", prefix, suggestion.text));
        }

        enhanced.push_str("\n---\n\n");
        enhanced.push_str(base_prompt);

        enhanced
    }

    /// Extract suggestions from a list of similar issues (shared logic).
    fn suggestions_from_similar(similar: &[SimilarIssue]) -> Vec<PromptSuggestion> {
        let mut suggestions = Vec::new();

        let successful: Vec<_> = similar
            .iter()
            .filter(|s| s.outcome.outcome.is_success())
            .collect();
        let failed: Vec<_> = similar
            .iter()
            .filter(|s| !s.outcome.outcome.is_success())
            .collect();

        if !successful.is_empty() {
            for s in &successful {
                if let Some(ref learnings) = s.outcome.learnings {
                    suggestions.push(PromptSuggestion {
                        suggestion_type: SuggestionType::AddContext,
                        text: format!(
                            "Similar issue was fixed successfully. Learning: {}",
                            learnings
                        ),
                        confidence: s.similarity,
                        based_on: vec![s.outcome.id],
                    });
                }
            }

            if successful.len() >= 2 {
                let ids: Vec<_> = successful.iter().map(|s| s.outcome.id).collect();
                suggestions.push(PromptSuggestion {
                    suggestion_type: SuggestionType::AddContext,
                    text: format!(
                        "{} similar issues were fixed successfully in the past.",
                        successful.len()
                    ),
                    confidence: successful.iter().map(|s| s.similarity).sum::<f64>()
                        / successful.len() as f64,
                    based_on: ids,
                });
            }
        }

        if !failed.is_empty() {
            let error_types: Vec<_> = failed
                .iter()
                .filter_map(|s| s.outcome.error_type.as_ref())
                .collect();

            if !error_types.is_empty() {
                let ids: Vec<_> = failed.iter().map(|s| s.outcome.id).collect();
                suggestions.push(PromptSuggestion {
                    suggestion_type: SuggestionType::Warning,
                    text: format!(
                        "Similar issues have failed before with errors: {}. Be careful.",
                        error_types
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    confidence: failed.iter().map(|s| s.similarity).sum::<f64>()
                        / failed.len() as f64,
                    based_on: ids,
                });
            }

            if failed.len() > successful.len() && similar.len() >= 3 {
                suggestions.push(PromptSuggestion {
                    suggestion_type: SuggestionType::Warning,
                    text: format!(
                        "Caution: {} out of {} similar issues failed. This may be difficult to fix automatically.",
                        failed.len(),
                        similar.len()
                    ),
                    confidence: 0.7,
                    based_on: failed.iter().map(|s| s.outcome.id).collect(),
                });
            }
        }

        suggestions.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        suggestions
    }

    /// Get the outcome tracker for direct access.
    pub fn tracker(&self) -> &OutcomeTracker {
        &self.tracker
    }

    /// Get mutable outcome tracker.
    pub fn tracker_mut(&mut self) -> &mut OutcomeTracker {
        &mut self.tracker
    }

    /// Get overall success rate.
    pub fn overall_success_rate(&self) -> f64 {
        self.tracker.success_rate(None)
    }

    /// Get success rate for a specific source.
    pub fn source_success_rate(&self, source: &str) -> f64 {
        self.tracker.success_rate(Some(source))
    }

    /// Get common error patterns.
    pub fn common_errors(&self, limit: usize) -> Vec<(String, usize)> {
        self.tracker.common_errors(limit)
    }

    /// Add learnings to an outcome.
    pub fn add_learnings(&mut self, outcome_id: i64, learnings: &str) -> Result<()> {
        self.tracker.add_learnings(outcome_id, learnings)
    }
}

impl Default for FeedbackAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FixAttemptStatus, IssuePriority, IssueStatus};

    fn create_test_issue(title: &str, description: &str, source: &str) -> Issue {
        Issue {
            id: format!("{}-1", source),
            short_id: format!("{}-1", source.to_uppercase()),
            title: title.to_string(),
            description: Some(description.to_string()),
            url: "https://example.com".to_string(),
            source: source.to_string(),
            priority: IssuePriority::Medium,
            status: IssueStatus::Open,
            metadata: Default::default(),
            created_at: None,
            updated_at: None,
        }
    }

    fn create_test_attempt(source: &str) -> FixAttempt {
        FixAttempt {
            id: 1,
            source: source.to_string(),
            issue_id: format!("{}-1", source),
            short_id: format!("{}-1", source.to_uppercase()),
            status: FixAttemptStatus::Success,
            pr_url: Some("https://github.com/test/pr/1".to_string()),
            github_repo: None,
            github_pr_number: None,
            error_message: None,
            attempted_at: chrono::Utc::now(),
            resolved_at: None,
            merged_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        }
    }

    #[test]
    fn test_record_outcome() {
        let mut analyzer = FeedbackAnalyzer::new();

        let issue = create_test_issue("API timeout error", "Timeout in user service", "linear");
        let attempt = create_test_attempt("linear");

        let id = analyzer
            .record_outcome(&attempt, &issue, "Fix the timeout", Outcome::Merged)
            .unwrap();

        assert_eq!(id, 1);
        assert_eq!(analyzer.tracker().all().len(), 1);
    }

    #[test]
    fn test_find_similar_no_sqlite_tracker() {
        let analyzer = FeedbackAnalyzer::new();
        // Without a sqlite tracker, find_similar returns empty
        let embedding = vec![1.0, 0.0, 0.0];
        let similar = analyzer.find_similar(&embedding);
        assert!(similar.is_empty());
    }

    #[test]
    fn test_enhance_prompt_no_results() {
        let analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("API error", "Server returns 500", "sentry");
        let embedding = vec![1.0, 0.0, 0.0];

        // Without sqlite tracker, should return base prompt unchanged
        let enhanced = analyzer.enhance_prompt("Fix this bug", &issue, &embedding);
        assert_eq!(enhanced, "Fix this bug");
    }

    #[test]
    fn test_suggestions_from_similar_with_learnings() {
        let outcome = FixOutcome {
            id: 1,
            attempt_id: 100,
            source: "linear".to_string(),
            issue_id: "123".to_string(),
            issue_text: "Test issue".to_string(),
            prompt_used: "Fix it".to_string(),
            outcome: Outcome::Merged,
            error_type: None,
            learnings: Some("Check token expiration".to_string()),
            keywords: vec![],
            embedding: None,
            created_at: chrono::Utc::now(),
        };

        let similar = vec![SimilarIssue {
            outcome,
            similarity: 0.9,
        }];

        let suggestions = FeedbackAnalyzer::suggestions_from_similar(&similar);
        assert!(!suggestions.is_empty());
        assert!(suggestions[0].text.contains("token expiration"));
    }

    #[test]
    fn test_suggestions_from_similar_with_failures() {
        let mut outcomes = Vec::new();
        for i in 0..4 {
            outcomes.push(SimilarIssue {
                outcome: FixOutcome {
                    id: i,
                    attempt_id: i as i64,
                    source: "linear".to_string(),
                    issue_id: format!("issue-{}", i),
                    issue_text: "Timeout error".to_string(),
                    prompt_used: "fix".to_string(),
                    outcome: Outcome::Failed,
                    error_type: Some("timeout".to_string()),
                    learnings: None,
                    keywords: vec![],
                    embedding: None,
                    created_at: chrono::Utc::now(),
                },
                similarity: 0.8,
            });
        }

        let suggestions = FeedbackAnalyzer::suggestions_from_similar(&outcomes);
        let has_warning = suggestions
            .iter()
            .any(|s| s.suggestion_type == SuggestionType::Warning);
        assert!(has_warning);
        let has_caution = suggestions.iter().any(|s| s.text.contains("Caution"));
        assert!(has_caution);
    }

    #[test]
    fn test_success_rate() {
        let mut analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("Test", "Test desc", "linear");
        let attempt = create_test_attempt("linear");

        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Failed)
            .unwrap();

        let rate = analyzer.overall_success_rate();
        assert!((rate - 0.666).abs() < 0.01); // ~66% success
    }

    #[test]
    fn test_common_errors() {
        let mut analyzer = FeedbackAnalyzer::new();

        let issue = create_test_issue("Test", "Test", "linear");
        let mut attempt = create_test_attempt("linear");

        attempt.error_message = Some("Connection timed out".to_string());
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Failed)
            .unwrap();
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Failed)
            .unwrap();

        attempt.error_message = Some("Permission denied".to_string());
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Failed)
            .unwrap();

        let errors = analyzer.common_errors(5);
        assert!(!errors.is_empty());

        // timeout should be most common (2 occurrences)
        assert_eq!(errors[0].0, "timeout");
        assert_eq!(errors[0].1, 2);
    }

    #[test]
    fn test_analyzer_default() {
        let analyzer = FeedbackAnalyzer::default();
        assert!((analyzer.overall_success_rate() - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_analyzer_with_settings() {
        let analyzer = FeedbackAnalyzer::with_settings(0.5, 10);
        // Just ensure it creates without error
        assert!(analyzer.find_similar(&[1.0, 0.0]).is_empty());
    }

    #[test]
    fn test_source_success_rate() {
        let mut analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("Test", "Test", "linear");
        let attempt = create_test_attempt("linear");

        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Failed)
            .unwrap();

        let rate = analyzer.source_success_rate("linear");
        assert!((rate - 0.5).abs() < 0.1);
    }

    #[test]
    fn test_source_success_rate_no_data() {
        let analyzer = FeedbackAnalyzer::new();
        let rate = analyzer.source_success_rate("nonexistent");
        assert!((rate - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_tracker_accessor() {
        let analyzer = FeedbackAnalyzer::new();
        let tracker = analyzer.tracker();
        assert!(tracker.all().is_empty());
    }

    #[test]
    fn test_tracker_mut_accessor() {
        let mut analyzer = FeedbackAnalyzer::new();
        let _ = analyzer.tracker_mut();
        // Just verify we can access it mutably
    }

    #[test]
    fn test_similar_issue_fields() {
        let outcome = FixOutcome {
            id: 1,
            attempt_id: 100,
            source: "linear".to_string(),
            issue_id: "123".to_string(),
            issue_text: "Test issue title".to_string(),
            prompt_used: "Fix it".to_string(),
            outcome: Outcome::Merged,
            error_type: None,
            learnings: None,
            keywords: vec!["test".to_string()],
            embedding: None,
            created_at: chrono::Utc::now(),
        };

        let similar = SimilarIssue {
            outcome,
            similarity: 0.85,
        };

        assert!((similar.similarity - 0.85).abs() < 0.01);
        assert_eq!(similar.outcome.issue_id, "123");
    }

    #[test]
    fn test_prompt_suggestion_fields() {
        let suggestion = PromptSuggestion {
            suggestion_type: SuggestionType::AddContext,
            text: "Add more context".to_string(),
            confidence: 0.75,
            based_on: vec![1, 2, 3],
        };

        assert_eq!(suggestion.suggestion_type, SuggestionType::AddContext);
        assert_eq!(suggestion.text, "Add more context");
        assert!((suggestion.confidence - 0.75).abs() < 0.01);
        assert_eq!(suggestion.based_on.len(), 3);
    }

    #[test]
    fn test_suggestion_types() {
        assert_eq!(SuggestionType::AddContext, SuggestionType::AddContext);
        assert_eq!(SuggestionType::AvoidPattern, SuggestionType::AvoidPattern);
        assert_eq!(
            SuggestionType::IncludeInstruction,
            SuggestionType::IncludeInstruction
        );
        assert_eq!(SuggestionType::Warning, SuggestionType::Warning);
        assert_ne!(SuggestionType::AddContext, SuggestionType::Warning);
    }

    #[test]
    fn test_load_outcomes() {
        let mut analyzer = FeedbackAnalyzer::new();

        // Build outcomes as if loaded from DB
        let issue = create_test_issue(
            "Database connection timeout",
            "PostgreSQL connection fails",
            "linear",
        );
        let attempt = create_test_attempt("linear");
        let mut outcome =
            FixOutcome::from_attempt(&attempt, &issue, "Fix the timeout", Outcome::Merged);
        outcome.id = 5;

        analyzer.load_outcomes(vec![outcome]);

        // Verify outcomes were loaded
        assert_eq!(analyzer.tracker().all().len(), 1);
        assert_eq!(analyzer.tracker().all()[0].id, 5);
    }

    #[test]
    fn test_common_errors_limit() {
        let mut analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("Test", "Test", "linear");
        let mut attempt = create_test_attempt("linear");

        // Add various error types
        for err_type in &["timeout", "permission", "network", "syntax", "runtime"] {
            attempt.error_message = Some(format!("{} error", err_type));
            analyzer
                .record_outcome(&attempt, &issue, "p", Outcome::Failed)
                .unwrap();
        }

        let errors = analyzer.common_errors(2);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn test_add_learnings() {
        let mut analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("Test", "Test", "linear");
        let attempt = create_test_attempt("linear");

        let id = analyzer
            .record_outcome(&attempt, &issue, "prompt", Outcome::Merged)
            .unwrap();
        analyzer
            .add_learnings(id, "Important lesson learned")
            .unwrap();

        // Verify the learning was added
        let tracker = analyzer.tracker();
        let outcomes = tracker.all();
        let outcome = outcomes.iter().find(|o| o.id == id).unwrap();
        assert_eq!(
            outcome.learnings.as_ref().unwrap(),
            "Important lesson learned"
        );
    }

    #[test]
    fn test_new_default_settings() {
        let analyzer = FeedbackAnalyzer::new();
        assert!(analyzer.find_similar(&[1.0, 0.0]).is_empty());
    }

    #[test]
    fn test_load_outcomes_replaces_state() {
        let mut analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("test", "test", "sentry");
        let attempt = create_test_attempt("sentry");
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();

        analyzer.load_outcomes(vec![]);
        assert!(analyzer.tracker().all().is_empty());
    }

    #[test]
    fn test_record_outcome_returns_sequential_ids() {
        let mut analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("test", "test", "sentry");
        let attempt = create_test_attempt("sentry");
        let id1 = analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();
        let id2 = analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Failed)
            .unwrap();
        assert!(id2 > id1);
    }
}
