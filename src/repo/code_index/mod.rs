//! Tree-sitter powered code indexing pipeline.
//!
//! Parses source files into ASTs, extracts symbols (functions, classes, structs, traits),
//! creates AST-aware chunks at semantic boundaries, embeds those chunks for vector similarity
//! search, and persists everything in SQLite. Incremental re-indexing via file hashing.

mod chunker;
mod languages;
mod parser;
pub mod types;

pub use types::{CodeChunk, CodeIndexStats, CodeSearchResult, CodeSymbol, Language, SymbolKind};

use crate::error::Result;
use crate::feedback::EmbeddingClient;
use crate::storage::SqliteTracker;
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
    tracker: Arc<SqliteTracker>,
    embedding_client: Arc<EmbeddingClient>,
    max_file_size: u64,
    batch_size: usize,
}

impl CodeIndexer {
    pub fn new(tracker: Arc<SqliteTracker>, embedding_client: Arc<EmbeddingClient>) -> Self {
        Self {
            tracker,
            embedding_client,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    pub fn with_config(
        tracker: Arc<SqliteTracker>,
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
    tracker: Arc<SqliteTracker>,
    embedding_client: Arc<EmbeddingClient>,
}

impl CodeSearchService {
    pub fn new(tracker: Arc<SqliteTracker>, embedding_client: Arc<EmbeddingClient>) -> Self {
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
}
