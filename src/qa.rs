//! Shared human Q&A utilities for semantic reuse and prompt context formatting.

use crate::config::AskConfig;
use crate::error::Result;
use crate::feedback::EmbeddingClient;
use crate::storage::FixAttemptTracker;
use crate::types::{BlockingQuestion, QaMatch};
use chrono::Utc;

/// Normalize free-form text for exact-match fallback.
pub fn normalize_text(input: &str) -> String {
    input
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a correlation token for ask notifications.
pub fn build_correlation_id(short_id: &str) -> String {
    let suffix: u32 = rand::random();
    format!(
        "{}-{}-{}",
        short_id.replace(|c: char| !c.is_ascii_alphanumeric(), ""),
        Utc::now().timestamp_millis(),
        suffix
    )
}

/// Generate an embedding when the embedding client is available.
pub async fn embed_text(client: Option<&EmbeddingClient>, text: &str) -> Option<Vec<f32>> {
    match client {
        Some(c) => c.embed(text).await.ok(),
        None => None,
    }
}

/// Find reusable Q&A matches with scoped-first/global-fallback retrieval.
pub fn find_reusable_qa(
    tracker: &dyn FixAttemptTracker,
    ask_config: &AskConfig,
    source: &str,
    repo: Option<&str>,
    question_norm: &str,
    question_embedding: Option<&[f32]>,
) -> Result<Vec<QaMatch>> {
    let scoped = tracker.find_similar_qa_scoped(
        source,
        repo,
        question_norm,
        question_embedding,
        ask_config.semantic_threshold_scoped,
        ask_config.max_reuse_candidates,
    )?;

    if !scoped.is_empty() {
        return Ok(scoped);
    }

    tracker.find_similar_qa_global(
        question_norm,
        question_embedding,
        ask_config.semantic_threshold_global,
        ask_config.max_reuse_candidates,
    )
}

/// Build prompt context from reusable Q&A snippets.
pub fn format_reuse_context(matches: &[QaMatch]) -> String {
    if matches.is_empty() {
        return String::new();
    }

    let mut out = String::from("## Reused Human Q&A Context\n");
    for m in matches {
        out.push_str(&format!(
            "- Q: {}\n  A: {}\n  Score: {:.3}\n",
            m.entry.question_text, m.entry.answer_text, m.final_score
        ));
    }
    out
}

/// Build prompt context for a concrete answer (reused or freshly asked).
pub fn format_answer_context(
    question: &BlockingQuestion,
    answer: &str,
    channel: &str,
    reused: bool,
) -> String {
    format!(
        "## Human Answer\nQuestion: {}\nAnswer: {}\nSource: {} ({})\n",
        question.question,
        answer.trim(),
        channel,
        if reused { "reused" } else { "asked" }
    )
}

/// Build prompt context for timeout fallback.
pub fn format_timeout_context(question: &BlockingQuestion) -> String {
    format!(
        "## Human Answer Timeout\nQuestion: {}\nNo human reply received in time. Proceed with best effort and explicitly call out uncertainty and assumptions.",
        question.question
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_text() {
        assert_eq!(normalize_text("  Hello   WORLD "), "hello world");
    }

    #[test]
    fn test_format_answer_context() {
        let q = BlockingQuestion {
            question: "Which branch?".to_string(),
            context: None,
            options: vec![],
            why: None,
        };
        let out = format_answer_context(&q, "main", "email", false);
        assert!(out.contains("Which branch?"));
        assert!(out.contains("main"));
    }
}
