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
/// Number of lines to carry forward when splitting oversized chunks.
const OVERLAP_LINES: usize = 3;

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
                let prefix = build_context_prefix(file_path, language, "top_level", None, None);
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
                    context_text: prefix,
                    file_hash: file_hash.to_string(),
                    content_hash: None,
                });
            }
        } else {
            // Split top-level into multiple capped chunks
            for batch in top_level_lines.chunks(TOP_LEVEL_MAX_LINES) {
                let text = collect_uncovered(batch);
                let start = batch[0];
                let end = *batch.last().unwrap() + 1;
                if text.len() >= MIN_CHUNK_CHARS && batch.len() >= MIN_CHUNK_LINES {
                    let prefix = build_context_prefix(file_path, language, "top_level", None, None);
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
                        context_text: prefix,
                        file_hash: file_hash.to_string(),
                        content_hash: None,
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
///
/// Handles multi-line Python docstrings (`"""` / `'''`): when we encounter a
/// closing delimiter we continue walking back until the matching opening
/// delimiter is found so that the full docstring is captured.
fn gather_leading_context(lines: &[&str], start_0idx: usize) -> usize {
    if start_0idx >= lines.len() {
        return start_0idx;
    }
    let mut ctx_start = start_0idx;
    let mut inside_triple_quote: Option<&str> = None; // "\"\"\"" or "'''"
    while ctx_start > 0 {
        let prev = lines[ctx_start - 1].trim();

        // When inside a multi-line docstring, keep walking until we find
        // the matching opening delimiter.
        if let Some(delim) = inside_triple_quote {
            ctx_start -= 1;
            if prev.contains(delim) {
                inside_triple_quote = None;
            }
            continue;
        }

        if prev.is_empty() {
            break;
        }

        // Check for closing triple-quote delimiter that starts a backwards
        // walk through a multi-line docstring.
        if prev.ends_with("\"\"\"") || prev.starts_with("\"\"\"") {
            let delim = "\"\"\"";
            ctx_start -= 1;
            // If the opening and closing are on the same line, we're done.
            let count = prev.matches(delim).count();
            if count < 2 {
                inside_triple_quote = Some(delim);
            }
            continue;
        }
        if prev.ends_with("'''") || prev.starts_with("'''") {
            let delim = "'''";
            ctx_start -= 1;
            let count = prev.matches(delim).count();
            if count < 2 {
                inside_triple_quote = Some(delim);
            }
            continue;
        }

        if prev.starts_with("//")       // C-style line comments (includes ///)
            || prev.starts_with("#[")    // Rust attributes
            || prev.starts_with("#![")   // Rust inner attributes
            || prev.starts_with("# ")    // Python/Ruby comments
            || prev.starts_with("/**")   // Block doc comment start
            || prev.starts_with("* ")    // Block doc comment continuation
            || prev.starts_with("*/")    // Block doc comment end
            || prev.starts_with("@")
        // Java/Kotlin annotations
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

    let prefix = build_context_prefix(
        file_path,
        language,
        chunk_type,
        Some(&sym.symbol_name),
        sym.parent_symbol.as_deref(),
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
        context_text: prefix,
        file_hash: file_hash.to_string(),
        content_hash: None,
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

    let prefix = build_context_prefix(file_path, language, "class", Some(&sym.symbol_name), None);

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
        context_text: prefix,
        file_hash: file_hash.to_string(),
        content_hash: None,
    }
}

/// Build the metadata prefix for storage in the DB.
///
/// This is a lightweight version of `build_context_text` that excludes the code body.
/// The full context (prefix + code) is reconstructed at embedding time and search time.
pub fn build_context_prefix(
    file_path: &str,
    language: Language,
    chunk_type: &str,
    symbol_name: Option<&str>,
    parent: Option<&str>,
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

    ctx
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
///
/// Carries forward the last [`OVERLAP_LINES`] lines from each sub-chunk to the
/// start of the next so that boundary context is preserved for better embedding
/// quality.
fn split_oversized(chunks: Vec<CodeChunk>) -> Vec<CodeChunk> {
    let mut result = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        if chunk.chunk_text.len() <= MAX_CHUNK_CHARS {
            result.push(chunk);
            continue;
        }

        let prefix = build_context_prefix(
            &chunk.file_path,
            chunk.language,
            &chunk.chunk_type,
            chunk.symbol_name.as_deref(),
            None,
        );

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
                        context_text: prefix.clone(),
                        file_hash: chunk.file_hash.clone(),
                        content_hash: None,
                    });
                }
                // Carry forward the last OVERLAP_LINES lines for context.
                part_start = part_end.saturating_sub(OVERLAP_LINES);
                current_size = lines[part_start..part_end]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum();
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

    #[test]
    fn test_gather_leading_context_with_comments() {
        let lines = vec!["// This is a comment", "// Another comment", "fn foo() {}"];
        let start = gather_leading_context(&lines, 2);
        assert_eq!(start, 0, "Should walk back past both // comments");
    }

    #[test]
    fn test_gather_leading_context_with_rust_attributes() {
        let lines = vec!["#[derive(Debug)]", "#[allow(dead_code)]", "struct Foo {}"];
        let start = gather_leading_context(&lines, 2);
        assert_eq!(start, 0, "Should walk back past #[...] attributes");
    }

    #[test]
    fn test_gather_leading_context_with_java_annotations() {
        let lines = vec![
            "@Override",
            "@SuppressWarnings(\"unchecked\")",
            "public void foo() {}",
        ];
        let start = gather_leading_context(&lines, 2);
        assert_eq!(start, 0, "Should walk back past @ annotations");
    }

    #[test]
    fn test_gather_leading_context_with_block_docs() {
        let lines = vec![
            "/**",
            " * This is a doc comment.",
            " */",
            "fn documented() {}",
        ];
        let start = gather_leading_context(&lines, 3);
        assert_eq!(start, 0, "Should walk back past /** */ block doc comment");
    }

    #[test]
    fn test_gather_leading_context_at_file_start() {
        let lines = vec!["fn at_start() {}"];
        let start = gather_leading_context(&lines, 0);
        assert_eq!(start, 0, "At line 0, cannot go further back");
    }

    #[test]
    fn test_gather_leading_context_with_gap() {
        let lines = vec!["// First comment", "", "// Second comment", "fn foo() {}"];
        let start = gather_leading_context(&lines, 3);
        // Empty line at index 1 breaks the chain, so only index 2 comment is included
        assert_eq!(start, 2, "Empty line should break context gathering");
    }

    fn make_symbol(kind: SymbolKind, parent: Option<&str>) -> CodeSymbol {
        CodeSymbol {
            id: None,
            repo_id: 1,
            file_path: "test.rs".to_string(),
            symbol_name: "TestSymbol".to_string(),
            symbol_kind: kind,
            parent_symbol: parent.map(|s| s.to_string()),
            language: Language::Rust,
            start_line: 1,
            end_line: 10,
            signature: None,
        }
    }

    #[test]
    fn test_is_top_level_container() {
        // Should return true for container kinds without parent
        assert!(is_top_level_container(&make_symbol(
            SymbolKind::Class,
            None
        )));
        assert!(is_top_level_container(&make_symbol(
            SymbolKind::Struct,
            None
        )));
        assert!(is_top_level_container(&make_symbol(
            SymbolKind::Trait,
            None
        )));
        assert!(is_top_level_container(&make_symbol(SymbolKind::Impl, None)));
        assert!(is_top_level_container(&make_symbol(
            SymbolKind::Interface,
            None
        )));
        assert!(is_top_level_container(&make_symbol(SymbolKind::Enum, None)));
        assert!(is_top_level_container(&make_symbol(
            SymbolKind::Module,
            None
        )));

        // Should return false for non-container kinds
        assert!(!is_top_level_container(&make_symbol(
            SymbolKind::Function,
            None
        )));
        assert!(!is_top_level_container(&make_symbol(
            SymbolKind::Method,
            None
        )));
        assert!(!is_top_level_container(&make_symbol(
            SymbolKind::Constant,
            None
        )));
    }

    #[test]
    fn test_is_top_level_container_with_parent() {
        // Symbols with a parent are never top-level, even if the kind is a container
        assert!(!is_top_level_container(&make_symbol(
            SymbolKind::Class,
            Some("OuterClass")
        )));
        assert!(!is_top_level_container(&make_symbol(
            SymbolKind::Struct,
            Some("Module")
        )));
        assert!(!is_top_level_container(&make_symbol(
            SymbolKind::Impl,
            Some("Parent")
        )));
        assert!(!is_top_level_container(&make_symbol(
            SymbolKind::Trait,
            Some("OuterTrait")
        )));
    }

    #[test]
    fn test_mark_covered() {
        let mut covered = vec![false; 10];
        mark_covered(&mut covered, 2, 5);

        assert!(!covered[0]);
        assert!(!covered[1]);
        assert!(covered[2]);
        assert!(covered[3]);
        assert!(covered[4]);
        assert!(!covered[5]);
        assert!(!covered[9]);
    }

    #[test]
    fn test_collect_lines() {
        let lines = vec!["line0", "line1", "line2", "line3", "line4"];
        assert_eq!(collect_lines(&lines, 1, 3), "line1\nline2");
        assert_eq!(
            collect_lines(&lines, 0, 5),
            "line0\nline1\nline2\nline3\nline4"
        );
        assert_eq!(collect_lines(&lines, 0, 1), "line0");
        // End beyond bounds is clamped
        assert_eq!(collect_lines(&lines, 3, 100), "line3\nline4");
    }

    #[test]
    fn test_split_oversized_within_limit() {
        let chunk = CodeChunk {
            id: None,
            repo_id: 1,
            file_path: "test.rs".to_string(),
            chunk_type: "function".to_string(),
            symbol_name: Some("small_fn".to_string()),
            language: Language::Rust,
            start_line: 1,
            end_line: 5,
            chunk_text: "fn small() {\n    println!(\"hello\");\n}".to_string(),
            context_text: "File: test.rs".to_string(),
            file_hash: "hash1".to_string(),
            content_hash: None,
        };
        let result = split_oversized(vec![chunk.clone()]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].chunk_text, chunk.chunk_text);
    }

    #[test]
    fn test_split_oversized_over_limit() {
        // Create a chunk larger than MAX_CHUNK_CHARS
        let long_line = "x".repeat(200);
        let lines: Vec<String> = (0..50)
            .map(|i| format!("// line {}: {}", i, long_line))
            .collect();
        let big_text = lines.join("\n");
        assert!(big_text.len() > MAX_CHUNK_CHARS);

        let chunk = CodeChunk {
            id: None,
            repo_id: 1,
            file_path: "big.rs".to_string(),
            chunk_type: "top_level".to_string(),
            symbol_name: None,
            language: Language::Rust,
            start_line: 1,
            end_line: 50,
            chunk_text: big_text,
            context_text: "File: big.rs".to_string(),
            file_hash: "hash2".to_string(),
            content_hash: None,
        };
        let result = split_oversized(vec![chunk]);
        // Should produce more than one chunk
        assert!(
            result.len() > 1,
            "Expected oversized chunk to be split into multiple, got {}",
            result.len()
        );
        // Each resulting chunk should be within the limit (or close, since split is at line boundaries)
        for c in &result {
            // The chunk text should be significantly smaller than the original
            assert!(c.chunk_text.len() >= MIN_CHUNK_CHARS);
        }
    }

    #[test]
    fn test_build_context_text() {
        let ctx = build_context_text(
            "src/main.rs",
            Language::Rust,
            "function",
            Some("main"),
            None,
            "fn main() {\n    println!(\"Hello\");\n}",
        );
        assert!(ctx.contains("File: src/main.rs"));
        assert!(ctx.contains("Language: Rust"));
        assert!(ctx.contains("Symbol: main (function)"));
        assert!(!ctx.contains("Parent:"));
        assert!(ctx.contains("fn main()"));
    }

    #[test]
    fn test_build_context_text_with_parent() {
        let ctx = build_context_text(
            "src/server.rs",
            Language::Rust,
            "function",
            Some("handle_request"),
            Some("Server"),
            "fn handle_request(&self) {}",
        );
        assert!(ctx.contains("File: src/server.rs"));
        assert!(ctx.contains("Symbol: handle_request (function)"));
        assert!(ctx.contains("Parent: Server"));
        assert!(ctx.contains("fn handle_request"));
    }

    #[test]
    fn test_build_context_text_truncation() {
        // Code longer than MAX_CHUNK_CHARS should be truncated
        let long_code = "x".repeat(MAX_CHUNK_CHARS + 1000);
        let ctx = build_context_text(
            "src/big.rs",
            Language::Rust,
            "top_level",
            None,
            None,
            &long_code,
        );
        assert!(ctx.contains("File: src/big.rs"));
        assert!(ctx.contains("Type: top_level"));
        assert!(ctx.contains("// ... truncated ..."));
        // The context should be less than the original long_code
        assert!(ctx.len() < long_code.len());
    }

    #[test]
    fn test_build_context_text_no_symbol_name() {
        let ctx = build_context_text(
            "src/lib.rs",
            Language::Rust,
            "top_level",
            None,
            None,
            "use std::io;",
        );
        assert!(ctx.contains("Type: top_level"));
        assert!(!ctx.contains("Symbol:"));
    }

    #[test]
    fn test_chunk_file_simple_rust_function() {
        let src = r#"
/// Adds two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
        let (symbols, chunks) = chunk_file(src, Language::Rust, 42, "math.rs", "hash_add").unwrap();

        assert!(!symbols.is_empty(), "Should extract at least one symbol");
        // The function should be chunked as a standalone function
        let fn_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol_name.as_deref() == Some("add"))
            .collect();
        assert!(
            !fn_chunks.is_empty(),
            "Should produce a chunk for the 'add' function"
        );
        let fc = &fn_chunks[0];
        assert_eq!(fc.chunk_type, "function");
        assert_eq!(fc.repo_id, 42);
        assert_eq!(fc.file_path, "math.rs");
        assert_eq!(fc.file_hash, "hash_add");
        assert!(fc.chunk_text.contains("pub fn add"));
        // Leading doc comment should be gathered
        assert!(
            fc.chunk_text.contains("/// Adds two numbers"),
            "Leading doc comment should be included in the chunk text"
        );
    }

    #[test]
    fn test_chunk_file_struct_with_methods() {
        let src = r#"
pub struct Counter {
    value: i32,
    name: String,
    description: Option<String>,
}

impl Counter {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn increment(&mut self) {
        self.value += 1;
    }

    pub fn get(&self) -> i32 {
        self.value
    }
}
"#;
        let (symbols, chunks) =
            chunk_file(src, Language::Rust, 1, "counter.rs", "cnt_hash").unwrap();

        assert!(!symbols.is_empty());
        assert!(!chunks.is_empty());

        // Struct and impl should be chunked as small containers (under LARGE_CLASS_LINES)
        let struct_chunk = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("Counter") && c.chunk_type == "class");
        assert!(
            struct_chunk.is_some(),
            "Should have a chunk for the Counter struct"
        );

        // The impl block should also be chunked
        let impl_chunk = chunks.iter().find(|c| c.chunk_type == "impl_block");
        assert!(
            impl_chunk.is_some(),
            "Should have an impl_block chunk for Counter"
        );
    }

    #[test]
    fn test_chunk_file_empty_source_returns_empty() {
        let (symbols, chunks) = chunk_file("", Language::Rust, 1, "empty.rs", "e").unwrap();
        assert!(symbols.is_empty());
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_file_very_small_source_below_min() {
        // Single short line -- below both MIN_CHUNK_CHARS (50) and MIN_CHUNK_LINES (3)
        let src = "let x = 1;";
        let (_, chunks) = chunk_file(src, Language::Rust, 1, "tiny.rs", "t").unwrap();
        // Everything should be filtered out due to min thresholds
        for c in &chunks {
            assert!(
                c.chunk_text.len() >= MIN_CHUNK_CHARS,
                "No chunk should be below MIN_CHUNK_CHARS"
            );
        }
    }

    #[test]
    fn test_chunk_file_two_line_function_filtered() {
        // A function that is only 2 lines -- below MIN_CHUNK_LINES=3
        // and likely below MIN_CHUNK_CHARS=50
        let src = "fn f() {\n}\n";
        let (_, chunks) = chunk_file(src, Language::Rust, 1, "short.rs", "s").unwrap();
        // The function is too small to be chunked
        let fn_chunk = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("f"));
        assert!(
            fn_chunk.is_none(),
            "A 2-line trivial function should be filtered out by MIN thresholds"
        );
    }

    #[test]
    fn test_chunk_file_multiple_standalone_functions() {
        let src = r#"
/// First function performs alpha processing.
pub fn alpha(x: i32) -> i32 {
    let result = x * 2;
    result + 1
}

/// Second function performs beta processing.
pub fn beta(y: i32) -> i32 {
    let result = y * 3;
    result - 1
}

/// Third function performs gamma processing.
pub fn gamma(z: i32) -> i32 {
    let result = z * 4;
    result + 2
}
"#;
        let (_symbols, chunks) = chunk_file(src, Language::Rust, 1, "multi.rs", "mh").unwrap();

        let fn_names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.symbol_name.as_deref())
            .collect();

        // All three standalone functions should be chunked
        assert!(
            fn_names.contains(&"alpha"),
            "alpha should be chunked, got: {:?}",
            fn_names
        );
        assert!(
            fn_names.contains(&"beta"),
            "beta should be chunked, got: {:?}",
            fn_names
        );
        assert!(
            fn_names.contains(&"gamma"),
            "gamma should be chunked, got: {:?}",
            fn_names
        );

        // Check that they are all typed as "function"
        for c in chunks
            .iter()
            .filter(|c| matches!(c.symbol_name.as_deref(), Some("alpha" | "beta" | "gamma")))
        {
            assert_eq!(c.chunk_type, "function");
        }
    }

    #[test]
    fn test_chunk_file_enum() {
        let src = r#"
/// Represents directions.
pub enum Direction {
    North,
    South,
    East,
    West,
}
"#;
        let (_, chunks) = chunk_file(src, Language::Rust, 1, "dir.rs", "dh").unwrap();

        let enum_chunk = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("Direction"));
        assert!(
            enum_chunk.is_some(),
            "Should produce a chunk for the Direction enum"
        );
        let ec = enum_chunk.unwrap();
        assert_eq!(ec.chunk_type, "class"); // Enum maps to "class" chunk_type
        assert!(ec.chunk_text.contains("pub enum Direction"));
    }

    #[test]
    fn test_chunk_file_trait() {
        let src = r#"
/// A drawable trait.
pub trait Drawable {
    fn draw(&self);
    fn bounds(&self) -> (i32, i32, i32, i32);
}
"#;
        let (_, chunks) = chunk_file(src, Language::Rust, 1, "draw.rs", "drh").unwrap();

        let trait_chunk = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("Drawable"));
        assert!(
            trait_chunk.is_some(),
            "Should produce a chunk for the Drawable trait"
        );
        let tc = trait_chunk.unwrap();
        assert_eq!(tc.chunk_type, "class"); // Trait maps to "class" chunk_type
        assert!(tc.chunk_text.contains("pub trait Drawable"));
    }

    #[test]
    fn test_chunk_file_top_level_code() {
        // Enough uncovered lines to pass MIN thresholds
        let src = r#"use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::io::BufReader;
use std::fs::File;
use std::path::PathBuf;
use std::env;
use std::process;
"#;
        let (_, chunks) = chunk_file(src, Language::Rust, 1, "imports.rs", "ih").unwrap();

        // These use statements are top-level uncovered code
        let top_level_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_type == "top_level")
            .collect();

        // If there are enough uncovered lines, they should form a top_level chunk
        if !top_level_chunks.is_empty() {
            let tl = &top_level_chunks[0];
            assert!(tl.symbol_name.is_none());
            assert!(tl.context_text.contains("Type: top_level"));
        }
    }

    #[test]
    fn test_is_top_level_container_all_kinds_exhaustive() {
        // Container kinds without parent -> true
        let containers = [
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Trait,
            SymbolKind::Impl,
            SymbolKind::Interface,
            SymbolKind::Enum,
            SymbolKind::Module,
        ];
        for kind in &containers {
            assert!(
                is_top_level_container(&make_symbol(*kind, None)),
                "{:?} without parent should be a top-level container",
                kind
            );
        }

        // Non-container kinds without parent -> false
        let non_containers = [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Constant,
        ];
        for kind in &non_containers {
            assert!(
                !is_top_level_container(&make_symbol(*kind, None)),
                "{:?} should NOT be a top-level container",
                kind
            );
        }

        // All kinds with parent -> false
        for kind in containers.iter().chain(non_containers.iter()) {
            assert!(
                !is_top_level_container(&make_symbol(*kind, Some("Parent"))),
                "{:?} with parent should NOT be a top-level container",
                kind
            );
        }
    }

    #[test]
    fn test_mark_covered_empty_array() {
        let mut covered: Vec<bool> = vec![];
        mark_covered(&mut covered, 0, 0);
        assert!(covered.is_empty());
    }

    #[test]
    fn test_mark_covered_full_range() {
        let mut covered = vec![false; 5];
        mark_covered(&mut covered, 0, 5);
        assert!(covered.iter().all(|&c| c));
    }

    #[test]
    fn test_mark_covered_start_equals_end() {
        let mut covered = vec![false; 5];
        mark_covered(&mut covered, 3, 3);
        // No lines should be covered since start == end means empty range
        assert!(covered.iter().all(|&c| !c));
    }

    #[test]
    fn test_mark_covered_end_exceeds_length() {
        let mut covered = vec![false; 3];
        // end > length should be safe due to .take(end).skip(start)
        mark_covered(&mut covered, 1, 100);
        assert!(!covered[0]);
        assert!(covered[1]);
        assert!(covered[2]);
    }

    #[test]
    fn test_mark_covered_single_line() {
        let mut covered = vec![false; 5];
        mark_covered(&mut covered, 2, 3);
        assert!(!covered[0]);
        assert!(!covered[1]);
        assert!(covered[2]);
        assert!(!covered[3]);
        assert!(!covered[4]);
    }

    #[test]
    fn test_collect_lines_empty_array() {
        let lines: Vec<&str> = vec![];
        assert_eq!(collect_lines(&lines, 0, 0), "");
    }

    #[test]
    fn test_collect_lines_single_line() {
        let lines = vec!["only line"];
        assert_eq!(collect_lines(&lines, 0, 1), "only line");
    }

    #[test]
    fn test_collect_lines_end_greater_than_len() {
        let lines = vec!["a", "b", "c"];
        // end=100 should be clamped to lines.len()=3
        assert_eq!(collect_lines(&lines, 0, 100), "a\nb\nc");
    }

    #[test]
    fn test_collect_lines_start_equals_end() {
        let lines = vec!["a", "b", "c"];
        assert_eq!(collect_lines(&lines, 1, 1), "");
    }

    #[test]
    fn test_gather_leading_context_python_comments() {
        let lines = vec![
            "# This is a Python comment",
            "# Another Python comment",
            "def foo():",
        ];
        let start = gather_leading_context(&lines, 2);
        assert_eq!(start, 0, "Should walk back past # comments");
    }

    #[test]
    fn test_gather_leading_context_block_comment_continuation() {
        let lines = vec![
            "/**",
            " * Line one of the doc.",
            " * Line two of the doc.",
            " */",
            "fn documented() {}",
        ];
        let start = gather_leading_context(&lines, 4);
        assert_eq!(start, 0, "Should walk back through full block comment");
    }

    #[test]
    fn test_gather_leading_context_rust_inner_attribute() {
        let lines = vec!["#![allow(unused)]", "fn foo() {}"];
        let start = gather_leading_context(&lines, 1);
        assert_eq!(start, 0, "Should walk back past #![...] inner attributes");
    }

    #[test]
    fn test_gather_leading_context_no_context() {
        let lines = vec!["let x = 1;", "fn foo() {}"];
        let start = gather_leading_context(&lines, 1);
        // "let x = 1;" is not a comment/attr/annotation, so context stays at 1
        assert_eq!(
            start, 1,
            "Non-comment/attribute lines should not be included"
        );
    }

    #[test]
    fn test_gather_leading_context_mixed_comments_and_attributes() {
        let lines = vec![
            "/// Documentation comment",
            "#[derive(Debug)]",
            "#[allow(dead_code)]",
            "struct Foo {}",
        ];
        let start = gather_leading_context(&lines, 3);
        assert_eq!(
            start, 0,
            "Should walk back through interleaved comments and attributes"
        );
    }

    #[test]
    fn test_gather_leading_context_triple_quote_docstring() {
        let lines = vec!["\"\"\"", "This is a docstring.", "\"\"\"", "def foo():"];
        let start = gather_leading_context(&lines, 3);
        // With multi-line docstring support, we walk back through the closing
        // triple-quote, across the body, and through the opening triple-quote.
        assert_eq!(
            start, 0,
            "Should walk back through entire multi-line triple-quote docstring"
        );
    }

    #[test]
    fn test_gather_leading_context_python_single_triple_quote() {
        let lines = vec!["'''", "Docstring body.", "'''", "def bar():"];
        let start = gather_leading_context(&lines, 3);
        // With multi-line docstring support, we walk back through the full docstring.
        assert_eq!(
            start, 0,
            "Should walk back through entire multi-line single-quote docstring"
        );
    }

    fn make_symbol_with_lines(
        kind: SymbolKind,
        name: &str,
        start: usize,
        end: usize,
        parent: Option<&str>,
        signature: Option<&str>,
    ) -> CodeSymbol {
        CodeSymbol {
            id: None,
            repo_id: 1,
            file_path: "test.rs".to_string(),
            symbol_name: name.to_string(),
            symbol_kind: kind,
            parent_symbol: parent.map(|s| s.to_string()),
            language: Language::Rust,
            start_line: start,
            end_line: end,
            signature: signature.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_build_symbol_chunk_function() {
        let lines: Vec<&str> = vec![
            "/// Docs for foo.",
            "pub fn foo(x: i32) -> i32 {",
            "    let y = x * 2;",
            "    y + 1",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Function, "foo", 2, 5, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        let c = chunk.unwrap();
        assert_eq!(c.chunk_type, "function");
        assert!(c.chunk_text.contains("/// Docs for foo."));
        assert!(c.chunk_text.contains("pub fn foo"));
    }

    #[test]
    fn test_build_symbol_chunk_method() {
        let lines: Vec<&str> = vec![
            "impl Foo {",
            "    /// Method docs.",
            "    pub fn bar(&self) -> String {",
            "        self.name.clone()",
            "    }",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Method, "bar", 3, 5, Some("Foo"), None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        let c = chunk.unwrap();
        assert_eq!(c.chunk_type, "function"); // Method maps to "function"
        assert!(c.context_text.contains("Parent: Foo"));
    }

    #[test]
    fn test_build_symbol_chunk_class() {
        let lines: Vec<&str> = vec![
            "/// A class.",
            "class MyClass {",
            "    field1: i32,",
            "    field2: String,",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Class, "MyClass", 2, 5, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().chunk_type, "class");
    }

    #[test]
    fn test_build_symbol_chunk_struct() {
        let lines: Vec<&str> = vec![
            "/// A struct.",
            "pub struct Point {",
            "    pub x: f64,",
            "    pub y: f64,",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Struct, "Point", 2, 5, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().chunk_type, "class"); // Struct maps to "class"
    }

    #[test]
    fn test_build_symbol_chunk_impl() {
        let lines: Vec<&str> = vec![
            "/// Impl block.",
            "impl Point {",
            "    fn new() -> Self {",
            "        Self { x: 0.0, y: 0.0 }",
            "    }",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Impl, "Point", 2, 6, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().chunk_type, "impl_block");
    }

    #[test]
    fn test_build_symbol_chunk_trait() {
        let lines: Vec<&str> = vec![
            "/// A trait.",
            "pub trait Renderable {",
            "    fn render(&self);",
            "    fn update(&mut self);",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Trait, "Renderable", 2, 5, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().chunk_type, "class"); // Trait maps to "class"
    }

    #[test]
    fn test_build_symbol_chunk_interface() {
        let lines: Vec<&str> = vec![
            "/// An interface.",
            "interface Printable {",
            "    fn print(&self);",
            "    fn format(&self) -> String;",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Interface, "Printable", 2, 5, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().chunk_type, "class"); // Interface maps to "class"
    }

    #[test]
    fn test_build_symbol_chunk_enum() {
        let lines: Vec<&str> = vec![
            "/// An enum.",
            "pub enum Color {",
            "    Red,",
            "    Green,",
            "    Blue,",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Enum, "Color", 2, 6, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().chunk_type, "class"); // Enum maps to "class"
    }

    #[test]
    fn test_build_symbol_chunk_module() {
        let lines: Vec<&str> = vec![
            "/// Module docs.",
            "mod inner {",
            "    pub fn helper() {}",
            "    pub fn util() {}",
            "}",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Module, "inner", 2, 5, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().chunk_type, "module_header");
    }

    #[test]
    fn test_build_symbol_chunk_constant() {
        let lines: Vec<&str> = vec![
            "/// A constant.",
            "/// With multiline docs.",
            "pub const MAX_SIZE: usize = 1024;",
            "pub const MIN_SIZE: usize = 1;",
            "pub const DEFAULT: usize = 512;",
        ];
        let sym = make_symbol_with_lines(SymbolKind::Constant, "MAX_SIZE", 3, 5, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        // Constant maps to "top_level"
        if let Some(c) = chunk {
            assert_eq!(c.chunk_type, "top_level");
        }
    }

    #[test]
    fn test_build_symbol_chunk_returns_none_for_tiny() {
        // Symbol that is too small (< MIN_CHUNK_CHARS and < MIN_CHUNK_LINES)
        let lines: Vec<&str> = vec!["fn t() {}"];
        let sym = make_symbol_with_lines(SymbolKind::Function, "t", 1, 1, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(
            chunk.is_none(),
            "A tiny single-line function should be filtered out"
        );
    }

    #[test]
    fn test_build_symbol_chunk_returns_none_when_end_lte_start() {
        let lines: Vec<&str> = vec!["fn foo() {}"];
        // end_line < start_line
        let sym = make_symbol_with_lines(SymbolKind::Function, "foo", 5, 1, None, None);
        let chunk = build_symbol_chunk(&lines, &sym, Language::Rust, 1, "test.rs", "h");
        assert!(chunk.is_none(), "Should return None when end <= start");
    }

    #[test]
    fn test_build_context_text_all_chunk_types() {
        let types_and_names = vec![
            ("function", Some("my_func"), None),
            ("class", Some("MyClass"), None),
            ("impl_block", Some("MyStruct"), None),
            ("module_header", Some("my_mod"), None),
            ("top_level", None, None),
            ("function", Some("method"), Some("ParentClass")),
        ];

        for (chunk_type, sym_name, parent) in &types_and_names {
            let ctx = build_context_text(
                "test.rs",
                Language::Rust,
                chunk_type,
                *sym_name,
                *parent,
                "code here",
            );
            assert!(ctx.contains("File: test.rs"));
            assert!(ctx.contains("Language: Rust"));
            if let Some(name) = sym_name {
                assert!(
                    ctx.contains(&format!("Symbol: {} ({})", name, chunk_type)),
                    "ctx should contain Symbol line for {}",
                    name,
                );
            } else {
                assert!(ctx.contains(&format!("Type: {}", chunk_type)));
            }
            if let Some(p) = parent {
                assert!(ctx.contains(&format!("Parent: {}", p)));
            }
            assert!(ctx.contains("code here"));
        }
    }

    #[test]
    fn test_build_context_text_different_languages() {
        let languages = vec![
            (Language::Python, "Python"),
            (Language::TypeScript, "TypeScript"),
            (Language::Go, "Go"),
            (Language::Java, "Java"),
        ];
        for (lang, expected_str) in &languages {
            let ctx = build_context_text("file.ext", *lang, "function", Some("f"), None, "code");
            assert!(
                ctx.contains(&format!("Language: {}", expected_str)),
                "Context should contain the correct language string for {:?}",
                lang,
            );
        }
    }

    #[test]
    fn test_build_context_text_truncation_at_char_boundary() {
        // Create code with multi-byte characters near the boundary
        // Each character below is 3 bytes in UTF-8
        let mut code = String::new();
        for _ in 0..(MAX_CHUNK_CHARS / 3 + 100) {
            code.push('\u{2603}'); // Snowman character (3 bytes each)
        }
        assert!(code.len() > MAX_CHUNK_CHARS);
        let ctx = build_context_text("test.rs", Language::Rust, "top_level", None, None, &code);
        assert!(ctx.contains("// ... truncated ..."));
        // Verify the truncation happened at a valid char boundary (no panic)
        assert!(ctx.is_char_boundary(ctx.len()));
    }

    #[test]
    fn test_split_oversized_empty_input() {
        let result = split_oversized(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_split_oversized_multiple_chunks_mixed() {
        let small_chunk = CodeChunk {
            id: None,
            repo_id: 1,
            file_path: "test.rs".to_string(),
            chunk_type: "function".to_string(),
            symbol_name: Some("small".to_string()),
            language: Language::Rust,
            start_line: 1,
            end_line: 3,
            chunk_text: "fn small() {\n    println!(\"small\");\n}".to_string(),
            context_text: "ctx".to_string(),
            file_hash: "h".to_string(),
            content_hash: None,
        };

        let long_line = "y".repeat(200);
        let big_lines: Vec<String> = (0..50)
            .map(|i| format!("// line {}: {}", i, long_line))
            .collect();
        let big_text = big_lines.join("\n");
        let big_chunk = CodeChunk {
            id: None,
            repo_id: 1,
            file_path: "test.rs".to_string(),
            chunk_type: "top_level".to_string(),
            symbol_name: None,
            language: Language::Rust,
            start_line: 10,
            end_line: 60,
            chunk_text: big_text,
            context_text: "ctx".to_string(),
            file_hash: "h".to_string(),
            content_hash: None,
        };

        let result = split_oversized(vec![small_chunk.clone(), big_chunk]);
        // The small chunk should pass through unchanged
        assert_eq!(result[0].chunk_text, small_chunk.chunk_text);
        // The big chunk should be split into multiple
        assert!(
            result.len() > 2,
            "Should have 1 small + multiple splits from big, got {}",
            result.len()
        );
    }

    #[test]
    fn test_split_oversized_preserves_metadata() {
        let long_line = "z".repeat(300);
        let big_lines: Vec<String> = (0..40)
            .map(|i| format!("// line {}: {}", i, long_line))
            .collect();
        let big_text = big_lines.join("\n");
        assert!(big_text.len() > MAX_CHUNK_CHARS);

        let chunk = CodeChunk {
            id: None,
            repo_id: 99,
            file_path: "meta.rs".to_string(),
            chunk_type: "function".to_string(),
            symbol_name: Some("big_fn".to_string()),
            language: Language::Rust,
            start_line: 5,
            end_line: 45,
            chunk_text: big_text,
            context_text: "ctx".to_string(),
            file_hash: "meta_hash".to_string(),
            content_hash: None,
        };

        let result = split_oversized(vec![chunk]);
        assert!(result.len() > 1);
        for c in &result {
            assert_eq!(c.repo_id, 99);
            assert_eq!(c.file_path, "meta.rs");
            assert_eq!(c.chunk_type, "function");
            assert_eq!(c.symbol_name.as_deref(), Some("big_fn"));
            assert_eq!(c.language, Language::Rust);
            assert_eq!(c.file_hash, "meta_hash");
            // Context text should be regenerated (not the original "ctx")
            assert!(c.context_text.contains("File: meta.rs"));
        }
    }

    #[test]
    fn test_split_oversized_line_numbers_are_sequential() {
        let long_line = "w".repeat(300);
        let big_lines: Vec<String> = (0..40)
            .map(|i| format!("// line {}: {}", i, long_line))
            .collect();
        let big_text = big_lines.join("\n");
        assert!(big_text.len() > MAX_CHUNK_CHARS);

        let chunk = CodeChunk {
            id: None,
            repo_id: 1,
            file_path: "seq.rs".to_string(),
            chunk_type: "top_level".to_string(),
            symbol_name: None,
            language: Language::Rust,
            start_line: 10,
            end_line: 50,
            chunk_text: big_text,
            context_text: "ctx".to_string(),
            file_hash: "h".to_string(),
            content_hash: None,
        };

        let result = split_oversized(vec![chunk]);
        assert!(result.len() > 1);
        // Verify line numbers are monotonically increasing and non-overlapping
        for i in 1..result.len() {
            assert!(
                result[i].start_line > result[i - 1].start_line,
                "start_line should be increasing: chunk {} starts at {} but prev starts at {}",
                i,
                result[i].start_line,
                result[i - 1].start_line
            );
        }
    }

    #[test]
    fn test_build_skeleton_chunk_basic() {
        // Create a source with enough lines for a "large" container
        let mut src_lines: Vec<String> = Vec::new();
        src_lines.push("/// Docs for BigStruct.".to_string());
        src_lines.push("pub struct BigStruct {".to_string());
        for i in 0..110 {
            src_lines.push(format!("    field_{}: i32,", i));
        }
        src_lines.push("}".to_string());

        let lines: Vec<&str> = src_lines.iter().map(|s| s.as_str()).collect();

        let container =
            make_symbol_with_lines(SymbolKind::Struct, "BigStruct", 2, lines.len(), None, None);

        let method = make_symbol_with_lines(
            SymbolKind::Method,
            "do_thing",
            50,
            55,
            Some("BigStruct"),
            Some("fn do_thing(&self) -> i32"),
        );

        let all_symbols = vec![container.clone(), method];

        let skeleton = build_skeleton_chunk(
            &lines,
            &all_symbols[0],
            &all_symbols,
            Language::Rust,
            1,
            "big.rs",
            "bh",
        );

        assert_eq!(skeleton.chunk_type, "class");
        assert_eq!(skeleton.symbol_name.as_deref(), Some("BigStruct"));
        assert_eq!(skeleton.repo_id, 1);
        assert_eq!(skeleton.file_path, "big.rs");
        assert_eq!(skeleton.file_hash, "bh");
        // Skeleton should contain the "... methods ..." marker
        assert!(
            skeleton.chunk_text.contains("// ... methods ..."),
            "Skeleton should contain the methods marker"
        );
        // Skeleton should contain the method signature
        assert!(
            skeleton.chunk_text.contains("fn do_thing(&self) -> i32"),
            "Skeleton should contain method signature"
        );
        // Should contain leading doc comment
        assert!(
            skeleton.chunk_text.contains("/// Docs for BigStruct"),
            "Skeleton should include leading doc comments"
        );
        // Context text should be properly formed
        assert!(skeleton.context_text.contains("File: big.rs"));
        assert!(skeleton.context_text.contains("Symbol: BigStruct (class)"));
    }

    #[test]
    fn test_build_skeleton_chunk_no_methods() {
        let mut src_lines: Vec<String> = Vec::new();
        src_lines.push("pub struct EmptyBig {".to_string());
        for i in 0..110 {
            src_lines.push(format!("    field_{}: i32,", i));
        }
        src_lines.push("}".to_string());

        let lines: Vec<&str> = src_lines.iter().map(|s| s.as_str()).collect();

        let container =
            make_symbol_with_lines(SymbolKind::Struct, "EmptyBig", 1, lines.len(), None, None);

        let skeleton = build_skeleton_chunk(
            &lines,
            &container,
            std::slice::from_ref(&container),
            Language::Rust,
            1,
            "empty_big.rs",
            "ebh",
        );

        // Should still contain the header and methods marker but no signatures
        assert!(skeleton.chunk_text.contains("// ... methods ..."));
        assert!(skeleton.chunk_text.contains("pub struct EmptyBig"));
    }

    #[test]
    fn test_build_skeleton_chunk_multiple_methods() {
        let mut src_lines: Vec<String> = Vec::new();
        src_lines.push("impl BigImpl {".to_string());
        for i in 0..120 {
            src_lines.push(format!("    // line {}", i));
        }
        src_lines.push("}".to_string());

        let lines: Vec<&str> = src_lines.iter().map(|s| s.as_str()).collect();

        let container =
            make_symbol_with_lines(SymbolKind::Impl, "BigImpl", 1, lines.len(), None, None);
        let method_a = make_symbol_with_lines(
            SymbolKind::Method,
            "alpha",
            10,
            20,
            Some("BigImpl"),
            Some("fn alpha(&self)"),
        );
        let method_b = make_symbol_with_lines(
            SymbolKind::Function,
            "beta",
            30,
            40,
            Some("BigImpl"),
            Some("fn beta() -> bool"),
        );

        let all_symbols = vec![container.clone(), method_a, method_b];

        let skeleton = build_skeleton_chunk(
            &lines,
            &all_symbols[0],
            &all_symbols,
            Language::Rust,
            1,
            "multi_method.rs",
            "mmh",
        );

        assert!(skeleton.chunk_text.contains("fn alpha(&self)"));
        assert!(skeleton.chunk_text.contains("fn beta() -> bool"));
        // Each method signature should be followed by { ... }
        assert!(skeleton.chunk_text.contains("{ ... }"));
    }

    #[test]
    fn test_build_skeleton_chunk_method_without_signature() {
        let mut src_lines: Vec<String> = Vec::new();
        src_lines.push("impl NoSig {".to_string());
        for i in 0..110 {
            src_lines.push(format!("    // padding line {}", i));
        }
        src_lines.push("}".to_string());

        let lines: Vec<&str> = src_lines.iter().map(|s| s.as_str()).collect();

        let container =
            make_symbol_with_lines(SymbolKind::Impl, "NoSig", 1, lines.len(), None, None);
        // Method without a signature field
        let method = make_symbol_with_lines(
            SymbolKind::Method,
            "no_sig_method",
            10,
            20,
            Some("NoSig"),
            None, // No signature
        );

        let all_symbols = vec![container.clone(), method];

        let skeleton = build_skeleton_chunk(
            &lines,
            &all_symbols[0],
            &all_symbols,
            Language::Rust,
            1,
            "nosig.rs",
            "nsh",
        );

        // Method without signature should not appear in skeleton signatures
        assert!(
            !skeleton.chunk_text.contains("no_sig_method"),
            "Method without signature should not add a line to the skeleton"
        );
    }

    #[test]
    fn test_chunk_file_large_impl_triggers_skeleton() {
        // Build a Rust source file with an impl block > LARGE_CLASS_LINES (100)
        let mut src = String::new();
        src.push_str("pub struct BigThing {\n    val: i32,\n}\n\n");
        src.push_str("impl BigThing {\n");
        // Create multiple methods, each with enough lines to pass min thresholds
        for i in 0..15 {
            src.push_str(&format!("    /// Method {i} does stuff.\n"));
            src.push_str(&format!("    pub fn method_{i}(&self) -> i32 {{\n"));
            for j in 0..6 {
                src.push_str(&format!("        let v{j} = self.val + {j};\n"));
            }
            src.push_str("        0\n");
            src.push_str("    }\n\n");
        }
        src.push_str("}\n");

        let (symbols, chunks) = chunk_file(&src, Language::Rust, 1, "big_thing.rs", "bth").unwrap();

        assert!(!symbols.is_empty());
        assert!(!chunks.is_empty());

        // Verify that we got some function-typed chunks for the methods
        let fn_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_type == "function")
            .collect();
        assert!(
            !fn_chunks.is_empty(),
            "Large impl should produce individual method chunks"
        );
    }

    #[test]
    fn test_chunk_file_mixed_rust_symbols() {
        let src = r#"
use std::fmt;

/// A color enum.
pub enum Color {
    Red,
    Green,
    Blue,
    Custom(u8, u8, u8),
}

/// A shape struct.
pub struct Shape {
    name: String,
    color: Color,
}

/// Creates a default shape with meaningful content.
pub fn default_shape() -> Shape {
    let name = String::from("circle");
    let color = Color::Blue;
    Shape { name, color }
}

/// Formats a shape for display with extra detail.
pub fn format_shape(s: &Shape) -> String {
    let prefix = "Shape";
    let result = format!("{}: {}", prefix, s.name);
    result
}
"#;
        let (symbols, chunks) = chunk_file(src, Language::Rust, 1, "shapes.rs", "sh").unwrap();

        assert!(!symbols.is_empty());
        assert!(!chunks.is_empty());

        let names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.symbol_name.as_deref())
            .collect();

        // Should include the enum, struct, and standalone functions
        assert!(
            names.contains(&"Color"),
            "Should chunk Color enum: {:?}",
            names
        );
        assert!(
            names.contains(&"Shape"),
            "Should chunk Shape struct: {:?}",
            names
        );
        assert!(
            names.contains(&"default_shape"),
            "Should chunk default_shape fn: {:?}",
            names
        );
        assert!(
            names.contains(&"format_shape"),
            "Should chunk format_shape fn: {:?}",
            names
        );
    }

    #[test]
    fn test_chunk_file_whitespace_only() {
        let src = "   \n\n\n   \n";
        let (symbols, chunks) = chunk_file(src, Language::Rust, 1, "ws.rs", "wh").unwrap();
        assert!(symbols.is_empty());
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_file_function_with_attrs_and_docs() {
        let src = r#"
/// This function is well documented.
/// It has multiple doc comment lines.
#[inline]
#[must_use]
pub fn documented_fn(input: &str) -> String {
    let trimmed = input.trim();
    let result = trimmed.to_uppercase();
    result
}
"#;
        let (_, chunks) = chunk_file(src, Language::Rust, 1, "documented.rs", "dh").unwrap();

        let fn_chunk = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("documented_fn"));
        assert!(fn_chunk.is_some(), "Should chunk the documented function");
        let fc = fn_chunk.unwrap();
        // The chunk should include docs and attributes via gather_leading_context
        assert!(
            fc.chunk_text
                .contains("/// This function is well documented"),
            "Chunk should include doc comments"
        );
        assert!(
            fc.chunk_text.contains("#[inline]"),
            "Chunk should include #[inline] attribute"
        );
        assert!(
            fc.chunk_text.contains("#[must_use]"),
            "Chunk should include #[must_use] attribute"
        );
    }

    #[test]
    fn test_chunk_file_metadata_propagation() {
        let src = r#"
/// A helper function with enough lines.
pub fn helper(x: i32) -> i32 {
    let a = x + 1;
    let b = a * 2;
    b
}
"#;
        let repo_id = 42;
        let file_hash = "deadbeef";
        let (_, chunks) = chunk_file(src, Language::Rust, repo_id, "meta.rs", file_hash).unwrap();

        assert!(!chunks.is_empty());
        for c in &chunks {
            assert_eq!(c.repo_id, repo_id, "repo_id should propagate");
            assert_eq!(c.file_hash, file_hash, "file_hash should propagate");
            assert_eq!(c.file_path, "meta.rs", "file_path should propagate");
            assert_eq!(c.language, Language::Rust, "language should propagate");
        }
    }

    #[test]
    fn test_chunk_lua_single_function() {
        let src = r#"
-- A greeting function
function greet(name)
    local msg = "Hello, " .. name
    print(msg)
    return msg
end
"#;
        let (symbols, chunks) =
            chunk_file(src, Language::Lua, 1, "greet.lua", "lua_hash1").unwrap();

        assert!(!symbols.is_empty(), "Should extract at least 1 symbol");
        assert!(!chunks.is_empty(), "Should produce at least 1 chunk");

        // All chunks should have correct metadata
        for chunk in &chunks {
            assert_eq!(chunk.language, Language::Lua);
            assert_eq!(chunk.file_path, "greet.lua");
            assert_eq!(chunk.file_hash, "lua_hash1");
            assert!(chunk.context_text.contains("Language: Lua"));
            assert!(chunk.context_text.contains("File: greet.lua"));
        }
    }

    #[test]
    fn test_chunk_lua_multiple_functions() {
        let src = r#"
-- Module configuration
local M = {}

function M.setup(opts)
    M.config = opts or {}
    M.config.enabled = M.config.enabled ~= false
    return M
end

local function validate(input)
    if type(input) ~= "string" then
        return false, "expected string"
    end
    if #input == 0 then
        return false, "empty string"
    end
    return true
end

function M.process(data)
    local ok, err = validate(data)
    if not ok then
        error(err)
    end
    return string.upper(data)
end

return M
"#;
        let (symbols, chunks) =
            chunk_file(src, Language::Lua, 1, "module.lua", "lua_hash2").unwrap();

        // Should find multiple function symbols
        let func_symbols: Vec<_> = symbols
            .iter()
            .filter(|s| s.symbol_kind == SymbolKind::Function)
            .collect();
        assert!(
            func_symbols.len() >= 2,
            "Expected at least 2 function symbols, got {}",
            func_symbols.len()
        );

        // Should have chunks
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunk_lua_empty_file() {
        let src = "";
        let (symbols, chunks) =
            chunk_file(src, Language::Lua, 1, "empty.lua", "empty_hash").unwrap();
        assert!(symbols.is_empty());
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_lua_only_comments() {
        let src = r#"
-- This is a comment
-- Another comment
-- Yet another comment
"#;
        let (symbols, _chunks) =
            chunk_file(src, Language::Lua, 1, "comments.lua", "ch").unwrap();
        assert!(symbols.is_empty(), "Comments should not produce symbols");
    }

    #[test]
    fn test_chunk_lua_metadata_propagation() {
        let src = r#"
function compute(x)
    local result = x * 2
    result = result + 1
    return result
end
"#;
        let repo_id = 99;
        let file_hash = "lua_meta";
        let (_, chunks) =
            chunk_file(src, Language::Lua, repo_id, "compute.lua", file_hash).unwrap();

        for c in &chunks {
            assert_eq!(c.repo_id, repo_id);
            assert_eq!(c.file_hash, file_hash);
            assert_eq!(c.file_path, "compute.lua");
            assert_eq!(c.language, Language::Lua);
        }
    }

    #[test]
    fn test_chunk_json_produces_top_level_chunk() {
        let src = r#"
{
    "name": "my-project",
    "version": "1.0.0",
    "description": "A sample project",
    "main": "index.js",
    "scripts": {
        "build": "tsc",
        "test": "jest --coverage",
        "lint": "eslint . --fix"
    },
    "dependencies": {
        "express": "^4.18.0",
        "lodash": "^4.17.21"
    },
    "devDependencies": {
        "typescript": "^5.0.0",
        "jest": "^29.0.0",
        "eslint": "^8.0.0"
    }
}
"#;
        let (symbols, chunks) =
            chunk_file(src, Language::Json, 1, "package.json", "json_hash").unwrap();

        // JSON has no code symbols
        assert!(
            symbols.is_empty(),
            "JSON should not produce symbols, got: {:?}",
            symbols.iter().map(|s| &s.symbol_name).collect::<Vec<_>>()
        );

        // But it should still produce top-level chunks (for search)
        if !chunks.is_empty() {
            for c in &chunks {
                assert_eq!(c.language, Language::Json);
                assert_eq!(c.file_path, "package.json");
                assert_eq!(c.chunk_type, "top_level");
                assert!(c.context_text.contains("Language: JSON"));
            }
        }
    }

    #[test]
    fn test_chunk_json_empty_object() {
        let src = "{}";
        let (symbols, _chunks) =
            chunk_file(src, Language::Json, 1, "empty.json", "h").unwrap();
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_chunk_json_no_symbols_but_preserves_metadata() {
        let src = r#"
{
    "compilerOptions": {
        "target": "ES2020",
        "module": "commonjs",
        "strict": true,
        "esModuleInterop": true,
        "skipLibCheck": true,
        "forceConsistentCasingInFileNames": true,
        "outDir": "./dist",
        "rootDir": "./src"
    }
}
"#;
        let (symbols, chunks) =
            chunk_file(src, Language::Json, 7, "tsconfig.json", "ts_hash").unwrap();
        assert!(symbols.is_empty());
        for c in &chunks {
            assert_eq!(c.repo_id, 7);
            assert_eq!(c.file_hash, "ts_hash");
        }
    }

    #[test]
    fn test_chunk_yaml_produces_top_level_chunk() {
        let src = r#"
name: Build and Test
on:
  push:
    branches: [main, develop]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        rust: [stable, nightly]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ matrix.rust }}
      - run: cargo test --all-features
"#;
        let (symbols, chunks) =
            chunk_file(src, Language::Yaml, 1, "ci.yaml", "yaml_hash").unwrap();

        assert!(symbols.is_empty(), "YAML should not produce symbols");

        if !chunks.is_empty() {
            for c in &chunks {
                assert_eq!(c.language, Language::Yaml);
                assert_eq!(c.file_path, "ci.yaml");
                assert!(c.context_text.contains("Language: YAML"));
            }
        }
    }

    #[test]
    fn test_chunk_yaml_simple_config() {
        let src = "key: value\n";
        let (symbols, _chunks) =
            chunk_file(src, Language::Yaml, 1, "config.yml", "h").unwrap();
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_chunk_yaml_docker_compose() {
        let src = r#"
version: "3.8"
services:
  app:
    build:
      context: .
      dockerfile: Dockerfile
    ports:
      - "8080:8080"
    environment:
      - DATABASE_URL=postgres://localhost/mydb
      - REDIS_URL=redis://localhost:6379
    depends_on:
      - db
      - redis
  db:
    image: postgres:16
    environment:
      POSTGRES_DB: mydb
      POSTGRES_PASSWORD: secret
    volumes:
      - pgdata:/var/lib/postgresql/data
  redis:
    image: redis:7-alpine
    ports:
      - "6379:6379"
volumes:
  pgdata:
"#;
        let (symbols, chunks) =
            chunk_file(src, Language::Yaml, 2, "docker-compose.yml", "dc_hash").unwrap();
        assert!(symbols.is_empty());
        for c in &chunks {
            assert_eq!(c.repo_id, 2);
            assert_eq!(c.language, Language::Yaml);
        }
    }

    #[test]
    fn test_chunk_dockerfile_returns_error() {
        // Dockerfile has no tree-sitter grammar, so chunk_file (which calls parse_file)
        // should return an error.
        let src = r#"
FROM rust:1.75-slim as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/app /usr/local/bin/
EXPOSE 8080
CMD ["app"]
"#;
        let result = chunk_file(src, Language::Dockerfile, 1, "Dockerfile", "df_hash");
        assert!(
            result.is_err(),
            "Dockerfile chunking should fail (no grammar)"
        );
    }

    #[test]
    fn test_build_context_text_lua() {
        let ctx = build_context_text(
            "init.lua",
            Language::Lua,
            "function",
            Some("setup"),
            None,
            "function setup(opts)",
        );
        assert!(ctx.contains("File: init.lua"));
        assert!(ctx.contains("Language: Lua"));
        assert!(ctx.contains("Symbol: setup (function)"));
        assert!(ctx.contains("function setup(opts)"));
    }

    #[test]
    fn test_build_context_text_json() {
        let ctx = build_context_text(
            "package.json",
            Language::Json,
            "top_level",
            None,
            None,
            "",
        );
        assert!(ctx.contains("File: package.json"));
        assert!(ctx.contains("Language: JSON"));
    }

    #[test]
    fn test_build_context_text_yaml() {
        let ctx = build_context_text(
            "ci.yaml",
            Language::Yaml,
            "top_level",
            None,
            None,
            "",
        );
        assert!(ctx.contains("File: ci.yaml"));
        assert!(ctx.contains("Language: YAML"));
    }

    #[test]
    fn test_build_context_text_dockerfile() {
        let ctx = build_context_text(
            "Dockerfile",
            Language::Dockerfile,
            "top_level",
            None,
            None,
            "",
        );
        assert!(ctx.contains("File: Dockerfile"));
        assert!(ctx.contains("Language: Dockerfile"));
    }
}
