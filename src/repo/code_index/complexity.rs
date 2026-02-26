//! Codebase complexity metrics computed from tree-sitter ASTs.
//!
//! Calculates cyclomatic complexity, function length, nesting depth, and file size
//! metrics for strategy selection (complex areas = more investigation time).

use super::languages;
use super::types::Language;

/// Complexity metrics for a single function.
#[derive(Debug, Clone, Default)]
pub struct FunctionComplexity {
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub cyclomatic: u32,
    pub line_count: u32,
    pub max_nesting: u32,
}

/// Complexity metrics for a single file.
#[derive(Debug, Clone, Default)]
pub struct FileComplexity {
    pub file_path: String,
    pub total_lines: u32,
    pub function_count: u32,
    pub functions: Vec<FunctionComplexity>,
    pub avg_cyclomatic: f64,
    pub max_cyclomatic: f64,
    pub avg_func_length: f64,
    pub max_func_length: f64,
    pub avg_nesting: f64,
    pub max_nesting: f64,
}

/// Aggregated complexity metrics across a repository.
#[derive(Debug, Clone, Default)]
pub struct RepoComplexity {
    pub total_files: usize,
    pub total_functions: usize,
    pub total_lines: u64,
    pub avg_cyclomatic: f64,
    pub p50_cyclomatic: f64,
    pub p95_cyclomatic: f64,
    pub max_cyclomatic: f64,
    pub avg_func_length: f64,
    pub p50_func_length: f64,
    pub p95_func_length: f64,
    pub max_func_length: f64,
    pub avg_nesting: f64,
    pub max_nesting: f64,
    pub avg_file_size: f64,
    pub p50_file_size: f64,
    pub p95_file_size: f64,
}

/// Analyze complexity of a single file.
pub fn analyze_file(source: &str, language: Language, file_path: &str) -> FileComplexity {
    let total_lines = source.lines().count() as u32;

    let Some(ts_lang) = languages::ts_language(language) else {
        return FileComplexity {
            file_path: file_path.to_string(),
            total_lines,
            ..Default::default()
        };
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return FileComplexity {
            file_path: file_path.to_string(),
            total_lines,
            ..Default::default()
        };
    }

    let Some(tree) = parser.parse(source, None) else {
        return FileComplexity {
            file_path: file_path.to_string(),
            total_lines,
            ..Default::default()
        };
    };

    let root = tree.root_node();
    let source_bytes = source.as_bytes();
    let mut functions = Vec::new();

    collect_functions(root, source_bytes, language, &mut functions, 0);

    let function_count = functions.len() as u32;

    let avg_cyclomatic = if functions.is_empty() {
        0.0
    } else {
        functions.iter().map(|f| f.cyclomatic as f64).sum::<f64>() / functions.len() as f64
    };

    let max_cyclomatic = functions
        .iter()
        .map(|f| f.cyclomatic as f64)
        .fold(0.0f64, f64::max);

    let avg_func_length = if functions.is_empty() {
        0.0
    } else {
        functions.iter().map(|f| f.line_count as f64).sum::<f64>() / functions.len() as f64
    };

    let max_func_length = functions
        .iter()
        .map(|f| f.line_count as f64)
        .fold(0.0f64, f64::max);

    let avg_nesting = if functions.is_empty() {
        0.0
    } else {
        functions.iter().map(|f| f.max_nesting as f64).sum::<f64>() / functions.len() as f64
    };

    let max_nesting = functions
        .iter()
        .map(|f| f.max_nesting as f64)
        .fold(0.0f64, f64::max);

    FileComplexity {
        file_path: file_path.to_string(),
        total_lines,
        function_count,
        functions,
        avg_cyclomatic,
        max_cyclomatic,
        avg_func_length,
        max_func_length,
        avg_nesting,
        max_nesting,
    }
}

/// Aggregate file-level complexity into repo-level stats.
pub fn aggregate(files: &[FileComplexity]) -> RepoComplexity {
    if files.is_empty() {
        return RepoComplexity::default();
    }

    let total_files = files.len();
    let total_functions: usize = files.iter().map(|f| f.function_count as usize).sum();
    let total_lines: u64 = files.iter().map(|f| f.total_lines as u64).sum();

    // Collect all function-level metrics
    let mut all_cyclomatic: Vec<f64> = files
        .iter()
        .flat_map(|f| f.functions.iter().map(|func| func.cyclomatic as f64))
        .collect();
    all_cyclomatic.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut all_func_lengths: Vec<f64> = files
        .iter()
        .flat_map(|f| f.functions.iter().map(|func| func.line_count as f64))
        .collect();
    all_func_lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut all_nesting: Vec<f64> = files
        .iter()
        .flat_map(|f| f.functions.iter().map(|func| func.max_nesting as f64))
        .collect();
    all_nesting.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut all_file_sizes: Vec<f64> = files.iter().map(|f| f.total_lines as f64).collect();
    all_file_sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    RepoComplexity {
        total_files,
        total_functions,
        total_lines,
        avg_cyclomatic: avg(&all_cyclomatic),
        p50_cyclomatic: percentile_f64(&all_cyclomatic, 50),
        p95_cyclomatic: percentile_f64(&all_cyclomatic, 95),
        max_cyclomatic: all_cyclomatic.last().copied().unwrap_or(0.0),
        avg_func_length: avg(&all_func_lengths),
        p50_func_length: percentile_f64(&all_func_lengths, 50),
        p95_func_length: percentile_f64(&all_func_lengths, 95),
        max_func_length: all_func_lengths.last().copied().unwrap_or(0.0),
        avg_nesting: avg(&all_nesting),
        max_nesting: all_nesting.last().copied().unwrap_or(0.0),
        avg_file_size: avg(&all_file_sizes),
        p50_file_size: percentile_f64(&all_file_sizes, 50),
        p95_file_size: percentile_f64(&all_file_sizes, 95),
    }
}

fn collect_functions(
    node: tree_sitter::Node,
    source: &[u8],
    language: Language,
    functions: &mut Vec<FunctionComplexity>,
    _depth: usize,
) {
    let node_type = node.kind();

    if is_function_node(language, node_type) {
        let name =
            extract_name(node, source, language).unwrap_or_else(|| "<anonymous>".to_string());
        let start_line = node.start_position().row as u32;
        let end_line = node.end_position().row as u32;
        let line_count = end_line.saturating_sub(start_line) + 1;

        let cyclomatic = compute_cyclomatic(node, language);
        let max_nesting = compute_max_nesting(node, language, 0);

        functions.push(FunctionComplexity {
            name,
            start_line,
            end_line,
            cyclomatic,
            line_count,
            max_nesting,
        });

        // Don't recurse into nested functions for top-level collection
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions(child, source, language, functions, _depth + 1);
    }
}

/// Compute cyclomatic complexity by counting decision points.
fn compute_cyclomatic(node: tree_sitter::Node, language: Language) -> u32 {
    let mut complexity: u32 = 1; // Base complexity

    fn walk(node: tree_sitter::Node, language: Language, complexity: &mut u32) {
        let kind = node.kind();

        if is_decision_point(kind, language) {
            *complexity += 1;
        }

        // Count logical operators as decision points
        if is_logical_operator(kind) {
            *complexity += 1;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            // Don't count nested function definitions
            if !is_function_node(language, child.kind()) || child.id() == node.id() {
                walk(child, language, complexity);
            }
        }
    }

    // Walk the function body, not the function itself
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, language, &mut complexity);
    } else {
        walk(node, language, &mut complexity);
    }

    complexity
}

/// Compute maximum nesting depth within a node.
fn compute_max_nesting(node: tree_sitter::Node, language: Language, current_depth: u32) -> u32 {
    let mut max_depth = current_depth;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Don't descend into nested functions
        if is_function_node(language, child.kind()) && child.id() != node.id() {
            continue;
        }

        let child_depth = if is_nesting_node(child.kind(), language) {
            current_depth + 1
        } else {
            current_depth
        };

        let nested = compute_max_nesting(child, language, child_depth);
        max_depth = max_depth.max(nested);
    }

    max_depth
}

fn is_decision_point(kind: &str, _language: Language) -> bool {
    matches!(
        kind,
        "if_expression"
            | "if_statement"
            | "if_let_expression"
            | "else_clause"
            | "elif_clause"
            | "match_expression"
            | "match_arm"
            | "switch_statement"
            | "switch_case"
            | "case_clause"
            | "for_expression"
            | "for_statement"
            | "for_in_statement"
            | "while_expression"
            | "while_statement"
            | "do_statement"
            | "catch_clause"
            | "except_clause"
            | "ternary_expression"
            | "conditional_expression"
            | "when_entry"
            | "guard_statement"
    )
}

fn is_logical_operator(kind: &str) -> bool {
    matches!(kind, "&&" | "||" | "and" | "or")
}

fn is_nesting_node(kind: &str, _language: Language) -> bool {
    matches!(
        kind,
        "if_expression"
            | "if_statement"
            | "if_let_expression"
            | "for_expression"
            | "for_statement"
            | "for_in_statement"
            | "while_expression"
            | "while_statement"
            | "do_statement"
            | "match_expression"
            | "switch_statement"
            | "try_expression"
            | "try_statement"
            | "block"
            | "closure_expression"
            | "lambda"
    )
}

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
        Language::CSharp => matches!(node_type, "method_declaration" | "constructor_declaration"),
        Language::Dart => matches!(
            node_type,
            "function_signature" | "method_signature" | "getter_signature" | "setter_signature"
        ),
        Language::Lua => matches!(
            node_type,
            "function_declaration" | "local_function" | "function_definition_statement"
        ),
        Language::Json | Language::Yaml | Language::Dockerfile => false,
    }
}

fn extract_name(node: tree_sitter::Node, source: &[u8], language: Language) -> Option<String> {
    let field = languages::name_field(language, node.kind())?;
    let name_node = node.child_by_field_name(field)?;
    let start = name_node.start_byte();
    let end = name_node.end_byte().min(source.len());
    if start >= end {
        return None;
    }
    Some(String::from_utf8_lossy(&source[start..end]).to_string())
}

fn avg(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn percentile_f64(sorted: &[f64], pct: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (pct as f64 / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_rust_function() {
        let source = r#"
fn simple() -> i32 {
    42
}
"#;
        let result = analyze_file(source, Language::Rust, "test.rs");
        assert_eq!(result.function_count, 1);
        assert_eq!(result.functions[0].cyclomatic, 1); // No decision points
    }

    #[test]
    fn test_complex_rust_function() {
        let source = r#"
fn complex(x: i32, y: bool) -> i32 {
    if x > 0 {
        if y {
            for i in 0..x {
                if i % 2 == 0 {
                    println!("{}", i);
                }
            }
        }
        x
    } else {
        0
    }
}
"#;
        let result = analyze_file(source, Language::Rust, "test.rs");
        assert_eq!(result.function_count, 1);
        assert!(result.functions[0].cyclomatic > 1);
        assert!(result.functions[0].max_nesting >= 2);
    }

    #[test]
    fn test_multiple_functions() {
        let source = r#"
fn foo() -> i32 { 1 }
fn bar() -> i32 { 2 }
fn baz() -> i32 { 3 }
"#;
        let result = analyze_file(source, Language::Rust, "test.rs");
        assert_eq!(result.function_count, 3);
    }

    #[test]
    fn test_aggregate_empty() {
        let result = aggregate(&[]);
        assert_eq!(result.total_files, 0);
        assert_eq!(result.total_functions, 0);
    }

    #[test]
    fn test_aggregate_basic() {
        let f1 = FileComplexity {
            file_path: "a.rs".into(),
            total_lines: 100,
            function_count: 5,
            functions: vec![
                FunctionComplexity {
                    name: "a".into(),
                    cyclomatic: 1,
                    line_count: 5,
                    max_nesting: 0,
                    ..Default::default()
                },
                FunctionComplexity {
                    name: "b".into(),
                    cyclomatic: 3,
                    line_count: 15,
                    max_nesting: 2,
                    ..Default::default()
                },
                FunctionComplexity {
                    name: "c".into(),
                    cyclomatic: 5,
                    line_count: 30,
                    max_nesting: 3,
                    ..Default::default()
                },
                FunctionComplexity {
                    name: "d".into(),
                    cyclomatic: 2,
                    line_count: 10,
                    max_nesting: 1,
                    ..Default::default()
                },
                FunctionComplexity {
                    name: "e".into(),
                    cyclomatic: 1,
                    line_count: 3,
                    max_nesting: 0,
                    ..Default::default()
                },
            ],
            avg_cyclomatic: 2.4,
            max_cyclomatic: 5.0,
            avg_func_length: 12.6,
            max_func_length: 30.0,
            avg_nesting: 1.2,
            max_nesting: 3.0,
        };

        let result = aggregate(&[f1]);
        assert_eq!(result.total_files, 1);
        assert_eq!(result.total_functions, 5);
        assert!(result.avg_cyclomatic > 0.0);
        assert!(result.max_cyclomatic >= 5.0);
    }

    #[test]
    fn test_percentile_f64() {
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((percentile_f64(&data, 50) - 3.0).abs() < 0.01);
    }

    #[test]
    fn test_javascript_complexity() {
        let source = r#"
function processData(items) {
    for (const item of items) {
        if (item.valid) {
            if (item.type === 'a') {
                handleA(item);
            } else if (item.type === 'b') {
                handleB(item);
            }
        }
    }
}
"#;
        let result = analyze_file(source, Language::JavaScript, "test.js");
        assert_eq!(result.function_count, 1);
        assert!(result.functions[0].cyclomatic > 1);
    }

    #[test]
    fn test_python_complexity_simple() {
        let source = r#"
def greet(name):
    return "Hello, " + name
"#;
        let result = analyze_file(source, Language::Python, "simple.py");
        assert_eq!(result.function_count, 1);
        assert_eq!(
            result.functions[0].cyclomatic, 1,
            "simple function should have cyclomatic complexity of 1"
        );
        assert_eq!(result.functions[0].name, "greet");
    }

    #[test]
    fn test_python_complexity_branching() {
        let source = r#"
def classify(value):
    if value < 0:
        return "negative"
    elif value == 0:
        return "zero"
    elif value < 10:
        return "small"
    else:
        return "large"

def process_items(items):
    results = []
    for item in items:
        if item is not None:
            if isinstance(item, str):
                results.append(item.upper())
            else:
                results.append(str(item))
    return results
"#;
        let result = analyze_file(source, Language::Python, "branching.py");
        assert_eq!(result.function_count, 2);

        // classify: if + elif + elif + else = several decision points
        let classify = &result.functions[0];
        assert_eq!(classify.name, "classify");
        assert!(
            classify.cyclomatic > 1,
            "classify should have cyclomatic > 1, got {}",
            classify.cyclomatic
        );

        // process_items: for + if + if = several decision points and nesting
        let process = &result.functions[1];
        assert_eq!(process.name, "process_items");
        assert!(
            process.cyclomatic > 1,
            "process_items should have cyclomatic > 1, got {}",
            process.cyclomatic
        );
        assert!(
            process.max_nesting >= 2,
            "process_items should have nesting >= 2, got {}",
            process.max_nesting
        );

        // File-level aggregates
        assert!(result.avg_cyclomatic > 1.0);
        assert!(result.max_cyclomatic >= classify.cyclomatic as f64);
    }

    #[test]
    fn test_typescript_complexity_simple() {
        let source = r#"
function greet(name: string): string {
    return name;
}
"#;
        let result = analyze_file(source, Language::TypeScript, "simple.ts");
        assert_eq!(result.function_count, 1);
        assert_eq!(result.functions[0].name, "greet");
        // A function with no branching or binary expressions has base complexity of 1.
        assert_eq!(
            result.functions[0].cyclomatic, 1,
            "simple TS function should have cyclomatic complexity of 1"
        );
    }

    #[test]
    fn test_typescript_complexity_branching() {
        let source = r#"
function handleRequest(req: Request): Response {
    if (!req.body) {
        return new Response("no body", { status: 400 });
    }
    if (req.method === "GET") {
        return handleGet(req);
    } else if (req.method === "POST") {
        return handlePost(req);
    } else {
        return new Response("not found", { status: 404 });
    }
}

function processArray(items: number[]): number[] {
    const result: number[] = [];
    for (const item of items) {
        if (item > 0) {
            if (item % 2 === 0) {
                result.push(item * 2);
            } else {
                result.push(item);
            }
        }
    }
    return result;
}
"#;
        let result = analyze_file(source, Language::TypeScript, "handler.ts");
        assert_eq!(result.function_count, 2);

        let handle_request = &result.functions[0];
        assert_eq!(handle_request.name, "handleRequest");
        assert!(
            handle_request.cyclomatic > 2,
            "handleRequest should have cyclomatic > 2, got {}",
            handle_request.cyclomatic
        );

        let process_array = &result.functions[1];
        assert_eq!(process_array.name, "processArray");
        assert!(
            process_array.cyclomatic > 1,
            "processArray should have cyclomatic > 1, got {}",
            process_array.cyclomatic
        );
        assert!(
            process_array.max_nesting >= 2,
            "processArray should have nesting >= 2, got {}",
            process_array.max_nesting
        );

        // File-level metrics
        assert!(result.avg_cyclomatic > 1.0);
        assert!(result.max_cyclomatic >= handle_request.cyclomatic as f64);
    }

    #[test]
    fn test_aggregate_multiple_files() {
        let rust_file = analyze_file(
            r#"
fn simple() -> i32 {
    42
}

fn branching(x: i32) -> &'static str {
    if x > 0 {
        if x > 100 {
            "big"
        } else {
            "small"
        }
    } else {
        "negative"
    }
}
"#,
            Language::Rust,
            "lib.rs",
        );

        let js_file = analyze_file(
            r#"
function validate(input) {
    if (!input) return false;
    if (typeof input !== 'string') return false;
    return input.length > 0;
}

function transform(items) {
    const results = [];
    for (const item of items) {
        if (item.active) {
            results.push(item.value);
        }
    }
    return results;
}
"#,
            Language::JavaScript,
            "utils.js",
        );

        let python_file = analyze_file(
            r#"
def factorial(n):
    if n <= 1:
        return 1
    return n * factorial(n - 1)

def fizzbuzz(n):
    for i in range(1, n + 1):
        if i % 15 == 0:
            print("FizzBuzz")
        elif i % 3 == 0:
            print("Fizz")
        elif i % 5 == 0:
            print("Buzz")
        else:
            print(i)
"#,
            Language::Python,
            "main.py",
        );

        // Verify individual file metrics before aggregation
        assert_eq!(rust_file.function_count, 2);
        assert!(js_file.function_count >= 2);
        assert_eq!(python_file.function_count, 2);

        let agg = aggregate(&[rust_file, js_file, python_file]);

        // Total files
        assert_eq!(agg.total_files, 3);

        // Total functions: 2 + 2 + 2 = 6 (minimum)
        assert!(
            agg.total_functions >= 6,
            "expected at least 6 total functions, got {}",
            agg.total_functions
        );

        // Total lines should be positive
        assert!(agg.total_lines > 0);

        // Averages should be reasonable
        assert!(
            agg.avg_cyclomatic >= 1.0,
            "avg cyclomatic should be >= 1.0, got {}",
            agg.avg_cyclomatic
        );
        assert!(
            agg.avg_func_length > 0.0,
            "avg func length should be > 0, got {}",
            agg.avg_func_length
        );

        // Max should be at least as large as average
        assert!(agg.max_cyclomatic >= agg.avg_cyclomatic);
        assert!(agg.max_func_length >= agg.avg_func_length);

        // Percentiles: p50 <= p95
        assert!(
            agg.p50_cyclomatic <= agg.p95_cyclomatic,
            "p50 ({}) should be <= p95 ({})",
            agg.p50_cyclomatic,
            agg.p95_cyclomatic
        );
        assert!(
            agg.p50_func_length <= agg.p95_func_length,
            "p50 func length ({}) should be <= p95 ({})",
            agg.p50_func_length,
            agg.p95_func_length
        );

        // File size stats
        assert!(agg.avg_file_size > 0.0);
        assert!(agg.p50_file_size > 0.0);
        assert!(agg.p50_file_size <= agg.p95_file_size);
    }

    #[test]
    fn test_lua_simple_function() {
        let source = r#"
function greet(name)
    print("Hello, " .. name)
end
"#;
        let result = analyze_file(source, Language::Lua, "greet.lua");
        assert_eq!(result.function_count, 1);
        assert_eq!(result.functions[0].cyclomatic, 1);
    }

    #[test]
    fn test_lua_branching_function() {
        let source = r#"
function classify(x)
    if x > 0 then
        if x > 100 then
            return "big"
        else
            return "small"
        end
    else
        return "negative"
    end
end
"#;
        let result = analyze_file(source, Language::Lua, "classify.lua");
        assert_eq!(result.function_count, 1);
        assert!(
            result.functions[0].cyclomatic > 1,
            "Expected cyclomatic > 1 for branching Lua function, got {}",
            result.functions[0].cyclomatic
        );
    }

    #[test]
    fn test_lua_multiple_functions() {
        let source = r#"
function foo()
    return 1
end

local function bar(x)
    if x then
        return true
    end
    return false
end

function baz()
    return 3
end
"#;
        let result = analyze_file(source, Language::Lua, "multi.lua");
        assert_eq!(result.function_count, 3);
    }

    #[test]
    fn test_lua_loop_complexity() {
        let source = r#"
function process(items)
    for i = 1, #items do
        if items[i] > 0 then
            print(items[i])
        end
    end
end
"#;
        let result = analyze_file(source, Language::Lua, "loop.lua");
        assert_eq!(result.function_count, 1);
        assert!(
            result.functions[0].cyclomatic > 1,
            "Expected cyclomatic > 1 for loop + branch, got {}",
            result.functions[0].cyclomatic
        );
        assert!(
            result.functions[0].max_nesting >= 1,
            "Expected nesting >= 1, got {}",
            result.functions[0].max_nesting
        );
    }

    #[test]
    fn test_lua_empty_file() {
        let source = "-- just a comment\nlocal x = 42\n";
        let result = analyze_file(source, Language::Lua, "empty.lua");
        assert_eq!(result.function_count, 0);
        assert!(result.functions.is_empty());
        assert!(result.total_lines > 0);
    }

    #[test]
    fn test_json_complexity_returns_no_functions() {
        let source = r#"
{
    "name": "my-package",
    "version": "1.0.0",
    "scripts": {
        "build": "tsc",
        "test": "jest"
    }
}
"#;
        let result = analyze_file(source, Language::Json, "package.json");
        assert_eq!(result.function_count, 0);
        assert!(result.functions.is_empty());
        assert!(result.total_lines > 0);
        assert_eq!(result.avg_cyclomatic, 0.0);
        assert_eq!(result.max_cyclomatic, 0.0);
    }

    #[test]
    fn test_json_empty_object() {
        let source = "{}";
        let result = analyze_file(source, Language::Json, "empty.json");
        assert_eq!(result.function_count, 0);
        assert_eq!(result.total_lines, 1);
    }

    #[test]
    fn test_yaml_complexity_returns_no_functions() {
        let source = r#"
name: CI
on:
  push:
    branches: [main]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo test
"#;
        let result = analyze_file(source, Language::Yaml, "ci.yaml");
        assert_eq!(result.function_count, 0);
        assert!(result.functions.is_empty());
        assert!(result.total_lines > 0);
        assert_eq!(result.avg_cyclomatic, 0.0);
    }

    #[test]
    fn test_yaml_simple_config() {
        let source = "host: localhost\nport: 5432\n";
        let result = analyze_file(source, Language::Yaml, "config.yml");
        assert_eq!(result.function_count, 0);
        assert_eq!(result.total_lines, 2);
    }

    #[test]
    fn test_dockerfile_complexity_returns_no_functions() {
        // Dockerfile has no tree-sitter grammar — should return basic file metrics only
        let source = r#"
FROM rust:1.75-slim as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/app /usr/local/bin/
CMD ["app"]
"#;
        let result = analyze_file(source, Language::Dockerfile, "Dockerfile");
        assert_eq!(result.function_count, 0);
        assert!(result.functions.is_empty());
        assert!(result.total_lines > 0);
        assert_eq!(result.avg_cyclomatic, 0.0);
        assert_eq!(result.max_cyclomatic, 0.0);
        assert_eq!(result.avg_func_length, 0.0);
    }

    #[test]
    fn test_aggregate_with_data_format_files() {
        // Mixing real code files with data format files
        let lua_file = analyze_file(
            r#"
function hello()
    print("hello")
end
"#,
            Language::Lua,
            "hello.lua",
        );

        let json_file = analyze_file(
            r#"{"key": "value"}"#,
            Language::Json,
            "config.json",
        );

        let yaml_file = analyze_file(
            "key: value\n",
            Language::Yaml,
            "config.yaml",
        );

        let dockerfile = analyze_file(
            "FROM rust:1.75\nRUN cargo build\n",
            Language::Dockerfile,
            "Dockerfile",
        );

        let agg = aggregate(&[lua_file, json_file, yaml_file, dockerfile]);
        assert_eq!(agg.total_files, 4);
        // Only the Lua file has functions
        assert!(agg.total_functions >= 1);
        assert!(agg.total_lines > 0);
    }

    #[test]
    fn test_lua_deeply_nested_function() {
        let source = r#"
function deep(x)
    if x > 0 then
        for i = 1, x do
            if i % 2 == 0 then
                while i > 1 do
                    i = i - 1
                end
            end
        end
    end
end
"#;
        let result = analyze_file(source, Language::Lua, "deep.lua");
        assert_eq!(result.function_count, 1);
        assert!(
            result.functions[0].max_nesting >= 2,
            "Expected deep nesting >= 2, got {}",
            result.functions[0].max_nesting
        );
        assert!(
            result.functions[0].cyclomatic > 2,
            "Expected cyclomatic > 2 for deeply nested code, got {}",
            result.functions[0].cyclomatic
        );
    }

    #[test]
    fn test_lua_function_with_logical_operators() {
        let source = r#"
function check(a, b, c)
    if a and b then
        return true
    end
    if a or c then
        return true
    end
    return false
end
"#;
        let result = analyze_file(source, Language::Lua, "logical.lua");
        assert_eq!(result.function_count, 1);
        // if + and + if + or = at least 4 decision points on top of base 1
        assert!(
            result.functions[0].cyclomatic >= 3,
            "Expected cyclomatic >= 3 with logical operators, got {}",
            result.functions[0].cyclomatic
        );
    }

    #[test]
    fn test_lua_function_line_count() {
        let source = r#"
function short()
    return 1
end

function medium(x)
    local a = x + 1
    local b = a * 2
    local c = b - 3
    local d = c / 4
    return d
end
"#;
        let result = analyze_file(source, Language::Lua, "lines.lua");
        assert_eq!(result.function_count, 2);

        let short = &result.functions[0];
        let medium = &result.functions[1];
        assert!(
            medium.line_count > short.line_count,
            "Medium function ({} lines) should be longer than short ({} lines)",
            medium.line_count,
            short.line_count
        );
        assert!(result.avg_func_length > 0.0);
        assert!(result.max_func_length >= medium.line_count as f64);
    }

    #[test]
    fn test_lua_local_function_complexity() {
        let source = r#"
local function process(items)
    local results = {}
    for _, item in ipairs(items) do
        if item.valid then
            if item.priority > 5 then
                table.insert(results, item)
            end
        end
    end
    return results
end
"#;
        let result = analyze_file(source, Language::Lua, "local.lua");
        assert_eq!(result.function_count, 1);
        assert!(
            result.functions[0].cyclomatic > 1,
            "Expected cyclomatic > 1, got {}",
            result.functions[0].cyclomatic
        );
    }

    #[test]
    fn test_json_preserves_total_lines() {
        let source = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n";
        let result = analyze_file(source, Language::Json, "small.json");
        assert_eq!(result.total_lines, 5);
        assert_eq!(result.function_count, 0);
        assert_eq!(result.avg_cyclomatic, 0.0);
    }

    #[test]
    fn test_yaml_preserves_total_lines() {
        let source = "a: 1\nb: 2\nc: 3\nd: 4\n";
        let result = analyze_file(source, Language::Yaml, "small.yml");
        assert_eq!(result.total_lines, 4);
        assert_eq!(result.function_count, 0);
    }

    #[test]
    fn test_dockerfile_preserves_total_lines() {
        let source = "FROM alpine\nRUN echo hello\nCMD sleep 1\n";
        let result = analyze_file(source, Language::Dockerfile, "Dockerfile");
        assert_eq!(result.total_lines, 3);
        assert_eq!(result.function_count, 0);
    }

    #[test]
    fn test_data_format_complexity_all_zero() {
        for (lang, src, path) in [
            (Language::Json, r#"{"key": "value"}"#, "test.json"),
            (Language::Yaml, "key: value\n", "test.yml"),
            (Language::Dockerfile, "FROM alpine\n", "Dockerfile"),
        ] {
            let result = analyze_file(src, lang, path);
            assert_eq!(
                result.function_count, 0,
                "{:?} should have 0 functions",
                lang
            );
            assert_eq!(
                result.avg_cyclomatic, 0.0,
                "{:?} should have 0 avg cyclomatic",
                lang
            );
            assert_eq!(
                result.max_cyclomatic, 0.0,
                "{:?} should have 0 max cyclomatic",
                lang
            );
            assert_eq!(
                result.avg_func_length, 0.0,
                "{:?} should have 0 avg func length",
                lang
            );
            assert_eq!(
                result.avg_nesting, 0.0,
                "{:?} should have 0 avg nesting",
                lang
            );
        }
    }
}
