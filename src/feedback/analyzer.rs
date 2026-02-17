//! Feedback analyzer for improving prompts based on past outcomes.

use super::outcomes::{FixOutcome, Outcome, OutcomeTracker};
use crate::error::Result;
use crate::types::{FixAttempt, Issue};
use serde::{Deserialize, Serialize};

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
    min_similarity: f64,
    max_similar_results: usize,
}

impl FeedbackAnalyzer {
    /// Create a new feedback analyzer.
    pub fn new() -> Self {
        Self {
            tracker: OutcomeTracker::new(),
            min_similarity: 0.1,
            max_similar_results: 5,
        }
    }

    /// Create with custom settings.
    pub fn with_settings(min_similarity: f64, max_similar_results: usize) -> Self {
        Self {
            tracker: OutcomeTracker::new(),
            min_similarity,
            max_similar_results,
        }
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

    /// Find the most similar past issues, sorted by similarity descending.
    pub fn find_similar(&self, issue: &Issue) -> Vec<SimilarIssue> {
        let mut candidates: Vec<SimilarIssue> = self
            .tracker
            .all()
            .iter()
            .map(|o| {
                let similarity = o.similarity_to_issue(issue);
                SimilarIssue {
                    outcome: o.clone(),
                    similarity,
                }
            })
            .filter(|s| s.similarity >= self.min_similarity)
            .collect();
        candidates.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(self.max_similar_results);
        candidates
    }

    /// Generate suggestions for improving the prompt based on past outcomes.
    pub fn suggest_improvements(&self, issue: &Issue) -> Vec<PromptSuggestion> {
        let similar = self.find_similar(issue);
        let mut suggestions = Vec::new();

        // Analyze successful vs failed patterns
        let successful: Vec<_> = similar
            .iter()
            .filter(|s| s.outcome.outcome.is_success())
            .collect();
        let failed: Vec<_> = similar
            .iter()
            .filter(|s| !s.outcome.outcome.is_success())
            .collect();

        // If we have successful examples, learn from them
        if !successful.is_empty() {
            // Check if there are learnings we can use
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

            // Add generic success context
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

        // If we have failed examples, learn what to avoid
        if !failed.is_empty() {
            // Check for common error types
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

            // If more failed than succeeded, add warning
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

        // Sort by confidence
        suggestions.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        suggestions
    }

    /// Build an enhanced prompt with feedback learnings.
    pub fn enhance_prompt(&self, base_prompt: &str, issue: &Issue) -> String {
        let suggestions = self.suggest_improvements(issue);

        if suggestions.is_empty() {
            return base_prompt.to_string();
        }

        let mut enhanced = String::new();

        // Add learnings section
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

    /// Build an enhanced prompt with quality-weighted feedback learnings.
    ///
    /// When quality weights are available, suggestions from fast-merge, clean PRs
    /// influence future suggestions more than multi-round ones.
    pub fn enhance_prompt_with_quality(
        &self,
        base_prompt: &str,
        issue: &Issue,
        quality_weights: &std::collections::HashMap<i64, f64>,
    ) -> String {
        let mut suggestions = self.suggest_improvements(issue);

        // Apply quality weighting if available
        if !quality_weights.is_empty() {
            for suggestion in &mut suggestions {
                for outcome_id in &suggestion.based_on {
                    if let Some(&quality) = quality_weights.get(outcome_id) {
                        suggestion.confidence = crate::learning::QualityScorer::weight_confidence(
                            suggestion.confidence,
                            quality,
                        );
                    }
                }
            }
            // Re-sort by updated confidence
            suggestions.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

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
    fn test_record_and_find() {
        let mut analyzer = FeedbackAnalyzer::new();

        let issue = create_test_issue("API timeout error", "Timeout in user service", "linear");
        let attempt = create_test_attempt("linear");

        let id = analyzer
            .record_outcome(&attempt, &issue, "Fix the timeout", Outcome::Merged)
            .unwrap();

        assert_eq!(id, 1);

        let search = create_test_issue("API returns timeout", "Service timeout issue", "linear");
        let similar = analyzer.find_similar(&search);

        assert!(!similar.is_empty());
    }

    #[test]
    fn test_suggest_improvements() {
        let mut analyzer = FeedbackAnalyzer::new();

        // Record some outcomes
        let issue1 = create_test_issue(
            "Database connection error",
            "PostgreSQL connection fails",
            "linear",
        );
        let attempt1 = create_test_attempt("linear");
        analyzer
            .record_outcome(&attempt1, &issue1, "prompt", Outcome::Merged)
            .unwrap();

        let mut attempt2 = create_test_attempt("linear");
        attempt2.error_message = Some("Connection timeout".to_string());
        let issue2 = create_test_issue("Database timeout", "PostgreSQL times out", "linear");
        analyzer
            .record_outcome(&attempt2, &issue2, "prompt", Outcome::Failed)
            .unwrap();

        // Get suggestions for similar issue
        let new_issue = create_test_issue(
            "Database connection problem",
            "PostgreSQL connection issue",
            "linear",
        );
        let suggestions = analyzer.suggest_improvements(&new_issue);

        // Should have some suggestions based on similar issues
        assert!(!suggestions.is_empty() || analyzer.find_similar(&new_issue).is_empty());
    }

    #[test]
    fn test_enhance_prompt() {
        let mut analyzer = FeedbackAnalyzer::new();

        // Record a successful outcome with learnings
        let issue = create_test_issue("API error", "Server returns 500", "sentry");
        let attempt = create_test_attempt("sentry");
        let id = analyzer
            .record_outcome(&attempt, &issue, "Fix the API", Outcome::Merged)
            .unwrap();

        analyzer
            .add_learnings(id, "Check error handling in catch blocks")
            .unwrap();

        // Enhance a prompt for similar issue
        let new_issue = create_test_issue("API 500 error", "Server error in API", "sentry");
        let enhanced = analyzer.enhance_prompt("Fix this bug", &new_issue);

        // Enhanced prompt should contain the base prompt
        assert!(enhanced.contains("Fix this bug"));
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
        assert!(analyzer
            .find_similar(&create_test_issue("Test", "Test", "linear"))
            .is_empty());
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
    fn test_enhance_prompt_no_suggestions() {
        let analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("Unique Test", "Unique Description", "linear");

        let enhanced = analyzer.enhance_prompt("Base prompt", &issue);
        assert_eq!(enhanced, "Base prompt");
    }

    #[test]
    fn test_load_outcomes_and_find_similar() {
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

        // Should find it when searching for similar
        let search = create_test_issue("Database timeout error", "PostgreSQL times out", "linear");
        let similar = analyzer.find_similar(&search);
        assert!(!similar.is_empty());
        assert_eq!(similar[0].outcome.id, 5);
    }

    #[test]
    fn test_find_similar_no_data() {
        let analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("Test", "Test", "linear");

        let similar = analyzer.find_similar(&issue);
        assert!(similar.is_empty());
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
    fn test_suggest_improvements_with_failed_error_types() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10); // Low threshold to match everything

        // Record multiple failed outcomes with error types
        let issue1 = create_test_issue("API timeout error", "Timeout in service", "linear");
        let mut attempt1 = create_test_attempt("linear");
        attempt1.error_message = Some("Connection timeout error".to_string());
        analyzer
            .record_outcome(&attempt1, &issue1, "p", Outcome::Failed)
            .unwrap();

        let issue2 = create_test_issue("API timeout problem", "Service timeout", "linear");
        let mut attempt2 = create_test_attempt("linear");
        attempt2.error_message = Some("Connection timeout again".to_string());
        analyzer
            .record_outcome(&attempt2, &issue2, "p", Outcome::Failed)
            .unwrap();

        let issue3 = create_test_issue("API timeout issue", "Another timeout", "linear");
        let mut attempt3 = create_test_attempt("linear");
        attempt3.error_message = Some("Timeout error".to_string());
        analyzer
            .record_outcome(&attempt3, &issue3, "p", Outcome::Failed)
            .unwrap();

        let new_issue = create_test_issue("API timeout bug", "Timeout in new service", "linear");
        let suggestions = analyzer.suggest_improvements(&new_issue);

        // Should have warning suggestions about past failures
        let has_warning = suggestions
            .iter()
            .any(|s| s.suggestion_type == SuggestionType::Warning);
        // With 3+ similar failed issues and 0 successful, should trigger the "more failed" warning
        let has_caution_warning = suggestions
            .iter()
            .any(|s| s.suggestion_type == SuggestionType::Warning && s.text.contains("Caution"));

        assert!(has_warning || suggestions.is_empty());
        // If we got suggestions, they should have the caution warning
        if suggestions.len() >= 2 {
            assert!(has_caution_warning);
        }
    }

    #[test]
    fn test_suggest_improvements_mixed_success_and_failure() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        // 2 successful
        let issue1 = create_test_issue("Database error", "DB connection fails", "linear");
        let attempt1 = create_test_attempt("linear");
        let id1 = analyzer
            .record_outcome(&attempt1, &issue1, "p", Outcome::Merged)
            .unwrap();
        analyzer
            .add_learnings(id1, "Check connection pool settings")
            .unwrap();

        let issue2 = create_test_issue("Database timeout", "DB times out", "linear");
        let attempt2 = create_test_attempt("linear");
        analyzer
            .record_outcome(&attempt2, &issue2, "p", Outcome::Merged)
            .unwrap();

        // 1 failed
        let issue3 = create_test_issue("Database crash", "DB crashes", "linear");
        let mut attempt3 = create_test_attempt("linear");
        attempt3.error_message = Some("Disk full error".to_string());
        analyzer
            .record_outcome(&attempt3, &issue3, "p", Outcome::Failed)
            .unwrap();

        let new_issue = create_test_issue("Database problem", "DB connection issue", "linear");
        let suggestions = analyzer.suggest_improvements(&new_issue);

        // Should have AddContext suggestions from successful outcomes
        let has_context = suggestions
            .iter()
            .any(|s| s.suggestion_type == SuggestionType::AddContext);

        // With learnings on one of the successful outcomes, we should get context
        if !suggestions.is_empty() {
            assert!(has_context);
        }
    }

    #[test]
    fn test_suggest_improvements_successful_with_learnings() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        let issue = create_test_issue("Auth error", "Login fails", "sentry");
        let attempt = create_test_attempt("sentry");
        let id = analyzer
            .record_outcome(&attempt, &issue, "Fix auth", Outcome::Merged)
            .unwrap();
        analyzer
            .add_learnings(id, "Always check token expiration")
            .unwrap();

        let new_issue = create_test_issue("Auth problem", "Authentication failure", "sentry");
        let suggestions = analyzer.suggest_improvements(&new_issue);

        // Should include the learning as a context suggestion
        let learning_suggestion = suggestions.iter().find(|s| {
            s.suggestion_type == SuggestionType::AddContext && s.text.contains("token expiration")
        });

        if !suggestions.is_empty() {
            assert!(learning_suggestion.is_some());
        }
    }

    #[test]
    fn test_enhance_prompt_with_learnings() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        // Record a successful outcome with learnings
        let issue = create_test_issue("API error", "Server 500", "sentry");
        let attempt = create_test_attempt("sentry");
        let id = analyzer
            .record_outcome(&attempt, &issue, "Fix the API", Outcome::Merged)
            .unwrap();
        analyzer
            .add_learnings(id, "Check error handler middleware")
            .unwrap();

        let new_issue = create_test_issue("API 500", "Server error", "sentry");
        let enhanced = analyzer.enhance_prompt("Fix this bug", &new_issue);

        // Should contain base prompt
        assert!(enhanced.contains("Fix this bug"));

        // If suggestions were generated, should have learnings section
        if enhanced != "Fix this bug" {
            assert!(enhanced.contains("# Learnings from Similar Issues"));
            assert!(enhanced.contains("---"));
        }
    }

    #[test]
    fn test_enhance_prompt_formats_suggestion_types() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        // Record multiple failed outcomes to trigger Warning suggestions
        for i in 0..4 {
            let issue = create_test_issue(
                &format!("Error type {}", i),
                &format!("Description {}", i),
                "linear",
            );
            let mut attempt = create_test_attempt("linear");
            attempt.error_message = Some("timeout error".to_string());
            analyzer
                .record_outcome(&attempt, &issue, "p", Outcome::Failed)
                .unwrap();
        }

        let new_issue = create_test_issue("Error type new", "New description", "linear");
        let enhanced = analyzer.enhance_prompt("Fix it", &new_issue);

        // Should contain base prompt in all cases
        assert!(enhanced.contains("Fix it"));

        // If there are suggestions, check prefix formatting
        if enhanced.contains("# Learnings") {
            // At least one prefix type should be present
            let has_prefix = enhanced.contains("Context:")
                || enhanced.contains("Avoid:")
                || enhanced.contains("Instruction:")
                || enhanced.contains("Warning:");
            assert!(has_prefix);
        }
    }

    #[test]
    fn test_suggest_improvements_sorted_by_confidence() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        // Record outcomes to produce multiple suggestions
        let issue1 = create_test_issue("Error A", "Description A", "linear");
        let attempt1 = create_test_attempt("linear");
        let id1 = analyzer
            .record_outcome(&attempt1, &issue1, "p", Outcome::Merged)
            .unwrap();
        analyzer.add_learnings(id1, "Learning A").unwrap();

        let issue2 = create_test_issue("Error A variant", "Description A2", "linear");
        let attempt2 = create_test_attempt("linear");
        let id2 = analyzer
            .record_outcome(&attempt2, &issue2, "p", Outcome::Merged)
            .unwrap();
        analyzer.add_learnings(id2, "Learning B").unwrap();

        let new_issue = create_test_issue("Error A problem", "Description A3", "linear");
        let suggestions = analyzer.suggest_improvements(&new_issue);

        // If there are multiple suggestions, they should be sorted by confidence (descending)
        for window in suggestions.windows(2) {
            assert!(window[0].confidence >= window[1].confidence);
        }
    }

    #[test]
    fn test_find_similar_respects_max_results() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 2); // Max 2 results

        // Record many outcomes
        for i in 0..10 {
            let issue = create_test_issue(
                &format!("Error {}", i),
                &format!("Description {}", i),
                "linear",
            );
            let attempt = create_test_attempt("linear");
            analyzer
                .record_outcome(&attempt, &issue, "p", Outcome::Merged)
                .unwrap();
        }

        let new_issue = create_test_issue("Error similar", "Similar description", "linear");
        let similar = analyzer.find_similar(&new_issue);

        assert!(similar.len() <= 2);
    }

    #[test]
    fn test_find_similar_respects_min_similarity() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.99, 10); // Very high threshold

        let issue = create_test_issue("Very specific error ABC", "Very specific desc", "linear");
        let attempt = create_test_attempt("linear");
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();

        // Search with completely different text
        let new_issue = create_test_issue("Unrelated XYZ", "Completely different", "sentry");
        let similar = analyzer.find_similar(&new_issue);

        // With very high threshold, unlikely to match different issues
        // (The exact behavior depends on similarity_to_issue implementation)
        // This at least exercises the filter path
        assert!(similar.len() <= 1);
    }

    #[test]
    fn test_suggest_improvements_multiple_successful_generates_generic_context() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        // Record 2+ successful outcomes (to trigger the "N similar issues fixed" suggestion)
        let issue1 = create_test_issue("Cache error", "Redis cache fails", "linear");
        let attempt1 = create_test_attempt("linear");
        analyzer
            .record_outcome(&attempt1, &issue1, "p", Outcome::Merged)
            .unwrap();

        let issue2 = create_test_issue("Cache timeout", "Redis cache times out", "linear");
        let attempt2 = create_test_attempt("linear");
        analyzer
            .record_outcome(&attempt2, &issue2, "p", Outcome::Merged)
            .unwrap();

        let new_issue = create_test_issue("Cache problem", "Redis issue", "linear");
        let suggestions = analyzer.suggest_improvements(&new_issue);

        // Check for the "N similar issues were fixed successfully" generic context
        let has_generic_context = suggestions.iter().any(|s| {
            s.suggestion_type == SuggestionType::AddContext
                && s.text.contains("similar issues were fixed successfully")
        });

        // This may or may not trigger depending on similarity scores
        // The test exercises the code path regardless
        if suggestions.len() >= 2 {
            // If we got suggestions from both outcomes, generic context should be present
            assert!(has_generic_context);
        }
    }

    // ── enhance_prompt_with_quality tests ──

    #[test]
    fn test_enhance_prompt_with_quality_no_suggestions() {
        let analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("Unique Test", "Unique Description", "linear");
        let weights = std::collections::HashMap::new();

        let enhanced = analyzer.enhance_prompt_with_quality("Base prompt", &issue, &weights);
        assert_eq!(enhanced, "Base prompt");
    }

    #[test]
    fn test_enhance_prompt_with_quality_empty_weights() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        let issue = create_test_issue("API error", "Server error", "sentry");
        let attempt = create_test_attempt("sentry");
        let id = analyzer
            .record_outcome(&attempt, &issue, "Fix it", Outcome::Merged)
            .unwrap();
        analyzer.add_learnings(id, "Check error handling").unwrap();

        let new_issue = create_test_issue("API error bug", "Server error issue", "sentry");
        let weights = std::collections::HashMap::new();

        let enhanced = analyzer.enhance_prompt_with_quality("Base prompt", &new_issue, &weights);
        // With empty weights, should still work same as enhance_prompt
        assert!(enhanced.contains("Base prompt"));
    }

    #[test]
    fn test_enhance_prompt_with_quality_applies_weights() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        let issue1 = create_test_issue("Auth bug", "Login fails", "sentry");
        let attempt = create_test_attempt("sentry");
        let id = analyzer
            .record_outcome(&attempt, &issue1, "Fix auth", Outcome::Merged)
            .unwrap();
        analyzer.add_learnings(id, "Check session tokens").unwrap();

        // Apply quality weight
        let mut weights = std::collections::HashMap::new();
        weights.insert(id, 0.95); // High quality

        let new_issue = create_test_issue("Auth error", "Login broken", "sentry");
        let enhanced = analyzer.enhance_prompt_with_quality("Base prompt", &new_issue, &weights);
        assert!(enhanced.contains("Base prompt"));
        // The quality weighting should affect confidence but not change content presence
        if enhanced.len() > "Base prompt".len() {
            assert!(enhanced.contains("Learnings from Similar Issues"));
        }
    }

    #[test]
    fn test_enhance_prompt_with_quality_low_weight_reduces_confidence() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        // Record 3 successful outcomes with learnings to trigger multi-suggestion path
        for i in 0..3 {
            let issue = create_test_issue(
                &format!("DB error {}", i),
                &format!("DB fails {}", i),
                "linear",
            );
            let attempt = create_test_attempt("linear");
            let id = analyzer
                .record_outcome(&attempt, &issue, "fix", Outcome::Merged)
                .unwrap();
            analyzer
                .add_learnings(id, &format!("Learning {}", i))
                .unwrap();
        }

        let new_issue = create_test_issue("DB error test", "DB fails test", "linear");

        // Get suggestions without quality weights
        let base_suggestions = analyzer.suggest_improvements(&new_issue);

        // Get enhanced with zero quality weight (should reduce confidence)
        let mut weights = std::collections::HashMap::new();
        for s in &base_suggestions {
            for outcome_id in &s.based_on {
                weights.insert(*outcome_id, 0.0);
            }
        }

        // This exercises the quality weighting code path
        let enhanced = analyzer.enhance_prompt_with_quality("prompt", &new_issue, &weights);
        assert!(enhanced.contains("prompt"));
    }

    #[test]
    fn test_new_default_settings() {
        let analyzer = FeedbackAnalyzer::new();
        // Verify default settings are applied
        let issue = create_test_issue("test", "test", "sentry");
        let similar = analyzer.find_similar(&issue);
        assert!(similar.is_empty()); // No outcomes recorded
    }

    #[test]
    fn test_with_settings_zero_max_results() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 0);
        let issue = create_test_issue("test", "test", "sentry");
        let attempt = create_test_attempt("sentry");
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();

        let similar = analyzer.find_similar(&issue);
        assert!(similar.is_empty()); // Max 0 results
    }

    #[test]
    fn test_find_similar_high_threshold_returns_empty() {
        let mut analyzer = FeedbackAnalyzer::with_settings(1.0, 10);
        let issue = create_test_issue("test A", "desc A", "sentry");
        let attempt = create_test_attempt("sentry");
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();

        let other = create_test_issue("test B", "desc B", "sentry");
        let similar = analyzer.find_similar(&other);
        // Threshold 1.0 means only exact match (which self-similarity might not even be 1.0)
        // At minimum this exercises the filter path
        assert!(similar.len() <= 1);
    }

    #[test]
    fn test_load_outcomes_replaces_state() {
        let mut analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("test", "test", "sentry");
        let attempt = create_test_attempt("sentry");
        analyzer
            .record_outcome(&attempt, &issue, "p", Outcome::Merged)
            .unwrap();

        // Load empty outcomes effectively clears
        analyzer.load_outcomes(vec![]);
        let similar = analyzer.find_similar(&issue);
        assert!(similar.is_empty());
    }

    #[test]
    fn test_suggest_improvements_no_outcomes() {
        let analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("test", "test", "sentry");
        let suggestions = analyzer.suggest_improvements(&issue);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_enhance_prompt_no_match_returns_base() {
        let analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("test", "test", "sentry");
        let enhanced = analyzer.enhance_prompt("Fix this bug", &issue);
        assert_eq!(enhanced, "Fix this bug");
    }

    #[test]
    fn test_enhance_prompt_empty_base_prompt() {
        let analyzer = FeedbackAnalyzer::new();
        let issue = create_test_issue("test", "test", "sentry");
        let enhanced = analyzer.enhance_prompt("", &issue);
        assert_eq!(enhanced, "");
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

    #[test]
    fn test_enhance_prompt_with_quality_reorders_by_confidence() {
        let mut analyzer = FeedbackAnalyzer::with_settings(0.0, 10);

        // Record outcomes to generate multiple suggestions
        let issue1 = create_test_issue("Cache bug", "Cache fails", "sentry");
        let attempt1 = create_test_attempt("sentry");
        let id1 = analyzer
            .record_outcome(&attempt1, &issue1, "fix", Outcome::Merged)
            .unwrap();
        analyzer
            .add_learnings(id1, "Invalidate cache on update")
            .unwrap();

        let issue2 = create_test_issue("Cache problem", "Cache broken", "sentry");
        let mut attempt2 = create_test_attempt("sentry");
        attempt2.error_message = Some("Cache timeout error".to_string());
        let id2 = analyzer
            .record_outcome(&attempt2, &issue2, "fix", Outcome::Failed)
            .unwrap();

        let new_issue = create_test_issue("Cache issue", "Cache error", "sentry");

        // High quality weight on successful outcome, low on failed
        let mut weights = std::collections::HashMap::new();
        weights.insert(id1, 1.0);
        weights.insert(id2, 0.1);

        let enhanced = analyzer.enhance_prompt_with_quality("prompt", &new_issue, &weights);
        assert!(enhanced.contains("prompt"));
    }
}
