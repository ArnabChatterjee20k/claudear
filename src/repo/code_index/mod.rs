//! Tree-sitter powered code indexing pipeline.
//!
//! Parses source files into ASTs, extracts symbols (functions, classes, structs, traits),
//! creates AST-aware chunks at semantic boundaries, embeds those chunks for vector similarity
//! search, and persists everything in SQLite. Incremental re-indexing via file hashing.

pub mod analyzer;
mod chunker;
pub mod complexity;
mod languages;
mod parser;
pub mod types;

pub use types::{CodeChunk, CodeIndexStats, CodeSearchResult, CodeSymbol, Language, SymbolKind};

use crate::error::Result;
use crate::feedback::EmbeddingClient;
use crate::storage::FixAttemptTracker;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use walkdir::WalkDir;

/// Maximum file size to index (default: 1 MB).
const DEFAULT_MAX_FILE_SIZE: u64 = 1024 * 1024;
/// Default embedding batch size.
const DEFAULT_BATCH_SIZE: usize = 32;

/// Write-side: indexes a repository's source code.
pub struct CodeIndexer {
    tracker: Arc<dyn FixAttemptTracker>,
    embedding_client: Arc<EmbeddingClient>,
    max_file_size: u64,
    batch_size: usize,
}

impl CodeIndexer {
    pub fn new(
        tracker: Arc<dyn FixAttemptTracker>,
        embedding_client: Arc<EmbeddingClient>,
    ) -> Self {
        Self {
            tracker,
            embedding_client,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    pub fn with_config(
        tracker: Arc<dyn FixAttemptTracker>,
        embedding_client: Arc<EmbeddingClient>,
        max_file_size_kb: u64,
        batch_size: usize,
    ) -> Self {
        Self {
            tracker,
            embedding_client,
            max_file_size: max_file_size_kb * 1024,
            batch_size: if batch_size == 0 {
                DEFAULT_BATCH_SIZE
            } else {
                batch_size
            },
        }
    }

    /// Index (or incrementally re-index) a repository.
    pub async fn index_repo(&self, repo_name: &str, repo_path: &Path) -> Result<CodeIndexStats> {
        let repo_id = self.tracker.get_or_create_repo_id(repo_name)?;
        let mut stats = CodeIndexStats::default();

        if let Ok(Some(stored_model)) = self.tracker.get_code_embedding_model(repo_id) {
            let current_model = self.embedding_client.model();
            if stored_model != current_model {
                tracing::warn!(
                    repo = %repo_name,
                    old_model = %stored_model,
                    new_model = %current_model,
                    "Embedding model changed — deleting all code data to force full re-index"
                );
                self.tracker.delete_all_code_data_for_repo(repo_id)?;
            }
        }

        // Collect all source files.
        let files = self.collect_source_files(repo_path);
        tracing::info!(
            repo = %repo_name,
            source_files = files.len(),
            "Starting code indexing"
        );

        let parallelism = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let repo_path_owned = repo_path.to_path_buf();
        let mut seen_paths: Vec<String> = Vec::with_capacity(files.len());
        let mut pending_chunks: Vec<types::CodeChunk> = Vec::new();
        let mut pending_symbols: Vec<types::CodeSymbol> = Vec::new();

        for file_group in files.chunks(parallelism) {
            let tracker = Arc::clone(&self.tracker);
            let rpo = &repo_path_owned;
            let file_results: Vec<FileProcessResult> = file_group
                .par_iter()
                .map(|(path, language)| {
                    let rel_path = path
                        .strip_prefix(rpo)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .to_string();

                    // Read file content.
                    let content = match std::fs::read_to_string(path) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::debug!(file = %rel_path, error = %e, "Failed to read file");
                            return FileProcessResult::Failed(());
                        }
                    };

                    // Compute hash for incremental detection.
                    let file_hash = sha256_hex(&content);

                    // Check if file is already indexed with this hash.
                    match tracker.code_chunk_hash_matches(repo_id, &rel_path, &file_hash) {
                        Ok(true) => return FileProcessResult::Skipped(rel_path),
                        Err(e) => {
                            tracing::debug!(file = %rel_path, error = %e, "Hash check failed");
                            return FileProcessResult::Failed(());
                        }
                        Ok(false) => {}
                    }

                    // Delete old data for this file before re-indexing.
                    if let Err(e) = tracker.delete_code_data_for_file(repo_id, &rel_path) {
                        tracing::debug!(file = %rel_path, error = %e, "Failed to delete old data");
                        return FileProcessResult::Failed(());
                    }

                    // Parse and chunk.
                    match chunker::chunk_file(&content, *language, repo_id, &rel_path, &file_hash) {
                        Ok((symbols, chunks)) => FileProcessResult::Parsed {
                            rel_path,
                            symbols,
                            chunks,
                        },
                        Err(e) => {
                            tracing::debug!(file = %rel_path, error = %e, "Failed to parse file");
                            FileProcessResult::Failed(())
                        }
                    }
                })
                .collect();

            for result in file_results {
                match result {
                    FileProcessResult::Parsed {
                        rel_path,
                        symbols,
                        chunks,
                    } => {
                        stats.files_processed += 1;
                        stats.symbols_extracted += symbols.len();
                        stats.chunks_created += chunks.len();
                        pending_symbols.extend(symbols);
                        pending_chunks.extend(chunks);
                        seen_paths.push(rel_path);
                    }
                    FileProcessResult::Skipped(rel_path) => {
                        stats.files_skipped += 1;
                        seen_paths.push(rel_path);
                    }
                    FileProcessResult::Failed(_) => {
                        stats.files_failed += 1;
                    }
                }

                // Flush batch if large enough.
                if pending_chunks.len() >= self.batch_size {
                    self.flush_batch(&mut pending_symbols, &mut pending_chunks, &mut stats)
                        .await?;
                }
            }
        }

        // Flush remaining.
        if !pending_chunks.is_empty() || !pending_symbols.is_empty() {
            self.flush_batch(&mut pending_symbols, &mut pending_chunks, &mut stats)
                .await?;
        }

        // Clean up entries for deleted files.
        self.tracker.cleanup_stale_code_data(repo_id, &seen_paths)?;

        tracing::info!(repo = %repo_name, %stats, "Code indexing complete");
        Ok(stats)
    }

    /// Flush a batch of symbols and chunks: store symbols, store chunks, embed, store embeddings.
    ///
    /// Content-hash deduplication (Issue #11): chunks with identical `chunk_text`
    /// share a single embedding computation. Each unique text is embedded once
    /// and the resulting vector is reused for all chunks with that content hash.
    async fn flush_batch(
        &self,
        symbols: &mut Vec<types::CodeSymbol>,
        chunks: &mut Vec<types::CodeChunk>,
        stats: &mut CodeIndexStats,
    ) -> Result<()> {
        // Store symbols.
        if !symbols.is_empty() {
            self.tracker.save_code_symbols(symbols)?;
            symbols.clear();
        }

        if chunks.is_empty() {
            return Ok(());
        }

        // Compute content hashes for deduplication.
        for chunk in chunks.iter_mut() {
            chunk.content_hash = Some(sha256_hex(&chunk.chunk_text));
        }

        // Store chunks and get their IDs.
        let chunk_ids = self.tracker.save_code_chunks(chunks)?;

        // --- Content-hash deduplication (Issue #11) ---
        // Group chunks by content_hash: embed each unique text only once.
        // Build full context (prefix + code) for embedding (Issue #6).
        let mut unique_texts: Vec<String> = Vec::new();
        let mut hash_to_embed_idx: HashMap<String, usize> = HashMap::new();
        let mut chunk_to_embed_idx: Vec<usize> = Vec::with_capacity(chunks.len());

        for chunk in chunks.iter() {
            let content_hash = chunk.content_hash.as_deref().unwrap_or("");
            if let Some(&idx) = hash_to_embed_idx.get(content_hash) {
                chunk_to_embed_idx.push(idx);
            } else {
                let idx = unique_texts.len();
                // Reconstruct full context text for embedding:
                // stored prefix (context_text) + "\n" + chunk_text
                let full_context = chunker::build_context_text(
                    &chunk.file_path,
                    chunk.language,
                    &chunk.chunk_type,
                    chunk.symbol_name.as_deref(),
                    None,
                    &chunk.chunk_text,
                );
                unique_texts.push(full_context);
                hash_to_embed_idx.insert(content_hash.to_string(), idx);
                chunk_to_embed_idx.push(idx);
            }
        }

        // Embed all unique texts in a single batch (Issue #3: no double-batching).
        let unique_refs: Vec<&str> = unique_texts.iter().map(|s| s.as_str()).collect();
        match self.embedding_client.embed_batch(&unique_refs).await {
            Ok(unique_embeddings) => {
                // Map each chunk to its embedding via the dedup index.
                let pairs: Vec<(i64, &[f32])> = chunk_ids
                    .iter()
                    .zip(chunk_to_embed_idx.iter())
                    .filter_map(|(&id, &embed_idx)| {
                        unique_embeddings
                            .get(embed_idx)
                            .map(|emb| (id, emb.as_slice()))
                    })
                    .collect();

                let model_name = self.embedding_client.model();
                self.tracker
                    .save_code_chunk_embeddings(&pairs, model_name)?;
                stats.embeddings_generated += pairs.len();
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to generate code chunk embeddings");
                // Delete the chunks that failed to embed so they get
                // re-processed on the next indexing run instead of
                // remaining as ghost chunks with no embeddings.
                if let Err(del_err) = self.tracker.delete_code_chunks_by_ids(&chunk_ids) {
                    tracing::warn!(error = %del_err, "Failed to delete unembed code chunks");
                }
            }
        }

        chunks.clear();
        Ok(())
    }

    /// Walk the repo and collect source files with their detected language.
    fn collect_source_files(&self, repo_path: &Path) -> Vec<(std::path::PathBuf, Language)> {
        let mut files = Vec::new();

        for entry in WalkDir::new(repo_path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Always include the root directory (depth 0) so that
                // repositories whose directory name starts with '.' or
                // matches a skip-name are still walked.
                if e.depth() == 0 {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                !name.starts_with('.')
                    && name != "node_modules"
                    && name != "vendor"
                    && name != "target"
                    && name != "build"
                    && name != "dist"
                    && name != "__pycache__"
            })
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            // Check file size.
            if let Ok(meta) = entry.metadata() {
                if meta.len() > self.max_file_size {
                    continue;
                }
            }

            // Check language.
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if let Some(lang) = Language::from_extension(ext) {
                    files.push((path.to_path_buf(), lang));
                }
            }
        }

        files
    }
}

/// Result from processing a single file in the parallel phase.
enum FileProcessResult {
    Parsed {
        rel_path: String,
        symbols: Vec<types::CodeSymbol>,
        chunks: Vec<types::CodeChunk>,
    },
    Skipped(String),
    Failed(()),
}

/// Read-side: search indexed code.
pub struct CodeSearchService {
    tracker: Arc<dyn FixAttemptTracker>,
    embedding_client: Arc<EmbeddingClient>,
}

impl CodeSearchService {
    pub fn new(
        tracker: Arc<dyn FixAttemptTracker>,
        embedding_client: Arc<EmbeddingClient>,
    ) -> Self {
        Self {
            tracker,
            embedding_client,
        }
    }

    /// Semantic vector search for code chunks matching a query.
    pub async fn search(
        &self,
        query: &str,
        repo_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<CodeSearchResult>> {
        let embedding = self.embedding_client.embed(query).await?;
        self.tracker.search_code_chunks(&embedding, repo_id, limit)
    }

    /// Find symbols by name (exact substring match).
    pub fn find_symbol(
        &self,
        name: &str,
        kind: Option<SymbolKind>,
        repo_id: Option<i64>,
    ) -> Result<Vec<CodeSymbol>> {
        self.tracker.find_code_symbols(name, kind, repo_id)
    }

    /// Find code chunks similar to a given code snippet.
    pub async fn find_similar_to_code(
        &self,
        snippet: &str,
        repo_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<CodeSearchResult>> {
        let embedding = self.embedding_client.embed(snippet).await?;
        self.tracker.search_code_chunks(&embedding, repo_id, limit)
    }
}

/// Build a search query for code search, tailored to the issue source.
///
/// For Sentry issues, includes error type, culprit, filename, and function metadata
/// which are highly relevant for matching stack traces to code. For other sources,
/// uses title + description.
pub fn build_code_search_query(issue: &crate::types::Issue) -> String {
    let mut parts = vec![issue.title.clone()];

    match issue.source.as_str() {
        "sentry" => {
            // Sentry issues carry rich error metadata that maps well to code
            if let Some(ref desc) = issue.description {
                parts.push(desc.clone());
            }
            if let Some(v) = issue.get_metadata::<String>("error_type") {
                parts.push(v);
            }
            if let Some(v) = issue.get_metadata::<String>("error_value") {
                parts.push(v);
            }
            if let Some(v) = issue.get_metadata::<String>("culprit") {
                if !v.is_empty() {
                    parts.push(v);
                }
            }
            if let Some(v) = issue.get_metadata::<String>("filename") {
                parts.push(v);
            }
            if let Some(v) = issue.get_metadata::<String>("function") {
                parts.push(v);
            }
        }
        _ => {
            if let Some(ref desc) = issue.description {
                parts.push(desc.clone());
            }
        }
    }

    parts.join(" ")
}

/// Format code search results as a markdown context section for inclusion in prompts.
pub fn format_code_search_context(results: &[CodeSearchResult]) -> String {
    use std::fmt::Write;

    if results.is_empty() {
        return String::new();
    }

    let mut context = String::from("\n\n## Relevant Code from Repository\n\n");
    context.push_str(
        "The following code snippets were found to be semantically relevant to this issue. ",
    );
    context.push_str("Use them to understand the codebase and inform your approach:\n\n");

    for (i, result) in results.iter().enumerate() {
        let chunk = &result.chunk;
        let _ = writeln!(
            context,
            "### {}. `{}` (Similarity: {:.0}%)",
            i + 1,
            chunk.file_path,
            result.score * 100.0,
        );

        if let Some(ref symbol) = chunk.symbol_name {
            let _ = writeln!(context, "**Symbol:** {} ({})", symbol, chunk.chunk_type);
        }

        let _ = writeln!(
            context,
            "**Lines:** {}-{} | **Language:** {}",
            chunk.start_line,
            chunk.end_line,
            chunk.language.as_str(),
        );

        // Truncate very long snippets to keep context manageable
        let code = if chunk.chunk_text.len() > 2000 {
            format!("{}...\n(truncated)", &chunk.chunk_text[..2000])
        } else {
            chunk.chunk_text.clone()
        };

        let _ = writeln!(
            context,
            "```{}\n{}\n```\n",
            chunk.language.as_str().to_lowercase(),
            code
        );
    }

    context
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Try to create an EmbeddingClient; returns `None` when the ONNX model
    /// cannot be downloaded (common in CI).  Tests that need an
    /// `Arc<EmbeddingClient>` should early-return when this returns `None`.
    fn try_embedding_client() -> Option<Arc<crate::feedback::EmbeddingClient>> {
        crate::feedback::EmbeddingClient::new(crate::feedback::EmbeddingConfig {
            pool_size: 1, // single instance is sufficient for tests
            ..Default::default()
        })
        .ok()
        .map(Arc::new)
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex("hello world");
        assert_eq!(hash.len(), 64); // SHA256 hex is 64 chars
                                    // Deterministic
        assert_eq!(hash, sha256_hex("hello world"));
        // Different input → different hash
        assert_ne!(hash, sha256_hex("hello world!"));
    }

    // ================================================================
    // sha256_hex: comprehensive edge cases and known-value tests
    // ================================================================

    #[test]
    fn test_sha256_hex_empty_string() {
        let hash = sha256_hex("");
        assert_eq!(hash.len(), 64);
        // SHA256 of empty string is a well-known constant
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_sha256_hex_known_value() {
        // SHA256("abc") is a well-known test vector
        let hash = sha256_hex("abc");
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_sha256_hex_deterministic() {
        let input = "fn main() { println!(\"Hello, world!\"); }";
        let hash1 = sha256_hex(input);
        let hash2 = sha256_hex(input);
        let hash3 = sha256_hex(input);
        assert_eq!(hash1, hash2);
        assert_eq!(hash2, hash3);
    }

    #[test]
    fn test_sha256_hex_different_inputs_different_hashes() {
        let inputs = ["a", "b", "c", "ab", "abc", "A", "B"];
        let hashes: Vec<String> = inputs.iter().map(|i| sha256_hex(i)).collect();
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(
                    hashes[i], hashes[j],
                    "Inputs '{}' and '{}' should produce different hashes",
                    inputs[i], inputs[j]
                );
            }
        }
    }

    #[test]
    fn test_sha256_hex_whitespace_sensitivity() {
        let hash1 = sha256_hex("hello world");
        let hash2 = sha256_hex("hello  world");
        let hash3 = sha256_hex("hello\tworld");
        let hash4 = sha256_hex("hello\nworld");

        assert_ne!(hash1, hash2, "Double space should differ from single");
        assert_ne!(hash1, hash3, "Tab should differ from space");
        assert_ne!(hash1, hash4, "Newline should differ from space");
    }

    #[test]
    fn test_sha256_hex_case_sensitivity() {
        let hash_lower = sha256_hex("hello");
        let hash_upper = sha256_hex("HELLO");
        assert_ne!(hash_lower, hash_upper, "SHA256 is case-sensitive");
    }

    #[test]
    fn test_sha256_hex_unicode() {
        let hash = sha256_hex("\u{1F600}\u{1F601}\u{1F602}");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_sha256_hex_long_content() {
        let content = "x".repeat(1_000_000);
        let hash = sha256_hex(&content);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_sha256_hex_only_lowercase_hex() {
        // SHA256 hex should use lowercase hex characters
        let hash = sha256_hex("test content");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "Hash should be lowercase hex only, got: {}",
            hash
        );
    }

    #[test]
    fn test_sha256_hex_newline_at_end_differs() {
        let hash1 = sha256_hex("content");
        let hash2 = sha256_hex("content\n");
        assert_ne!(hash1, hash2, "Trailing newline changes the hash");
    }

    #[test]
    fn test_sha256_hex_null_byte_in_content() {
        let hash1 = sha256_hex("hello\0world");
        let hash2 = sha256_hex("helloworld");
        assert_ne!(hash1, hash2, "Null byte should be included in hash");
    }

    // ================================================================
    // Constants
    // ================================================================

    #[test]
    fn test_default_max_file_size_is_1mb() {
        assert_eq!(DEFAULT_MAX_FILE_SIZE, 1024 * 1024);
        assert_eq!(DEFAULT_MAX_FILE_SIZE, 1_048_576);
    }

    #[test]
    fn test_default_batch_size_is_32() {
        assert_eq!(DEFAULT_BATCH_SIZE, 32);
    }

    // ================================================================
    // collect_source_files: tests using temp directories
    // ================================================================

    #[test]
    fn test_collect_source_files_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        // Need a CodeIndexer to call collect_source_files.
        // We create a minimal one with in-memory tracker and a
        // mock embedding client. Since collect_source_files is sync
        // and doesn't use embedding_client, we just need the struct.
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert!(files.is_empty(), "Empty dir should yield no source files");
    }

    #[test]
    fn test_collect_source_files_finds_rust_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Readme").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 2, "Should find 2 .rs files");
        assert!(files.iter().all(|(_, lang)| *lang == Language::Rust));
    }

    #[test]
    fn test_collect_source_files_skips_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join(".hidden");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("secret.rs"), "fn secret() {}").unwrap();
        std::fs::write(dir.path().join("visible.rs"), "fn visible() {}").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        let names: Vec<_> = files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"visible.rs".to_string()));
        assert!(!names.contains(&"secret.rs".to_string()));
    }

    #[test]
    fn test_collect_source_files_skips_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("index.js"), "module.exports = {}").unwrap();
        std::fs::write(dir.path().join("app.js"), "console.log('hi')").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].0.file_name().unwrap().to_string_lossy() == "app.js");
    }

    #[test]
    fn test_collect_source_files_skips_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("compiled.rs"), "fn compiled() {}").unwrap();
        std::fs::write(dir.path().join("src.rs"), "fn src() {}").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].0.file_name().unwrap().to_string_lossy() == "src.rs");
    }

    #[test]
    fn test_collect_source_files_skips_vendor_and_build_and_dist() {
        let dir = tempfile::tempdir().unwrap();
        for dirname in &["vendor", "build", "dist"] {
            let d = dir.path().join(dirname);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join("lib.rs"), "fn lib() {}").unwrap();
        }
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(
            files.len(),
            1,
            "Should only find main.rs, not files in vendor/build/dist"
        );
    }

    #[test]
    fn test_collect_source_files_skips_pycache() {
        let dir = tempfile::tempdir().unwrap();
        let pycache = dir.path().join("__pycache__");
        std::fs::create_dir(&pycache).unwrap();
        std::fs::write(pycache.join("module.py"), "print('cached')").unwrap();
        std::fs::write(dir.path().join("app.py"), "print('app')").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, Language::Python);
    }

    #[test]
    fn test_collect_source_files_multiple_languages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("app.ts"), "const x = 1;").unwrap();
        std::fs::write(dir.path().join("lib.py"), "def f(): pass").unwrap();
        std::fs::write(dir.path().join("main.go"), "package main").unwrap();
        std::fs::write(dir.path().join("App.java"), "class App {}").unwrap();
        std::fs::write(dir.path().join("unknown.xyz"), "???").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(
            files.len(),
            5,
            "Should find 5 supported language files (not .xyz)"
        );

        let languages: std::collections::HashSet<Language> =
            files.iter().map(|(_, l)| *l).collect();
        assert!(languages.contains(&Language::Rust));
        assert!(languages.contains(&Language::TypeScript));
        assert!(languages.contains(&Language::Python));
        assert!(languages.contains(&Language::Go));
        assert!(languages.contains(&Language::Java));
    }

    #[test]
    fn test_collect_source_files_skips_large_files() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file larger than DEFAULT_MAX_FILE_SIZE (1MB)
        let large_content = "x".repeat(DEFAULT_MAX_FILE_SIZE as usize + 1);
        std::fs::write(dir.path().join("large.rs"), &large_content).unwrap();
        std::fs::write(dir.path().join("small.rs"), "fn small() {}").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 1, "Should skip the large file");
        assert!(files[0].0.file_name().unwrap().to_string_lossy() == "small.rs");
    }

    #[test]
    fn test_collect_source_files_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src").join("models");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("user.rs"), "struct User {}").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 2, "Should find files in nested directories");
    }

    // ================================================================
    // CodeIndexer: constructor variants
    // ================================================================

    #[test]
    fn test_code_indexer_new_defaults() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        assert_eq!(indexer.max_file_size, DEFAULT_MAX_FILE_SIZE);
        assert_eq!(indexer.batch_size, DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn test_code_indexer_with_config_custom_values() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::with_config(tracker, embedding_client, 512, 64);

        // max_file_size_kb is multiplied by 1024
        assert_eq!(indexer.max_file_size, 512 * 1024);
        assert_eq!(indexer.batch_size, 64);
    }

    #[test]
    fn test_code_indexer_with_config_zero_batch_size_uses_default() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::with_config(tracker, embedding_client, 256, 0);

        assert_eq!(
            indexer.batch_size, DEFAULT_BATCH_SIZE,
            "batch_size=0 should fall back to DEFAULT_BATCH_SIZE"
        );
    }

    #[test]
    fn test_code_indexer_with_config_small_file_size() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::with_config(tracker, embedding_client, 1, 1);

        assert_eq!(indexer.max_file_size, 1024); // 1 KB
        assert_eq!(indexer.batch_size, 1);
    }

    // ================================================================
    // CodeSearchService: constructor
    // ================================================================

    #[test]
    fn test_code_search_service_new() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let _service = CodeSearchService::new(tracker, embedding_client);
        // Constructor should not panic
    }

    // ================================================================
    // collect_source_files with custom max_file_size
    // ================================================================

    #[test]
    fn test_collect_source_files_custom_max_size() {
        let dir = tempfile::tempdir().unwrap();
        // Write a 2KB file
        let content_2kb = "x".repeat(2048);
        std::fs::write(dir.path().join("big.rs"), &content_2kb).unwrap();
        // Write a 500-byte file
        let content_small = "y".repeat(500);
        std::fs::write(dir.path().join("small.rs"), &content_small).unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        // Set max_file_size to 1KB
        let indexer = CodeIndexer::with_config(tracker, embedding_client, 1, 32);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(files.len(), 1, "Only files under 1KB should be collected");
        assert!(files[0].0.file_name().unwrap().to_string_lossy() == "small.rs");
    }

    // ================================================================
    // collect_source_files: file extension coverage
    // ================================================================

    #[test]
    fn test_collect_source_files_all_supported_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let extensions_and_languages = vec![
            ("test.rs", Language::Rust),
            ("test.ts", Language::TypeScript),
            ("test.tsx", Language::Tsx),
            ("test.js", Language::JavaScript),
            ("test.jsx", Language::JavaScript),
            ("test.mjs", Language::JavaScript),
            ("test.cjs", Language::JavaScript),
            ("test.py", Language::Python),
            ("test.pyi", Language::Python),
            ("test.go", Language::Go),
            ("test.java", Language::Java),
            ("test.c", Language::C),
            ("test.h", Language::C),
            ("test.cpp", Language::Cpp),
            ("test.cc", Language::Cpp),
            ("test.rb", Language::Ruby),
            ("test.php", Language::Php),
            ("test.swift", Language::Swift),
            ("test.kt", Language::Kotlin),
            ("test.kts", Language::Kotlin),
        ];

        for (filename, _) in &extensions_and_languages {
            std::fs::write(dir.path().join(filename), "// content").unwrap();
        }
        // Also write unsupported extensions
        std::fs::write(dir.path().join("test.txt"), "text").unwrap();
        std::fs::write(dir.path().join("test.md"), "# markdown").unwrap();
        std::fs::write(dir.path().join("test.json"), "{}").unwrap();

        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let Some(embedding_client) = try_embedding_client() else {
            return;
        };
        let indexer = CodeIndexer::new(tracker, embedding_client);

        let files = indexer.collect_source_files(dir.path());
        assert_eq!(
            files.len(),
            extensions_and_languages.len(),
            "Should find all supported extensions and skip unsupported ones"
        );

        // Verify each expected language is present
        for (filename, expected_lang) in &extensions_and_languages {
            let found = files.iter().any(|(path, lang)| {
                path.file_name().unwrap().to_string_lossy() == *filename && *lang == *expected_lang
            });
            assert!(
                found,
                "Expected to find {} with language {:?}",
                filename, expected_lang
            );
        }
    }

    // ================================================================
    // format_code_search_context tests
    // ================================================================

    fn make_chunk(
        file_path: &str,
        symbol_name: Option<&str>,
        chunk_type: &str,
        language: Language,
        start_line: usize,
        end_line: usize,
        chunk_text: &str,
    ) -> CodeChunk {
        CodeChunk {
            id: None,
            repo_id: 1,
            file_path: file_path.to_string(),
            chunk_type: chunk_type.to_string(),
            symbol_name: symbol_name.map(|s| s.to_string()),
            language,
            start_line,
            end_line,
            chunk_text: chunk_text.to_string(),
            context_text: String::new(),
            file_hash: String::new(),
            content_hash: None,
        }
    }

    fn make_search_result(chunk: CodeChunk, score: f64) -> CodeSearchResult {
        CodeSearchResult { chunk, score }
    }

    #[test]
    fn test_format_code_search_context_empty() {
        let result = super::format_code_search_context(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_code_search_context_single_result() {
        let chunk = make_chunk(
            "src/main.rs",
            Some("main"),
            "function",
            Language::Rust,
            1,
            10,
            "fn main() {\n    println!(\"hello\");\n}",
        );
        let results = vec![make_search_result(chunk, 0.95)];

        let ctx = super::format_code_search_context(&results);

        assert!(ctx.contains("## Relevant Code from Repository"));
        assert!(ctx.contains("### 1. `src/main.rs` (Similarity: 95%)"));
        assert!(ctx.contains("**Symbol:** main (function)"));
        assert!(ctx.contains("**Lines:** 1-10 | **Language:** Rust"));
        assert!(ctx.contains("```rust"));
        assert!(ctx.contains("fn main()"));
    }

    #[test]
    fn test_format_code_search_context_multiple_results() {
        let chunk1 = make_chunk(
            "src/lib.rs",
            Some("process"),
            "function",
            Language::Rust,
            20,
            40,
            "fn process() {}",
        );
        let chunk2 = make_chunk(
            "src/app.ts",
            Some("App"),
            "class",
            Language::TypeScript,
            1,
            50,
            "class App {}",
        );
        let results = vec![
            make_search_result(chunk1, 0.88),
            make_search_result(chunk2, 0.72),
        ];

        let ctx = super::format_code_search_context(&results);

        assert!(ctx.contains("### 1. `src/lib.rs` (Similarity: 88%)"));
        assert!(ctx.contains("### 2. `src/app.ts` (Similarity: 72%)"));
        assert!(ctx.contains("**Symbol:** process (function)"));
        assert!(ctx.contains("**Symbol:** App (class)"));
        assert!(ctx.contains("```rust"));
        assert!(ctx.contains("```typescript"));
    }

    #[test]
    fn test_format_code_search_context_no_symbol() {
        let chunk = make_chunk(
            "src/utils.py",
            None,
            "module",
            Language::Python,
            1,
            5,
            "import os",
        );
        let results = vec![make_search_result(chunk, 0.60)];

        let ctx = super::format_code_search_context(&results);

        assert!(ctx.contains("### 1. `src/utils.py` (Similarity: 60%)"));
        // No symbol line should be present
        assert!(!ctx.contains("**Symbol:**"));
        assert!(ctx.contains("**Lines:** 1-5 | **Language:** Python"));
        assert!(ctx.contains("```python"));
    }

    #[test]
    fn test_format_code_search_context_truncates_long_code() {
        let long_code = "x".repeat(3000);
        let chunk = make_chunk(
            "src/big.rs",
            Some("big_fn"),
            "function",
            Language::Rust,
            1,
            100,
            &long_code,
        );
        let results = vec![make_search_result(chunk, 0.50)];

        let ctx = super::format_code_search_context(&results);

        assert!(ctx.contains("(truncated)"));
        // The truncated code should be 2000 chars + "...\n(truncated)"
        assert!(!ctx.contains(&"x".repeat(3000)));
        assert!(ctx.contains(&"x".repeat(2000)));
    }

    #[test]
    fn test_format_code_search_context_similarity_rounding() {
        let chunk = make_chunk(
            "src/test.go",
            None,
            "function",
            Language::Go,
            1,
            1,
            "func test() {}",
        );
        let results = vec![make_search_result(chunk, 0.8567)];

        let ctx = super::format_code_search_context(&results);
        // 0.8567 * 100 = 85.67, rounded to 86%
        assert!(ctx.contains("Similarity: 86%"));
    }

    #[test]
    fn test_format_code_search_context_all_languages_lowercase_fence() {
        for (lang, expected_fence) in [
            (Language::Rust, "```rust"),
            (Language::TypeScript, "```typescript"),
            (Language::JavaScript, "```javascript"),
            (Language::Python, "```python"),
            (Language::Go, "```go"),
            (Language::Java, "```java"),
            (Language::Cpp, "```c++"),
            (Language::C, "```c"),
        ] {
            let chunk = make_chunk("test.x", None, "function", lang, 1, 1, "code");
            let results = vec![make_search_result(chunk, 0.5)];
            let ctx = super::format_code_search_context(&results);
            assert!(
                ctx.contains(expected_fence),
                "Expected fence '{}' for {:?}, got:\n{}",
                expected_fence,
                lang,
                ctx
            );
        }
    }

    #[test]
    fn test_format_code_search_context_contains_header_text() {
        let chunk = make_chunk("a.rs", None, "function", Language::Rust, 1, 1, "x");
        let results = vec![make_search_result(chunk, 0.9)];
        let ctx = super::format_code_search_context(&results);
        assert!(ctx.contains("semantically relevant to this issue"));
        assert!(ctx.contains("Use them to understand the codebase"));
    }

    // ================================================================
    // build_code_search_query tests
    // ================================================================

    #[test]
    fn test_build_code_search_query_default_source() {
        let issue = crate::types::Issue::new("1", "T-1", "Bug title", "url", "jira");
        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "Bug title");
    }

    #[test]
    fn test_build_code_search_query_default_source_with_description() {
        let mut issue = crate::types::Issue::new("1", "T-1", "Bug title", "url", "linear");
        issue.description = Some("The login page crashes".to_string());
        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "Bug title The login page crashes");
    }

    #[test]
    fn test_build_code_search_query_sentry_basic() {
        let issue =
            crate::types::Issue::new("1", "SENTRY-1", "TypeError", "url", "sentry");
        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "TypeError");
    }

    #[test]
    fn test_build_code_search_query_sentry_with_all_metadata() {
        let mut issue =
            crate::types::Issue::new("1", "SENTRY-1", "TypeError", "url", "sentry");
        issue.description = Some("Cannot read property 'x'".to_string());
        issue.set_metadata("error_type", "TypeError");
        issue.set_metadata("error_value", "Cannot read property 'x' of undefined");
        issue.set_metadata("culprit", "app.js:main");
        issue.set_metadata("filename", "src/app.js");
        issue.set_metadata("function", "handleClick");

        let query = super::build_code_search_query(&issue);

        assert!(query.contains("TypeError"));
        assert!(query.contains("Cannot read property 'x'"));
        assert!(query.contains("Cannot read property 'x' of undefined"));
        assert!(query.contains("app.js:main"));
        assert!(query.contains("src/app.js"));
        assert!(query.contains("handleClick"));
    }

    #[test]
    fn test_build_code_search_query_sentry_partial_metadata() {
        let mut issue =
            crate::types::Issue::new("1", "SENTRY-1", "NullPointerException", "url", "sentry");
        issue.set_metadata("error_type", "NullPointerException");
        issue.set_metadata("filename", "com/app/Service.java");
        // No error_value, culprit, or function

        let query = super::build_code_search_query(&issue);

        assert!(query.contains("NullPointerException"));
        assert!(query.contains("com/app/Service.java"));
        // Should not have extra spaces from missing fields
        assert!(!query.contains("  "));
    }

    #[test]
    fn test_build_code_search_query_sentry_empty_culprit_excluded() {
        let mut issue =
            crate::types::Issue::new("1", "SENTRY-1", "Error", "url", "sentry");
        issue.set_metadata("culprit", "");

        let query = super::build_code_search_query(&issue);
        // Empty culprit should not be included
        assert_eq!(query, "Error");
    }

    #[test]
    fn test_build_code_search_query_sentry_with_description_no_metadata() {
        let mut issue =
            crate::types::Issue::new("1", "SENTRY-1", "Error", "url", "sentry");
        issue.description = Some("Something went wrong".to_string());

        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "Error Something went wrong");
    }

    #[test]
    fn test_build_code_search_query_discord_uses_default_path() {
        let mut issue = crate::types::Issue::new("1", "D-1", "Help needed", "url", "discord");
        issue.description = Some("My app crashes on startup".to_string());
        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "Help needed My app crashes on startup");
    }

    #[test]
    fn test_build_code_search_query_slack_uses_default_path() {
        let mut issue = crate::types::Issue::new("1", "S-1", "Alert", "url", "slack");
        issue.description = Some("High CPU usage".to_string());
        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "Alert High CPU usage");
    }

    #[test]
    fn test_build_code_search_query_no_description_non_sentry() {
        let issue = crate::types::Issue::new("1", "T-1", "Title only", "url", "github");
        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "Title only");
    }

    #[test]
    fn test_build_code_search_query_sentry_no_description_no_metadata() {
        let issue = crate::types::Issue::new("1", "S-1", "Error", "url", "sentry");
        let query = super::build_code_search_query(&issue);
        assert_eq!(query, "Error");
    }
}
