//! Tree-sitter based source code parsing and symbol extraction.

use super::languages::{classify_node, is_container, name_field, ts_language};
use super::types::{CodeSymbol, Language, SymbolKind};
use crate::error::Result;
use tree_sitter::{Parser, Tree};

/// Parse a source file into a tree-sitter AST.
pub fn parse_file(source: &str, language: Language) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language(language))
        .map_err(|e| crate::error::Error::Other(format!("Failed to set language: {}", e)))?;

    parser
        .parse(source, None)
        .ok_or_else(|| crate::error::Error::Other("Tree-sitter parse returned None".into()))
}

/// Extract all symbols from a parsed AST.
pub fn extract_symbols(
    tree: &Tree,
    source: &[u8],
    language: Language,
    repo_id: i64,
    file_path: &str,
) -> Vec<CodeSymbol> {
    let mut symbols = Vec::new();
    let root = tree.root_node();
    extract_symbols_recursive(
        root,
        source,
        language,
        repo_id,
        file_path,
        None, // no parent
        &mut symbols,
    );
    symbols
}

fn extract_symbols_recursive(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    language: Language,
    repo_id: i64,
    file_path: &str,
    parent_name: Option<&str>,
    symbols: &mut Vec<CodeSymbol>,
) {
    let node_type = node.kind();

    if let Some(mut kind) = classify_node(language, node_type) {
        // If this is a function/method inside a container, reclassify as Method.
        if kind == SymbolKind::Function && parent_name.is_some() {
            kind = SymbolKind::Method;
        }

        let symbol_name = extract_name(node, source, language, node_type);
        let signature = extract_signature(node, source, language, node_type);

        if let Some(name) = symbol_name {
            symbols.push(CodeSymbol {
                id: None,
                repo_id,
                file_path: file_path.to_string(),
                symbol_name: name.clone(),
                symbol_kind: kind,
                parent_symbol: parent_name.map(|s| s.to_string()),
                language,
                start_line: node.start_position().row + 1, // 1-indexed
                end_line: node.end_position().row + 1,
                signature,
            });

            // If this is a container, recurse into children with this as parent.
            if is_container(language, node_type) {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    extract_symbols_recursive(
                        child,
                        source,
                        language,
                        repo_id,
                        file_path,
                        Some(&name),
                        symbols,
                    );
                }
                return; // Don't double-recurse
            }
        }
    }

    // Recurse into children for non-container nodes.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_symbols_recursive(
            child,
            source,
            language,
            repo_id,
            file_path,
            parent_name,
            symbols,
        );
    }
}

/// Extract the name of a symbol from its AST node.
fn extract_name(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    language: Language,
    node_type: &str,
) -> Option<String> {
    // C/C++ function_definition needs special handling: the "declarator" field
    // points to a function_declarator which includes parameter list text.
    // We must drill down to the identifier.
    if matches!(language, Language::C | Language::Cpp) && node_type == "function_definition" {
        if let Some(decl) = node.child_by_field_name("declarator") {
            return extract_c_function_name(decl, source);
        }
    }

    // For Go type_declaration, dig into type_spec children.
    if language == Language::Go && node_type == "type_declaration" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "type_spec" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    return Some(node_text(name_node, source));
                }
            }
        }
    }

    // Try the language-specific field name.
    if let Some(field) = name_field(language, node_type) {
        if let Some(name_node) = node.child_by_field_name(field) {
            return Some(node_text(name_node, source));
        }
    }

    None
}

/// Drill into C/C++ declarator nesting to find the identifier.
fn extract_c_function_name(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(node, source)),
        "function_declarator" | "pointer_declarator" | "parenthesized_declarator" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                extract_c_function_name(decl, source)
            } else {
                // Fall back: scan all children (including unnamed) for an identifier
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "identifier" || child.kind() == "field_identifier" {
                        return Some(node_text(child, source));
                    }
                }
                // Then try recursive extraction on named children
                let mut cursor2 = node.walk();
                for child in node.named_children(&mut cursor2) {
                    if let Some(name) = extract_c_function_name(child, source) {
                        return Some(name);
                    }
                }
                None
            }
        }
        "qualified_identifier" | "template_function" | "destructor_name" => {
            Some(node_text(node, source))
        }
        _ => None,
    }
}

/// Extract the signature line(s) of a symbol (everything before the body).
fn extract_signature(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    _language: Language,
    _node_type: &str,
) -> Option<String> {
    // Find the body child (usually "block", "body", "declaration_list", etc.)
    let body_start = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("block"))
        .map(|b| b.start_byte());

    let sig_end = body_start.unwrap_or(node.end_byte());
    let sig_start = node.start_byte();

    if sig_start >= sig_end || sig_end > source.len() {
        return None;
    }

    let sig = String::from_utf8_lossy(&source[sig_start..sig_end]).to_string();
    let trimmed = sig.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Get the UTF-8 text of a tree-sitter node.
fn node_text(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rust_function() {
        let src = r#"
fn hello_world() {
    println!("hello");
}
"#;
        let tree = parse_file(src, Language::Rust).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Rust, 1, "test.rs");

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].symbol_name, "hello_world");
        assert_eq!(symbols[0].symbol_kind, SymbolKind::Function);
        assert!(symbols[0].signature.is_some());
    }

    #[test]
    fn test_parse_rust_struct_and_impl() {
        let src = r#"
struct MyStruct {
    field: i32,
}

impl MyStruct {
    fn new(val: i32) -> Self {
        Self { field: val }
    }

    fn get_field(&self) -> i32 {
        self.field
    }
}
"#;
        let tree = parse_file(src, Language::Rust).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Rust, 1, "test.rs");

        let names: Vec<&str> = symbols.iter().map(|s| s.symbol_name.as_str()).collect();
        assert!(names.contains(&"MyStruct"), "names = {:?}", names);
        // impl block should be extracted
        assert!(symbols.iter().any(|s| s.symbol_kind == SymbolKind::Impl));
        // Methods inside impl
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.symbol_kind == SymbolKind::Method)
            .collect();
        assert_eq!(methods.len(), 2);
        assert!(methods
            .iter()
            .all(|m| m.parent_symbol.as_deref() == Some("MyStruct")));
    }

    #[test]
    fn test_parse_python_class() {
        let src = r#"
class MyClass:
    def __init__(self, x):
        self.x = x

    def compute(self):
        return self.x * 2
"#;
        let tree = parse_file(src, Language::Python).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Python, 1, "test.py");

        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "MyClass" && s.symbol_kind == SymbolKind::Class));
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.symbol_kind == SymbolKind::Method)
            .collect();
        assert_eq!(methods.len(), 2);
        assert!(methods
            .iter()
            .all(|m| m.parent_symbol.as_deref() == Some("MyClass")));
    }

    #[test]
    fn test_parse_javascript_function() {
        let src = r#"
function greet(name) {
    return `Hello, ${name}!`;
}
"#;
        let tree = parse_file(src, Language::JavaScript).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::JavaScript, 1, "test.js");

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].symbol_name, "greet");
        assert_eq!(symbols[0].symbol_kind, SymbolKind::Function);
    }

    #[test]
    fn test_parse_go_function_and_method() {
        let src = r#"
package main

func Hello() string {
    return "hello"
}

type Server struct {
    port int
}

func (s *Server) Start() error {
    return nil
}
"#;
        let tree = parse_file(src, Language::Go).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Go, 1, "main.go");

        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "Hello" && s.symbol_kind == SymbolKind::Function));
        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "Start" && s.symbol_kind == SymbolKind::Method));
        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "Server" && s.symbol_kind == SymbolKind::Struct));
    }

    #[test]
    fn test_parse_java_class() {
        let src = r#"
class MyService {
    public void process() {
        System.out.println("processing");
    }

    private int compute(int x) {
        return x * 2;
    }
}
"#;
        let tree = parse_file(src, Language::Java).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Java, 1, "MyService.java");

        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "MyService" && s.symbol_kind == SymbolKind::Class));
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.symbol_kind == SymbolKind::Method)
            .collect();
        assert_eq!(
            methods.len(),
            2,
            "symbols: {:?}",
            symbols
                .iter()
                .map(|s| (&s.symbol_name, s.symbol_kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_c_function() {
        let src = r#"
int main(int argc, char **argv) {
    return 0;
}
"#;
        let tree = parse_file(src, Language::C).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::C, 1, "main.c");

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].symbol_name, "main");
        assert_eq!(symbols[0].symbol_kind, SymbolKind::Function);
    }

    #[test]
    fn test_parse_ruby_class() {
        let src = r#"
class Dog
  def bark
    puts "woof"
  end
end
"#;
        let tree = parse_file(src, Language::Ruby).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Ruby, 1, "dog.rb");

        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "Dog" && s.symbol_kind == SymbolKind::Class));
        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "bark" && s.symbol_kind == SymbolKind::Method));
    }

    #[test]
    fn test_parse_typescript_interface() {
        let src = r#"
interface User {
    name: string;
    age: number;
}

function getUser(id: string): User {
    return { name: "Alice", age: 30 };
}
"#;
        let tree = parse_file(src, Language::TypeScript).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::TypeScript, 1, "user.ts");

        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "User" && s.symbol_kind == SymbolKind::Interface));
        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "getUser" && s.symbol_kind == SymbolKind::Function));
    }

    #[test]
    fn test_parse_php_class() {
        let src = r#"<?php
class UserService {
    public function getUser(int $id): User {
        return new User($id);
    }

    private function validate(int $id): bool {
        return $id > 0;
    }
}

function helper(): string {
    return "hello";
}
"#;
        let tree = parse_file(src, Language::Php).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Php, 1, "UserService.php");

        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "UserService" && s.symbol_kind == SymbolKind::Class));
        assert!(symbols
            .iter()
            .any(|s| s.symbol_name == "helper" && s.symbol_kind == SymbolKind::Function));
    }

    #[test]
    fn test_parse_swift_class_and_function() {
        let src = r#"
class UserManager {
    func getUser(id: Int) -> String {
        return "user"
    }
}

func distance(from a: Int, to b: Int) -> Int {
    return b - a
}
"#;
        let tree = parse_file(src, Language::Swift).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Swift, 1, "manager.swift");

        assert!(
            symbols
                .iter()
                .any(|s| s.symbol_name == "UserManager" && s.symbol_kind == SymbolKind::Class),
            "symbols: {:?}",
            symbols
                .iter()
                .map(|s| (&s.symbol_name, s.symbol_kind))
                .collect::<Vec<_>>()
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.symbol_name == "distance" && s.symbol_kind == SymbolKind::Function),
            "symbols: {:?}",
            symbols
                .iter()
                .map(|s| (&s.symbol_name, s.symbol_kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_kotlin_class() {
        let src = r#"
class Calculator {
    fun add(a: Int, b: Int): Int {
        return a + b
    }

    fun subtract(a: Int, b: Int): Int {
        return a - b
    }
}

fun main(args: Array<String>) {
    val calc = Calculator()
    println(calc.add(1, 2))
}
"#;
        let tree = parse_file(src, Language::Kotlin).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Kotlin, 1, "Calculator.kt");

        assert!(
            symbols
                .iter()
                .any(|s| s.symbol_name == "Calculator" && s.symbol_kind == SymbolKind::Class),
            "symbols: {:?}",
            symbols
                .iter()
                .map(|s| (&s.symbol_name, s.symbol_kind))
                .collect::<Vec<_>>()
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.symbol_name == "main" && s.symbol_kind == SymbolKind::Function),
            "symbols: {:?}",
            symbols
                .iter()
                .map(|s| (&s.symbol_name, s.symbol_kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_empty_file() {
        let src = "";
        let tree = parse_file(src, Language::Rust).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Rust, 1, "empty.rs");
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_line_numbers_are_1_indexed() {
        let src = "fn foo() {}\n";
        let tree = parse_file(src, Language::Rust).unwrap();
        let symbols = extract_symbols(&tree, src.as_bytes(), Language::Rust, 1, "test.rs");
        assert_eq!(symbols[0].start_line, 1);
        assert_eq!(symbols[0].end_line, 1);
    }
}
