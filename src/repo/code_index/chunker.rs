//! AST-aware semantic chunking of source files.
//!
//! Produces chunks aligned to symbol boundaries (functions, classes, top-level code)
//! suitable for embedding and vector similarity search.

use super::parser::{extract_symbols, parse_file};
use super::types::{CodeChunk, CodeSymbol, Language, SymbolKind};
use crate::error::Result;

/// Maximum chunk size in characters (~1500 tokens).
const MAX_CHUNK_CHARS: usize = 6000;
/// Minimum chunk size — trivial chunks are merged into enclosing context.
const MIN_CHUNK_LINES: usize = 3;
const MIN_CHUNK_CHARS: usize = 50;
/// Large class threshold: classes above this line count get split.
const LARGE_CLASS_LINES: usize = 100;
/// Top-level chunk cap.
const TOP_LEVEL_MAX_LINES: usize = 200;

/// Chunk a source file using its AST, producing semantic chunks for embedding.
pub fn chunk_file(
    source: &str,
    language: Language,
    repo_id: i64,
    file_path: &str,
    file_hash: &str,
) -> Result<(Vec<CodeSymbol>, Vec<CodeChunk>)> {
    let tree = parse_file(source, language)?;
    let symbols = extract_symbols(&tree, source.as_bytes(), language, repo_id, file_path);

    let lines: Vec<&str> = source.lines().collect();
    let mut chunks = Vec::new();
    let mut covered_lines = vec![false; lines.len()];

    // Phase 1: Create chunks from top-level container symbols (classes, impls, traits).
    for sym in symbols.iter().filter(|s| is_top_level_container(s)) {
        let start = sym.start_line.saturating_sub(1); // convert to 0-indexed
        let end = sym.end_line.min(lines.len());
        let line_count = end.saturating_sub(start);

        if line_count > LARGE_CLASS_LINES {
            // Large container: skeleton + per-method chunks
            chunks.push(build_skeleton_chunk(
                &lines, sym, &symbols, language, repo_id, file_path, file_hash,
            ));

            // Create individual method chunks
            for method in symbols.iter().filter(|s| {
                s.parent_symbol.as_deref() == Some(&sym.symbol_name)
                    && matches!(s.symbol_kind, SymbolKind::Method | SymbolKind::Function)
            }) {
                let m_start = method.start_line.saturating_sub(1);
                let m_end = method.end_line.min(lines.len());
                if let Some(chunk) =
                    build_symbol_chunk(&lines, method, language, repo_id, file_path, file_hash)
                {
                    chunks.push(chunk);
                }
                mark_covered(&mut covered_lines, m_start, m_end);
            }
        } else {
            // Small container: single chunk
            if let Some(chunk) =
                build_symbol_chunk(&lines, sym, language, repo_id, file_path, file_hash)
            {
                chunks.push(chunk);
            }
        }
        mark_covered(&mut covered_lines, start, end);
    }

    // Phase 2: Create chunks from standalone functions (not inside containers).
    for sym in symbols
        .iter()
        .filter(|s| s.parent_symbol.is_none() && matches!(s.symbol_kind, SymbolKind::Function))
    {
        let start = sym.start_line.saturating_sub(1);
        let end = sym.end_line.min(lines.len());
        if !covered_lines.get(start).copied().unwrap_or(true) {
            if let Some(chunk) =
                build_symbol_chunk(&lines, sym, language, repo_id, file_path, file_hash)
            {
                chunks.push(chunk);
            }
            mark_covered(&mut covered_lines, start, end);
        }
    }

    // Phase 3: Collect uncovered top-level code into a top_level chunk.
    // Only include lines that are actually uncovered (skip covered gaps).
    let mut top_level_lines: Vec<usize> = Vec::new();
    for (i, &covered) in covered_lines.iter().enumerate() {
        if !covered && !lines.get(i).unwrap_or(&"").trim().is_empty() {
            top_level_lines.push(i);
        }
    }

    if !top_level_lines.is_empty() {
        // Collect only the uncovered lines rather than a contiguous range
        // that could re-include already-chunked lines.
        let collect_uncovered = |idxs: &[usize]| -> String {
            idxs.iter()
                .map(|&i| lines.get(i).copied().unwrap_or(""))
                .collect::<Vec<&str>>()
                .join("\n")
        };

        if top_level_lines.len() <= TOP_LEVEL_MAX_LINES {
            let text = collect_uncovered(&top_level_lines);
            let start = top_level_lines[0];
            let end = *top_level_lines.last().unwrap() + 1;
            if text.len() >= MIN_CHUNK_CHARS && top_level_lines.len() >= MIN_CHUNK_LINES {
                let context =
                    build_context_text(file_path, language, "top_level", None, None, &text);
                chunks.push(CodeChunk {
                    id: None,
                    repo_id,
                    file_path: file_path.to_string(),
                    chunk_type: "top_level".to_string(),
                    symbol_name: None,
                    language,
                    start_line: start + 1,
                    end_line: end,
                    chunk_text: text,
                    context_text: context,
                    file_hash: file_hash.to_string(),
                });
            }
        } else {
            // Split top-level into multiple capped chunks
            for batch in top_level_lines.chunks(TOP_LEVEL_MAX_LINES) {
                let text = collect_uncovered(batch);
                let start = batch[0];
                let end = *batch.last().unwrap() + 1;
                if text.len() >= MIN_CHUNK_CHARS && batch.len() >= MIN_CHUNK_LINES {
                    let context =
                        build_context_text(file_path, language, "top_level", None, None, &text);
                    chunks.push(CodeChunk {
                        id: None,
                        repo_id,
                        file_path: file_path.to_string(),
                        chunk_type: "top_level".to_string(),
                        symbol_name: None,
                        language,
                        start_line: start + 1,
                        end_line: end,
                        chunk_text: text,
                        context_text: context,
                        file_hash: file_hash.to_string(),
                    });
                }
            }
        }
    }

    // Phase 4: Split any oversized chunks.
    let final_chunks = split_oversized(chunks);

    Ok((symbols, final_chunks))
}

fn is_top_level_container(sym: &CodeSymbol) -> bool {
    sym.parent_symbol.is_none()
        && matches!(
            sym.symbol_kind,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Trait
                | SymbolKind::Impl
                | SymbolKind::Interface
                | SymbolKind::Enum
                | SymbolKind::Module
        )
}

fn mark_covered(covered: &mut [bool], start: usize, end: usize) {
    for line in covered.iter_mut().take(end).skip(start) {
        *line = true;
    }
}

fn collect_lines(lines: &[&str], start: usize, end: usize) -> String {
    lines[start..end.min(lines.len())].join("\n")
}

/// Gather preceding comment/attribute lines for a symbol.
fn gather_leading_context(lines: &[&str], start_0idx: usize) -> usize {
    let mut ctx_start = start_0idx;
    while ctx_start > 0 {
        let prev = lines[ctx_start - 1].trim();
        if prev.starts_with("//")       // C-style line comments (includes ///)
            || prev.starts_with("#[")    // Rust attributes
            || prev.starts_with("#![")   // Rust inner attributes
            || prev.starts_with("# ")    // Python/Ruby comments
            || prev.starts_with("/**")   // Block doc comment start
            || prev.starts_with("* ")    // Block doc comment continuation
            || prev.starts_with("*/")    // Block doc comment end
            || prev.starts_with("@")     // Java/Kotlin annotations
            || prev.starts_with("'''")   // Python triple-quote docstrings
            || prev.starts_with("\"\"\"")
        // Python triple-quote docstrings
        {
            ctx_start -= 1;
        } else {
            break;
        }
    }
    ctx_start
}

fn build_symbol_chunk(
    lines: &[&str],
    sym: &CodeSymbol,
    language: Language,
    repo_id: i64,
    file_path: &str,
    file_hash: &str,
) -> Option<CodeChunk> {
    let raw_start = sym.start_line.saturating_sub(1);
    let start = gather_leading_context(lines, raw_start);
    let end = sym.end_line.min(lines.len());

    if end <= start {
        return None;
    }

    let text = collect_lines(lines, start, end);
    if text.len() < MIN_CHUNK_CHARS || (end - start) < MIN_CHUNK_LINES {
        return None;
    }

    let chunk_type = match sym.symbol_kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "function",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "class",
        SymbolKind::Impl => "impl_block",
        SymbolKind::Trait => "class",
        SymbolKind::Interface => "class",
        SymbolKind::Enum => "class",
        SymbolKind::Module => "module_header",
        SymbolKind::Constant => "top_level",
    };

    let context = build_context_text(
        file_path,
        language,
        chunk_type,
        Some(&sym.symbol_name),
        sym.parent_symbol.as_deref(),
        &text,
    );

    Some(CodeChunk {
        id: None,
        repo_id,
        file_path: file_path.to_string(),
        chunk_type: chunk_type.to_string(),
        symbol_name: Some(sym.symbol_name.clone()),
        language,
        start_line: start + 1,
        end_line: end,
        chunk_text: text,
        context_text: context,
        file_hash: file_hash.to_string(),
    })
}

/// Build a skeleton chunk for a large class (declaration + method signatures only).
fn build_skeleton_chunk(
    lines: &[&str],
    sym: &CodeSymbol,
    all_symbols: &[CodeSymbol],
    language: Language,
    repo_id: i64,
    file_path: &str,
    file_hash: &str,
) -> CodeChunk {
    let raw_start = sym.start_line.saturating_sub(1);
    let start = gather_leading_context(lines, raw_start);
    let end = sym.end_line.min(lines.len());

    // Collect leading comments/attributes + first few lines of the class.
    let mut skeleton = String::new();
    if start < raw_start {
        skeleton.push_str(&collect_lines(lines, start, raw_start));
        skeleton.push('\n');
    }
    // First 5 lines of the class body
    let header_end = (raw_start + 5).min(end);
    skeleton.push_str(&collect_lines(lines, raw_start, header_end));
    skeleton.push_str("\n\n    // ... methods ...\n\n");

    for method in all_symbols.iter().filter(|s| {
        s.parent_symbol.as_deref() == Some(&sym.symbol_name)
            && matches!(s.symbol_kind, SymbolKind::Method | SymbolKind::Function)
    }) {
        if let Some(ref sig) = method.signature {
            skeleton.push_str("    ");
            skeleton.push_str(sig);
            skeleton.push_str(" { ... }\n");
        }
    }

    let context = build_context_text(
        file_path,
        language,
        "class",
        Some(&sym.symbol_name),
        None,
        &skeleton,
    );

    CodeChunk {
        id: None,
        repo_id,
        file_path: file_path.to_string(),
        chunk_type: "class".to_string(),
        symbol_name: Some(sym.symbol_name.clone()),
        language,
        start_line: start + 1,
        end_line: end,
        chunk_text: skeleton,
        context_text: context,
        file_hash: file_hash.to_string(),
    }
}

/// Build enriched context text for embedding.
pub fn build_context_text(
    file_path: &str,
    language: Language,
    chunk_type: &str,
    symbol_name: Option<&str>,
    parent: Option<&str>,
    code: &str,
) -> String {
    let mut ctx = format!("File: {}\nLanguage: {}\n", file_path, language);

    if let Some(name) = symbol_name {
        ctx.push_str(&format!("Symbol: {} ({})\n", name, chunk_type));
    } else {
        ctx.push_str(&format!("Type: {}\n", chunk_type));
    }

    if let Some(p) = parent {
        ctx.push_str(&format!("Parent: {}\n", p));
    }

    ctx.push('\n');
    // Truncate code if absurdly long for the context field
    if code.len() > MAX_CHUNK_CHARS {
        // Find the nearest char boundary at or before MAX_CHUNK_CHARS to avoid
        // panicking on multi-byte UTF-8 sequences.
        let mut end = MAX_CHUNK_CHARS;
        while end > 0 && !code.is_char_boundary(end) {
            end -= 1;
        }
        ctx.push_str(&code[..end]);
        ctx.push_str("\n// ... truncated ...");
    } else {
        ctx.push_str(code);
    }

    ctx
}

/// Split chunks that exceed MAX_CHUNK_CHARS.
fn split_oversized(chunks: Vec<CodeChunk>) -> Vec<CodeChunk> {
    let mut result = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        if chunk.chunk_text.len() <= MAX_CHUNK_CHARS {
            result.push(chunk);
            continue;
        }

        // Split at line boundaries near the limit.
        let lines: Vec<&str> = chunk.chunk_text.lines().collect();
        let mut part_start = 0;
        let mut current_size = 0;
        let base_start_line = chunk.start_line;

        for (i, line) in lines.iter().enumerate() {
            current_size += line.len() + 1; // +1 for newline
            if current_size >= MAX_CHUNK_CHARS || i == lines.len() - 1 {
                let part_end = i + 1;
                let text = lines[part_start..part_end].join("\n");
                if text.len() >= MIN_CHUNK_CHARS {
                    let context = build_context_text(
                        &chunk.file_path,
                        chunk.language,
                        &chunk.chunk_type,
                        chunk.symbol_name.as_deref(),
                        None,
                        &text,
                    );
                    result.push(CodeChunk {
                        id: None,
                        repo_id: chunk.repo_id,
                        file_path: chunk.file_path.clone(),
                        chunk_type: chunk.chunk_type.clone(),
                        symbol_name: chunk.symbol_name.clone(),
                        language: chunk.language,
                        start_line: base_start_line + part_start,
                        end_line: base_start_line + part_end - 1,
                        chunk_text: text,
                        context_text: context,
                        file_hash: chunk.file_hash.clone(),
                    });
                }
                part_start = part_end;
                current_size = 0;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_rust_file() {
        let src = r#"
use std::io;

/// A greeter.
pub struct Greeter {
    name: String,
}

impl Greeter {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }

    pub fn greet(&self) -> String {
        format!("Hello, {}!", self.name)
    }
}

fn standalone() {
    println!("I'm standalone");
}
"#;
        let (symbols, chunks) = chunk_file(src, Language::Rust, 1, "greeter.rs", "abc123").unwrap();

        assert!(!symbols.is_empty());
        assert!(!chunks.is_empty());

        // Should have at least: struct chunk, impl chunk or method chunks, standalone fn chunk
        let chunk_types: Vec<&str> = chunks.iter().map(|c| c.chunk_type.as_str()).collect();
        assert!(
            chunk_types.contains(&"function")
                || chunk_types.contains(&"class")
                || chunk_types.contains(&"impl_block"),
            "chunk_types = {:?}",
            chunk_types
        );

        // All chunks should have context_text populated
        for chunk in &chunks {
            assert!(chunk.context_text.contains("File: greeter.rs"));
            assert!(chunk.context_text.contains("Language: Rust"));
            assert_eq!(chunk.file_hash, "abc123");
        }
    }

    #[test]
    fn test_chunk_python_class_and_function() {
        let src = r#"
import os

class Calculator:
    def add(self, a, b):
        return a + b

    def subtract(self, a, b):
        return a - b

def main():
    calc = Calculator()
    print(calc.add(1, 2))
"#;
        let (_, chunks) = chunk_file(src, Language::Python, 1, "calc.py", "hash1").unwrap();
        assert!(!chunks.is_empty());

        // Check that the class is chunked
        assert!(chunks
            .iter()
            .any(|c| c.symbol_name.as_deref() == Some("Calculator")));
    }

    #[test]
    fn test_chunk_empty_file() {
        let src = "";
        let (symbols, chunks) = chunk_file(src, Language::Rust, 1, "empty.rs", "h").unwrap();
        assert!(symbols.is_empty());
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_build_context_text_format() {
        let ctx = build_context_text(
            "src/auth.rs",
            Language::Rust,
            "function",
            Some("authenticate"),
            Some("AuthService"),
            "pub fn authenticate(&self) -> bool { true }",
        );
        assert!(ctx.contains("File: src/auth.rs"));
        assert!(ctx.contains("Language: Rust"));
        assert!(ctx.contains("Symbol: authenticate (function)"));
        assert!(ctx.contains("Parent: AuthService"));
        assert!(ctx.contains("pub fn authenticate"));
    }

    #[test]
    fn test_min_chunk_filtering() {
        // A file with only a tiny constant — should be filtered out or kept as top_level
        let src = "const X: i32 = 1;\n";
        let (_, chunks) = chunk_file(src, Language::Rust, 1, "tiny.rs", "h").unwrap();
        // Tiny chunks (< MIN_CHUNK_LINES or MIN_CHUNK_CHARS) are filtered
        for chunk in &chunks {
            assert!(chunk.chunk_text.len() >= MIN_CHUNK_CHARS);
        }
    }
}
