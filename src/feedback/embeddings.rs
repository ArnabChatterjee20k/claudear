//! Embedding generation using fastembed (local, no external dependencies).
//!
//! Uses the Nomic Embed Text model for generating embeddings.

use crate::error::{Error, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::sync::{Arc, Mutex};

/// Configuration for the embedding service.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Model to use for embeddings.
    pub model: EmbeddingModel,
    /// Whether to show download progress.
    pub show_download_progress: bool,
    /// Custom cache directory (uses system default if None).
    pub cache_dir: Option<String>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: EmbeddingModel::NomicEmbedTextV15,
            show_download_progress: true,
            cache_dir: None,
        }
    }
}

impl EmbeddingConfig {
    /// Create from environment variables.
    pub fn from_env() -> Self {
        let model = std::env::var("EMBEDDING_MODEL")
            .ok()
            .and_then(|m| match m.to_lowercase().as_str() {
                "nomic-embed-text" | "nomic" => Some(EmbeddingModel::NomicEmbedTextV15),
                "all-minilm" | "minilm" => Some(EmbeddingModel::AllMiniLML6V2),
                "bge-small" | "bge" => Some(EmbeddingModel::BGESmallENV15),
                _ => None,
            })
            .unwrap_or(EmbeddingModel::NomicEmbedTextV15);

        let cache_dir = std::env::var("EMBEDDING_CACHE_DIR").ok();

        Self {
            model,
            show_download_progress: true,
            cache_dir,
        }
    }

    /// Use a smaller, faster model.
    pub fn fast() -> Self {
        Self {
            model: EmbeddingModel::AllMiniLML6V2,
            show_download_progress: true,
            cache_dir: None,
        }
    }
}

/// Client for generating embeddings using fastembed.
///
/// Thread-safe wrapper around the TextEmbedding model.
pub struct EmbeddingClient {
    model: Arc<Mutex<TextEmbedding>>,
    dimension: usize,
    model_name: String,
}

impl EmbeddingClient {
    /// Create a new embedding client.
    ///
    /// This will download the model on first use if not cached.
    pub fn new(config: EmbeddingConfig) -> Result<Self> {
        let dimension = match config.model {
            EmbeddingModel::NomicEmbedTextV15 => 768,
            EmbeddingModel::NomicEmbedTextV1 => 768,
            EmbeddingModel::AllMiniLML6V2 => 384,
            EmbeddingModel::BGESmallENV15 => 384,
            EmbeddingModel::BGEBaseENV15 => 768,
            EmbeddingModel::BGELargeENV15 => 1024,
            _ => 768, // Default
        };

        let model_name = format!("{:?}", config.model);

        let mut init_options = InitOptions::new(config.model)
            .with_show_download_progress(config.show_download_progress);

        if let Some(cache_dir) = config.cache_dir {
            init_options = init_options.with_cache_dir(cache_dir.into());
        }

        let model = TextEmbedding::try_new(init_options)
            .map_err(|e| Error::Other(format!("Failed to initialize embedding model: {}", e)))?;

        tracing::info!(
            "Initialized embedding model: {} ({}d)",
            model_name,
            dimension
        );

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            dimension,
            model_name,
        })
    }

    /// Create with default configuration from environment.
    pub fn from_env() -> Result<Self> {
        Self::new(EmbeddingConfig::from_env())
    }

    /// Generate embedding for text.
    ///
    /// Uses `spawn_blocking` because ONNX inference is CPU-bound and would
    /// otherwise block a tokio worker thread.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let model = self.model.clone();
        let text = text.to_string();

        tokio::task::spawn_blocking(move || {
            let mut model = model
                .lock()
                .map_err(|e| Error::Other(format!("Embedding model lock poisoned: {}", e)))?;

            let embeddings = model
                .embed(vec![&text], None)
                .map_err(|e| Error::Other(format!("Failed to generate embedding: {}", e)))?;

            embeddings
                .into_iter()
                .next()
                .ok_or_else(|| Error::Other("No embedding returned".to_string()))
        })
        .await
        .map_err(|e| Error::Other(format!("Embedding task panicked: {}", e)))?
    }

    /// Generate embeddings for multiple texts.
    ///
    /// Uses `spawn_blocking` because ONNX inference is CPU-bound.
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let model = self.model.clone();
        let texts_owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();

        tokio::task::spawn_blocking(move || {
            let mut model = model
                .lock()
                .map_err(|e| Error::Other(format!("Embedding model lock poisoned: {}", e)))?;

            model
                .embed(texts_owned, None)
                .map_err(|e| Error::Other(format!("Failed to generate embeddings: {}", e)))
        })
        .await
        .map_err(|e| Error::Other(format!("Embedding batch task panicked: {}", e)))?
    }

    /// Check if the embedding model is available.
    pub fn is_available(&self) -> bool {
        true // Always available since it's embedded
    }

    /// Get the model name.
    pub fn model(&self) -> &str {
        &self.model_name
    }

    /// Get the embedding dimension for the configured model.
    pub fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Calculate cosine similarity between two vectors.
///
/// Uses an iterator pattern that is more amenable to LLVM auto-vectorization
/// than an indexed loop, which can yield significant speedups on 768-dim vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let (dot_product, norm_a, norm_b) = a
        .iter()
        .zip(b.iter())
        .fold((0.0f32, 0.0f32, 0.0f32), |(dot, na, nb), (&x, &y)| {
            (dot + x * y, na + x * x, nb + y * y)
        });

    let norm_a = norm_a.sqrt();
    let norm_b = norm_b.sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

/// Calculate Euclidean distance between two vectors.
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::MAX;
    }

    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        .sqrt()
}

/// Normalize a vector to unit length.
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// Embedding with metadata for similarity search results.
#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    /// The ID of the item.
    pub id: i64,
    /// The similarity score (0.0 to 1.0 for cosine).
    pub similarity: f32,
    /// The original text (optional).
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let similarity = cosine_similarity(&a, &b);
        assert!((similarity - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let similarity = cosine_similarity(&a, &b);
        assert!(similarity.abs() < 0.0001);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let similarity = cosine_similarity(&a, &b);
        assert!((similarity + 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_different_lengths() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_euclidean_distance_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let distance = euclidean_distance(&a, &b);
        assert!(distance.abs() < 0.0001);
    }

    #[test]
    fn test_euclidean_distance() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let distance = euclidean_distance(&a, &b);
        assert!((distance - 5.0).abs() < 0.0001);
    }

    #[test]
    fn test_normalize() {
        let v = vec![3.0, 4.0];
        let normalized = normalize(&v);
        let norm: f32 = normalized.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_normalize_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        let normalized = normalize(&v);
        assert_eq!(normalized, v);
    }

    #[test]
    fn test_embedding_config_default() {
        let config = EmbeddingConfig::default();
        assert!(matches!(config.model, EmbeddingModel::NomicEmbedTextV15));
    }

    #[test]
    fn test_embedding_config_fast() {
        let config = EmbeddingConfig::fast();
        assert!(matches!(config.model, EmbeddingModel::AllMiniLML6V2));
    }

    #[test]
    fn test_cosine_similarity_zero_vectors() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        // Should return 0 for zero vector
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_partial_overlap() {
        let a = vec![1.0, 1.0, 0.0];
        let b = vec![1.0, 0.0, 1.0];
        let similarity = cosine_similarity(&a, &b);
        // Should be around 0.5
        assert!(similarity > 0.3 && similarity < 0.7);
    }

    #[test]
    fn test_euclidean_distance_different_lengths() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0];
        let distance = euclidean_distance(&a, &b);
        assert_eq!(distance, f32::MAX);
    }

    #[test]
    fn test_euclidean_distance_unit_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let distance = euclidean_distance(&a, &b);
        // Should be sqrt(2) ≈ 1.414
        assert!((distance - 1.414).abs() < 0.01);
    }

    #[test]
    fn test_normalize_unit_vector() {
        let v = vec![1.0, 0.0, 0.0];
        let normalized = normalize(&v);
        // Already unit length
        assert!((normalized[0] - 1.0).abs() < 0.0001);
        assert!(normalized[1].abs() < 0.0001);
        assert!(normalized[2].abs() < 0.0001);
    }

    #[test]
    fn test_normalize_large_vector() {
        let v = vec![100.0, 0.0];
        let normalized = normalize(&v);
        let norm: f32 = normalized.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_embedding_result_fields() {
        let result = EmbeddingResult {
            id: 42,
            similarity: 0.95,
            text: Some("test text".to_string()),
        };

        assert_eq!(result.id, 42);
        assert!((result.similarity - 0.95).abs() < 0.0001);
        assert_eq!(result.text, Some("test text".to_string()));
    }

    #[test]
    fn test_embedding_result_no_text() {
        let result = EmbeddingResult {
            id: 1,
            similarity: 0.5,
            text: None,
        };

        assert!(result.text.is_none());
    }

    // ── Edge case tests ──

    #[test]
    fn test_cosine_similarity_single_element() {
        assert!((cosine_similarity(&[3.0], &[5.0]) - 1.0).abs() < 0.001);
        assert!((cosine_similarity(&[-3.0], &[5.0]) + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_nan_handling() {
        let a = vec![f32::NAN, 1.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        // NaN in input should propagate to NaN output
        assert!(sim.is_nan());
    }

    #[test]
    fn test_cosine_similarity_infinity() {
        let a = vec![f32::INFINITY, 0.0];
        let b = vec![1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        // Infinity should still produce a valid result (inf/inf = NaN, or 1.0)
        assert!(sim.is_nan() || (sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_very_small_values() {
        let a = vec![1e-30, 1e-30];
        let b = vec![1e-30, 1e-30];
        let sim = cosine_similarity(&a, &b);
        // Should be close to 1.0 for identical vectors, even with tiny values
        // May be 0.0 if underflow makes norms 0
        assert!(
            sim == 0.0 || (sim - 1.0).abs() < 0.01,
            "expected 0.0 or ~1.0, got {}",
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_very_large_values() {
        let a = vec![1e30, 1e30];
        let b = vec![1e30, 1e30];
        let sim = cosine_similarity(&a, &b);
        // May overflow, check it doesn't panic
        assert!(sim.is_finite() || sim.is_nan());
    }

    #[test]
    fn test_cosine_similarity_mixed_signs() {
        let a = vec![1.0, -1.0, 1.0, -1.0];
        let b = vec![1.0, -1.0, 1.0, -1.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_one_zero_one_nonzero() {
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        // Both zero vectors
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_euclidean_distance_empty() {
        // Same-length empty vectors should have 0 distance
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let dist = euclidean_distance(&a, &b);
        assert!((dist - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_euclidean_distance_single_element() {
        let a = vec![0.0];
        let b = vec![5.0];
        let dist = euclidean_distance(&a, &b);
        assert!((dist - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_euclidean_distance_negative_values() {
        let a = vec![-3.0, 0.0];
        let b = vec![0.0, 4.0];
        let dist = euclidean_distance(&a, &b);
        assert!((dist - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_euclidean_distance_symmetric() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert!((euclidean_distance(&a, &b) - euclidean_distance(&b, &a)).abs() < 0.0001);
    }

    #[test]
    fn test_normalize_single_element() {
        let v = vec![5.0];
        let n = normalize(&v);
        assert!((n[0] - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_normalize_negative_values() {
        let v = vec![-3.0, -4.0];
        let n = normalize(&v);
        let norm: f32 = n.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001);
        // Signs should be preserved
        assert!(n[0] < 0.0);
        assert!(n[1] < 0.0);
    }

    #[test]
    fn test_normalize_already_unit() {
        let v = vec![0.6, 0.8]; // 0.36 + 0.64 = 1.0
        let n = normalize(&v);
        assert!((n[0] - 0.6).abs() < 0.001);
        assert!((n[1] - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_normalize_empty() {
        let v: Vec<f32> = vec![];
        let n = normalize(&v);
        assert!(n.is_empty());
    }

    #[test]
    fn test_embedding_config_debug() {
        let config = EmbeddingConfig::default();
        // Should implement Debug without panicking
        let debug_str = format!("{:?}", config);
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn test_embedding_result_clone() {
        let result = EmbeddingResult {
            id: 1,
            similarity: 0.9,
            text: Some("test".to_string()),
        };
        let cloned = result.clone();
        assert_eq!(cloned.id, 1);
        assert!((cloned.similarity - 0.9).abs() < 0.001);
        assert_eq!(cloned.text, Some("test".to_string()));
    }
}
