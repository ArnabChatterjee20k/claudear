//! Type definitions for code indexing.
//!
//! These types are defined in `claudear_core::types` and re-exported here
//! for backward compatibility.

pub use claudear_core::types::{
    CodeChunk, CodeIndexStats, CodeSearchResult, CodeSymbol, FileComplexity, FunctionComplexity,
    Language, SymbolKind,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::Tsx));
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        assert_eq!(Language::from_extension("c"), Some(Language::C));
        assert_eq!(Language::from_extension("cpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("rb"), Some(Language::Ruby));
        assert_eq!(Language::from_extension("php"), Some(Language::Php));
        assert_eq!(Language::from_extension("swift"), Some(Language::Swift));
        assert_eq!(Language::from_extension("kt"), Some(Language::Kotlin));
        assert_eq!(Language::from_extension("kts"), Some(Language::Kotlin));
        assert_eq!(Language::from_extension("cs"), Some(Language::CSharp));
        assert_eq!(Language::from_extension("dart"), Some(Language::Dart));
        assert_eq!(Language::from_extension("yaml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("json"), Some(Language::Json));
        assert_eq!(Language::from_extension("lua"), Some(Language::Lua));
        assert_eq!(Language::from_extension("txt"), None);
    }

    #[test]
    fn test_symbol_kind_round_trip() {
        for kind in [
            SymbolKind::Function,
            SymbolKind::Class,
            SymbolKind::Method,
            SymbolKind::Struct,
            SymbolKind::Impl,
            SymbolKind::Interface,
            SymbolKind::Trait,
            SymbolKind::Enum,
            SymbolKind::Module,
            SymbolKind::Constant,
        ] {
            assert_eq!(SymbolKind::from_str_loose(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn test_language_display() {
        assert_eq!(Language::Rust.to_string(), "Rust");
        assert_eq!(Language::Cpp.to_string(), "C++");
        assert_eq!(Language::TypeScript.to_string(), "TypeScript");
        assert_eq!(Language::CSharp.to_string(), "C#");
        assert_eq!(Language::Dart.to_string(), "Dart");
        assert_eq!(Language::Yaml.to_string(), "YAML");
        assert_eq!(Language::Json.to_string(), "JSON");
        assert_eq!(Language::Dockerfile.to_string(), "Dockerfile");
        assert_eq!(Language::Lua.to_string(), "Lua");
    }

    #[test]
    fn test_language_from_filename() {
        assert_eq!(
            Language::from_filename("Dockerfile"),
            Some(Language::Dockerfile)
        );
        assert_eq!(Language::from_filename("README.md"), None);
    }

    #[test]
    fn test_language_from_filename_case_sensitive() {
        assert_eq!(
            Language::from_filename("Dockerfile"),
            Some(Language::Dockerfile)
        );
        assert_eq!(Language::from_filename("dockerfile"), None);
        assert_eq!(Language::from_filename("DOCKERFILE"), None);
        assert_eq!(Language::from_filename("DockerFile"), None);
    }

    #[test]
    fn test_language_from_filename_similar_names() {
        assert_eq!(Language::from_filename("Dockerfile.dev"), None);
        assert_eq!(Language::from_filename("Dockerfile.prod"), None);
        assert_eq!(Language::from_filename("Makefile"), None);
    }

    #[test]
    fn test_language_from_extension_yaml_aliases() {
        assert_eq!(Language::from_extension("yaml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("YAML"), None);
    }

    #[test]
    fn test_language_from_extension_all_new_extensions() {
        assert_eq!(Language::from_extension("yaml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("json"), Some(Language::Json));
        assert_eq!(Language::from_extension("lua"), Some(Language::Lua));
    }

    #[test]
    fn test_language_as_str_new_languages() {
        assert_eq!(Language::Yaml.as_str(), "YAML");
        assert_eq!(Language::Json.as_str(), "JSON");
        assert_eq!(Language::Dockerfile.as_str(), "Dockerfile");
        assert_eq!(Language::Lua.as_str(), "Lua");
    }

    #[test]
    fn test_language_serde_round_trip() {
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
            Language::CSharp,
            Language::Dart,
            Language::Yaml,
            Language::Json,
            Language::Dockerfile,
            Language::Lua,
        ];
        for lang in all_langs {
            let json = serde_json::to_string(&lang).unwrap();
            let deserialized: Language = serde_json::from_str(&json).unwrap();
            assert_eq!(
                lang, deserialized,
                "Round-trip failed for {:?}: serialized as {}",
                lang, json
            );
        }
    }

    #[test]
    fn test_language_serde_lowercase_names() {
        let json = serde_json::to_string(&Language::Yaml).unwrap();
        assert_eq!(json, r#""yaml""#);
        let json = serde_json::to_string(&Language::Json).unwrap();
        assert_eq!(json, r#""json""#);
        let json = serde_json::to_string(&Language::Dockerfile).unwrap();
        assert_eq!(json, r#""dockerfile""#);
        let json = serde_json::to_string(&Language::Lua).unwrap();
        assert_eq!(json, r#""lua""#);
    }

    #[test]
    fn test_language_from_extension_returns_none_for_unknown() {
        let unknown_exts = [
            "xml", "txt", "md", "toml", "ini", "cfg", "csv", "html", "css",
        ];
        for ext in unknown_exts {
            assert_eq!(
                Language::from_extension(ext),
                None,
                "Extension '{}' should return None",
                ext
            );
        }
    }

    #[test]
    fn test_code_index_stats_display() {
        let stats = CodeIndexStats {
            files_processed: 10,
            files_skipped: 5,
            files_failed: 1,
            symbols_extracted: 50,
            chunks_created: 30,
            embeddings_generated: 30,
        };
        let s = stats.to_string();
        assert!(s.contains("processed=10"));
        assert!(s.contains("skipped=5"));
    }
}
