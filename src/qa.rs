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
    use crate::types::QaKnowledgeEntry;

    fn make_question(question: &str) -> BlockingQuestion {
        BlockingQuestion {
            question: question.to_string(),
            context: None,
            options: vec![],
            why: None,
        }
    }

    fn make_qa_entry(question: &str, answer: &str) -> QaKnowledgeEntry {
        QaKnowledgeEntry {
            id: 1,
            source: "test-source".to_string(),
            repo: None,
            issue_id: "ISSUE-1".to_string(),
            short_id: "abc123".to_string(),
            question_text: question.to_string(),
            question_norm: normalize_text(question),
            question_embedding: None,
            answer_text: answer.to_string(),
            answer_norm: normalize_text(answer),
            answer_embedding: None,
            channel: "slack".to_string(),
            responder: None,
            correlation_id: "corr-1".to_string(),
            asked_at: Utc::now(),
            answered_at: Utc::now(),
            success_count: 0,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        }
    }

    fn make_qa_match(question: &str, answer: &str, score: f64) -> QaMatch {
        QaMatch {
            entry: make_qa_entry(question, answer),
            semantic_similarity: score,
            historical_success_rate: 1.0,
            final_score: score,
        }
    }

    // ---- normalize_text ----

    #[test]
    fn test_normalize_text() {
        assert_eq!(normalize_text("  Hello   WORLD "), "hello world");
    }

    #[test]
    fn test_normalize_text_empty_string() {
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn test_normalize_text_all_whitespace() {
        assert_eq!(normalize_text("   \t  \n  "), "");
    }

    #[test]
    fn test_normalize_text_already_normalized() {
        assert_eq!(normalize_text("hello world"), "hello world");
    }

    #[test]
    fn test_normalize_text_mixed_case_multiple_spaces() {
        assert_eq!(normalize_text("FOO   bar   BAZ"), "foo bar baz");
    }

    #[test]
    fn test_normalize_text_tabs_and_newlines() {
        assert_eq!(normalize_text("hello\t\tworld\nfoo"), "hello world foo");
    }

    #[test]
    fn test_normalize_text_unicode() {
        assert_eq!(normalize_text("  Héllo  Wörld  "), "héllo wörld");
    }

    #[test]
    fn test_normalize_text_leading_trailing_whitespace() {
        assert_eq!(normalize_text("   trimmed   "), "trimmed");
    }

    #[test]
    fn test_normalize_text_single_word() {
        assert_eq!(normalize_text("WORD"), "word");
    }

    // ---- build_correlation_id ----

    #[test]
    fn test_build_correlation_id_non_empty() {
        let id = build_correlation_id("abc");
        assert!(!id.is_empty());
    }

    #[test]
    fn test_build_correlation_id_contains_sanitized_short_id() {
        let id = build_correlation_id("myid");
        assert!(id.starts_with("myid-"));
    }

    #[test]
    fn test_build_correlation_id_strips_special_chars() {
        let id = build_correlation_id("a@b#c!");
        assert!(id.starts_with("abc-"));
    }

    #[test]
    fn test_build_correlation_id_different_on_each_call() {
        let id1 = build_correlation_id("test");
        let id2 = build_correlation_id("test");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_build_correlation_id_hyphenated_three_parts() {
        let id = build_correlation_id("foo");
        let parts: Vec<&str> = id.splitn(3, '-').collect();
        assert_eq!(
            parts.len(),
            3,
            "Expected 3 hyphen-separated parts, got: {id}"
        );
        assert_eq!(parts[0], "foo");
        assert!(!parts[1].is_empty(), "Timestamp part should not be empty");
        assert!(!parts[2].is_empty(), "Random suffix should not be empty");
    }

    // ---- format_answer_context ----

    #[test]
    fn test_format_answer_context() {
        let q = make_question("Which branch?");
        let out = format_answer_context(&q, "main", "email", false);
        assert!(out.contains("Which branch?"));
        assert!(out.contains("main"));
    }

    #[test]
    fn test_format_answer_context_reused_vs_asked() {
        let q = make_question("Which branch?");
        let reused = format_answer_context(&q, "main", "email", true);
        let asked = format_answer_context(&q, "main", "email", false);
        assert!(reused.contains("(reused)"));
        assert!(asked.contains("(asked)"));
        assert!(!reused.contains("(asked)"));
        assert!(!asked.contains("(reused)"));
    }

    #[test]
    fn test_format_answer_context_trims_answer_whitespace() {
        let q = make_question("Color?");
        let out = format_answer_context(&q, "  blue  ", "slack", false);
        assert!(out.contains("Answer: blue\n"));
        assert!(!out.contains("  blue  "));
    }

    #[test]
    fn test_format_answer_context_all_optional_fields() {
        let q = BlockingQuestion {
            question: "Deploy?".to_string(),
            context: Some("CI passed".to_string()),
            options: vec!["yes".to_string(), "no".to_string()],
            why: Some("Blocking release".to_string()),
        };
        let out = format_answer_context(&q, "yes", "slack", false);
        assert!(out.contains("Deploy?"));
        assert!(out.contains("Answer: yes"));
        assert!(out.contains("Source: slack (asked)"));
    }

    #[test]
    fn test_format_answer_context_no_optional_fields() {
        let q = make_question("Proceed?");
        let out = format_answer_context(&q, "ok", "email", true);
        assert!(out.contains("## Human Answer\n"));
        assert!(out.contains("Question: Proceed?"));
        assert!(out.contains("Answer: ok"));
        assert!(out.contains("Source: email (reused)"));
    }

    #[test]
    fn test_format_answer_context_empty_answer() {
        let q = make_question("Anything?");
        let out = format_answer_context(&q, "", "slack", false);
        assert!(out.contains("Answer: \n"));
    }

    #[test]
    fn test_format_answer_context_unicode() {
        let q = make_question("Quel nom?");
        let out = format_answer_context(&q, "café", "général", false);
        assert!(out.contains("Quel nom?"));
        assert!(out.contains("café"));
        assert!(out.contains("général"));
    }

    #[test]
    fn test_format_answer_context_empty_channel() {
        let q = make_question("Ok?");
        let out = format_answer_context(&q, "yes", "", false);
        assert!(out.contains("Source:  (asked)"));
    }

    // ---- format_timeout_context ----

    #[test]
    fn test_format_timeout_context_basic() {
        let q = make_question("Approve deploy?");
        let out = format_timeout_context(&q);
        assert!(out.contains("Approve deploy?"));
        assert!(out.contains("## Human Answer Timeout"));
        assert!(out.contains("No human reply received in time"));
        assert!(out.contains("uncertainty and assumptions"));
    }

    #[test]
    fn test_format_timeout_context_special_characters() {
        let q = make_question("What about <script> & \"quotes\"?");
        let out = format_timeout_context(&q);
        assert!(out.contains("<script>"));
        assert!(out.contains("& \"quotes\"?"));
    }

    #[test]
    fn test_format_timeout_context_contains_header_and_instruction() {
        let q = make_question("Confirm?");
        let out = format_timeout_context(&q);
        assert!(out.starts_with("## Human Answer Timeout\n"));
        assert!(out.contains("Proceed with best effort"));
        assert!(out.contains("explicitly call out"));
    }

    // ---- format_reuse_context ----

    #[test]
    fn test_format_reuse_context_empty() {
        let result = format_reuse_context(&[]);
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_reuse_context_single_match() {
        let matches = vec![make_qa_match("Is it safe?", "Yes", 0.95)];
        let out = format_reuse_context(&matches);
        assert!(out.contains("- Q: Is it safe?\n"));
        assert!(out.contains("  A: Yes\n"));
        assert!(out.contains("  Score: 0.950\n"));
    }

    #[test]
    fn test_format_reuse_context_multiple_matches() {
        let matches = vec![
            make_qa_match("First?", "Alpha", 0.9),
            make_qa_match("Second?", "Beta", 0.8),
            make_qa_match("Third?", "Gamma", 0.7),
        ];
        let out = format_reuse_context(&matches);
        assert!(out.contains("- Q: First?"));
        assert!(out.contains("- Q: Second?"));
        assert!(out.contains("- Q: Third?"));
        assert!(out.contains("  A: Alpha"));
        assert!(out.contains("  A: Beta"));
        assert!(out.contains("  A: Gamma"));
    }

    #[test]
    fn test_format_reuse_context_contains_header() {
        let matches = vec![make_qa_match("Q?", "A", 0.5)];
        let out = format_reuse_context(&matches);
        assert!(out.starts_with("## Reused Human Q&A Context\n"));
    }

    #[test]
    fn test_format_reuse_context_score_three_decimal_places() {
        let matches = vec![
            make_qa_match("Q1?", "A1", 0.123456),
            make_qa_match("Q2?", "A2", 1.0),
            make_qa_match("Q3?", "A3", 0.0),
        ];
        let out = format_reuse_context(&matches);
        assert!(out.contains("Score: 0.123\n"));
        assert!(out.contains("Score: 1.000\n"));
        assert!(out.contains("Score: 0.000\n"));
    }

    // ── Edge case tests ──

    #[test]
    fn test_normalize_text_very_long_string() {
        let long = "word ".repeat(10_000);
        let normalized = normalize_text(&long);
        assert!(!normalized.is_empty());
        assert!(!normalized.starts_with(' '));
        assert!(!normalized.ends_with(' '));
    }

    #[test]
    fn test_normalize_text_only_special_chars() {
        assert_eq!(normalize_text("!@#$%^&*()"), "!@#$%^&*()");
    }

    #[test]
    fn test_normalize_text_mixed_whitespace_types() {
        assert_eq!(normalize_text("a\tb\nc\rd"), "a b c d");
    }

    #[test]
    fn test_build_correlation_id_empty_short_id() {
        let id = build_correlation_id("");
        assert!(id.starts_with('-'));
    }

    #[test]
    fn test_build_correlation_id_very_long() {
        let long_id = "a".repeat(1000);
        let id = build_correlation_id(&long_id);
        assert!(id.len() > 1000);
    }

    #[test]
    fn test_build_correlation_id_all_special_chars() {
        let id = build_correlation_id("!@#$%^&*()");
        assert!(id.starts_with('-'));
    }

    #[test]
    fn test_format_reuse_context_score_negative() {
        let matches = vec![make_qa_match("Q?", "A", -0.5)];
        let out = format_reuse_context(&matches);
        assert!(out.contains("Score: -0.500"));
    }

    #[test]
    fn test_format_answer_context_multiline_answer() {
        let q = make_question("Details?");
        let out = format_answer_context(&q, "Line 1\nLine 2\nLine 3", "slack", false);
        assert!(out.contains("Line 1\nLine 2\nLine 3"));
    }

    #[test]
    fn test_format_timeout_context_empty_question() {
        let q = make_question("");
        let out = format_timeout_context(&q);
        assert!(out.contains("Question: "));
    }

    #[test]
    fn test_format_reuse_context_high_precision_scores() {
        let matches = vec![
            make_qa_match("Q1?", "A1", 0.999999),
            make_qa_match("Q2?", "A2", 0.000001),
        ];
        let out = format_reuse_context(&matches);
        assert!(out.contains("Score: 1.000"));
        assert!(out.contains("Score: 0.000"));
    }

    #[test]
    fn test_format_answer_context_very_long_answer() {
        let q = make_question("What?");
        let answer = "x".repeat(10_000);
        let out = format_answer_context(&q, &answer, "ch", false);
        assert!(out.len() > 10_000);
    }

    #[test]
    fn test_find_reusable_qa_with_sqlite_tracker() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let ask_config = AskConfig::default();
        let result = find_reusable_qa(
            &tracker,
            &ask_config,
            "linear",
            Some("org/repo"),
            "how",
            None,
        )
        .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_reusable_qa_no_repo() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let ask_config = AskConfig::default();
        let result = find_reusable_qa(&tracker, &ask_config, "sentry", None, "what", None).unwrap();
        assert!(result.is_empty());
    }
}
