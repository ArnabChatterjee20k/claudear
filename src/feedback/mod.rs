//! AI Feedback Loop module.
//!
//! Learns from past fix attempts to improve future prompts.
//! Tracks outcomes of merged vs closed PRs and uses similarity
//! matching to find relevant learnings.

mod analyzer;
mod embeddings;
mod outcomes;

pub use analyzer::{FeedbackAnalyzer, PromptSuggestion, SimilarIssue};
pub use embeddings::{
    cosine_similarity, euclidean_distance, normalize, EmbeddingClient, EmbeddingConfig,
    EmbeddingResult, MemoryVectorStore,
};
pub use outcomes::{FixOutcome, Outcome, OutcomeTracker};
