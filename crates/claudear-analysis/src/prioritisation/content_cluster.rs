//! Content-based issue clustering.
//!
//! Groups issues that share `(error_type, culprit)` and have similar titles,
//! using Jaccard token similarity or embedding-based cosine similarity.

use crate::feedback::cosine_similarity;
use chrono::Utc;
use claudear_config::config::PrioritisationConfig;
use claudear_core::types::{ContentCluster, Issue, MatchResult};
use std::collections::HashMap;

/// Detect content clusters among a set of candidate issues.
///
/// Phase 1: bucket by `(error_type, culprit)` exact match.
/// Phase 2: verify similarity within each bucket. Prefers embedding-based
/// cosine similarity when embeddings are available for all sampled issues;
/// otherwise falls back to Jaccard token similarity on titles.
/// Discard buckets below `min_content_cluster_size` or below `cluster_similarity_threshold`.
pub fn detect(
    candidates: &[(Issue, MatchResult)],
    config: &PrioritisationConfig,
    embeddings: &HashMap<String, Vec<f32>>,
) -> Vec<ContentCluster> {
    // Phase 1: bucket by (error_type, culprit) exact match.
    let mut buckets: HashMap<(String, String), Vec<usize>> = HashMap::new();

    for (idx, (issue, _)) in candidates.iter().enumerate() {
        let error_type = issue
            .get_metadata::<String>("error_type")
            .unwrap_or_default();
        let culprit = issue.get_metadata::<String>("culprit").unwrap_or_default();

        // Skip issues with no clustering signals.
        if error_type.is_empty() && culprit.is_empty() {
            continue;
        }

        let key = (error_type, culprit);
        buckets.entry(key).or_default().push(idx);
    }

    let mut clusters = Vec::new();

    /// Maximum number of issues to sample per bucket for pairwise similarity.
    /// Caps the O(N^2) comparison to at most 50*49/2 = 1225 pairs.
    const MAX_BUCKET_SAMPLE: usize = 50;

    for ((error_type, culprit), indices) in buckets {
        if indices.len() < config.min_content_cluster_size {
            continue;
        }

        // Phase 2: compute average pairwise similarity.
        // Cap the sample to avoid O(N^2) blow-up on large buckets.
        let sampled_indices: &[usize] = if indices.len() > MAX_BUCKET_SAMPLE {
            &indices[..MAX_BUCKET_SAMPLE]
        } else {
            &indices
        };
        let titles: Vec<&str> = sampled_indices
            .iter()
            .map(|&i| candidates[i].0.title.as_str())
            .collect();

        // Try embedding-based similarity first; fall back to Jaccard on titles.
        let sampled_ids: Vec<&str> = sampled_indices
            .iter()
            .map(|&i| candidates[i].0.id.as_str())
            .collect();
        let avg_sim = average_pairwise_embedding_similarity(&sampled_ids, embeddings)
            .unwrap_or_else(|| average_pairwise_similarity(&titles));

        if avg_sim < config.cluster_similarity_threshold {
            continue;
        }

        // Only include issues that were part of the similarity sample.
        // Issues beyond the sample cap were not verified for similarity.
        let issue_ids: Vec<String> = sampled_indices
            .iter()
            .map(|&i| candidates[i].0.id.clone())
            .collect();

        let cluster_key = format!(
            "{}::{}",
            if error_type.is_empty() {
                "_"
            } else {
                &error_type
            },
            if culprit.is_empty() { "_" } else { &culprit }
        );

        let source = candidates[indices[0]].0.source.clone();

        clusters.push(ContentCluster {
            id: 0,
            cluster_key,
            source,
            representative_issue_id: issue_ids[0].clone(),
            issue_ids,
            error_type: if error_type.is_empty() {
                None
            } else {
                Some(error_type)
            },
            culprit: if culprit.is_empty() {
                None
            } else {
                Some(culprit)
            },
            avg_similarity: avg_sim,
            status: "active".into(),
            created_at: Utc::now(),
        });
    }

    clusters
}

/// Compute Jaccard token similarity between two strings.
///
/// Tokens are whitespace-separated, lowercased words.
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let set_a: std::collections::HashSet<String> =
        a.split_whitespace().map(|w| w.to_lowercase()).collect();
    let set_b: std::collections::HashSet<String> =
        b.split_whitespace().map(|w| w.to_lowercase()).collect();

    if set_a.is_empty() && set_b.is_empty() {
        return 0.0;
    }

    let intersection = set_a.intersection(&set_b).count() as f64;
    let union = set_a.union(&set_b).count() as f64;

    intersection / union
}

/// Format cluster context for prompt injection.
pub fn format_cluster_context(cluster: &ContentCluster) -> String {
    let mut ctx = format!(
        "Content cluster '{}' ({} issues)",
        cluster.cluster_key,
        cluster.issue_ids.len()
    );
    if let Some(ref et) = cluster.error_type {
        ctx.push_str(&format!(", error_type={}", et));
    }
    if let Some(ref c) = cluster.culprit {
        ctx.push_str(&format!(", culprit={}", c));
    }
    ctx.push_str(&format!(
        ", avg_similarity={:.0}%",
        cluster.avg_similarity * 100.0
    ));
    ctx
}

/// Compute average pairwise Jaccard similarity for a list of titles.
fn average_pairwise_similarity(titles: &[&str]) -> f64 {
    if titles.len() < 2 {
        return 1.0;
    }

    let mut total = 0.0;
    let mut count = 0u64;

    for i in 0..titles.len() {
        for j in (i + 1)..titles.len() {
            total += title_similarity(titles[i], titles[j]);
            count += 1;
        }
    }

    if count == 0 {
        return 0.0;
    }

    total / count as f64
}

/// Compute cosine similarity between two issues using their embedding vectors.
///
/// Returns `Some(similarity)` if both IDs have embeddings, `None` otherwise.
fn embedding_similarity(
    id_a: &str,
    id_b: &str,
    embeddings: &HashMap<String, Vec<f32>>,
) -> Option<f64> {
    let vec_a = embeddings.get(id_a)?;
    let vec_b = embeddings.get(id_b)?;
    Some(cosine_similarity(vec_a, vec_b) as f64)
}

/// Compute average pairwise embedding similarity for a list of issue IDs.
///
/// Returns `Some(avg)` if all pairs have embeddings, `None` if any pair is missing
/// (caller should fall back to Jaccard).
fn average_pairwise_embedding_similarity(
    issue_ids: &[&str],
    embeddings: &HashMap<String, Vec<f32>>,
) -> Option<f64> {
    if issue_ids.len() < 2 {
        return Some(1.0);
    }

    let mut total = 0.0;
    let mut count = 0u64;

    for i in 0..issue_ids.len() {
        for j in (i + 1)..issue_ids.len() {
            let sim = embedding_similarity(issue_ids[i], issue_ids[j], embeddings)?;
            total += sim;
            count += 1;
        }
    }

    if count == 0 {
        return Some(0.0);
    }

    Some(total / count as f64)
}

/// Get the set of issue IDs that belong to any cluster.
pub fn clustered_issue_ids(clusters: &[ContentCluster]) -> std::collections::HashSet<String> {
    clusters
        .iter()
        .flat_map(|c| c.issue_ids.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudear_core::types::{Issue, MatchPriority, MatchResult};

    fn make_candidate(
        id: &str,
        title: &str,
        error_type: &str,
        culprit: &str,
    ) -> (Issue, MatchResult) {
        let mut issue = Issue::new(id, id, title, "url", "sentry");
        if !error_type.is_empty() {
            issue.set_metadata("error_type", error_type);
        }
        if !culprit.is_empty() {
            issue.set_metadata("culprit", culprit);
        }
        let mr = MatchResult::matched("test", MatchPriority::Normal);
        (issue, mr)
    }

    #[test]
    fn title_similarity_identical() {
        assert_eq!(title_similarity("foo bar", "foo bar"), 1.0);
    }

    #[test]
    fn title_similarity_disjoint() {
        assert_eq!(title_similarity("foo bar", "baz qux"), 0.0);
    }

    #[test]
    fn title_similarity_partial() {
        // "foo bar baz" vs "foo bar qux" -> intersection={foo,bar}=2, union={foo,bar,baz,qux}=4
        let sim = title_similarity("foo bar baz", "foo bar qux");
        assert!((sim - 0.5).abs() < 0.001);
    }

    #[test]
    fn title_similarity_case_insensitive() {
        assert_eq!(title_similarity("Foo Bar", "foo bar"), 1.0);
    }

    #[test]
    fn detect_clusters_basic() {
        let candidates = vec![
            make_candidate(
                "1",
                "TypeError in payment handler",
                "TypeError",
                "payment.handler",
            ),
            make_candidate(
                "2",
                "TypeError in payment processor",
                "TypeError",
                "payment.handler",
            ),
        ];
        let config = PrioritisationConfig {
            min_content_cluster_size: 2,
            cluster_similarity_threshold: 0.3,
            ..Default::default()
        };
        let clusters = detect(&candidates, &config, &HashMap::new());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].issue_ids.len(), 2);
    }

    #[test]
    fn detect_clusters_below_threshold() {
        let candidates = vec![
            make_candidate("1", "completely different title", "TypeError", "x"),
            make_candidate("2", "another unrelated heading", "TypeError", "x"),
        ];
        let config = PrioritisationConfig {
            min_content_cluster_size: 2,
            cluster_similarity_threshold: 0.90,
            ..Default::default()
        };
        let clusters = detect(&candidates, &config, &HashMap::new());
        assert!(clusters.is_empty());
    }

    #[test]
    fn detect_clusters_below_min_size() {
        let candidates = vec![make_candidate(
            "1",
            "TypeError in handler",
            "TypeError",
            "x",
        )];
        let config = PrioritisationConfig::default();
        let clusters = detect(&candidates, &config, &HashMap::new());
        assert!(clusters.is_empty());
    }

    #[test]
    fn sampling_cap_limits_cluster_membership() {
        // Create more candidates than MAX_BUCKET_SAMPLE (50) in a single bucket.
        // After the fix (C4), the cluster should only contain the sampled subset.
        let count = 60;
        let candidates: Vec<(Issue, MatchResult)> = (0..count)
            .map(|i| {
                make_candidate(
                    &format!("id-{i}"),
                    "TypeError in payment handler",
                    "TypeError",
                    "payment.handler",
                )
            })
            .collect();
        let config = PrioritisationConfig {
            min_content_cluster_size: 2,
            cluster_similarity_threshold: 0.3,
            ..Default::default()
        };
        let clusters = detect(&candidates, &config, &HashMap::new());
        assert_eq!(clusters.len(), 1);
        // With 60 issues but a sample cap of 50, only 50 should be in the cluster
        assert_eq!(
            clusters[0].issue_ids.len(),
            50,
            "Cluster membership should be limited to the sampling cap (50), got {}",
            clusters[0].issue_ids.len()
        );
    }

    #[test]
    fn detect_clusters_with_embeddings() {
        // Two issues with identical (error_type, culprit) and high cosine similarity
        // but completely different titles (would fail Jaccard).
        let candidates = vec![
            make_candidate("e1", "completely different title", "TypeError", "handler"),
            make_candidate("e2", "another unrelated heading", "TypeError", "handler"),
        ];

        // Create embedding vectors with high cosine similarity.
        // [1, 0, 0] dot [0.99, 0.14, 0] / (1 * sqrt(0.99^2 + 0.14^2)) ~= 0.99
        let mut embeddings: HashMap<String, Vec<f32>> = HashMap::new();
        embeddings.insert("e1".into(), vec![1.0, 0.0, 0.0]);
        embeddings.insert("e2".into(), vec![0.99, 0.14, 0.0]);

        let config = PrioritisationConfig {
            min_content_cluster_size: 2,
            cluster_similarity_threshold: 0.90,
            ..Default::default()
        };

        // With empty embeddings, Jaccard on these titles would be far below 0.90
        // so no cluster would be formed.
        let clusters_jaccard = detect(&candidates, &config, &HashMap::new());
        assert!(
            clusters_jaccard.is_empty(),
            "Jaccard fallback should NOT cluster these dissimilar titles"
        );

        // With embeddings, cosine similarity is ~0.99 which exceeds the 0.90 threshold.
        let clusters_emb = detect(&candidates, &config, &embeddings);
        assert_eq!(
            clusters_emb.len(),
            1,
            "Embedding path should form a cluster"
        );
        assert_eq!(clusters_emb[0].issue_ids.len(), 2);
        assert!(clusters_emb[0].avg_similarity > 0.90);
    }

    #[test]
    fn embedding_similarity_partial_fallback() {
        // When only one issue has an embedding, the system should fall back to Jaccard.
        let candidates = vec![
            make_candidate(
                "f1",
                "TypeError in payment handler",
                "TypeError",
                "payment.handler",
            ),
            make_candidate(
                "f2",
                "TypeError in payment processor",
                "TypeError",
                "payment.handler",
            ),
        ];

        let mut embeddings: HashMap<String, Vec<f32>> = HashMap::new();
        // Only f1 has an embedding, f2 does not.
        embeddings.insert("f1".into(), vec![1.0, 0.0, 0.0]);

        let config = PrioritisationConfig {
            min_content_cluster_size: 2,
            cluster_similarity_threshold: 0.3,
            ..Default::default()
        };

        // Should still form a cluster via Jaccard fallback since titles are similar.
        let clusters = detect(&candidates, &config, &embeddings);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].issue_ids.len(), 2);
    }

    #[test]
    fn embedding_similarity_function_basic() {
        let mut embeddings: HashMap<String, Vec<f32>> = HashMap::new();
        embeddings.insert("a".into(), vec![1.0, 0.0, 0.0]);
        embeddings.insert("b".into(), vec![1.0, 0.0, 0.0]);
        embeddings.insert("c".into(), vec![0.0, 1.0, 0.0]);

        // Identical vectors -> similarity 1.0
        let sim = embedding_similarity("a", "b", &embeddings);
        assert!((sim.unwrap() - 1.0).abs() < 0.001);

        // Orthogonal vectors -> similarity 0.0
        let sim = embedding_similarity("a", "c", &embeddings);
        assert!(sim.unwrap().abs() < 0.001);

        // Missing ID -> None
        let sim = embedding_similarity("a", "missing", &embeddings);
        assert!(sim.is_none());
    }

    #[test]
    fn format_cluster_context_output() {
        let cluster = ContentCluster {
            id: 1,
            cluster_key: "TypeError::payment.handler".into(),
            source: "sentry".into(),
            representative_issue_id: "1".into(),
            issue_ids: vec!["1".into(), "2".into()],
            error_type: Some("TypeError".into()),
            culprit: Some("payment.handler".into()),
            avg_similarity: 0.75,
            status: "active".into(),
            created_at: Utc::now(),
        };
        let ctx = format_cluster_context(&cluster);
        assert!(ctx.contains("TypeError::payment.handler"));
        assert!(ctx.contains("2 issues"));
        assert!(ctx.contains("75%"));
    }
}
