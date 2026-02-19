# Plan: Replace all keyword/Jaccard classification with semantic similarity

## Summary

Three subsystems use basic keyword matching or Jaccard similarity instead of semantic embeddings. All three need to be migrated to use the existing `EmbeddingClient` infrastructure.

---

## 1. Review Classifier (`src/learning/review_classifier.rs`)

**Current:** `ReviewClassifier::classify()` uses hardcoded keyword lists to categorize review comments into `ReviewCategory` variants (Security, MissingTests, etc.) via `lower.contains(kw)`.

**Change:** Replace keyword matching with embedding-based classification. Pre-compute a reference embedding for each `ReviewCategory` from its canonical description text. At classification time, embed the comment and pick the category whose reference embedding has the highest cosine similarity (with a minimum threshold — below it, return `Other`).

### Changes:
- **`src/learning/review_classifier.rs`**:
  - Add `EmbeddingClient` as a field on `ReviewClassifier` (make it a struct with state instead of a unit struct with static methods).
  - Add an `async fn new(embedding_client: Arc<EmbeddingClient>) -> Result<Self>` that pre-computes reference embeddings for each category from canonical description strings (e.g., "This code has a security vulnerability, XSS, CSRF, injection, or authentication issue" for Security).
  - Change `classify()` from sync keyword matching to `async fn classify(&self, comment_body: &str) -> Result<ReviewCategory>` that embeds the input and returns the nearest category.
  - Add a minimum similarity threshold constant (e.g. 0.3) — below which `Other` is returned.
  - Update `process_review_comments()` and `check_promotion_threshold()` signatures to take `&self` instead of being static.
- **`src/watcher.rs` (~line 725)**: Update call sites to use the instance-based `ReviewClassifier`.
- **`src/learning/repo_knowledge.rs` (~line 578)**: Update test call sites.
- **`src/learning/mod.rs`**: No changes to exports (still exports `ReviewClassifier`).
- **Tests**: Rewrite all `review_classifier` tests. Since embedding requires a model, tests will either:
  - Use a test helper that creates a real `EmbeddingClient` with the fast/small model (`AllMiniLML6V2`).
  - Or keep a small subset of integration tests and remove the pure keyword tests.

---

## 2. Content Clustering (`src/prioritisation/content_cluster.rs`)

**Current:** `title_similarity()` uses Jaccard token similarity on whitespace-split lowercased words. `average_pairwise_similarity()` computes pairwise Jaccard over title strings to decide if issues should cluster.

**Change:** Replace Jaccard with cosine similarity on embeddings.

### Changes:
- **`src/prioritisation/content_cluster.rs`**:
  - Change `detect()` to accept pre-computed embeddings for each candidate issue (a `&HashMap<String, Vec<f32>>` mapping issue ID to embedding vector).
  - Replace `title_similarity()` with a new function that looks up embeddings and computes `cosine_similarity()` from `crate::feedback::cosine_similarity`.
  - Replace `average_pairwise_similarity()` to work on embedding vectors instead of title strings.
  - Remove the old `title_similarity()` function and `is_common_word` / Jaccard logic.
- **`src/prioritisation/mod.rs`**: Update `prioritise()` to pass embeddings map to `content_cluster::detect()`. The caller already has access to the embedding infrastructure — issue embeddings are computed upstream.
- **Tests**: Update all content_cluster tests and the prioritisation integration tests to provide mock embeddings.

---

## 3. Outcome keyword extraction & error categorization (`src/feedback/outcomes.rs`)

**Current:** `FixOutcome::extract_keywords()` splits text on whitespace, filters common words, and stores keywords. `categorize_error()` uses hardcoded keyword matching to classify error messages into categories like "timeout", "permission", etc. `FixOutcome::similarity()` returns 0.0 when embeddings are missing (no keyword fallback).

**Change:** Remove keyword extraction and keyword-based error categorization. Rely entirely on embeddings for similarity, and use embedding-based classification for error categorization.

### Changes:
- **`src/feedback/outcomes.rs`**:
  - Remove `extract_keywords()`, `is_common_word()`, and the `keywords` field from `FixOutcome`.
  - Replace `categorize_error()` with `async fn categorize_error(error: &str, embedding_client: &EmbeddingClient) -> Result<String>` that embeds the error message and compares against reference embeddings for each error category (same pattern as review classifier).
  - Update `FixOutcome::from_attempt()` — remove keyword extraction call, make error categorization use embeddings (this means `from_attempt` needs to either become async or defer error categorization).
  - Since `from_attempt` is called in sync contexts, the cleanest approach is to make `categorize_error` a separate step: store `error_type: None` initially and set it later via a new `pub fn set_error_type(&mut self, error_type: String)` method, with the async categorization happening in the caller.
  - `similarity()` already returns 0.0 when no embeddings — this stays unchanged (no keyword fallback needed).
- **`src/storage/sqlite.rs`**: The `keywords` column in `feedback_outcomes` table stays (for backwards compat with existing data) but we stop writing new values to it. Update the insert to write an empty array.
- **Tests**: Remove all keyword and `categorize_error` keyword-matching tests. Add embedding-based categorization tests.

---

## Execution order

1. **Outcomes** (3) — Remove keywords, update error categorization. Fewest downstream deps.
2. **Content Clustering** (2) — Replace Jaccard with cosine similarity on embeddings.
3. **Review Classifier** (1) — Replace keyword classification with embedding classification.

Each step should compile and pass tests before moving to the next.
