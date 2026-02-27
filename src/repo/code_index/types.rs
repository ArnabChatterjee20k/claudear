//! Type definitions for code indexing.

use serde::{Deserialize, Serialize};

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Java,
    C,
    Cpp,
    Ruby,
    Php,
    Swift,
    Kotlin,
    CSharp,
    Dart,
    Yaml,
    Json,
    Dockerfile,
    Lua,
}

impl Language {
    /// Return the display name used in context text and DB storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::Swift => "Swift",
            Self::Kotlin => "Kotlin",
            Self::CSharp => "C#",
            Self::Dart => "Dart",
            Self::Yaml => "YAML",
            Self::Json => "JSON",
            Self::Dockerfile => "Dockerfile",
            Self::Lua => "Lua",
        }
    }

    /// Infer language from a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some(Self::Cpp),
            "rb" => Some(Self::Ruby),
            "php" => Some(Self::Php),
            "swift" => Some(Self::Swift),
            "kt" | "kts" => Some(Self::Kotlin),
            "cs" => Some(Self::CSharp),
            "dart" => Some(Self::Dart),
            "yaml" | "yml" => Some(Self::Yaml),
            "json" => Some(Self::Json),
            "lua" => Some(Self::Lua),
            _ => None,
        }
    }

    /// Infer language from a filename (for extensionless files like `Dockerfile`).
    pub fn from_filename(name: &str) -> Option<Self> {
        match name {
            "Dockerfile" => Some(Self::Dockerfile),
            _ => None,
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Kind of extracted symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Struct,
    Impl,
    Interface,
    Trait,
    Enum,
    Module,
    Constant,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Impl => "impl",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Enum => "enum",
            Self::Module => "module",
            Self::Constant => "constant",
        }
    }

    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s {
            "function" => Some(Self::Function),
            "class" => Some(Self::Class),
            "method" => Some(Self::Method),
            "struct" => Some(Self::Struct),
            "impl" => Some(Self::Impl),
            "interface" => Some(Self::Interface),
            "trait" => Some(Self::Trait),
            "enum" => Some(Self::Enum),
            "module" => Some(Self::Module),
            "constant" => Some(Self::Constant),
            _ => None,
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An extracted code symbol (function, class, struct, etc.).
#[derive(Debug, Clone)]
pub struct CodeSymbol {
    pub id: Option<i64>,
    pub repo_id: i64,
    pub file_path: String,
    pub symbol_name: String,
    pub symbol_kind: SymbolKind,
    pub parent_symbol: Option<String>,
    pub language: Language,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
}

/// A semantic code chunk for embedding.
#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub id: Option<i64>,
    pub repo_id: i64,
    pub file_path: String,
    pub chunk_type: String,
    pub symbol_name: Option<String>,
    pub language: Language,
    pub start_line: usize,
    pub end_line: usize,
    pub chunk_text: String,
    pub context_text: String,
    pub file_hash: String,
    pub content_hash: Option<String>,
}

/// A code search result with similarity score.
#[derive(Debug, Clone)]
pub struct CodeSearchResult {
    pub chunk: CodeChunk,
    pub score: f64,
}

/// Statistics from a code indexing run.
#[derive(Debug, Clone, Default)]
pub struct CodeIndexStats {
    pub files_processed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub symbols_extracted: usize,
    pub chunks_created: usize,
    pub embeddings_generated: usize,
}

impl std::fmt::Display for CodeIndexStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "processed={}, skipped={}, failed={}, symbols={}, chunks={}, embeddings={}",
            self.files_processed,
            self.files_skipped,
            self.files_failed,
            self.symbols_extracted,
            self.chunks_created,
            self.embeddings_generated,
        )
    }
}

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
        // Only exact "Dockerfile" matches
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
        // Files with "Dockerfile" as prefix + extension should NOT match
        // (they have an extension so from_extension handles them, returning None)
        assert_eq!(Language::from_filename("Dockerfile.dev"), None);
        assert_eq!(Language::from_filename("Dockerfile.prod"), None);
        assert_eq!(Language::from_filename("Makefile"), None);
    }

    #[test]
    fn test_language_from_extension_yaml_aliases() {
        assert_eq!(Language::from_extension("yaml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("YAML"), None); // case sensitive
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
        // All language variants should serialize and deserialize correctly
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
        // serde(rename_all = "lowercase") should produce lowercase JSON keys
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
