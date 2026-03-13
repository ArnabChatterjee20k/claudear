//! LLM-enhanced analysis trait and types.
//!
//! Defines the interface for LLM-assisted issue triage, review classification,
//! log extraction, and cross-repo correlation enrichment. Implementations live
//! in `claudear-engine`; callers fall back to heuristics when no LLM is available.

use claudear_core::types::{BlastRadius, ExtractedLearnings, Issue, MatchResult, ReviewCategory};

/// LLM assessment for a single issue in the prioritisation batch.
#[derive(Debug, Clone)]
pub struct LlmIssueAssessment {
    pub issue_id: String,
    pub severity: f64,
    pub blast_radius: BlastRadius,
    pub fingerprint: String,
}

/// LLM-extracted learnings from an execution log.
#[derive(Debug, Clone)]
pub struct LlmLogAnalysis {
    pub learnings: ExtractedLearnings,
    pub fix_approach: String,
    pub strategy_summary: String,
}

/// LLM explanation of a cross-repo correlation.
#[derive(Debug, Clone)]
pub struct LlmCorrelationExplanation {
    pub repo_a: String,
    pub repo_b: String,
    pub explanation: String,
    pub confidence: f64,
}

/// Trait for LLM-enhanced analysis across the pipeline.
pub trait LlmAnalyzer: Send + Sync {
    /// Batch-assess issues for severity, blast radius, and clustering fingerprint.
    fn assess_issues(&self, candidates: &[(Issue, MatchResult)])
        -> Option<Vec<LlmIssueAssessment>>;

    /// Classify a review comment into a category.
    fn classify_review(&self, comment_body: &str) -> Option<ReviewCategory>;

    /// Extract structured learnings from execution log text.
    fn extract_learnings(&self, log_text: &str) -> Option<LlmLogAnalysis>;

    /// Enrich detected cross-repo correlations with causal explanations.
    fn explain_correlations(
        &self,
        correlations: &[(String, String, i64)],
        issues_context: &str,
    ) -> Vec<LlmCorrelationExplanation>;
}
