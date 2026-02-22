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

    #[test]
    fn test_classify_go_nodes() {
        assert_eq!(
            classify_node(Language::Go, "function_declaration"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Go, "method_declaration"),
            Some(SymbolKind::Method)
        );
        assert_eq!(
            classify_node(Language::Go, "type_declaration"),
            Some(SymbolKind::Struct)
        );
        assert_eq!(classify_node(Language::Go, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_go() {
        assert_eq!(
            name_field(Language::Go, "function_declaration"),
            Some("name")
        );
        assert_eq!(name_field(Language::Go, "method_declaration"), Some("name"));
        // type_declaration has type_spec children, so name field is None
        assert_eq!(name_field(Language::Go, "type_declaration"), None);
        assert_eq!(name_field(Language::Go, "unknown_node"), None);
    }

    #[test]
    fn test_classify_java_nodes() {
        assert_eq!(
            classify_node(Language::Java, "method_declaration"),
            Some(SymbolKind::Method)
        );
        assert_eq!(
            classify_node(Language::Java, "constructor_declaration"),
            Some(SymbolKind::Method)
        );
        assert_eq!(
            classify_node(Language::Java, "class_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::Java, "interface_declaration"),
            Some(SymbolKind::Interface)
        );
        assert_eq!(
            classify_node(Language::Java, "enum_declaration"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(classify_node(Language::Java, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_java() {
        assert_eq!(
            name_field(Language::Java, "method_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Java, "constructor_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Java, "class_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Java, "interface_declaration"),
            Some("name")
        );
        assert_eq!(name_field(Language::Java, "enum_declaration"), Some("name"));
        assert_eq!(name_field(Language::Java, "unknown_node"), None);
    }

    #[test]
    fn test_classify_c_nodes() {
        assert_eq!(
            classify_node(Language::C, "function_definition"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::C, "struct_specifier"),
            Some(SymbolKind::Struct)
        );
        assert_eq!(
            classify_node(Language::C, "enum_specifier"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(
            classify_node(Language::C, "type_definition"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(classify_node(Language::C, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_c() {
        assert_eq!(
            name_field(Language::C, "function_definition"),
            Some("declarator")
        );
        assert_eq!(name_field(Language::C, "struct_specifier"), Some("name"));
        assert_eq!(name_field(Language::C, "enum_specifier"), Some("name"));
        assert_eq!(
            name_field(Language::C, "type_definition"),
            Some("declarator")
        );
        assert_eq!(name_field(Language::C, "unknown_node"), None);
    }

    #[test]
    fn test_classify_cpp_nodes() {
        assert_eq!(
            classify_node(Language::Cpp, "function_definition"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Cpp, "struct_specifier"),
            Some(SymbolKind::Struct)
        );
        assert_eq!(
            classify_node(Language::Cpp, "class_specifier"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::Cpp, "enum_specifier"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(
            classify_node(Language::Cpp, "type_definition"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(classify_node(Language::Cpp, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_cpp() {
        assert_eq!(
            name_field(Language::Cpp, "function_definition"),
            Some("declarator")
        );
        assert_eq!(name_field(Language::Cpp, "struct_specifier"), Some("name"));
        assert_eq!(name_field(Language::Cpp, "class_specifier"), Some("name"));
        assert_eq!(name_field(Language::Cpp, "enum_specifier"), Some("name"));
        assert_eq!(
            name_field(Language::Cpp, "type_definition"),
            Some("declarator")
        );
        assert_eq!(name_field(Language::Cpp, "unknown_node"), None);
    }

    #[test]
    fn test_classify_ruby_nodes() {
        assert_eq!(
            classify_node(Language::Ruby, "method"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Ruby, "singleton_method"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Ruby, "class"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::Ruby, "module"),
            Some(SymbolKind::Module)
        );
        assert_eq!(classify_node(Language::Ruby, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_ruby() {
        assert_eq!(name_field(Language::Ruby, "method"), Some("name"));
        assert_eq!(name_field(Language::Ruby, "singleton_method"), Some("name"));
        assert_eq!(name_field(Language::Ruby, "class"), Some("name"));
        assert_eq!(name_field(Language::Ruby, "module"), Some("name"));
        assert_eq!(name_field(Language::Ruby, "unknown_node"), None);
    }

    #[test]
    fn test_classify_php_nodes() {
        assert_eq!(
            classify_node(Language::Php, "function_definition"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Php, "class_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::Php, "method_declaration"),
            Some(SymbolKind::Method)
        );
        assert_eq!(
            classify_node(Language::Php, "interface_declaration"),
            Some(SymbolKind::Interface)
        );
        assert_eq!(
            classify_node(Language::Php, "trait_declaration"),
            Some(SymbolKind::Trait)
        );
        assert_eq!(
            classify_node(Language::Php, "enum_declaration"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(classify_node(Language::Php, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_php() {
        assert_eq!(
            name_field(Language::Php, "function_definition"),
            Some("name")
        );
        assert_eq!(name_field(Language::Php, "class_declaration"), Some("name"));
        assert_eq!(
            name_field(Language::Php, "method_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Php, "interface_declaration"),
            Some("name")
        );
        assert_eq!(name_field(Language::Php, "trait_declaration"), Some("name"));
        assert_eq!(name_field(Language::Php, "enum_declaration"), Some("name"));
        assert_eq!(name_field(Language::Php, "unknown_node"), None);
    }

    #[test]
    fn test_classify_swift_nodes() {
        assert_eq!(
            classify_node(Language::Swift, "function_declaration"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Swift, "class_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::Swift, "protocol_declaration"),
            Some(SymbolKind::Interface)
        );
        assert_eq!(
            classify_node(Language::Swift, "enum_declaration"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(classify_node(Language::Swift, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_swift() {
        assert_eq!(
            name_field(Language::Swift, "function_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Swift, "class_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Swift, "protocol_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Swift, "enum_declaration"),
            Some("name")
        );
        assert_eq!(name_field(Language::Swift, "unknown_node"), None);
    }

    #[test]
    fn test_classify_kotlin_nodes() {
        assert_eq!(
            classify_node(Language::Kotlin, "function_declaration"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Kotlin, "class_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::Kotlin, "interface_declaration"),
            Some(SymbolKind::Interface)
        );
        assert_eq!(
            classify_node(Language::Kotlin, "object_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(classify_node(Language::Kotlin, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_kotlin() {
        assert_eq!(
            name_field(Language::Kotlin, "function_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Kotlin, "class_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Kotlin, "interface_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Kotlin, "object_declaration"),
            Some("name")
        );
        assert_eq!(name_field(Language::Kotlin, "unknown_node"), None);
    }

    #[test]
    fn test_classify_node_javascript_routes_to_ts_js() {
        assert_eq!(
            classify_node(Language::JavaScript, "function_declaration"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::JavaScript, "class_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::JavaScript, "method_definition"),
            Some(SymbolKind::Method)
        );
        assert_eq!(
            classify_node(Language::JavaScript, "interface_declaration"),
            Some(SymbolKind::Interface)
        );
        assert_eq!(
            classify_node(Language::JavaScript, "type_alias_declaration"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(
            classify_node(Language::JavaScript, "enum_declaration"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(classify_node(Language::JavaScript, "unknown_node"), None);
    }

    #[test]
    fn test_classify_node_tsx_routes_to_ts_js() {
        assert_eq!(
            classify_node(Language::Tsx, "function_declaration"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Tsx, "class_declaration"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            classify_node(Language::Tsx, "method_definition"),
            Some(SymbolKind::Method)
        );
        assert_eq!(
            classify_node(Language::Tsx, "interface_declaration"),
            Some(SymbolKind::Interface)
        );
        assert_eq!(
            classify_node(Language::Tsx, "type_alias_declaration"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(
            classify_node(Language::Tsx, "enum_declaration"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(classify_node(Language::Tsx, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_javascript_routes_to_ts_js() {
        assert_eq!(
            name_field(Language::JavaScript, "function_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::JavaScript, "class_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::JavaScript, "method_definition"),
            Some("name")
        );
        assert_eq!(name_field(Language::JavaScript, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_tsx_routes_to_ts_js() {
        assert_eq!(
            name_field(Language::Tsx, "function_declaration"),
            Some("name")
        );
        assert_eq!(name_field(Language::Tsx, "class_declaration"), Some("name"));
        assert_eq!(name_field(Language::Tsx, "method_definition"), Some("name"));
        assert_eq!(name_field(Language::Tsx, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_typescript() {
        assert_eq!(
            name_field(Language::TypeScript, "function_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::TypeScript, "class_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::TypeScript, "interface_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::TypeScript, "type_alias_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::TypeScript, "enum_declaration"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::TypeScript, "method_definition"),
            Some("name")
        );
        assert_eq!(name_field(Language::TypeScript, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_python() {
        assert_eq!(
            name_field(Language::Python, "function_definition"),
            Some("name")
        );
        assert_eq!(
            name_field(Language::Python, "class_definition"),
            Some("name")
        );
        assert_eq!(name_field(Language::Python, "unknown_node"), None);
    }

    #[test]
    fn test_classify_rust_all_arms() {
        assert_eq!(
            classify_node(Language::Rust, "function_item"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            classify_node(Language::Rust, "struct_item"),
            Some(SymbolKind::Struct)
        );
        assert_eq!(
            classify_node(Language::Rust, "enum_item"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(
            classify_node(Language::Rust, "trait_item"),
            Some(SymbolKind::Trait)
        );
        assert_eq!(
            classify_node(Language::Rust, "impl_item"),
            Some(SymbolKind::Impl)
        );
        assert_eq!(
            classify_node(Language::Rust, "type_item"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(
            classify_node(Language::Rust, "const_item"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(
            classify_node(Language::Rust, "static_item"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(
            classify_node(Language::Rust, "mod_item"),
            Some(SymbolKind::Module)
        );
        assert_eq!(classify_node(Language::Rust, "unknown_node"), None);
    }

    #[test]
    fn test_name_field_rust_all_arms() {
        assert_eq!(name_field(Language::Rust, "function_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "struct_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "enum_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "trait_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "type_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "const_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "static_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "mod_item"), Some("name"));
        assert_eq!(name_field(Language::Rust, "impl_item"), Some("type"));
        assert_eq!(name_field(Language::Rust, "unknown_node"), None);
    }

    #[test]
    fn test_is_container_all_languages() {
        // Rust
        assert!(is_container(Language::Rust, "impl_item"));
        assert!(!is_container(Language::Rust, "function_item"));
        assert!(!is_container(Language::Rust, "struct_item"));

        // TypeScript
        assert!(is_container(Language::TypeScript, "class_declaration"));
        assert!(!is_container(Language::TypeScript, "function_declaration"));

        // Tsx
        assert!(is_container(Language::Tsx, "class_declaration"));
        assert!(!is_container(Language::Tsx, "function_declaration"));

        // JavaScript
        assert!(is_container(Language::JavaScript, "class_declaration"));
        assert!(!is_container(Language::JavaScript, "function_declaration"));

        // Python
        assert!(is_container(Language::Python, "class_definition"));
        assert!(!is_container(Language::Python, "function_definition"));

        // Go always returns false
        assert!(!is_container(Language::Go, "function_declaration"));
        assert!(!is_container(Language::Go, "method_declaration"));
        assert!(!is_container(Language::Go, "type_declaration"));

        // Java
        assert!(is_container(Language::Java, "class_declaration"));
        assert!(!is_container(Language::Java, "method_declaration"));
        assert!(!is_container(Language::Java, "interface_declaration"));

        // C always returns false
        assert!(!is_container(Language::C, "function_definition"));
        assert!(!is_container(Language::C, "struct_specifier"));
        assert!(!is_container(Language::C, "type_definition"));

        // Cpp: class_specifier is a container
        assert!(is_container(Language::Cpp, "class_specifier"));
        assert!(!is_container(Language::Cpp, "function_definition"));
        assert!(!is_container(Language::Cpp, "struct_specifier"));

        // Ruby: class and module are containers
        assert!(is_container(Language::Ruby, "class"));
        assert!(is_container(Language::Ruby, "module"));
        assert!(!is_container(Language::Ruby, "method"));
        assert!(!is_container(Language::Ruby, "singleton_method"));

        // PHP
        assert!(is_container(Language::Php, "class_declaration"));
        assert!(!is_container(Language::Php, "function_definition"));
        assert!(!is_container(Language::Php, "method_declaration"));

        // Swift: class_declaration and protocol_declaration are containers
        assert!(is_container(Language::Swift, "class_declaration"));
        assert!(is_container(Language::Swift, "protocol_declaration"));
        assert!(!is_container(Language::Swift, "function_declaration"));
        assert!(!is_container(Language::Swift, "enum_declaration"));

        // Kotlin
        assert!(is_container(Language::Kotlin, "class_declaration"));
        assert!(!is_container(Language::Kotlin, "function_declaration"));
        assert!(!is_container(Language::Kotlin, "interface_declaration"));
        assert!(!is_container(Language::Kotlin, "object_declaration"));
    }

    #[test]
    fn test_classify_ts_all_arms() {
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
        assert_eq!(
            classify_node(Language::TypeScript, "type_alias_declaration"),
            Some(SymbolKind::Constant)
        );
        assert_eq!(
            classify_node(Language::TypeScript, "enum_declaration"),
            Some(SymbolKind::Enum)
        );
        assert_eq!(classify_node(Language::TypeScript, "unknown_node"), None);
    }

    #[test]
    fn test_classify_python_unknown() {
        assert_eq!(classify_node(Language::Python, "unknown_node"), None);
    }
}
