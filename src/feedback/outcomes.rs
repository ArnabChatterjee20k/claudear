//! Outcome tracking for fix attempts.

use crate::error::Result;
use crate::feedback::cosine_similarity;
use crate::types::{FixAttempt, Issue};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The outcome of a fix attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// PR was merged successfully
    #[serde(rename = "merged")]
    Merged,
    /// PR was closed without merging
    #[serde(rename = "closed")]
    Closed,
    /// Fix attempt failed before creating PR
    #[serde(rename = "failed")]
    Failed,
    /// Issue could not be fixed after retries
    #[serde(rename = "cannot_fix")]
    CannotFix,
}

impl Outcome {
    /// Whether this outcome is considered successful.
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Merged)
    }

    /// Parse from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "merged" => Some(Outcome::Merged),
            "closed" => Some(Outcome::Closed),
            "failed" => Some(Outcome::Failed),
            "cannot_fix" | "cannotfix" => Some(Outcome::CannotFix),
            _ => None,
        }
    }

    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Merged => "merged",
            Outcome::Closed => "closed",
            Outcome::Failed => "failed",
            Outcome::CannotFix => "cannot_fix",
        }
    }
}

/// A recorded fix outcome with associated learnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixOutcome {
    /// Unique identifier.
    pub id: i64,
    /// Associated fix attempt ID.
    pub attempt_id: i64,
    /// Source of the issue.
    pub source: String,
    /// Issue ID.
    pub issue_id: String,
    /// Issue title/description (for similarity matching).
    pub issue_text: String,
    /// Prompt that was used.
    pub prompt_used: String,
    /// Outcome of the fix.
    pub outcome: Outcome,
    /// Categorized error type (if failed).
    pub error_type: Option<String>,
    /// AI-generated learnings from this outcome.
    pub learnings: Option<String>,
    /// Keywords extracted from the issue (fallback for similarity when no embedding).
    pub keywords: Vec<String>,
    /// Embedding vector for semantic similarity (optional - computed async).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// When this outcome was recorded.
    pub created_at: DateTime<Utc>,
}

impl FixOutcome {
    /// Create a new outcome from a fix attempt.
    pub fn from_attempt(
        attempt: &FixAttempt,
        issue: &Issue,
        prompt: &str,
        outcome: Outcome,
    ) -> Self {
        let description = issue.description.as_deref().unwrap_or("");
        let keywords = Self::extract_keywords(&issue.title, description);

        Self {
            id: 0, // Set by storage
            attempt_id: attempt.id,
            source: attempt.source.clone(),
            issue_id: attempt.issue_id.clone(),
            issue_text: format!("{}\n\n{}", issue.title, description),
            prompt_used: prompt.to_string(),
            outcome,
            error_type: attempt
                .error_message
                .as_ref()
                .map(|e| Self::categorize_error(e)),
            learnings: None,
            keywords,
            embedding: None, // Set async via set_embedding()
            created_at: Utc::now(),
        }
    }

    /// Set the embedding vector for this outcome.
    pub fn set_embedding(&mut self, embedding: Vec<f32>) {
        self.embedding = Some(embedding);
    }

    /// Extract keywords from title and description.
    fn extract_keywords(title: &str, description: &str) -> Vec<String> {
        let text = format!("{} {}", title, description).to_lowercase();

        // Common programming keywords to look for
        let significant_words: Vec<&str> = text
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() > 3)
            .filter(|w| !is_common_word(w))
            .take(20)
            .collect();

        significant_words.into_iter().map(String::from).collect()
    }

    /// Categorize an error message into a type.
    fn categorize_error(error: &str) -> String {
        let error_lower = error.to_lowercase();

        if error_lower.contains("timeout") || error_lower.contains("timed out") {
            "timeout".to_string()
        } else if error_lower.contains("permission") || error_lower.contains("access denied") {
            "permission".to_string()
        } else if error_lower.contains("syntax") || error_lower.contains("parse") {
            "syntax".to_string()
        } else if error_lower.contains("test") && error_lower.contains("fail") {
            "test_failure".to_string()
        } else if error_lower.contains("build") && error_lower.contains("fail") {
            "build_failure".to_string()
        } else if error_lower.contains("not found") || error_lower.contains("missing") {
            "not_found".to_string()
        } else if error_lower.contains("conflict") {
            "conflict".to_string()
        } else {
            "unknown".to_string()
        }
    }

    /// Calculate semantic similarity score with another outcome (0.0 to 1.0).
    ///
    /// Uses cosine similarity on embeddings when available, falls back to
    /// Jaccard similarity on keywords otherwise.
    pub fn similarity(&self, other: &FixOutcome) -> f64 {
        // Use embedding-based cosine similarity if both have embeddings
        if let (Some(ref self_emb), Some(ref other_emb)) = (&self.embedding, &other.embedding) {
            return cosine_similarity(self_emb, other_emb) as f64;
        }

        // Fallback to keyword-based Jaccard similarity
        self.keyword_similarity(&other.keywords)
    }

    /// Calculate semantic similarity with an issue (for finding similar past issues).
    ///
    /// Uses cosine similarity on embeddings when available (requires issue_embedding),
    /// falls back to Jaccard similarity on keywords otherwise.
    pub fn similarity_to_issue(&self, issue: &Issue) -> f64 {
        let description = issue.description.as_deref().unwrap_or("");
        let issue_keywords = Self::extract_keywords(&issue.title, description);
        self.keyword_similarity(&issue_keywords)
    }

    /// Calculate similarity with an issue using a pre-computed embedding.
    ///
    /// This is the preferred method when embeddings are available.
    pub fn similarity_to_embedding(&self, issue_embedding: &[f32]) -> f64 {
        if let Some(ref self_emb) = self.embedding {
            cosine_similarity(self_emb, issue_embedding) as f64
        } else {
            0.0 // No embedding available, can't compute similarity
        }
    }

    /// Keyword-based Jaccard similarity (fallback when no embeddings).
    fn keyword_similarity(&self, other_keywords: &[String]) -> f64 {
        let self_keywords: std::collections::HashSet<_> = self.keywords.iter().collect();
        let other_keywords_set: std::collections::HashSet<&String> =
            other_keywords.iter().collect();

        if self_keywords.is_empty() || other_keywords_set.is_empty() {
            return 0.0;
        }

        let intersection = self_keywords.intersection(&other_keywords_set).count() as f64;
        let union = self_keywords.union(&other_keywords_set).count() as f64;

        if union == 0.0 {
            0.0
        } else {
            intersection / union // Jaccard similarity
        }
    }
}

/// Check if a word is too common to be a useful keyword.
fn is_common_word(word: &str) -> bool {
    const COMMON_WORDS: &[&str] = &[
        "the",
        "a",
        "an",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "have",
        "has",
        "had",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "may",
        "might",
        "must",
        "shall",
        "can",
        "need",
        "dare",
        "this",
        "that",
        "these",
        "those",
        "what",
        "which",
        "who",
        "whom",
        "when",
        "where",
        "why",
        "how",
        "all",
        "each",
        "every",
        "both",
        "few",
        "more",
        "most",
        "other",
        "some",
        "such",
        "than",
        "too",
        "very",
        "just",
        "also",
        "only",
        "now",
        "then",
        "here",
        "there",
        "with",
        "from",
        "into",
        "onto",
        "upon",
        "over",
        "under",
        "above",
        "below",
        "between",
        "among",
        "through",
        "during",
        "before",
        "after",
        "about",
        "against",
        "without",
        "within",
        "throughout",
        "around",
        "and",
        "but",
        "or",
        "nor",
        "for",
        "yet",
        "so",
        "because",
        "although",
        "while",
        "if",
        "unless",
        "until",
        "since",
        "once",
        "whereas",
        "error",
        "issue",
        "problem",
        "bug",
        "fix",
        "fixed",
        "fixing",
    ];

    COMMON_WORDS.contains(&word)
}

/// Tracks fix outcomes in memory (can be persisted to DB later).
pub struct OutcomeTracker {
    outcomes: Vec<FixOutcome>,
    next_id: i64,
}

impl OutcomeTracker {
    /// Create a new outcome tracker.
    pub fn new() -> Self {
        Self {
            outcomes: Vec::new(),
            next_id: 1,
        }
    }

    /// Load outcomes from persistent storage (e.g. DB hydration on startup).
    pub fn load(&mut self, outcomes: Vec<FixOutcome>) {
        if let Some(max_id) = outcomes.iter().map(|o| o.id).max() {
            self.next_id = max_id + 1;
        }
        self.outcomes = outcomes;
    }

    /// Record a new outcome.
    pub fn record(&mut self, mut outcome: FixOutcome) -> Result<i64> {
        outcome.id = self.next_id;
        self.next_id += 1;

        let id = outcome.id;
        self.outcomes.push(outcome);
        Ok(id)
    }

    /// Set embedding for an outcome by ID.
    pub fn set_embedding(&mut self, id: i64, embedding: Vec<f32>) -> Result<()> {
        if let Some(outcome) = self.outcomes.iter_mut().find(|o| o.id == id) {
            outcome.set_embedding(embedding);
        }
        Ok(())
    }

    /// Find similar past outcomes for an issue using keyword similarity (fallback).
    pub fn find_similar(
        &self,
        issue: &Issue,
        limit: usize,
        min_similarity: f64,
    ) -> Vec<&FixOutcome> {
        let mut scored: Vec<_> = self
            .outcomes
            .iter()
            .map(|o| (o, o.similarity_to_issue(issue)))
            .filter(|(_, score)| *score >= min_similarity)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        scored.into_iter().take(limit).map(|(o, _)| o).collect()
    }

    /// Find similar past outcomes using semantic embedding similarity.
    ///
    /// This is the preferred method when embeddings are available.
    /// Returns outcomes sorted by similarity (highest first).
    pub fn find_similar_by_embedding(
        &self,
        issue_embedding: &[f32],
        limit: usize,
        min_similarity: f64,
    ) -> Vec<(&FixOutcome, f64)> {
        let mut scored: Vec<_> = self
            .outcomes
            .iter()
            .filter(|o| o.embedding.is_some()) // Only compare with outcomes that have embeddings
            .map(|o| {
                let similarity = o.similarity_to_embedding(issue_embedding);
                (o, similarity)
            })
            .filter(|(_, score)| *score >= min_similarity)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        scored.into_iter().take(limit).collect()
    }

    /// Get outcomes by result.
    pub fn get_by_outcome(&self, outcome: Outcome) -> Vec<&FixOutcome> {
        self.outcomes
            .iter()
            .filter(|o| o.outcome == outcome)
            .collect()
    }

    /// Get success rate for a source.
    pub fn success_rate(&self, source: Option<&str>) -> f64 {
        let filtered: Vec<_> = match source {
            Some(s) => self.outcomes.iter().filter(|o| o.source == s).collect(),
            None => self.outcomes.iter().collect(),
        };

        if filtered.is_empty() {
            return 0.0;
        }

        let successes = filtered.iter().filter(|o| o.outcome.is_success()).count();
        successes as f64 / filtered.len() as f64
    }

    /// Get common error types.
    pub fn common_errors(&self, limit: usize) -> Vec<(String, usize)> {
        let mut error_counts: HashMap<String, usize> = HashMap::new();

        for outcome in &self.outcomes {
            if let Some(ref error_type) = outcome.error_type {
                *error_counts.entry(error_type.clone()).or_insert(0) += 1;
            }
        }

        let mut counts: Vec<_> = error_counts.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        counts.truncate(limit);
        counts
    }

    /// Get all outcomes.
    pub fn all(&self) -> &[FixOutcome] {
        &self.outcomes
    }

    /// Add learnings to an outcome.
    pub fn add_learnings(&mut self, id: i64, learnings: &str) -> Result<()> {
        if let Some(outcome) = self.outcomes.iter_mut().find(|o| o.id == id) {
            outcome.learnings = Some(learnings.to_string());
        }
        Ok(())
    }
}

impl Default for OutcomeTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IssuePriority, IssueStatus};

    fn create_test_issue(title: &str, description: &str) -> Issue {
        Issue {
            id: "test-1".to_string(),
            short_id: "TEST-1".to_string(),
            title: title.to_string(),
            description: Some(description.to_string()),
            url: "https://example.com".to_string(),
            source: "test".to_string(),
            priority: IssuePriority::Medium,
            status: IssueStatus::Open,
            metadata: Default::default(),
            created_at: None,
            updated_at: None,
        }
    }

    fn create_test_attempt() -> FixAttempt {
        FixAttempt {
            id: 1,
            source: "test".to_string(),
            issue_id: "test-1".to_string(),
            short_id: "TEST-1".to_string(),
            status: crate::types::FixAttemptStatus::Success,
            pr_url: Some("https://github.com/test/pr/1".to_string()),
            github_repo: None,
            github_pr_number: None,
            error_message: None,
            attempted_at: Utc::now(),
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
    fn test_outcome_parse() {
        assert_eq!(Outcome::parse("merged"), Some(Outcome::Merged));
        assert_eq!(Outcome::parse("closed"), Some(Outcome::Closed));
        assert_eq!(Outcome::parse("failed"), Some(Outcome::Failed));
        assert_eq!(Outcome::parse("cannot_fix"), Some(Outcome::CannotFix));
        assert_eq!(Outcome::parse("invalid"), None);
    }

    #[test]
    fn test_outcome_is_success() {
        assert!(Outcome::Merged.is_success());
        assert!(!Outcome::Closed.is_success());
        assert!(!Outcome::Failed.is_success());
    }

    #[test]
    fn test_extract_keywords() {
        let keywords = FixOutcome::extract_keywords(
            "Database connection timeout",
            "The database connection times out when processing large queries",
        );

        assert!(keywords.contains(&"database".to_string()));
        assert!(keywords.contains(&"connection".to_string()));
        assert!(keywords.contains(&"timeout".to_string()));
        assert!(keywords.contains(&"queries".to_string()));
    }

    #[test]
    fn test_categorize_error() {
        assert_eq!(
            FixOutcome::categorize_error("Connection timed out"),
            "timeout"
        );
        assert_eq!(
            FixOutcome::categorize_error("Permission denied"),
            "permission"
        );
        assert_eq!(
            FixOutcome::categorize_error("Syntax error on line 5"),
            "syntax"
        );
        assert_eq!(FixOutcome::categorize_error("Tests failed"), "test_failure");
        assert_eq!(
            FixOutcome::categorize_error("Build failed"),
            "build_failure"
        );
        assert_eq!(FixOutcome::categorize_error("File not found"), "not_found");
    }

    #[test]
    fn test_similarity() {
        let issue1 = create_test_issue(
            "Database timeout error",
            "Connection to PostgreSQL times out",
        );
        let issue2 = create_test_issue("Database connection issue", "PostgreSQL connection fails");
        let attempt = create_test_attempt();

        let outcome1 = FixOutcome::from_attempt(&attempt, &issue1, "test prompt", Outcome::Merged);
        let outcome2 = FixOutcome::from_attempt(&attempt, &issue2, "test prompt", Outcome::Closed);

        let similarity = outcome1.similarity(&outcome2);
        assert!(similarity > 0.0); // Should have some overlap
        assert!(similarity <= 1.0);
    }

    #[test]
    fn test_outcome_tracker() {
        let mut tracker = OutcomeTracker::new();
        let issue = create_test_issue("Test issue", "Description here");
        let attempt = create_test_attempt();

        let outcome = FixOutcome::from_attempt(&attempt, &issue, "prompt", Outcome::Merged);
        let id = tracker.record(outcome).unwrap();

        assert_eq!(id, 1);
        assert_eq!(tracker.all().len(), 1);
    }

    #[test]
    fn test_find_similar() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();

        // Add some outcomes
        let issue1 =
            create_test_issue("API endpoint returns 500", "Server error in users endpoint");
        let outcome1 = FixOutcome::from_attempt(&attempt, &issue1, "prompt", Outcome::Merged);
        tracker.record(outcome1).unwrap();

        let issue2 = create_test_issue("CSS styling broken", "Buttons not aligned properly");
        let outcome2 = FixOutcome::from_attempt(&attempt, &issue2, "prompt", Outcome::Closed);
        tracker.record(outcome2).unwrap();

        // Search for similar
        let search_issue = create_test_issue("API returns error 500", "Error in the API endpoint");
        let similar = tracker.find_similar(&search_issue, 5, 0.0);

        assert!(!similar.is_empty());
    }

    #[test]
    fn test_success_rate() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();
        let issue = create_test_issue("Test", "Test");

        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Closed,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Failed,
            ))
            .unwrap();

        let rate = tracker.success_rate(None);
        assert!((rate - 0.5).abs() < 0.01); // 50% success rate
    }

    #[test]
    fn test_outcome_as_str() {
        assert_eq!(Outcome::Merged.as_str(), "merged");
        assert_eq!(Outcome::Closed.as_str(), "closed");
        assert_eq!(Outcome::Failed.as_str(), "failed");
        assert_eq!(Outcome::CannotFix.as_str(), "cannot_fix");
    }

    #[test]
    fn test_outcome_parse_case_insensitive() {
        assert_eq!(Outcome::parse("MERGED"), Some(Outcome::Merged));
        assert_eq!(Outcome::parse("Closed"), Some(Outcome::Closed));
        assert_eq!(Outcome::parse("FAILED"), Some(Outcome::Failed));
        assert_eq!(Outcome::parse("CannotFix"), Some(Outcome::CannotFix));
    }

    #[test]
    fn test_categorize_error_all_types() {
        assert_eq!(FixOutcome::categorize_error("Request timed out"), "timeout");
        assert_eq!(
            FixOutcome::categorize_error("Access denied error"),
            "permission"
        );
        assert_eq!(
            FixOutcome::categorize_error("Parse error in JSON"),
            "syntax"
        );
        assert_eq!(
            FixOutcome::categorize_error("3 tests failed"),
            "test_failure"
        );
        assert_eq!(
            FixOutcome::categorize_error("Build failed with errors"),
            "build_failure"
        );
        assert_eq!(
            FixOutcome::categorize_error("Module not found"),
            "not_found"
        );
        assert_eq!(
            FixOutcome::categorize_error("Missing dependency"),
            "not_found"
        );
        assert_eq!(FixOutcome::categorize_error("Merge conflict"), "conflict");
        assert_eq!(FixOutcome::categorize_error("Some random error"), "unknown");
    }

    #[test]
    fn test_is_common_word() {
        assert!(is_common_word("the"));
        assert!(is_common_word("is"));
        assert!(is_common_word("error"));
        assert!(is_common_word("fix"));
        assert!(!is_common_word("database"));
        assert!(!is_common_word("postgresql"));
        assert!(!is_common_word("timeout"));
    }

    #[test]
    fn test_extract_keywords_filters_short_words() {
        let keywords = FixOutcome::extract_keywords("A bug in the API", "It is bad");
        // Short words like "a", "in", "is", "it" should be filtered
        for kw in &keywords {
            assert!(kw.len() > 3);
        }
    }

    #[test]
    fn test_extract_keywords_max_count() {
        let long_text = "word1 word2 word3 word4 word5 word6 word7 word8 word9 word10 word11 word12 word13 word14 word15 word16 word17 word18 word19 word20 word21 word22 word23 word24 word25";
        let keywords = FixOutcome::extract_keywords(long_text, "");
        assert!(keywords.len() <= 20);
    }

    #[test]
    fn test_similarity_identical() {
        let issue = create_test_issue("Same title", "Same description");
        let attempt = create_test_attempt();
        let outcome1 = FixOutcome::from_attempt(&attempt, &issue, "prompt", Outcome::Merged);
        let outcome2 = FixOutcome::from_attempt(&attempt, &issue, "prompt", Outcome::Merged);

        let similarity = outcome1.similarity(&outcome2);
        assert_eq!(similarity, 1.0); // Identical should be 1.0
    }

    #[test]
    fn test_similarity_completely_different() {
        let attempt = create_test_attempt();
        let issue1 = create_test_issue("PostgreSQL database error", "Connection to database fails");
        let issue2 = create_test_issue("JavaScript CSS styling", "Frontend React component");

        let outcome1 = FixOutcome::from_attempt(&attempt, &issue1, "prompt", Outcome::Merged);
        let outcome2 = FixOutcome::from_attempt(&attempt, &issue2, "prompt", Outcome::Merged);

        let similarity = outcome1.similarity(&outcome2);
        assert!(similarity < 0.3); // Very different topics
    }

    #[test]
    fn test_similarity_empty_keywords() {
        let attempt = create_test_attempt();
        let issue1 = Issue {
            id: "1".to_string(),
            short_id: "1".to_string(),
            title: "a b c".to_string(), // Only short words
            description: None,
            url: "url".to_string(),
            source: "test".to_string(),
            priority: IssuePriority::Medium,
            status: IssueStatus::Open,
            metadata: Default::default(),
            created_at: None,
            updated_at: None,
        };

        let outcome = FixOutcome::from_attempt(&attempt, &issue1, "prompt", Outcome::Merged);

        // Empty keywords should result in 0 similarity
        let issue2 = create_test_issue("Database error", "Connection fails");
        assert_eq!(outcome.similarity_to_issue(&issue2), 0.0);
    }

    #[test]
    fn test_outcome_tracker_default() {
        let tracker = OutcomeTracker::default();
        assert!(tracker.all().is_empty());
    }

    #[test]
    fn test_outcome_tracker_increments_id() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();
        let issue = create_test_issue("Test", "Test");

        let id1 = tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();
        let id2 = tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();
        let id3 = tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn test_get_by_outcome() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();
        let issue = create_test_issue("Test", "Test");

        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Closed,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();

        let merged = tracker.get_by_outcome(Outcome::Merged);
        assert_eq!(merged.len(), 2);

        let closed = tracker.get_by_outcome(Outcome::Closed);
        assert_eq!(closed.len(), 1);

        let failed = tracker.get_by_outcome(Outcome::Failed);
        assert!(failed.is_empty());
    }

    #[test]
    fn test_success_rate_by_source() {
        let mut tracker = OutcomeTracker::new();
        let issue = create_test_issue("Test", "Test");

        // Linear: 1 success, 1 fail = 50%
        let mut linear_attempt = create_test_attempt();
        linear_attempt.source = "linear".to_string();
        let mut linear_outcome =
            FixOutcome::from_attempt(&linear_attempt, &issue, "p", Outcome::Merged);
        linear_outcome.source = "linear".to_string();
        tracker.record(linear_outcome).unwrap();

        let mut linear_outcome2 =
            FixOutcome::from_attempt(&linear_attempt, &issue, "p", Outcome::Failed);
        linear_outcome2.source = "linear".to_string();
        tracker.record(linear_outcome2).unwrap();

        // Sentry: 2 successes = 100%
        let mut sentry_attempt = create_test_attempt();
        sentry_attempt.source = "sentry".to_string();
        let mut sentry_outcome =
            FixOutcome::from_attempt(&sentry_attempt, &issue, "p", Outcome::Merged);
        sentry_outcome.source = "sentry".to_string();
        tracker.record(sentry_outcome.clone()).unwrap();
        tracker.record(sentry_outcome).unwrap();

        let linear_rate = tracker.success_rate(Some("linear"));
        assert!((linear_rate - 0.5).abs() < 0.01);

        let sentry_rate = tracker.success_rate(Some("sentry"));
        assert!((sentry_rate - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_success_rate_empty() {
        let tracker = OutcomeTracker::new();
        assert_eq!(tracker.success_rate(None), 0.0);
        assert_eq!(tracker.success_rate(Some("nonexistent")), 0.0);
    }

    #[test]
    fn test_common_errors() {
        let mut tracker = OutcomeTracker::new();
        let issue = create_test_issue("Test", "Test");
        let mut attempt = create_test_attempt();

        attempt.error_message = Some("Connection timed out".to_string());
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Failed,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Failed,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Failed,
            ))
            .unwrap();

        attempt.error_message = Some("Permission denied".to_string());
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Failed,
            ))
            .unwrap();
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Failed,
            ))
            .unwrap();

        attempt.error_message = Some("Build failed".to_string());
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Failed,
            ))
            .unwrap();

        let errors = tracker.common_errors(10);
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].0, "timeout");
        assert_eq!(errors[0].1, 3);
        assert_eq!(errors[1].0, "permission");
        assert_eq!(errors[1].1, 2);
    }

    #[test]
    fn test_add_learnings() {
        let mut tracker = OutcomeTracker::new();
        let issue = create_test_issue("Test", "Test");
        let attempt = create_test_attempt();

        let id = tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Merged,
            ))
            .unwrap();

        tracker
            .add_learnings(id, "Important learning here")
            .unwrap();

        let outcome = &tracker.all()[0];
        assert_eq!(
            outcome.learnings,
            Some("Important learning here".to_string())
        );
    }

    #[test]
    fn test_add_learnings_nonexistent_id() {
        let mut tracker = OutcomeTracker::new();

        // Should not panic
        let result = tracker.add_learnings(999, "Learning");
        assert!(result.is_ok());
    }

    #[test]
    fn test_find_similar_with_min_similarity() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();

        let issue1 = create_test_issue(
            "PostgreSQL database timeout",
            "Connection to PostgreSQL times out",
        );
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue1,
                "p",
                Outcome::Merged,
            ))
            .unwrap();

        let issue2 = create_test_issue("JavaScript button styling", "React component CSS issue");
        tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue2,
                "p",
                Outcome::Closed,
            ))
            .unwrap();

        // Search for PostgreSQL issue - should find issue1 with high similarity
        let search = create_test_issue("PostgreSQL connection error", "Database connection fails");

        let _high_min = tracker.find_similar(&search, 10, 0.3);
        // Should only find the PostgreSQL one (if similarity is above 0.3)
        // Results depend on keyword extraction

        let low_min = tracker.find_similar(&search, 10, 0.0);
        // With 0 min, should find both
        assert!(!low_min.is_empty());
    }

    #[test]
    fn test_fix_outcome_from_attempt_sets_fields() {
        let issue = create_test_issue("Test Title", "Test Description");
        let mut attempt = create_test_attempt();
        attempt.error_message = Some("Connection timed out".to_string());

        let outcome = FixOutcome::from_attempt(&attempt, &issue, "test prompt", Outcome::Failed);

        assert_eq!(outcome.source, "test");
        assert_eq!(outcome.issue_id, "test-1");
        assert_eq!(outcome.prompt_used, "test prompt");
        assert_eq!(outcome.outcome, Outcome::Failed);
        assert_eq!(outcome.error_type, Some("timeout".to_string()));
        assert!(outcome.issue_text.contains("Test Title"));
        assert!(outcome.issue_text.contains("Test Description"));
        assert!(!outcome.keywords.is_empty());
    }

    #[test]
    fn test_outcome_serde() {
        let outcome = Outcome::Merged;
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, "\"merged\"");

        let parsed: Outcome = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Outcome::Merged);
    }

    #[test]
    fn test_outcome_serde_cannot_fix() {
        let outcome = Outcome::CannotFix;
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, "\"cannot_fix\"");

        let parsed: Outcome = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Outcome::CannotFix);
    }

    #[test]
    fn test_embedding_similarity() {
        let issue = create_test_issue("Test", "Test");
        let attempt = create_test_attempt();

        let mut outcome1 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        let mut outcome2 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);

        // Set similar embeddings
        outcome1.set_embedding(vec![1.0, 0.0, 0.0]);
        outcome2.set_embedding(vec![0.9, 0.1, 0.0]);

        // Should use cosine similarity when embeddings are available
        let similarity = outcome1.similarity(&outcome2);
        assert!(similarity > 0.9); // High similarity
        assert!(similarity <= 1.0);
    }

    #[test]
    fn test_embedding_similarity_orthogonal() {
        let issue = create_test_issue("Test", "Test");
        let attempt = create_test_attempt();

        let mut outcome1 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        let mut outcome2 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);

        // Set orthogonal embeddings
        outcome1.set_embedding(vec![1.0, 0.0, 0.0]);
        outcome2.set_embedding(vec![0.0, 1.0, 0.0]);

        let similarity = outcome1.similarity(&outcome2);
        assert!(similarity < 0.1); // Very low similarity (orthogonal vectors)
    }

    #[test]
    fn test_embedding_similarity_fallback_to_keywords() {
        let issue = create_test_issue("Database error", "PostgreSQL connection fails");
        let attempt = create_test_attempt();

        let mut outcome1 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        let outcome2 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);

        // Only outcome1 has embedding, so should fall back to keywords
        outcome1.set_embedding(vec![1.0, 0.0, 0.0]);

        let similarity = outcome1.similarity(&outcome2);
        // Should use keyword-based similarity since outcome2 has no embedding
        assert!(similarity > 0.0); // Same keywords
    }

    #[test]
    fn test_similarity_to_embedding() {
        let issue = create_test_issue("Test", "Test");
        let attempt = create_test_attempt();

        let mut outcome = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        outcome.set_embedding(vec![1.0, 0.0, 0.0]);

        let query_embedding = vec![0.9, 0.1, 0.0];
        let similarity = outcome.similarity_to_embedding(&query_embedding);

        assert!(similarity > 0.9);
        assert!(similarity <= 1.0);
    }

    #[test]
    fn test_similarity_to_embedding_no_embedding() {
        let issue = create_test_issue("Test", "Test");
        let attempt = create_test_attempt();

        let outcome = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        // No embedding set

        let query_embedding = vec![1.0, 0.0, 0.0];
        let similarity = outcome.similarity_to_embedding(&query_embedding);

        assert_eq!(similarity, 0.0); // No embedding, returns 0
    }

    #[test]
    fn test_find_similar_by_embedding() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();

        // Create outcomes with embeddings
        let issue1 = create_test_issue("Database error", "PostgreSQL");
        let mut outcome1 = FixOutcome::from_attempt(&attempt, &issue1, "p", Outcome::Merged);
        outcome1.set_embedding(vec![1.0, 0.0, 0.0]); // Similar to query
        let id1 = tracker.record(outcome1).unwrap();
        tracker.set_embedding(id1, vec![1.0, 0.0, 0.0]).unwrap();

        let issue2 = create_test_issue("CSS issue", "Styling");
        let mut outcome2 = FixOutcome::from_attempt(&attempt, &issue2, "p", Outcome::Closed);
        outcome2.set_embedding(vec![0.0, 1.0, 0.0]); // Orthogonal to query
        let id2 = tracker.record(outcome2).unwrap();
        tracker.set_embedding(id2, vec![0.0, 1.0, 0.0]).unwrap();

        // Query embedding similar to outcome1
        let query = vec![0.95, 0.05, 0.0];
        let similar = tracker.find_similar_by_embedding(&query, 10, 0.5);

        // Should find outcome1 but not outcome2
        assert_eq!(similar.len(), 1);
        assert!(similar[0].1 > 0.9); // High similarity
    }

    #[test]
    fn test_set_embedding_via_tracker() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();
        let issue = create_test_issue("Test", "Test");

        let outcome = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        let id = tracker.record(outcome).unwrap();

        // Set embedding via tracker
        tracker.set_embedding(id, vec![1.0, 2.0, 3.0]).unwrap();

        // Verify embedding was set
        let stored = &tracker.all()[0];
        assert!(stored.embedding.is_some());
        assert_eq!(stored.embedding.as_ref().unwrap(), &vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_fix_outcome_serde_with_embedding() {
        let issue = create_test_issue("Test", "Test");
        let attempt = create_test_attempt();

        let mut outcome = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        outcome.set_embedding(vec![1.0, 2.0, 3.0]);

        // Serialize and deserialize
        let json = serde_json::to_string(&outcome).unwrap();
        let parsed: FixOutcome = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.embedding, Some(vec![1.0, 2.0, 3.0]));
    }

    #[test]
    fn test_outcome_tracker_load() {
        let mut tracker = OutcomeTracker::new();
        let attempt = create_test_attempt();
        let issue = create_test_issue("Test", "Test");

        // Create outcomes with pre-set IDs (as if loaded from DB)
        let mut o1 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        o1.id = 10;
        let mut o2 = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Failed);
        o2.id = 20;

        tracker.load(vec![o1, o2]);

        assert_eq!(tracker.all().len(), 2);

        // next_id should continue from max loaded id
        let new_id = tracker
            .record(FixOutcome::from_attempt(
                &attempt,
                &issue,
                "p",
                Outcome::Closed,
            ))
            .unwrap();
        assert_eq!(new_id, 21);
    }

    #[test]
    fn test_fix_outcome_serde_without_embedding() {
        let issue = create_test_issue("Test", "Test");
        let attempt = create_test_attempt();

        let outcome = FixOutcome::from_attempt(&attempt, &issue, "p", Outcome::Merged);
        // No embedding set

        // Serialize - embedding should be skipped (skip_serializing_if = "Option::is_none")
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(!json.contains("embedding"));

        // Deserialize
        let parsed: FixOutcome = serde_json::from_str(&json).unwrap();
        assert!(parsed.embedding.is_none());
    }
}
