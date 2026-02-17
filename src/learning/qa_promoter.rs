//! System 3: Promote repeated Q&A answers to standing instructions.

use crate::error::Result;
use crate::storage::FixAttemptTracker;
use crate::types::PromotedInstruction;
use chrono::Utc;

pub struct QaPromoter;

impl QaPromoter {
    /// Scan Q&A knowledge for repeated question clusters and promote answers.
    ///
    /// Groups by repo, finds answers that repeat >= `min_occurrences` times,
    /// and promotes them to standing instructions.
    ///
    /// When an `embedding_client` is available, uses cosine similarity to cluster
    /// questions. Otherwise falls back to normalized text matching.
    pub fn scan_and_promote(
        tracker: &dyn FixAttemptTracker,
        embedding_client: Option<&crate::feedback::EmbeddingClient>,
        min_occurrences: usize,
        similarity_threshold: f64,
    ) -> Result<usize> {
        // Get all Q&A entries, grouped by repo
        // We query globally and group manually since the trait gives us scoped queries
        let all_qa = tracker.find_similar_qa_global("", None, 0.0, 10000)?;

        if all_qa.is_empty() {
            return Ok(0);
        }

        // Group by (repo, normalized answer)
        let mut groups: std::collections::HashMap<(String, String), Vec<&crate::types::QaMatch>> =
            std::collections::HashMap::new();

        for qa in &all_qa {
            let repo = qa.entry.repo.clone().unwrap_or_default();
            let answer_key = qa.entry.answer_norm.clone();
            groups.entry((repo, answer_key)).or_default().push(qa);
        }

        let mut promoted_count = 0;

        for ((repo, _answer_key), entries) in &groups {
            if entries.len() < min_occurrences || repo.is_empty() {
                continue;
            }

            // Check if questions are similar enough
            let should_promote = if embedding_client.is_some() {
                // Use embedding similarity if available
                Self::check_embedding_similarity(entries, similarity_threshold)
            } else {
                // Fallback: same normalized answer appearing multiple times
                true
            };

            if !should_promote {
                continue;
            }

            // Get the most common answer text
            let answer_text = &entries[0].entry.answer_text;

            // Create the promoted instruction
            let instruction = PromotedInstruction {
                id: 0,
                repo: repo.clone(),
                source_type: "qa_promotion".to_string(),
                instruction_text: answer_text.clone(),
                occurrence_count: entries.len() as i64,
                confidence: Self::compute_confidence(entries),
                is_active: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };

            tracker.upsert_promoted_instruction(&instruction)?;
            promoted_count += 1;
        }

        Ok(promoted_count)
    }

    /// Format promoted instructions for prompt injection.
    pub fn format_promoted_context(instructions: &[PromotedInstruction]) -> String {
        if instructions.is_empty() {
            return String::new();
        }

        let mut output = String::from("# Standing Instructions (from repeated Q&A)\n\n");

        for instruction in instructions {
            output.push_str(&format!(
                "- {} (confidence: {:.0}%, seen {} times)\n",
                instruction.instruction_text,
                instruction.confidence * 100.0,
                instruction.occurrence_count
            ));
        }

        output.push('\n');
        output
    }

    fn check_embedding_similarity(entries: &[&crate::types::QaMatch], threshold: f64) -> bool {
        // If we have embeddings, check pairwise similarity of the questions
        let embeddings: Vec<&Vec<f32>> = entries
            .iter()
            .filter_map(|e| e.entry.question_embedding.as_ref())
            .collect();

        if embeddings.len() < 2 {
            return true; // Not enough embeddings, fall back to count-based
        }

        // Check if first two are similar enough
        let sim = crate::feedback::cosine_similarity(embeddings[0], embeddings[1]);
        sim >= threshold as f32
    }

    fn compute_confidence(entries: &[&crate::types::QaMatch]) -> f64 {
        // Base confidence from occurrence count
        let count_confidence = (entries.len() as f64 / 5.0).min(1.0);

        // Boost from historical success rate
        let total_success: f64 = entries
            .iter()
            .map(|e| e.historical_success_rate)
            .sum::<f64>();
        let avg_success = if entries.is_empty() {
            0.0
        } else {
            total_success / entries.len() as f64
        };

        count_confidence * 0.6 + avg_success * 0.4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_promoted_context_empty() {
        assert!(QaPromoter::format_promoted_context(&[]).is_empty());
    }

    #[test]
    fn test_format_promoted_context() {
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
        let ctx = QaPromoter::format_promoted_context(&instructions);
        assert!(ctx.contains("Always use the async API"));
        assert!(ctx.contains("80%"));
        assert!(ctx.contains("3 times"));
    }

    #[test]
    fn test_format_promoted_context_multiple() {
        let instructions = vec![
            PromotedInstruction {
                id: 1,
                repo: "foo/bar".to_string(),
                source_type: "qa_promotion".to_string(),
                instruction_text: "First instruction".to_string(),
                occurrence_count: 5,
                confidence: 0.9,
                is_active: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            PromotedInstruction {
                id: 2,
                repo: "foo/bar".to_string(),
                source_type: "qa_promotion".to_string(),
                instruction_text: "Second instruction".to_string(),
                occurrence_count: 2,
                confidence: 0.6,
                is_active: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];
        let ctx = QaPromoter::format_promoted_context(&instructions);
        assert!(ctx.contains("First instruction"));
        assert!(ctx.contains("Second instruction"));
        assert!(ctx.contains("Standing Instructions"));
        // Check formatting
        assert!(ctx.contains("90%"));
        assert!(ctx.contains("5 times"));
        assert!(ctx.contains("60%"));
        assert!(ctx.contains("2 times"));
    }

    #[test]
    fn test_format_promoted_context_header() {
        let instructions = vec![PromotedInstruction {
            id: 1,
            repo: "r".to_string(),
            source_type: "qa".to_string(),
            instruction_text: "test".to_string(),
            occurrence_count: 1,
            confidence: 0.5,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let ctx = QaPromoter::format_promoted_context(&instructions);
        assert!(ctx.starts_with("# Standing Instructions (from repeated Q&A)"));
    }

    #[test]
    fn test_compute_confidence() {
        use crate::types::{QaKnowledgeEntry, QaMatch};

        let make_entry = |success_rate: f64| -> QaMatch {
            QaMatch {
                entry: QaKnowledgeEntry {
                    id: 0,
                    source: "linear".to_string(),
                    repo: Some("org/repo".to_string()),
                    issue_id: "iss".to_string(),
                    short_id: "I".to_string(),
                    question_text: "q".to_string(),
                    question_norm: "q".to_string(),
                    question_embedding: None,
                    answer_text: "a".to_string(),
                    answer_norm: "a".to_string(),
                    answer_embedding: None,
                    channel: "discord".to_string(),
                    responder: Some("user".to_string()),
                    correlation_id: "c".to_string(),
                    asked_at: Utc::now(),
                    answered_at: Utc::now(),
                    success_count: 0,
                    failure_count: 0,
                    last_used_at: None,
                    metadata: None,
                },
                semantic_similarity: 0.9,
                historical_success_rate: success_rate,
                final_score: 0.9,
            }
        };

        // 5 entries => count_confidence = min(5/5, 1.0) = 1.0
        // avg success = 0.8
        // total = 1.0 * 0.6 + 0.8 * 0.4 = 0.6 + 0.32 = 0.92
        let entries: Vec<QaMatch> = (0..5).map(|_| make_entry(0.8)).collect();
        let refs: Vec<&QaMatch> = entries.iter().collect();
        let confidence = QaPromoter::compute_confidence(&refs);
        assert!(
            (confidence - 0.92).abs() < 0.01,
            "expected ~0.92, got {}",
            confidence
        );

        // 2 entries => count_confidence = 2/5 = 0.4
        // avg success = 1.0
        // total = 0.4 * 0.6 + 1.0 * 0.4 = 0.24 + 0.4 = 0.64
        let entries2: Vec<QaMatch> = (0..2).map(|_| make_entry(1.0)).collect();
        let refs2: Vec<&QaMatch> = entries2.iter().collect();
        let confidence2 = QaPromoter::compute_confidence(&refs2);
        assert!(
            (confidence2 - 0.64).abs() < 0.01,
            "expected ~0.64, got {}",
            confidence2
        );
    }

    // ── Integration tests with SqliteTracker ──

    #[test]
    fn test_scan_and_promote_no_data() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();
        let count = QaPromoter::scan_and_promote(&tracker, None, 2, 0.8).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_scan_and_promote_with_qa_entries() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // Store QA entries with the same normalized answer for the same repo
        for i in 0..3 {
            let entry = crate::types::QaKnowledgeEntry {
                id: 0,
                source: "linear".to_string(),
                repo: Some("org/repo".to_string()),
                issue_id: format!("issue-{}", i),
                short_id: format!("I-{}", i),
                question_text: format!("How do I run tests? (variant {})", i),
                question_norm: "how do i run tests".to_string(),
                question_embedding: None,
                answer_text: "Run cargo test in the project root".to_string(),
                answer_norm: "run cargo test in the project root".to_string(),
                answer_embedding: None,
                channel: "discord".to_string(),
                responder: Some("user".to_string()),
                correlation_id: format!("corr-{}", i),
                asked_at: Utc::now(),
                answered_at: Utc::now(),
                success_count: 1,
                failure_count: 0,
                last_used_at: None,
                metadata: None,
            };
            tracker.store_qa_knowledge(&entry).unwrap();
        }

        // Now scan and promote — need min_occurrences=2
        QaPromoter::scan_and_promote(&tracker, None, 2, 0.8).unwrap();
    }

    #[test]
    fn test_scan_and_promote_skips_empty_repo() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // Store entries without a repo — should be skipped
        for i in 0..3 {
            let entry = crate::types::QaKnowledgeEntry {
                id: 0,
                source: "linear".to_string(),
                repo: None, // No repo
                issue_id: format!("issue-{}", i),
                short_id: format!("I-{}", i),
                question_text: "How do I run tests?".to_string(),
                question_norm: "how do i run tests".to_string(),
                question_embedding: None,
                answer_text: "Run cargo test".to_string(),
                answer_norm: "run cargo test".to_string(),
                answer_embedding: None,
                channel: "discord".to_string(),
                responder: Some("user".to_string()),
                correlation_id: format!("corr-{}", i),
                asked_at: Utc::now(),
                answered_at: Utc::now(),
                success_count: 0,
                failure_count: 0,
                last_used_at: None,
                metadata: None,
            };
            tracker.store_qa_knowledge(&entry).unwrap();
        }

        let count = QaPromoter::scan_and_promote(&tracker, None, 2, 0.8).unwrap();
        // Should not promote anything because repo is empty
        assert_eq!(count, 0);
    }

    #[test]
    fn test_scan_and_promote_below_threshold() {
        let tracker = crate::storage::SqliteTracker::in_memory().unwrap();

        // Only 1 entry — below min_occurrences=2
        let entry = crate::types::QaKnowledgeEntry {
            id: 0,
            source: "linear".to_string(),
            repo: Some("org/repo".to_string()),
            issue_id: "issue-1".to_string(),
            short_id: "I-1".to_string(),
            question_text: "How do I run tests?".to_string(),
            question_norm: "how do i run tests".to_string(),
            question_embedding: None,
            answer_text: "Run cargo test".to_string(),
            answer_norm: "run cargo test".to_string(),
            answer_embedding: None,
            channel: "discord".to_string(),
            responder: Some("user".to_string()),
            correlation_id: "corr-1".to_string(),
            asked_at: Utc::now(),
            answered_at: Utc::now(),
            success_count: 0,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        };
        tracker.store_qa_knowledge(&entry).unwrap();

        let count = QaPromoter::scan_and_promote(&tracker, None, 2, 0.8).unwrap();
        assert_eq!(count, 0);
    }
}
