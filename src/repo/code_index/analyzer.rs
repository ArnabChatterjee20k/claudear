//! Code style pattern detection via tree-sitter AST analysis.
//!
//! Extracts style patterns from parsed source files: early returns, naming conventions,
//! magic numbers, const preference, error handling patterns, and function length.

use super::languages;
use super::types::Language;

/// Aggregated style analysis for a single file.
#[derive(Debug, Clone, Default)]
pub struct FileStyleAnalysis {
    /// Number of functions analyzed.
    pub function_count: usize,
    /// Functions using early returns (return in first 3 lines of body).
    pub early_return_count: usize,
    /// Count of magic numbers (numeric literals not in const/static assignments).
    pub magic_number_count: usize,
    /// snake_case function names count.
    pub snake_case_functions: usize,
    /// camelCase function names count.
    pub camel_case_functions: usize,
    /// PascalCase type names count.
    pub pascal_case_types: usize,
    /// Other-cased type names count.
    pub other_case_types: usize,
    /// const declarations (JS/TS).
    pub const_declarations: usize,
    /// let declarations (JS/TS).
    pub let_declarations: usize,
    /// Rust: ? operator usage count.
    pub question_mark_ops: usize,
    /// Rust: unwrap() calls.
    pub unwrap_calls: usize,
    /// Rust: expect() calls.
    pub expect_calls: usize,
    /// Function body line counts (for distribution analysis).
    pub function_lengths: Vec<usize>,
}

/// Repo-level aggregated style patterns.
#[derive(Debug, Clone)]
pub struct RepoStyleSummary {
    pub total_functions: usize,
    pub early_return_pct: f64,
    pub magic_number_density: f64,
    pub naming_convention_functions: String,
    pub naming_convention_types: String,
    pub const_preference_pct: Option<f64>,
    pub error_handling_style: Option<String>,
    pub avg_function_length: f64,
    pub p50_function_length: usize,
    pub p95_function_length: usize,
    pub patterns: Vec<(String, String, f64)>,
}

/// Analyze a single file's style patterns using tree-sitter.
pub fn analyze_file(source: &str, language: Language) -> FileStyleAnalysis {
    let mut analysis = FileStyleAnalysis::default();

    let ts_lang = languages::ts_language(language);
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return analysis;
    }

    let Some(tree) = parser.parse(source, None) else {
        return analysis;
    };

    let root = tree.root_node();
    let source_bytes = source.as_bytes();

    analyze_node(root, source_bytes, language, &mut analysis, 0);

    analysis
}

fn analyze_node(
    node: tree_sitter::Node,
    source: &[u8],
    language: Language,
    analysis: &mut FileStyleAnalysis,
    depth: usize,
) {
    let node_type = node.kind();

    // Check if this is a function-like node
    if is_function_node(language, node_type) {
        analysis.function_count += 1;

        // Extract function name and check naming convention
        if let Some(name) = extract_name(node, source, language) {
            if is_snake_case(&name) {
                analysis.snake_case_functions += 1;
            } else if is_camel_case(&name) {
                analysis.camel_case_functions += 1;
            }
        }

        // Check for early returns
        if has_early_return(node, source, language) {
            analysis.early_return_count += 1;
        }

        // Compute function length
        let start_line = node.start_position().row;
        let end_line = node.end_position().row;
        let length = end_line.saturating_sub(start_line) + 1;
        analysis.function_lengths.push(length);
    }

    // Check if this is a type-like node
    if is_type_node(language, node_type) {
        if let Some(name) = extract_name(node, source, language) {
            if is_pascal_case(&name) {
                analysis.pascal_case_types += 1;
            } else {
                analysis.other_case_types += 1;
            }
        }
    }

    // Check for magic numbers (numeric literals not in const/static context)
    if is_numeric_literal(node_type) && depth > 0 && !is_in_const_context(node, language) {
        analysis.magic_number_count += 1;
    }

    // JS/TS: track const vs let
    if matches!(
        language,
        Language::TypeScript | Language::Tsx | Language::JavaScript
    ) && node_type == "lexical_declaration"
    {
        let text = node_text(node, source);
        if text.starts_with("const ") {
            analysis.const_declarations += 1;
        } else if text.starts_with("let ") {
            analysis.let_declarations += 1;
        }
    }

    // Rust: track error handling patterns
    if language == Language::Rust {
        let text = node_text(node, source);
        if node_type == "try_expression"
            || text.contains('?') && node_type == "expression_statement"
        {
            analysis.question_mark_ops += 1;
        }
        if node_type == "call_expression" || node_type == "method_call_expression" {
            if text.ends_with(".unwrap()") || text.contains(".unwrap()") {
                analysis.unwrap_calls += 1;
            }
            if text.ends_with(".expect(") || text.contains(".expect(") {
                analysis.expect_calls += 1;
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        analyze_node(child, source, language, analysis, depth + 1);
    }
}

/// Aggregate per-file analyses into a repo-level summary.
pub fn aggregate(analyses: &[FileStyleAnalysis]) -> RepoStyleSummary {
    let total_functions: usize = analyses.iter().map(|a| a.function_count).sum();
    let total_early_returns: usize = analyses.iter().map(|a| a.early_return_count).sum();
    let total_magic_numbers: usize = analyses.iter().map(|a| a.magic_number_count).sum();
    let total_snake: usize = analyses.iter().map(|a| a.snake_case_functions).sum();
    let total_camel: usize = analyses.iter().map(|a| a.camel_case_functions).sum();
    let total_pascal: usize = analyses.iter().map(|a| a.pascal_case_types).sum();
    let total_other_types: usize = analyses.iter().map(|a| a.other_case_types).sum();
    let total_const: usize = analyses.iter().map(|a| a.const_declarations).sum();
    let total_let: usize = analyses.iter().map(|a| a.let_declarations).sum();
    let total_question: usize = analyses.iter().map(|a| a.question_mark_ops).sum();
    let total_unwrap: usize = analyses.iter().map(|a| a.unwrap_calls).sum();
    let total_expect: usize = analyses.iter().map(|a| a.expect_calls).sum();

    let mut all_lengths: Vec<usize> = analyses
        .iter()
        .flat_map(|a| a.function_lengths.iter().copied())
        .collect();
    all_lengths.sort_unstable();

    let early_return_pct = if total_functions > 0 {
        total_early_returns as f64 / total_functions as f64 * 100.0
    } else {
        0.0
    };

    let magic_number_density = if total_functions > 0 {
        total_magic_numbers as f64 / total_functions as f64
    } else {
        0.0
    };

    let naming_convention_functions = if total_snake + total_camel == 0 {
        "unknown".to_string()
    } else if total_snake as f64 / (total_snake + total_camel) as f64 > 0.7 {
        "snake_case".to_string()
    } else if total_camel as f64 / (total_snake + total_camel) as f64 > 0.7 {
        "camelCase".to_string()
    } else {
        "mixed".to_string()
    };

    let naming_convention_types = if total_pascal + total_other_types == 0 {
        "unknown".to_string()
    } else if total_pascal as f64 / (total_pascal + total_other_types) as f64 > 0.7 {
        "PascalCase".to_string()
    } else {
        "mixed".to_string()
    };

    let const_preference_pct = if total_const + total_let > 0 {
        Some(total_const as f64 / (total_const + total_let) as f64 * 100.0)
    } else {
        None
    };

    let error_handling_style = if total_question + total_unwrap + total_expect > 0 {
        let total_err = total_question + total_unwrap + total_expect;
        if total_question as f64 / total_err as f64 > 0.7 {
            Some("? operator (idiomatic)".to_string())
        } else if total_unwrap as f64 / total_err as f64 > 0.3 {
            Some("mixed (frequent unwrap)".to_string())
        } else {
            Some("mixed".to_string())
        }
    } else {
        None
    };

    let avg_function_length = if all_lengths.is_empty() {
        0.0
    } else {
        all_lengths.iter().sum::<usize>() as f64 / all_lengths.len() as f64
    };

    let p50_function_length = percentile(&all_lengths, 50);
    let p95_function_length = percentile(&all_lengths, 95);

    // Build confirmed patterns (>70% threshold)
    let mut patterns = Vec::new();

    if total_functions >= 5 {
        let er_ratio = total_early_returns as f64 / total_functions as f64;
        if er_ratio > 0.7 {
            patterns.push((
                "style_early_returns".to_string(),
                format!("{:.0}% of functions use early returns", er_ratio * 100.0),
                er_ratio,
            ));
        }

        if naming_convention_functions == "snake_case" {
            let ratio = total_snake as f64 / (total_snake + total_camel).max(1) as f64;
            patterns.push((
                "style_naming_functions".to_string(),
                "snake_case function naming".to_string(),
                ratio,
            ));
        } else if naming_convention_functions == "camelCase" {
            let ratio = total_camel as f64 / (total_snake + total_camel).max(1) as f64;
            patterns.push((
                "style_naming_functions".to_string(),
                "camelCase function naming".to_string(),
                ratio,
            ));
        }

        if naming_convention_types == "PascalCase" {
            let ratio = total_pascal as f64 / (total_pascal + total_other_types).max(1) as f64;
            patterns.push((
                "style_naming_types".to_string(),
                "PascalCase type naming".to_string(),
                ratio,
            ));
        }
    }

    if let Some(pct) = const_preference_pct {
        if pct > 70.0 {
            patterns.push((
                "style_const_preference".to_string(),
                format!("{:.0}% const preference over let", pct),
                pct / 100.0,
            ));
        }
    }

    if magic_number_density < 0.1 && total_functions >= 5 {
        patterns.push((
            "style_magic_numbers".to_string(),
            "Low magic number usage".to_string(),
            1.0 - magic_number_density,
        ));
    }

    RepoStyleSummary {
        total_functions,
        early_return_pct,
        magic_number_density,
        naming_convention_functions,
        naming_convention_types,
        const_preference_pct,
        error_handling_style,
        avg_function_length,
        p50_function_length,
        p95_function_length,
        patterns,
    }
}

// -- Helper functions --

fn is_function_node(language: Language, node_type: &str) -> bool {
    match language {
        Language::Rust => matches!(node_type, "function_item"),
        Language::TypeScript | Language::Tsx | Language::JavaScript => {
            matches!(
                node_type,
                "function_declaration" | "method_definition" | "arrow_function"
            )
        }
        Language::Python => matches!(node_type, "function_definition"),
        Language::Go => matches!(node_type, "function_declaration" | "method_declaration"),
        Language::Java => matches!(node_type, "method_declaration" | "constructor_declaration"),
        Language::C | Language::Cpp => matches!(node_type, "function_definition"),
        Language::Ruby => matches!(node_type, "method" | "singleton_method"),
        Language::Php => matches!(node_type, "function_definition" | "method_declaration"),
        Language::Swift => matches!(node_type, "function_declaration"),
        Language::Kotlin => matches!(node_type, "function_declaration"),
    }
}

fn is_type_node(language: Language, node_type: &str) -> bool {
    match language {
        Language::Rust => matches!(node_type, "struct_item" | "enum_item" | "trait_item"),
        Language::TypeScript | Language::Tsx | Language::JavaScript => {
            matches!(node_type, "class_declaration" | "interface_declaration")
        }
        Language::Python => matches!(node_type, "class_definition"),
        Language::Go => matches!(node_type, "type_declaration"),
        Language::Java => matches!(
            node_type,
            "class_declaration" | "interface_declaration" | "enum_declaration"
        ),
        Language::C | Language::Cpp => matches!(
            node_type,
            "struct_specifier" | "class_specifier" | "enum_specifier"
        ),
        Language::Ruby => matches!(node_type, "class" | "module"),
        Language::Php => matches!(node_type, "class_declaration" | "interface_declaration"),
        Language::Swift => matches!(node_type, "class_declaration" | "protocol_declaration"),
        Language::Kotlin => matches!(node_type, "class_declaration" | "interface_declaration"),
    }
}

fn is_numeric_literal(node_type: &str) -> bool {
    matches!(
        node_type,
        "integer_literal" | "float_literal" | "number" | "number_literal"
    )
}

fn is_in_const_context(node: tree_sitter::Node, language: Language) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match language {
            Language::Rust => {
                if matches!(parent.kind(), "const_item" | "static_item") {
                    return true;
                }
            }
            Language::TypeScript | Language::Tsx | Language::JavaScript => {
                if parent.kind() == "lexical_declaration" {
                    let text = parent.child(0).map(|c| c.kind()).unwrap_or("");
                    if text == "const" {
                        return true;
                    }
                }
            }
            _ => {
                // For other languages, check for common constant patterns
                if parent.kind().contains("const") || parent.kind().contains("static") {
                    return true;
                }
            }
        }
        current = parent.parent();
    }
    false
}

fn has_early_return(node: tree_sitter::Node, source: &[u8], language: Language) -> bool {
    let return_type = match language {
        Language::Rust => "return_expression",
        Language::Python => "return_statement",
        _ => "return_statement",
    };

    // Find the function body
    let body = find_body(node, language);
    let Some(body) = body else {
        return false;
    };

    let body_start = body.start_position().row;

    // Check if there's a return in the first 3 lines of the body
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == return_type || contains_return(child, return_type) {
            let return_line = child.start_position().row;
            if return_line <= body_start + 3 {
                return true;
            }
        }
    }

    let _ = source; // may be used in future refinements
    false
}

fn contains_return(node: tree_sitter::Node, return_type: &str) -> bool {
    if node.kind() == return_type {
        return true;
    }
    // Check control flow and block children recursively (but not nested functions)
    if is_control_flow(node.kind())
        || node.kind() == "block"
        || node.kind() == "expression_statement"
    {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if contains_return(child, return_type) {
                return true;
            }
        }
    }
    false
}

fn is_control_flow(node_type: &str) -> bool {
    matches!(
        node_type,
        "if_expression"
            | "if_statement"
            | "if_let_expression"
            | "match_expression"
            | "guard_statement"
    )
}

fn find_body(node: tree_sitter::Node, language: Language) -> Option<tree_sitter::Node> {
    let body_field = match language {
        Language::Rust => "body",
        Language::Python => "body",
        Language::TypeScript | Language::Tsx | Language::JavaScript => "body",
        Language::Go => "body",
        Language::Java => "body",
        Language::C | Language::Cpp => "body",
        Language::Ruby => "body",
        Language::Php => "body",
        Language::Swift => "body",
        Language::Kotlin => "body",
    };
    node.child_by_field_name(body_field)
}

fn extract_name(node: tree_sitter::Node, source: &[u8], language: Language) -> Option<String> {
    let field = languages::name_field(language, node.kind())?;
    let name_node = node.child_by_field_name(field)?;
    Some(node_text(name_node, source))
}

fn node_text(node: tree_sitter::Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    if start >= end {
        return String::new();
    }
    String::from_utf8_lossy(&source[start..end]).to_string()
}

fn is_snake_case(s: &str) -> bool {
    if s.is_empty() || s.starts_with('_') && s.len() == 1 {
        return false;
    }
    // snake_case: lowercase letters, digits, underscores, no uppercase
    s.chars()
        .all(|c| c.is_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_camel_case(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // camelCase: starts lowercase, has at least one uppercase
    s.chars().next().is_some_and(|c| c.is_lowercase()) && s.chars().any(|c| c.is_uppercase())
}

fn is_pascal_case(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // PascalCase: starts uppercase
    s.chars().next().is_some_and(|c| c.is_uppercase())
}

fn percentile(sorted: &[usize], pct: usize) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (pct as f64 / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_rust_file() {
        let source = r#"
fn early_return_example(x: i32) -> bool {
    if x < 0 {
        return false;
    }
    x > 10
}

fn normal_function(x: i32) -> i32 {
    let y = x * 2;
    let z = y + 1;
    z
}

struct MyStruct {
    field: i32,
}

enum MyEnum {
    A,
    B,
}
"#;
        let analysis = analyze_file(source, Language::Rust);
        assert_eq!(analysis.function_count, 2);
        assert!(analysis.early_return_count >= 1);
        assert!(analysis.snake_case_functions >= 2);
        assert!(analysis.pascal_case_types >= 2);
        assert_eq!(analysis.function_lengths.len(), 2);
    }

    #[test]
    fn test_analyze_js_file() {
        let source = r#"
function myFunction(x) {
    if (!x) return null;
    return x * 2;
}

class MyClass {
    constructor() {
        this.value = 42;
    }
}

const MAX_SIZE = 100;
let counter = 0;
"#;
        let analysis = analyze_file(source, Language::JavaScript);
        assert!(analysis.function_count >= 1);
        assert!(analysis.camel_case_functions >= 1);
        assert!(analysis.pascal_case_types >= 1);
    }

    #[test]
    fn test_naming_detection() {
        assert!(is_snake_case("my_function"));
        assert!(is_snake_case("get_value"));
        assert!(!is_snake_case("myFunction"));
        assert!(!is_snake_case("MyClass"));

        assert!(is_camel_case("myFunction"));
        assert!(is_camel_case("getValue"));
        assert!(!is_camel_case("my_function"));
        assert!(!is_camel_case("MyClass"));

        assert!(is_pascal_case("MyClass"));
        assert!(is_pascal_case("HttpRequest"));
        assert!(!is_pascal_case("myFunction"));
        assert!(!is_pascal_case("my_function"));
    }

    #[test]
    fn test_percentile() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&data, 50), 6);
        assert_eq!(percentile(&data, 95), 10);
        assert_eq!(percentile(&data, 0), 1);
    }

    #[test]
    fn test_percentile_empty() {
        let data: Vec<usize> = vec![];
        assert_eq!(percentile(&data, 50), 0);
    }

    #[test]
    fn test_aggregate_basic() {
        let a1 = FileStyleAnalysis {
            function_count: 10,
            early_return_count: 8,
            snake_case_functions: 9,
            camel_case_functions: 1,
            pascal_case_types: 5,
            other_case_types: 0,
            function_lengths: vec![5, 10, 15, 20, 25, 30, 35, 40, 45, 50],
            ..Default::default()
        };
        let summary = aggregate(&[a1]);
        assert_eq!(summary.total_functions, 10);
        assert!(summary.early_return_pct > 70.0);
        assert_eq!(summary.naming_convention_functions, "snake_case");
        assert_eq!(summary.naming_convention_types, "PascalCase");
        assert!(!summary.patterns.is_empty());
    }

    #[test]
    fn test_aggregate_empty() {
        let summary = aggregate(&[]);
        assert_eq!(summary.total_functions, 0);
        assert_eq!(summary.avg_function_length, 0.0);
    }

    #[test]
    fn test_analyze_js_const_let_tracking() {
        let source = r#"
const API_URL = "https://example.com";
const MAX_RETRIES = 3;
const DEFAULT_TIMEOUT = 5000;
let counter = 0;
let isActive = false;

function fetchData(url) {
    if (!url) return null;
    const response = fetch(url);
    return response;
}

function processItems(items) {
    const results = [];
    for (const item of items) {
        if (item.valid) {
            results.push(item);
        }
    }
    return results;
}

class DataProcessor {
    constructor() {
        this.cache = new Map();
    }
}
"#;
        let analysis = analyze_file(source, Language::JavaScript);
        // Should detect functions (fetchData, processItems, constructor)
        assert!(
            analysis.function_count >= 2,
            "expected at least 2 functions, got {}",
            analysis.function_count
        );
        // Should detect const declarations
        assert!(
            analysis.const_declarations >= 3,
            "expected at least 3 const declarations, got {}",
            analysis.const_declarations
        );
        // Should detect let declarations
        assert!(
            analysis.let_declarations >= 1,
            "expected at least 1 let declaration, got {}",
            analysis.let_declarations
        );
        // camelCase function names (fetchData, processItems)
        assert!(
            analysis.camel_case_functions >= 2,
            "expected at least 2 camelCase functions, got {}",
            analysis.camel_case_functions
        );
        // PascalCase type (DataProcessor)
        assert!(
            analysis.pascal_case_types >= 1,
            "expected at least 1 PascalCase type, got {}",
            analysis.pascal_case_types
        );
    }

    #[test]
    fn test_analyze_python_style() {
        let source = r#"
def validate_input(data):
    if data is None:
        return False
    if not isinstance(data, dict):
        return False
    return True

def process_data(items):
    result = []
    for item in items:
        if item > 0:
            result.append(item * 2)
    return result

def calculate_total(values):
    total = 0
    for v in values:
        total += v
    return total

class DataManager:
    def __init__(self):
        self.items = []

    def add_item(self, item):
        self.items.append(item)

class EventHandler:
    pass
"#;
        let analysis = analyze_file(source, Language::Python);
        // Should detect top-level and class-level functions
        assert!(
            analysis.function_count >= 3,
            "expected at least 3 functions, got {}",
            analysis.function_count
        );
        // Python uses snake_case naming
        assert!(
            analysis.snake_case_functions >= 3,
            "expected at least 3 snake_case functions, got {}",
            analysis.snake_case_functions
        );
        // Early returns in validate_input
        assert!(
            analysis.early_return_count >= 1,
            "expected at least 1 early return, got {}",
            analysis.early_return_count
        );
        // PascalCase types (DataManager, EventHandler)
        assert!(
            analysis.pascal_case_types >= 2,
            "expected at least 2 PascalCase types, got {}",
            analysis.pascal_case_types
        );
        // Function lengths should be recorded
        assert!(
            analysis.function_lengths.len() >= 3,
            "expected at least 3 function lengths, got {}",
            analysis.function_lengths.len()
        );
    }

    #[test]
    fn test_aggregate_multiple_files() {
        // Simulate a Rust-style file: snake_case, early returns, low magic numbers
        let rust_analysis = FileStyleAnalysis {
            function_count: 8,
            early_return_count: 6,
            snake_case_functions: 8,
            camel_case_functions: 0,
            pascal_case_types: 4,
            other_case_types: 0,
            magic_number_count: 0,
            question_mark_ops: 10,
            unwrap_calls: 1,
            expect_calls: 0,
            function_lengths: vec![5, 8, 12, 15, 20, 25, 30, 40],
            ..Default::default()
        };

        // Simulate a JS-style file: camelCase, const preference
        let js_analysis = FileStyleAnalysis {
            function_count: 6,
            early_return_count: 4,
            snake_case_functions: 0,
            camel_case_functions: 6,
            pascal_case_types: 2,
            other_case_types: 0,
            magic_number_count: 1,
            const_declarations: 10,
            let_declarations: 2,
            function_lengths: vec![3, 7, 10, 15, 20, 50],
            ..Default::default()
        };

        // Simulate another Rust file
        let rust_analysis_2 = FileStyleAnalysis {
            function_count: 4,
            early_return_count: 3,
            snake_case_functions: 4,
            camel_case_functions: 0,
            pascal_case_types: 2,
            other_case_types: 0,
            magic_number_count: 0,
            question_mark_ops: 5,
            unwrap_calls: 0,
            expect_calls: 1,
            function_lengths: vec![10, 15, 20, 35],
            ..Default::default()
        };

        let summary = aggregate(&[rust_analysis, js_analysis, rust_analysis_2]);

        // Total functions: 8 + 6 + 4 = 18
        assert_eq!(summary.total_functions, 18);

        // Early return pct: (6 + 4 + 3) / 18 = 72.2%
        assert!(
            summary.early_return_pct > 70.0,
            "expected early_return_pct > 70, got {}",
            summary.early_return_pct
        );

        // Naming: snake_case = 12, camelCase = 6 => snake ratio = 12/18 = 66.7%, below 70 threshold = mixed
        assert_eq!(summary.naming_convention_functions, "mixed");

        // Types: pascal = 8, other = 0 => PascalCase
        assert_eq!(summary.naming_convention_types, "PascalCase");

        // Const preference: 10 / 12 = 83.3%
        assert!(summary.const_preference_pct.is_some());
        assert!(
            summary.const_preference_pct.unwrap() > 80.0,
            "expected const_preference_pct > 80, got {}",
            summary.const_preference_pct.unwrap()
        );

        // Error handling: question=15, unwrap=1, expect=1 => 15/17 = 88% => idiomatic
        assert!(summary.error_handling_style.is_some());
        assert!(
            summary
                .error_handling_style
                .as_ref()
                .unwrap()
                .contains("idiomatic"),
            "expected idiomatic error handling, got {:?}",
            summary.error_handling_style
        );

        // Magic number density: 1 / 18 = 0.055 => low
        assert!(
            summary.magic_number_density < 0.1,
            "expected magic_number_density < 0.1, got {}",
            summary.magic_number_density
        );

        // Percentile checks: 18 function lengths total, sorted
        assert!(summary.avg_function_length > 0.0);
        assert!(summary.p50_function_length > 0);
        assert!(summary.p95_function_length > summary.p50_function_length);

        // Patterns should be populated (total_functions >= 5)
        assert!(
            !summary.patterns.is_empty(),
            "expected at least one confirmed pattern"
        );

        // Should contain low magic numbers pattern
        let has_magic_number_pattern = summary
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_magic_numbers");
        assert!(
            has_magic_number_pattern,
            "expected style_magic_numbers pattern in {:?}",
            summary.patterns
        );

        // Should contain const preference pattern
        let has_const_pattern = summary
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_const_preference");
        assert!(
            has_const_pattern,
            "expected style_const_preference pattern in {:?}",
            summary.patterns
        );
    }
}
