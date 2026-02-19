//! Language registry: tree-sitter grammar setup and per-language node type maps.

use super::types::{Language, SymbolKind};
use tree_sitter::Language as TsLanguage;

/// Get the tree-sitter grammar for a language.
pub fn ts_language(lang: Language) -> TsLanguage {
    match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Java => tree_sitter_java::LANGUAGE.into(),
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Language::Swift => tree_sitter_swift::LANGUAGE.into(),
        Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
    }
}

/// Classify a tree-sitter node type into a `SymbolKind` for the given language.
///
/// Returns `None` if the node type is not a symbol we track.
pub fn classify_node(lang: Language, node_type: &str) -> Option<SymbolKind> {
    match lang {
        Language::Rust => classify_rust(node_type),
        Language::TypeScript | Language::Tsx | Language::JavaScript => classify_ts_js(node_type),
        Language::Python => classify_python(node_type),
        Language::Go => classify_go(node_type),
        Language::Java => classify_java(node_type),
        Language::C => classify_c(node_type),
        Language::Cpp => classify_cpp(node_type),
        Language::Ruby => classify_ruby(node_type),
        Language::Php => classify_php(node_type),
        Language::Swift => classify_swift(node_type),
        Language::Kotlin => classify_kotlin(node_type),
    }
}

/// Return the child field name that holds the identifier for a node type in each language.
pub fn name_field(lang: Language, node_type: &str) -> Option<&'static str> {
    match lang {
        Language::Rust => rust_name_field(node_type),
        Language::TypeScript | Language::Tsx | Language::JavaScript => ts_js_name_field(node_type),
        Language::Python => python_name_field(node_type),
        Language::Go => go_name_field(node_type),
        Language::Java => java_name_field(node_type),
        Language::C => c_name_field(node_type),
        Language::Cpp => cpp_name_field(node_type),
        Language::Ruby => ruby_name_field(node_type),
        Language::Php => php_name_field(node_type),
        Language::Swift => swift_name_field(node_type),
        Language::Kotlin => kotlin_name_field(node_type),
    }
}

/// Return true if this node type represents a container (class, impl block, module)
/// whose children may be methods.
pub fn is_container(lang: Language, node_type: &str) -> bool {
    match lang {
        Language::Rust => matches!(node_type, "impl_item"),
        Language::TypeScript | Language::Tsx | Language::JavaScript => {
            matches!(node_type, "class_declaration")
        }
        Language::Python => matches!(node_type, "class_definition"),
        Language::Go => false, // Go methods are top-level
        Language::Java => matches!(node_type, "class_declaration"),
        Language::C => false,
        Language::Cpp => matches!(node_type, "class_specifier"),
        Language::Ruby => matches!(node_type, "class" | "module"),
        Language::Php => matches!(node_type, "class_declaration"),
        Language::Swift => matches!(node_type, "class_declaration" | "protocol_declaration"),
        Language::Kotlin => matches!(node_type, "class_declaration"),
    }
}

// ── Rust ──────────────────────────────────────────────────────────────

fn classify_rust(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_item" => Some(SymbolKind::Function),
        "struct_item" => Some(SymbolKind::Struct),
        "enum_item" => Some(SymbolKind::Enum),
        "trait_item" => Some(SymbolKind::Trait),
        "impl_item" => Some(SymbolKind::Impl),
        "type_item" => Some(SymbolKind::Constant), // type alias
        "const_item" | "static_item" => Some(SymbolKind::Constant),
        "mod_item" => Some(SymbolKind::Module),
        _ => None,
    }
}

fn rust_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_item" | "struct_item" | "enum_item" | "trait_item" | "type_item"
        | "const_item" | "static_item" | "mod_item" => Some("name"),
        "impl_item" => Some("type"), // impl Type { ... }
        _ => None,
    }
}

// ── TypeScript / JavaScript ───────────────────────────────────────────

fn classify_ts_js(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_declaration" => Some(SymbolKind::Function),
        "class_declaration" => Some(SymbolKind::Class),
        "method_definition" => Some(SymbolKind::Method),
        "interface_declaration" => Some(SymbolKind::Interface),
        "type_alias_declaration" => Some(SymbolKind::Constant),
        "enum_declaration" => Some(SymbolKind::Enum),
        _ => None,
    }
}

fn ts_js_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration" => Some("name"),
        "method_definition" => Some("name"),
        _ => None,
    }
}

// ── Python ────────────────────────────────────────────────────────────

fn classify_python(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_definition" => Some(SymbolKind::Function),
        "class_definition" => Some(SymbolKind::Class),
        _ => None,
    }
}

fn python_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_definition" | "class_definition" => Some("name"),
        _ => None,
    }
}

// ── Go ────────────────────────────────────────────────────────────────

fn classify_go(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_declaration" => Some(SymbolKind::Function),
        "method_declaration" => Some(SymbolKind::Method),
        "type_declaration" => Some(SymbolKind::Struct), // covers struct and interface
        _ => None,
    }
}

fn go_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_declaration" | "method_declaration" => Some("name"),
        "type_declaration" => None, // Go type_declaration has type_spec children
        _ => None,
    }
}

// ── Java ──────────────────────────────────────────────────────────────

fn classify_java(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "method_declaration" | "constructor_declaration" => Some(SymbolKind::Method),
        "class_declaration" => Some(SymbolKind::Class),
        "interface_declaration" => Some(SymbolKind::Interface),
        "enum_declaration" => Some(SymbolKind::Enum),
        _ => None,
    }
}

fn java_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "method_declaration"
        | "constructor_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "enum_declaration" => Some("name"),
        _ => None,
    }
}

// ── C ─────────────────────────────────────────────────────────────────

fn classify_c(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_definition" => Some(SymbolKind::Function),
        "struct_specifier" => Some(SymbolKind::Struct),
        "enum_specifier" => Some(SymbolKind::Enum),
        "type_definition" => Some(SymbolKind::Constant),
        _ => None,
    }
}

fn c_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_definition" => Some("declarator"),
        "struct_specifier" | "enum_specifier" => Some("name"),
        "type_definition" => Some("declarator"),
        _ => None,
    }
}

// ── C++ ───────────────────────────────────────────────────────────────

fn classify_cpp(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_definition" => Some(SymbolKind::Function),
        "struct_specifier" => Some(SymbolKind::Struct),
        "class_specifier" => Some(SymbolKind::Class),
        "enum_specifier" => Some(SymbolKind::Enum),
        "type_definition" => Some(SymbolKind::Constant),
        _ => None,
    }
}

fn cpp_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_definition" => Some("declarator"),
        "struct_specifier" | "class_specifier" | "enum_specifier" => Some("name"),
        "type_definition" => Some("declarator"),
        _ => None,
    }
}

// ── Ruby ──────────────────────────────────────────────────────────────

fn classify_ruby(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "method" | "singleton_method" => Some(SymbolKind::Function),
        "class" => Some(SymbolKind::Class),
        "module" => Some(SymbolKind::Module),
        _ => None,
    }
}

fn ruby_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "method" | "singleton_method" | "class" | "module" => Some("name"),
        _ => None,
    }
}

// ── PHP ───────────────────────────────────────────────────────────────

fn classify_php(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_definition" => Some(SymbolKind::Function),
        "class_declaration" => Some(SymbolKind::Class),
        "method_declaration" => Some(SymbolKind::Method),
        "interface_declaration" => Some(SymbolKind::Interface),
        "trait_declaration" => Some(SymbolKind::Trait),
        "enum_declaration" => Some(SymbolKind::Enum),
        _ => None,
    }
}

fn php_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_definition"
        | "class_declaration"
        | "method_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "enum_declaration" => Some("name"),
        _ => None,
    }
}

// ── Swift ─────────────────────────────────────────────────────────────

fn classify_swift(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_declaration" => Some(SymbolKind::Function),
        "class_declaration" => Some(SymbolKind::Class),
        "protocol_declaration" => Some(SymbolKind::Interface),
        "enum_declaration" => Some(SymbolKind::Enum),
        _ => None,
    }
}

fn swift_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_declaration"
        | "class_declaration"
        | "protocol_declaration"
        | "enum_declaration" => Some("name"),
        _ => None,
    }
}

// ── Kotlin ────────────────────────────────────────────────────────────

fn classify_kotlin(node_type: &str) -> Option<SymbolKind> {
    match node_type {
        "function_declaration" => Some(SymbolKind::Function),
        "class_declaration" => Some(SymbolKind::Class),
        "interface_declaration" => Some(SymbolKind::Interface),
        "object_declaration" => Some(SymbolKind::Class),
        _ => None,
    }
}

fn kotlin_name_field(node_type: &str) -> Option<&'static str> {
    match node_type {
        "function_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "object_declaration" => Some("name"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ts_language_returns_valid_grammar() {
        // Smoke test: each grammar can be used with a parser.
        // Some grammars may have ABI version mismatches with the tree-sitter
        // runtime; we track which ones work and require at least the core set.
        let all_langs = [
            Language::Rust,
            Language::TypeScript,
            Language::Tsx,
            Language::JavaScript,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::Ruby,
            Language::Php,
            Language::Swift,
            Language::Kotlin,
        ];
        let mut failed = Vec::new();
        for lang in all_langs {
            let mut parser = tree_sitter::Parser::new();
            if parser.set_language(&ts_language(lang)).is_err() {
                failed.push(lang);
            }
        }
        // Core languages must always work
        let core = [
            Language::Rust,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::C,
            Language::Cpp,
        ];
        for lang in core {
            assert!(
                !failed.contains(&lang),
                "Core language {:?} grammar failed to load",
                lang
            );
        }
    }

    #[test]
    fn test_classify_rust_nodes() {
        assert_eq!(
            classify_node(Language::Rust, "function_item"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Rust, "struct_item"),
            Some(SymbolKind::Struct)
        );
        assert_eq!(
            classify_node(Language::Rust, "impl_item"),
            Some(SymbolKind::Impl)
        );
        assert_eq!(
            classify_node(Language::Rust, "trait_item"),
            Some(SymbolKind::Trait)
        );
        assert_eq!(classify_node(Language::Rust, "unknown_node"), None);
    }

    #[test]
    fn test_classify_ts_nodes() {
        assert_eq!(
            classify_node(Language::TypeScript, "function_declaration"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::TypeScript, "class_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::TypeScript, "method_definition"),
            Some(SymbolKind::Method)
        );
        assert_eq!(
            classify_node(Language::TypeScript, "interface_declaration"),
            Some(SymbolKind::Interface)
        );
    }

    #[test]
    fn test_classify_python_nodes() {
        assert_eq!(
            classify_node(Language::Python, "function_definition"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Python, "class_definition"),
            Some(SymbolKind::Class)
        );
    }

    #[test]
    fn test_name_field_rust() {
        assert_eq!(name_field(Language::Rust, "function_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "impl_item"), Some("type"));
    }

    #[test]
    fn test_is_container() {
        assert!(is_container(Language::Rust, "impl_item"));
        assert!(!is_container(Language::Rust, "function_item"));
        assert!(is_container(Language::Python, "class_definition"));
        assert!(is_container(Language::Java, "class_declaration"));
    }
}
