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

    #[test]
    fn test_analyze_typescript_const_let_and_arrow_functions() {
        let source = r#"
const fetchData = (url: string): Promise<Response> => {
    if (!url) return Promise.reject("no url");
    return fetch(url);
};

const processItem = (item: number): number => {
    return item * 2;
};

function handleRequest(req: Request): Response {
    if (!req.body) {
        return new Response("empty");
    }
    const data = JSON.parse(req.body);
    return new Response(data);
}

class RequestHandler {
    handle() {}
}

const MAX_SIZE = 100;
const API_URL = "https://example.com";
let counter = 0;
let isReady = false;
"#;
        let analysis = analyze_file(source, Language::TypeScript);

        // arrow_function counts: fetchData, processItem
        // function_declaration: handleRequest
        // method_definition: handle
        assert!(
            analysis.function_count >= 3,
            "expected at least 3 functions, got {}",
            analysis.function_count
        );

        // const declarations: fetchData, processItem assigned via const lexical_declaration,
        // plus MAX_SIZE, API_URL
        assert!(
            analysis.const_declarations >= 2,
            "expected at least 2 const declarations, got {}",
            analysis.const_declarations
        );

        // let declarations: counter, isReady
        assert!(
            analysis.let_declarations >= 2,
            "expected at least 2 let declarations, got {}",
            analysis.let_declarations
        );

        // camelCase functions: fetchData, processItem, handleRequest (all start lowercase, have uppercase)
        assert!(
            analysis.camel_case_functions >= 1,
            "expected at least 1 camelCase function, got {}",
            analysis.camel_case_functions
        );

        // PascalCase type: RequestHandler
        assert!(
            analysis.pascal_case_types >= 1,
            "expected at least 1 PascalCase type, got {}",
            analysis.pascal_case_types
        );
    }

    #[test]
    fn test_analyze_typescript_function_declaration_detection() {
        let source = r#"
function greetUser(name: string): string {
    return `Hello, ${name}`;
}

function calculateSum(a: number, b: number): number {
    return a + b;
}
"#;
        let analysis = analyze_file(source, Language::TypeScript);
        assert!(
            analysis.function_count >= 2,
            "expected at least 2 function_declarations, got {}",
            analysis.function_count
        );
        // Both are camelCase
        assert!(
            analysis.camel_case_functions >= 2,
            "expected at least 2 camelCase functions, got {}",
            analysis.camel_case_functions
        );
    }

    #[test]
    fn test_analyze_python_function_and_class_detection() {
        let source = r#"
def get_user(user_id):
    if user_id is None:
        return None
    return db.fetch(user_id)

def process_batch(items):
    results = []
    for item in items:
        results.append(item)
    return results

class UserService:
    def create_user(self, name):
        return {"name": name}

class DatabaseConnection:
    pass
"#;
        let analysis = analyze_file(source, Language::Python);

        // function_definition: get_user, process_batch, create_user (methods are also function_definition)
        assert!(
            analysis.function_count >= 3,
            "expected at least 3 functions, got {}",
            analysis.function_count
        );

        // class_definition: UserService, DatabaseConnection
        assert!(
            analysis.pascal_case_types >= 2,
            "expected at least 2 PascalCase types, got {}",
            analysis.pascal_case_types
        );

        // snake_case functions
        assert!(
            analysis.snake_case_functions >= 2,
            "expected at least 2 snake_case functions, got {}",
            analysis.snake_case_functions
        );

        // Early return in get_user (return None on line 2 of body)
        assert!(
            analysis.early_return_count >= 1,
            "expected at least 1 early return, got {}",
            analysis.early_return_count
        );
    }

    #[test]
    fn test_analyze_go_function_and_method_declaration() {
        let source = r#"
package main

func processRequest(r Request) Response {
    if r.Body == nil {
        return Response{}
    }
    return Response{Body: r.Body}
}

func calculateTotal(values []int) int {
    total := 0
    for _, v := range values {
        total += v
    }
    return total
}

type Server struct {
    Port int
}

func (s *Server) handleConnection(conn net.Conn) {
    defer conn.Close()
    buf := make([]byte, 1024)
    conn.Read(buf)
}
"#;
        let analysis = analyze_file(source, Language::Go);

        // function_declaration: processRequest, calculateTotal
        // method_declaration: handleConnection
        assert!(
            analysis.function_count >= 3,
            "expected at least 3 functions (incl. method), got {}",
            analysis.function_count
        );

        // camelCase: processRequest, calculateTotal, handleConnection
        assert!(
            analysis.camel_case_functions >= 3,
            "expected at least 3 camelCase functions, got {}",
            analysis.camel_case_functions
        );

        // Early return in processRequest
        assert!(
            analysis.early_return_count >= 1,
            "expected at least 1 early return, got {}",
            analysis.early_return_count
        );

        // Function lengths recorded
        assert!(
            analysis.function_lengths.len() >= 3,
            "expected at least 3 function lengths, got {}",
            analysis.function_lengths.len()
        );
    }

    #[test]
    fn test_analyze_empty_source() {
        let analysis = analyze_file("", Language::Rust);
        assert_eq!(analysis.function_count, 0);
        assert_eq!(analysis.early_return_count, 0);
        assert_eq!(analysis.magic_number_count, 0);
        assert_eq!(analysis.snake_case_functions, 0);
        assert_eq!(analysis.camel_case_functions, 0);
        assert_eq!(analysis.pascal_case_types, 0);
        assert_eq!(analysis.other_case_types, 0);
        assert_eq!(analysis.const_declarations, 0);
        assert_eq!(analysis.let_declarations, 0);
        assert_eq!(analysis.question_mark_ops, 0);
        assert_eq!(analysis.unwrap_calls, 0);
        assert_eq!(analysis.expect_calls, 0);
        assert!(analysis.function_lengths.is_empty());
    }

    #[test]
    fn test_analyze_empty_source_python() {
        let analysis = analyze_file("", Language::Python);
        assert_eq!(analysis.function_count, 0);
        assert!(analysis.function_lengths.is_empty());
    }

    #[test]
    fn test_analyze_empty_source_go() {
        let analysis = analyze_file("", Language::Go);
        assert_eq!(analysis.function_count, 0);
        assert!(analysis.function_lengths.is_empty());
    }

    #[test]
    fn test_analyze_unparseable_source() {
        // Garbage text that won't parse into meaningful AST nodes
        let source = "}}}}{{{{::::@@@@####";
        let analysis = analyze_file(source, Language::Rust);
        assert_eq!(analysis.function_count, 0);
    }

    #[test]
    fn test_analyze_magic_numbers_rust() {
        let source = r#"
const MAX: i32 = 100;
static THRESHOLD: f64 = 0.5;

fn compute(x: i32) -> i32 {
    let y = x * 42;
    let z = y + 7;
    z
}
"#;
        let analysis = analyze_file(source, Language::Rust);
        // 42 and 7 are magic numbers (not in const/static context)
        // 100 and 0.5 are in const/static context
        assert!(
            analysis.magic_number_count >= 2,
            "expected at least 2 magic numbers, got {}",
            analysis.magic_number_count
        );
    }

    #[test]
    fn test_analyze_magic_numbers_javascript() {
        let source = r#"
const MAX_SIZE = 100;

function calculate(x) {
    let y = x * 42;
    let z = y + 7;
    return z;
}
"#;
        let analysis = analyze_file(source, Language::JavaScript);
        // 42 and 7 are magic (inside function, not const assignment)
        // 100 is in const context
        assert!(
            analysis.magic_number_count >= 2,
            "expected at least 2 magic numbers, got {}",
            analysis.magic_number_count
        );
    }

    #[test]
    fn test_analyze_early_return_rust() {
        let source = r#"
fn guard_clause(x: Option<i32>) -> i32 {
    if x.is_none() {
        return 0;
    }
    x.unwrap() * 2
}

fn no_early_return(x: i32) -> i32 {
    let a = x + 1;
    let b = a + 2;
    let c = b + 3;
    let d = c + 4;
    if d > 100 {
        return d;
    }
    d
}
"#;
        let analysis = analyze_file(source, Language::Rust);
        assert_eq!(analysis.function_count, 2);
        // guard_clause has early return; no_early_return's return is past line 3 of body
        assert!(
            analysis.early_return_count >= 1,
            "expected at least 1 early return, got {}",
            analysis.early_return_count
        );
    }

    #[test]
    fn test_analyze_early_return_python() {
        let source = r#"
def validate(data):
    if data is None:
        return False
    return True

def long_function(items):
    a = 1
    b = 2
    c = 3
    d = 4
    if d > 10:
        return d
    return a + b + c
"#;
        let analysis = analyze_file(source, Language::Python);
        // validate has an early return
        assert!(
            analysis.early_return_count >= 1,
            "expected at least 1 early return, got {}",
            analysis.early_return_count
        );
    }

    #[test]
    fn test_analyze_rust_error_handling_question_mark() {
        let source = r#"
fn read_file(path: &str) -> Result<String, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    Ok(content)
}
"#;
        let analysis = analyze_file(source, Language::Rust);
        assert!(
            analysis.question_mark_ops >= 1,
            "expected at least 1 ? operator, got {}",
            analysis.question_mark_ops
        );
    }

    #[test]
    fn test_analyze_rust_error_handling_unwrap() {
        let source = r#"
fn dangerous(x: Option<i32>) -> i32 {
    x.unwrap()
}

fn also_dangerous(x: Result<i32, String>) -> i32 {
    x.unwrap()
}
"#;
        let analysis = analyze_file(source, Language::Rust);
        assert!(
            analysis.unwrap_calls >= 1,
            "expected at least 1 unwrap call, got {}",
            analysis.unwrap_calls
        );
    }

    #[test]
    fn test_is_snake_case_empty() {
        assert!(!is_snake_case(""));
    }

    #[test]
    fn test_is_snake_case_single_underscore() {
        assert!(!is_snake_case("_"));
    }

    #[test]
    fn test_is_snake_case_valid() {
        assert!(is_snake_case("my_function"));
        assert!(is_snake_case("get_value"));
        assert!(is_snake_case("process"));
        assert!(is_snake_case("a"));
    }

    #[test]
    fn test_is_snake_case_with_digits() {
        assert!(is_snake_case("get_item_2"));
        assert!(is_snake_case("value3"));
        assert!(is_snake_case("parse_v2_config"));
    }

    #[test]
    fn test_is_snake_case_rejects_uppercase() {
        assert!(!is_snake_case("myFunction"));
        assert!(!is_snake_case("MyClass"));
        assert!(!is_snake_case("getURL"));
    }

    #[test]
    fn test_is_snake_case_leading_underscore_multi_char() {
        // "_foo" starts with '_' but len > 1, so the guard doesn't trigger
        // Then all chars are lowercase/digit/underscore => true
        assert!(is_snake_case("_foo"));
        assert!(is_snake_case("__init__"));
    }

    #[test]
    fn test_is_camel_case_empty() {
        assert!(!is_camel_case(""));
    }

    #[test]
    fn test_is_camel_case_starts_lowercase_has_uppercase() {
        assert!(is_camel_case("myFunction"));
        assert!(is_camel_case("getValue"));
        assert!(is_camel_case("parseJSON"));
    }

    #[test]
    fn test_is_camel_case_all_lowercase() {
        // Starts lowercase but has no uppercase => not camelCase
        assert!(!is_camel_case("process"));
        assert!(!is_camel_case("a"));
        assert!(!is_camel_case("hello"));
    }

    #[test]
    fn test_is_camel_case_starts_uppercase() {
        // PascalCase is not camelCase
        assert!(!is_camel_case("MyClass"));
        assert!(!is_camel_case("HttpRequest"));
    }

    #[test]
    fn test_is_pascal_case_empty() {
        assert!(!is_pascal_case(""));
    }

    #[test]
    fn test_is_pascal_case_starts_uppercase() {
        assert!(is_pascal_case("MyClass"));
        assert!(is_pascal_case("HttpRequest"));
        assert!(is_pascal_case("A"));
        assert!(is_pascal_case("URL"));
    }

    #[test]
    fn test_is_pascal_case_starts_lowercase() {
        assert!(!is_pascal_case("myFunction"));
        assert!(!is_pascal_case("getValue"));
        assert!(!is_pascal_case("_private"));
    }

    #[test]
    fn test_percentile_single_element() {
        assert_eq!(percentile(&[42], 0), 42);
        assert_eq!(percentile(&[42], 50), 42);
        assert_eq!(percentile(&[42], 100), 42);
    }

    #[test]
    fn test_percentile_two_elements() {
        let data = vec![10, 20];
        assert_eq!(percentile(&data, 0), 10);
        assert_eq!(
            percentile(&data, 50),
            15_usize.max(data[((50_f64 / 100.0 * 1.0).round() as usize).min(1)])
        );
        assert_eq!(percentile(&data, 100), 20);
    }

    #[test]
    fn test_percentile_various() {
        let data = vec![1, 2, 3, 4, 5];
        assert_eq!(percentile(&data, 0), 1);
        assert_eq!(percentile(&data, 25), 2); // idx = round(0.25 * 4) = 1 => data[1] = 2
        assert_eq!(percentile(&data, 50), 3); // idx = round(0.5 * 4) = 2 => data[2] = 3
        assert_eq!(percentile(&data, 75), 4); // idx = round(0.75 * 4) = 3 => data[3] = 4
        assert_eq!(percentile(&data, 100), 5); // idx = round(1.0 * 4) = 4 => data[4] = 5
    }

    #[test]
    fn test_is_numeric_literal_matches() {
        assert!(is_numeric_literal("integer_literal"));
        assert!(is_numeric_literal("float_literal"));
        assert!(is_numeric_literal("number"));
        assert!(is_numeric_literal("number_literal"));
    }

    #[test]
    fn test_is_numeric_literal_non_matches() {
        assert!(!is_numeric_literal("string_literal"));
        assert!(!is_numeric_literal("identifier"));
        assert!(!is_numeric_literal("boolean_literal"));
        assert!(!is_numeric_literal(""));
    }

    #[test]
    fn test_is_control_flow_matches() {
        assert!(is_control_flow("if_expression"));
        assert!(is_control_flow("if_statement"));
        assert!(is_control_flow("if_let_expression"));
        assert!(is_control_flow("match_expression"));
        assert!(is_control_flow("guard_statement"));
    }

    #[test]
    fn test_is_control_flow_non_matches() {
        assert!(!is_control_flow("for_statement"));
        assert!(!is_control_flow("while_statement"));
        assert!(!is_control_flow("function_item"));
        assert!(!is_control_flow("block"));
        assert!(!is_control_flow(""));
    }

    #[test]
    fn test_aggregate_single_analysis() {
        let a = FileStyleAnalysis {
            function_count: 3,
            early_return_count: 1,
            snake_case_functions: 3,
            camel_case_functions: 0,
            pascal_case_types: 2,
            other_case_types: 0,
            magic_number_count: 1,
            function_lengths: vec![5, 10, 15],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert_eq!(summary.total_functions, 3);
        assert!((summary.early_return_pct - 33.333).abs() < 1.0);
        assert_eq!(summary.naming_convention_functions, "snake_case");
        assert_eq!(summary.naming_convention_types, "PascalCase");
        assert!(summary.const_preference_pct.is_none());
        assert!(summary.error_handling_style.is_none());
        assert!(summary.avg_function_length > 0.0);
        // With <5 functions, no patterns should be generated (except const/magic ones that also require >=5)
        let style_patterns: Vec<_> = summary
            .patterns
            .iter()
            .filter(|(name, _, _)| {
                name.starts_with("style_early") || name.starts_with("style_naming")
            })
            .collect();
        assert!(
            style_patterns.is_empty(),
            "expected no early_return/naming patterns with <5 functions, got {:?}",
            style_patterns
        );
    }

    #[test]
    fn test_aggregate_naming_snake_case_dominant() {
        let a = FileStyleAnalysis {
            function_count: 10,
            snake_case_functions: 8,
            camel_case_functions: 2,
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // 8/10 = 80% > 70%
        assert_eq!(summary.naming_convention_functions, "snake_case");
    }

    #[test]
    fn test_aggregate_naming_camel_case_dominant() {
        let a = FileStyleAnalysis {
            function_count: 10,
            snake_case_functions: 1,
            camel_case_functions: 9,
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // 9/10 = 90% > 70%
        assert_eq!(summary.naming_convention_functions, "camelCase");
    }

    #[test]
    fn test_aggregate_naming_mixed() {
        let a = FileStyleAnalysis {
            function_count: 10,
            snake_case_functions: 5,
            camel_case_functions: 5,
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // 5/10 = 50%, neither > 70%
        assert_eq!(summary.naming_convention_functions, "mixed");
    }

    #[test]
    fn test_aggregate_naming_unknown_no_functions() {
        let a = FileStyleAnalysis {
            function_count: 0,
            snake_case_functions: 0,
            camel_case_functions: 0,
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert_eq!(summary.naming_convention_functions, "unknown");
    }

    #[test]
    fn test_aggregate_type_naming_pascal_case() {
        let a = FileStyleAnalysis {
            function_count: 5,
            pascal_case_types: 8,
            other_case_types: 2,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // 8/10 = 80% > 70%
        assert_eq!(summary.naming_convention_types, "PascalCase");
    }

    #[test]
    fn test_aggregate_type_naming_mixed() {
        let a = FileStyleAnalysis {
            function_count: 5,
            pascal_case_types: 5,
            other_case_types: 5,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // 5/10 = 50%, not > 70%
        assert_eq!(summary.naming_convention_types, "mixed");
    }

    #[test]
    fn test_aggregate_type_naming_unknown() {
        let a = FileStyleAnalysis {
            function_count: 5,
            pascal_case_types: 0,
            other_case_types: 0,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert_eq!(summary.naming_convention_types, "unknown");
    }

    #[test]
    fn test_aggregate_const_preference_some() {
        let a = FileStyleAnalysis {
            function_count: 5,
            const_declarations: 8,
            let_declarations: 2,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // 8 / 10 * 100 = 80%
        assert!(summary.const_preference_pct.is_some());
        let pct = summary.const_preference_pct.unwrap();
        assert!((pct - 80.0).abs() < 0.1, "expected ~80.0, got {}", pct);
    }

    #[test]
    fn test_aggregate_const_preference_none() {
        let a = FileStyleAnalysis {
            function_count: 5,
            const_declarations: 0,
            let_declarations: 0,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert!(summary.const_preference_pct.is_none());
    }

    #[test]
    fn test_aggregate_const_preference_all_let() {
        let a = FileStyleAnalysis {
            function_count: 5,
            const_declarations: 0,
            let_declarations: 10,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert!(summary.const_preference_pct.is_some());
        let pct = summary.const_preference_pct.unwrap();
        assert!(pct.abs() < 0.1, "expected ~0.0, got {}", pct);
    }

    #[test]
    fn test_aggregate_error_handling_idiomatic() {
        let a = FileStyleAnalysis {
            function_count: 10,
            question_mark_ops: 8,
            unwrap_calls: 1,
            expect_calls: 1,
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // 8/10 = 80% > 70%
        assert!(summary.error_handling_style.is_some());
        assert_eq!(
            summary.error_handling_style.as_deref().unwrap(),
            "? operator (idiomatic)"
        );
    }

    #[test]
    fn test_aggregate_error_handling_frequent_unwrap() {
        let a = FileStyleAnalysis {
            function_count: 10,
            question_mark_ops: 3,
            unwrap_calls: 5,
            expect_calls: 2,
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // question: 3/10 = 30% (not > 70%)
        // unwrap: 5/10 = 50% (> 30%)
        assert!(summary.error_handling_style.is_some());
        assert_eq!(
            summary.error_handling_style.as_deref().unwrap(),
            "mixed (frequent unwrap)"
        );
    }

    #[test]
    fn test_aggregate_error_handling_mixed() {
        let a = FileStyleAnalysis {
            function_count: 10,
            question_mark_ops: 5,
            unwrap_calls: 2,
            expect_calls: 3,
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // question: 5/10 = 50% (not > 70%)
        // unwrap: 2/10 = 20% (not > 30%)
        assert!(summary.error_handling_style.is_some());
        assert_eq!(summary.error_handling_style.as_deref().unwrap(), "mixed");
    }

    #[test]
    fn test_aggregate_error_handling_none() {
        let a = FileStyleAnalysis {
            function_count: 10,
            question_mark_ops: 0,
            unwrap_calls: 0,
            expect_calls: 0,
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert!(summary.error_handling_style.is_none());
    }

    #[test]
    fn test_aggregate_patterns_with_enough_functions() {
        let a = FileStyleAnalysis {
            function_count: 10,
            early_return_count: 9,   // 90% > 70% => early_returns pattern
            snake_case_functions: 9, // 90% snake => naming pattern
            camel_case_functions: 1,
            pascal_case_types: 8,
            other_case_types: 2,   // 80% pascal => naming types pattern
            magic_number_count: 0, // density 0 < 0.1 => magic numbers pattern
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert!(summary.total_functions >= 5);

        let pattern_names: Vec<&str> = summary
            .patterns
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();

        assert!(
            pattern_names.contains(&"style_early_returns"),
            "expected style_early_returns pattern, got {:?}",
            pattern_names
        );
        assert!(
            pattern_names.contains(&"style_naming_functions"),
            "expected style_naming_functions pattern, got {:?}",
            pattern_names
        );
        assert!(
            pattern_names.contains(&"style_naming_types"),
            "expected style_naming_types pattern, got {:?}",
            pattern_names
        );
        assert!(
            pattern_names.contains(&"style_magic_numbers"),
            "expected style_magic_numbers pattern, got {:?}",
            pattern_names
        );
    }

    #[test]
    fn test_aggregate_patterns_camel_case_naming() {
        let a = FileStyleAnalysis {
            function_count: 10,
            early_return_count: 1,
            snake_case_functions: 1,
            camel_case_functions: 9, // 90% camelCase
            pascal_case_types: 0,
            other_case_types: 0,
            magic_number_count: 5, // density 0.5 >= 0.1 => no magic number pattern
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);

        let naming_pattern = summary
            .patterns
            .iter()
            .find(|(name, _, _)| name == "style_naming_functions");
        assert!(
            naming_pattern.is_some(),
            "expected style_naming_functions pattern"
        );
        assert!(
            naming_pattern.unwrap().1.contains("camelCase"),
            "expected camelCase naming description, got {}",
            naming_pattern.unwrap().1
        );
    }

    #[test]
    fn test_aggregate_patterns_not_enough_functions() {
        let a = FileStyleAnalysis {
            function_count: 4,
            early_return_count: 4, // 100% but < 5 functions
            snake_case_functions: 4,
            camel_case_functions: 0,
            pascal_case_types: 4,
            other_case_types: 0,
            magic_number_count: 0,
            function_lengths: vec![5; 4],
            ..Default::default()
        };
        let summary = aggregate(&[a]);

        // With <5 functions, no early_return, naming, or magic_number patterns
        let filtered: Vec<_> = summary
            .patterns
            .iter()
            .filter(|(name, _, _)| {
                name == "style_early_returns"
                    || name == "style_naming_functions"
                    || name == "style_naming_types"
                    || name == "style_magic_numbers"
            })
            .collect();
        assert!(
            filtered.is_empty(),
            "expected no function-count-gated patterns with <5 functions, got {:?}",
            filtered
        );
    }

    #[test]
    fn test_aggregate_magic_number_low_density_pattern() {
        let a = FileStyleAnalysis {
            function_count: 10,
            magic_number_count: 0, // density = 0 < 0.1
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        let has_pattern = summary
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_magic_numbers");
        assert!(has_pattern, "expected style_magic_numbers pattern");
    }

    #[test]
    fn test_aggregate_magic_number_high_density_no_pattern() {
        let a = FileStyleAnalysis {
            function_count: 10,
            magic_number_count: 5, // density = 0.5 >= 0.1
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        let has_pattern = summary
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_magic_numbers");
        assert!(
            !has_pattern,
            "expected no style_magic_numbers pattern with high density"
        );
    }

    #[test]
    fn test_aggregate_p50_p95_function_length() {
        let a = FileStyleAnalysis {
            function_count: 10,
            function_lengths: vec![2, 4, 6, 8, 10, 12, 14, 16, 18, 20],
            ..Default::default()
        };
        let summary = aggregate(&[a]);

        // sorted: [2, 4, 6, 8, 10, 12, 14, 16, 18, 20]
        // p50: idx = round(0.5 * 9) = round(4.5) = 5 => data[4] = 10 (0-indexed: data[5-1]... let's compute)
        // Actually: idx = (50/100 * 9).round() = 4.5.round() = 5 => data[4] = 10 (wait, .round() of 4.5 = 4 or 5?)
        // In Rust, (4.5_f64).round() = 5.0, so idx = 5, data[5] = 12
        assert_eq!(summary.p50_function_length, 12);

        // p95: idx = round(0.95 * 9) = round(8.55) = 9 => data[8] = 18... wait
        // (8.55).round() = 9 => data[9] = 20
        assert_eq!(summary.p95_function_length, 20);
    }

    #[test]
    fn test_aggregate_p50_p95_empty_lengths() {
        let a = FileStyleAnalysis {
            function_count: 0,
            function_lengths: vec![],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert_eq!(summary.p50_function_length, 0);
        assert_eq!(summary.p95_function_length, 0);
        assert_eq!(summary.avg_function_length, 0.0);
    }

    #[test]
    fn test_aggregate_p50_p95_single_length() {
        let a = FileStyleAnalysis {
            function_count: 1,
            function_lengths: vec![42],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        assert_eq!(summary.p50_function_length, 42);
        assert_eq!(summary.p95_function_length, 42);
        assert!((summary.avg_function_length - 42.0).abs() < 0.01);
    }

    #[test]
    fn test_aggregate_combines_multiple_analyses() {
        let a1 = FileStyleAnalysis {
            function_count: 3,
            early_return_count: 1,
            magic_number_count: 2,
            snake_case_functions: 3,
            camel_case_functions: 0,
            pascal_case_types: 1,
            other_case_types: 1,
            const_declarations: 2,
            let_declarations: 1,
            question_mark_ops: 3,
            unwrap_calls: 0,
            expect_calls: 0,
            function_lengths: vec![5, 10, 15],
        };
        let a2 = FileStyleAnalysis {
            function_count: 4,
            early_return_count: 2,
            magic_number_count: 3,
            snake_case_functions: 4,
            camel_case_functions: 0,
            pascal_case_types: 2,
            other_case_types: 0,
            const_declarations: 3,
            let_declarations: 0,
            question_mark_ops: 4,
            unwrap_calls: 1,
            expect_calls: 0,
            function_lengths: vec![8, 12, 20, 25],
        };
        let summary = aggregate(&[a1, a2]);

        assert_eq!(summary.total_functions, 7);
        // early_return: 3/7 = 42.9%
        assert!((summary.early_return_pct - 42.857).abs() < 1.0);
        // magic_number density: 5/7 = 0.714
        assert!((summary.magic_number_density - 0.714).abs() < 0.01);
        // snake: 7, camel: 0 => 100% snake
        assert_eq!(summary.naming_convention_functions, "snake_case");
        // pascal: 3, other: 1 => 75% > 70%
        assert_eq!(summary.naming_convention_types, "PascalCase");
        // const: 5, let: 1 => 83.3%
        assert!(summary.const_preference_pct.is_some());
        assert!((summary.const_preference_pct.unwrap() - 83.333).abs() < 1.0);
        // question: 7, unwrap: 1, expect: 0 => 7/8 = 87.5% > 70% => idiomatic
        assert_eq!(
            summary.error_handling_style.as_deref().unwrap(),
            "? operator (idiomatic)"
        );
        // 7 function lengths total
        assert_eq!(summary.p50_function_length, 12);
    }

    #[test]
    fn test_aggregate_const_preference_pattern_generated() {
        let a = FileStyleAnalysis {
            function_count: 5,
            const_declarations: 8,
            let_declarations: 2,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // const pct = 80% > 70% => pattern
        let has_pattern = summary
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_const_preference");
        assert!(has_pattern, "expected style_const_preference pattern");
    }

    #[test]
    fn test_aggregate_const_preference_pattern_not_generated() {
        let a = FileStyleAnalysis {
            function_count: 5,
            const_declarations: 5,
            let_declarations: 5,
            function_lengths: vec![5; 5],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        // const pct = 50% <= 70% => no pattern
        let has_pattern = summary
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_const_preference");
        assert!(
            !has_pattern,
            "expected no style_const_preference pattern with 50% const"
        );
    }

    #[test]
    fn test_analyze_js_arrow_function_detection() {
        let source = r#"
const processItem = (item) => {
    return item * 2;
};

const filterValid = (items) => {
    return items.filter(i => i > 0);
};
"#;
        let analysis = analyze_file(source, Language::JavaScript);
        // arrow_function nodes: processItem, filterValid, and the inner arrow in filter
        assert!(
            analysis.function_count >= 2,
            "expected at least 2 arrow functions, got {}",
            analysis.function_count
        );
    }

    #[test]
    fn test_analyze_go_method_declaration() {
        let source = r#"
package main

type Handler struct {
    Name string
}

func (h *Handler) handleRequest(r Request) Response {
    if r == nil {
        return Response{}
    }
    return Response{Name: h.Name}
}

func (h *Handler) processData(data []byte) error {
    if len(data) == 0 {
        return nil
    }
    return nil
}
"#;
        let analysis = analyze_file(source, Language::Go);
        // method_declaration: handleRequest, processData
        assert!(
            analysis.function_count >= 2,
            "expected at least 2 method declarations, got {}",
            analysis.function_count
        );
        // camelCase: handleRequest, processData
        assert!(
            analysis.camel_case_functions >= 2,
            "expected at least 2 camelCase functions, got {}",
            analysis.camel_case_functions
        );
    }

    #[test]
    fn test_analyze_typescript_interface_type_detection() {
        let source = r#"
interface UserProfile {
    name: string;
    age: number;
}

interface ApiResponse {
    data: any;
    status: number;
}

class userService {
    getUser(): UserProfile {
        return { name: "test", age: 25 };
    }
}
"#;
        let analysis = analyze_file(source, Language::TypeScript);
        // PascalCase types: UserProfile, ApiResponse (interface_declaration)
        assert!(
            analysis.pascal_case_types >= 2,
            "expected at least 2 PascalCase types (interfaces), got {}",
            analysis.pascal_case_types
        );
        // userService is a class with lowercase start -> other_case_types
        assert!(
            analysis.other_case_types >= 1,
            "expected at least 1 other_case type (lowercase class), got {}",
            analysis.other_case_types
        );
    }

    #[test]
    fn test_analyze_rust_type_nodes() {
        let source = r#"
struct my_struct {
    field: i32,
}

enum MyEnum {
    A,
    B,
}

trait MyTrait {
    fn do_something(&self);
}
"#;
        let analysis = analyze_file(source, Language::Rust);
        // PascalCase types: MyEnum, MyTrait
        assert!(
            analysis.pascal_case_types >= 2,
            "expected at least 2 PascalCase types, got {}",
            analysis.pascal_case_types
        );
        // other_case_types: my_struct (snake_case, not PascalCase)
        assert!(
            analysis.other_case_types >= 1,
            "expected at least 1 other_case type, got {}",
            analysis.other_case_types
        );
    }

    #[test]
    fn test_analyze_rust_no_magic_in_const_context() {
        let source = r#"
const MAX: i32 = 100;
static MIN: i32 = 0;

fn foo() -> i32 {
    MAX
}
"#;
        let analysis = analyze_file(source, Language::Rust);
        // 100 and 0 are in const/static context => not magic numbers
        assert_eq!(
            analysis.magic_number_count, 0,
            "expected 0 magic numbers in const/static context, got {}",
            analysis.magic_number_count
        );
    }

    #[test]
    fn test_analyze_js_const_context_not_magic() {
        let source = r#"
const MAX_SIZE = 100;
const THRESHOLD = 0.5;
"#;
        let analysis = analyze_file(source, Language::JavaScript);
        // Numbers in const declarations should not be magic numbers
        assert_eq!(
            analysis.magic_number_count, 0,
            "expected 0 magic numbers in JS const context, got {}",
            analysis.magic_number_count
        );
    }

    #[test]
    fn test_analyze_whitespace_only_source() {
        let source = "   \n\n\t\t  \n  ";
        let analysis = analyze_file(source, Language::Rust);
        assert_eq!(analysis.function_count, 0);
        assert_eq!(analysis.magic_number_count, 0);
    }

    #[test]
    fn test_aggregate_zero_functions_no_division_by_zero() {
        let summary = aggregate(&[]);
        assert_eq!(summary.early_return_pct, 0.0);
        assert_eq!(summary.magic_number_density, 0.0);
        assert_eq!(summary.avg_function_length, 0.0);
        assert_eq!(summary.p50_function_length, 0);
        assert_eq!(summary.p95_function_length, 0);
        assert_eq!(summary.naming_convention_functions, "unknown");
        assert_eq!(summary.naming_convention_types, "unknown");
        assert!(summary.const_preference_pct.is_none());
        assert!(summary.error_handling_style.is_none());
        assert!(summary.patterns.is_empty());
    }

    #[test]
    fn test_aggregate_early_return_boundary() {
        // Exactly 70% should NOT trigger pattern (requires >70%)
        let a = FileStyleAnalysis {
            function_count: 10,
            early_return_count: 7, // 70% exactly, not > 70%
            function_lengths: vec![5; 10],
            ..Default::default()
        };
        let summary = aggregate(&[a]);
        let has_pattern = summary
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_early_returns");
        assert!(
            !has_pattern,
            "expected no style_early_returns at exactly 70%"
        );

        // 71% should trigger
        let a2 = FileStyleAnalysis {
            function_count: 100,
            early_return_count: 71,
            function_lengths: vec![5; 100],
            ..Default::default()
        };
        let summary2 = aggregate(&[a2]);
        let has_pattern2 = summary2
            .patterns
            .iter()
            .any(|(name, _, _)| name == "style_early_returns");
        assert!(has_pattern2, "expected style_early_returns at 71%");
    }
}
