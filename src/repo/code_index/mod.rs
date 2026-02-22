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
use sha2::{Digest, Sha256};
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

        // Collect all source files.
        let files = self.collect_source_files(repo_path);
        tracing::info!(
            repo = %repo_name,
            source_files = files.len(),
            "Starting code indexing"
        );

        // Track which file paths we see so we can clean up stale entries.
        let mut seen_paths: Vec<String> = Vec::with_capacity(files.len());

        // Batch of chunks pending embedding.
        let mut pending_chunks: Vec<types::CodeChunk> = Vec::new();
        let mut pending_symbols: Vec<types::CodeSymbol> = Vec::new();

        for (path, language) in &files {
            let rel_path = path
                .strip_prefix(repo_path)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            // Read file content.
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(file = %rel_path, error = %e, "Failed to read file");
                    stats.files_failed += 1;
                    continue;
                }
            };

            // Compute hash for incremental detection.
            let file_hash = sha256_hex(&content);

            // Check if file is already indexed with this hash.
            if self
                .tracker
                .code_chunk_hash_matches(repo_id, &rel_path, &file_hash)?
            {
                stats.files_skipped += 1;
                seen_paths.push(rel_path);
                continue;
            }

            // Delete old data for this file before re-indexing.
            self.tracker.delete_code_data_for_file(repo_id, &rel_path)?;

            // Parse and chunk.
            match chunker::chunk_file(&content, *language, repo_id, &rel_path, &file_hash) {
                Ok((symbols, chunks)) => {
                    stats.files_processed += 1;
                    stats.symbols_extracted += symbols.len();
                    stats.chunks_created += chunks.len();
                    pending_symbols.extend(symbols);
                    pending_chunks.extend(chunks);
                    seen_paths.push(rel_path);
                }
                Err(e) => {
                    tracing::debug!(file = %rel_path, error = %e, "Failed to parse file");
                    stats.files_failed += 1;
                }
            }

            // Flush batch if large enough.
            if pending_chunks.len() >= self.batch_size {
                self.flush_batch(&mut pending_symbols, &mut pending_chunks, &mut stats)
                    .await?;
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

        // Store chunks and get their IDs.
        let chunk_ids = self.tracker.save_code_chunks(chunks)?;

        // Generate embeddings for context_text.
        let texts: Vec<&str> = chunks.iter().map(|c| c.context_text.as_str()).collect();

        // Batch in groups of batch_size.
        for (batch_idx, text_batch) in texts.chunks(self.batch_size).enumerate() {
            match self.embedding_client.embed_batch(text_batch).await {
                Ok(embeddings) => {
                    let start = batch_idx * self.batch_size;
                    let pairs: Vec<(i64, &[f32])> = embeddings
                        .iter()
                        .enumerate()
                        .filter_map(|(i, emb)| {
                            chunk_ids.get(start + i).map(|&id| (id, emb.as_slice()))
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
                    let start = batch_idx * self.batch_size;
                    let end = (start + text_batch.len()).min(chunk_ids.len());
                    let failed_ids: Vec<i64> = chunk_ids[start..end].to_vec();
                    if let Err(del_err) = self.tracker.delete_code_chunks_by_ids(&failed_ids) {
                        tracing::warn!(error = %del_err, "Failed to delete unembed code chunks");
                    }
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

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let inputs = vec!["a", "b", "c", "ab", "abc", "A", "B"];
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
        let indexer = CodeIndexer::new(tracker, embedding_client);

        assert_eq!(indexer.max_file_size, DEFAULT_MAX_FILE_SIZE);
        assert_eq!(indexer.batch_size, DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn test_code_indexer_with_config_custom_values() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
        let indexer = CodeIndexer::with_config(tracker, embedding_client, 512, 64);

        // max_file_size_kb is multiplied by 1024
        assert_eq!(indexer.max_file_size, 512 * 1024);
        assert_eq!(indexer.batch_size, 64);
    }

    #[test]
    fn test_code_indexer_with_config_zero_batch_size_uses_default() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
        let indexer = CodeIndexer::with_config(tracker, embedding_client, 256, 0);

        assert_eq!(
            indexer.batch_size, DEFAULT_BATCH_SIZE,
            "batch_size=0 should fall back to DEFAULT_BATCH_SIZE"
        );
    }

    #[test]
    fn test_code_indexer_with_config_small_file_size() {
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
        let embedding_client =
            Arc::new(crate::feedback::EmbeddingClient::new(Default::default()).unwrap());
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
}
