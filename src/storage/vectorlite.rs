//! Vectorlite integration for HNSW-based similarity search.
//!
//! Uses the vectorlite SQLite extension for efficient approximate nearest neighbor search.

use crate::error::{Error, Result};
use rusqlite::{params, Connection};
use std::path::Path;

/// Configuration for the vector store.
#[derive(Debug, Clone)]
pub struct VectorStoreConfig {
    /// Embedding dimension (768 for nomic-embed-text).
    pub dimension: usize,
    /// HNSW max elements (capacity).
    pub max_elements: usize,
    /// HNSW ef_construction parameter (higher = better quality, slower build).
    pub ef_construction: usize,
    /// HNSW M parameter (connections per node).
    pub m: usize,
    /// Distance type: "l2", "cosine", or "ip".
    pub distance_type: String,
}

impl Default for VectorStoreConfig {
    fn default() -> Self {
        Self {
            dimension: 768, // nomic-embed-text
            max_elements: 10000,
            ef_construction: 200,
            m: 16,
            distance_type: "cosine".to_string(),
        }
    }
}

/// Load the vectorlite extension into a SQLite connection.
///
/// # Safety
/// This enables extension loading which can execute arbitrary code.
/// Only load trusted extensions.
pub fn load_vectorlite_extension(conn: &Connection, extension_path: &Path) -> Result<()> {
    // Enable extension loading
    unsafe {
        conn.load_extension_enable()?;
    }

    // Load vectorlite
    // SAFETY: We only load the vectorlite extension from trusted paths
    unsafe {
        conn.load_extension(extension_path, None)
            .map_err(|e| Error::Other(format!("Failed to load vectorlite extension: {}", e)))?;
    }

    // Disable extension loading for safety
    conn.load_extension_disable()?;

    Ok(())
}

/// Try to load vectorlite from common paths.
pub fn try_load_vectorlite(conn: &Connection) -> Result<bool> {
    let common_paths = [
        // Docker/Linux paths
        "/usr/local/lib/vectorlite.so",
        "/usr/lib/vectorlite.so",
        "/app/lib/vectorlite.so",
        // macOS paths
        "/usr/local/lib/vectorlite.dylib",
        "/opt/homebrew/lib/vectorlite.dylib",
        // Local development
        "./vectorlite.so",
        "./vectorlite.dylib",
    ];

    for path in common_paths {
        let path = Path::new(path);
        if path.exists() {
            match load_vectorlite_extension(conn, path) {
                Ok(()) => {
                    tracing::info!("Loaded vectorlite from {:?}", path);
                    return Ok(true);
                }
                Err(e) => {
                    tracing::warn!("Failed to load vectorlite from {:?}: {}", path, e);
                }
            }
        }
    }

    // Also try from VECTORLITE_PATH env var
    if let Ok(path) = std::env::var("VECTORLITE_PATH") {
        let path = Path::new(&path);
        if path.exists() {
            load_vectorlite_extension(conn, path)?;
            tracing::info!("Loaded vectorlite from VECTORLITE_PATH: {:?}", path);
            return Ok(true);
        }
    }

    tracing::warn!("Vectorlite extension not found, vector search will be disabled");
    Ok(false)
}

/// Create the vector virtual table for outcome embeddings.
pub fn create_vector_table(conn: &Connection, config: &VectorStoreConfig) -> Result<()> {
    let sql = format!(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS outcome_embeddings USING vectorlite(
            embedding float32[{dimension}] {distance_type},
            hnsw(max_elements={max_elements}, ef_construction={ef_construction}, M={m})
        )
        "#,
        dimension = config.dimension,
        distance_type = config.distance_type,
        max_elements = config.max_elements,
        ef_construction = config.ef_construction,
        m = config.m,
    );

    conn.execute_batch(&sql)?;
    Ok(())
}

/// Insert an embedding into the vector store.
pub fn insert_embedding(conn: &Connection, rowid: i64, embedding: &[f32]) -> Result<()> {
    let blob = embedding_to_blob(embedding);
    conn.execute(
        "INSERT INTO outcome_embeddings(rowid, embedding) VALUES (?, ?)",
        params![rowid, blob],
    )?;
    Ok(())
}

/// Update an embedding in the vector store.
pub fn update_embedding(conn: &Connection, rowid: i64, embedding: &[f32]) -> Result<()> {
    // Vectorlite doesn't support UPDATE, so delete and reinsert
    conn.execute(
        "DELETE FROM outcome_embeddings WHERE rowid = ?",
        params![rowid],
    )?;
    insert_embedding(conn, rowid, embedding)
}

/// Delete an embedding from the vector store.
pub fn delete_embedding(conn: &Connection, rowid: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM outcome_embeddings WHERE rowid = ?",
        params![rowid],
    )?;
    Ok(())
}

/// Search for similar embeddings using HNSW.
///
/// Returns (rowid, distance) pairs sorted by distance.
pub fn search_similar(
    conn: &Connection,
    query: &[f32],
    k: usize,
    ef: usize,
) -> Result<Vec<(i64, f32)>> {
    let blob = embedding_to_blob(query);

    let mut stmt = conn.prepare(
        "SELECT rowid, distance FROM outcome_embeddings WHERE knn_search(embedding, knn_param(?, ?, ?))"
    )?;

    let results = stmt
        .query_map(params![blob, k as i64, ef as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f32>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Search for similar embeddings with rowid filtering.
pub fn search_similar_filtered(
    conn: &Connection,
    query: &[f32],
    k: usize,
    ef: usize,
    rowid_filter: &[i64],
) -> Result<Vec<(i64, f32)>> {
    if rowid_filter.is_empty() {
        return search_similar(conn, query, k, ef);
    }

    let blob = embedding_to_blob(query);
    let placeholders: Vec<String> = rowid_filter.iter().map(|_| "?".to_string()).collect();
    let filter_sql = placeholders.join(", ");

    let sql = format!(
        "SELECT rowid, distance FROM outcome_embeddings WHERE knn_search(embedding, knn_param(?, ?, ?)) AND rowid IN ({})",
        filter_sql
    );

    let mut stmt = conn.prepare(&sql)?;

    // Build params: blob, k, ef, then all rowids
    let mut params: Vec<Box<dyn rusqlite::ToSql>> =
        vec![Box::new(blob), Box::new(k as i64), Box::new(ef as i64)];
    for id in rowid_filter {
        params.push(Box::new(*id));
    }

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let results = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f32>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Convert f32 slice to blob for SQLite storage.
fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Convert blob back to f32 vector.
#[allow(dead_code)]
fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Check if vectorlite is available in this connection.
pub fn is_vectorlite_available(conn: &Connection) -> bool {
    conn.execute_batch("SELECT vectorlite_info()").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_to_blob_roundtrip() {
        let embedding = vec![1.0f32, 2.5, -3.7, 0.0, 100.123];
        let blob = embedding_to_blob(&embedding);
        let restored = blob_to_embedding(&blob);

        assert_eq!(embedding.len(), restored.len());
        for (a, b) in embedding.iter().zip(restored.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_vector_store_config_default() {
        let config = VectorStoreConfig::default();
        assert_eq!(config.dimension, 768);
        assert_eq!(config.distance_type, "cosine");
    }
}
