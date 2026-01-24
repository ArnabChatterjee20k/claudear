//! Issue context extraction for repository inference.
//!
//! Extracts searchable context (filenames, functions, keywords) from issues
//! to enable automated repository inference.

use crate::types::Issue;
use regex_lite::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Pre-compiled regex patterns for text extraction.
/// These are compiled once at first use and reused for all subsequent calls.
static PATH_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[\s"'`(])([a-zA-Z_./\\-]+\.(rs|js|ts|tsx|jsx|py|php|go|java|rb|swift|kt|c|cpp|h|hpp|cs|vue|svelte|html|css|scss|sass|yaml|yml|json|xml|md))\b"#
    ).ok()
});

static REPO_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s:,(`])([a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+)(?:$|[\s,):`])").ok()
});

static CLASS_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"\b([A-Z][a-zA-Z0-9]+(?:Controller|Service|Handler|Manager|Repository|Factory|Provider|Module|Component|Middleware))\b").ok()
});

static ERROR_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\b([A-Z][a-zA-Z0-9]*(?:Error|Exception|Failure))\b").ok());

/// Extracted context from an issue for repository inference.
#[derive(Debug, Clone, Default)]
pub struct IssueContext {
    /// Extracted file paths or filenames.
    pub filenames: Vec<String>,
    /// Extracted function names.
    pub functions: Vec<String>,
    /// Other searchable keywords.
    pub keywords: Vec<String>,
    /// Extracted repository references (org/repo format).
    pub repos: Vec<String>,
    /// Raw text used for extraction (for debugging).
    pub raw_text: String,
}

impl IssueContext {
    /// Create a new empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Extract context from any issue based on its source.
    pub fn from_issue(issue: &Issue) -> Self {
        match issue.source.as_str() {
            "sentry" => Self::from_sentry(issue),
            "linear" => Self::from_linear(issue),
            _ => Self::from_generic(issue),
        }
    }

    /// Extract context from a Sentry issue.
    ///
    /// Sentry issues have structured metadata:
    /// - `metadata.filename` - direct file path
    /// - `metadata.function` - function name
    /// - `culprit` - often "file in function" format
    pub fn from_sentry(issue: &Issue) -> Self {
        let mut context = Self::new();

        // Extract filename from metadata
        if let Some(filename) = issue.metadata.get("filename").and_then(|v| v.as_str()) {
            context.filenames.push(filename.to_string());
        }

        // Extract function from metadata
        if let Some(function) = issue.metadata.get("function").and_then(|v| v.as_str()) {
            context.functions.push(function.to_string());
        }

        // Extract culprit (often contains file and function info)
        if let Some(culprit) = issue.metadata.get("culprit").and_then(|v| v.as_str()) {
            // Culprit format is often "file in function" or just "file"
            let parts: Vec<_> = culprit.split(" in ").collect();
            if parts.len() >= 2 {
                context.filenames.push(parts[0].trim().to_string());
                context.functions.push(parts[1].trim().to_string());
            } else if !parts.is_empty() {
                // Could be just a file path
                if looks_like_path(parts[0]) {
                    context.filenames.push(parts[0].trim().to_string());
                }
            }
        }

        // Extract from stack trace if available
        if let Some(stacktrace) = issue.metadata.get("stacktrace").and_then(|v| v.as_str()) {
            extract_from_stacktrace(&mut context, stacktrace);
        }

        // Extract from description
        let description = issue.description.as_deref().unwrap_or("");
        let message = issue
            .metadata
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let raw_text = format!("{}\n{}\n{}", issue.title, description, message);
        extract_from_text(&mut context, &raw_text);
        context.raw_text = raw_text;

        // Deduplicate
        context.deduplicate();

        context
    }

    /// Extract context from a Linear issue.
    ///
    /// Linear issues have less structured metadata but may have:
    /// - Code blocks in description
    /// - File path references
    /// - Stack traces pasted in
    pub fn from_linear(issue: &Issue) -> Self {
        let mut context = Self::new();

        // Combine title and description for extraction
        let description = issue.description.as_deref().unwrap_or("");
        let raw_text = format!("{}\n{}", issue.title, description);

        // Extract file paths from text
        extract_from_text(&mut context, &raw_text);
        context.raw_text = raw_text;

        // Extract from code blocks (markdown)
        extract_from_code_blocks(&mut context, description);

        // Look for specific keywords in labels
        if let Some(labels) = issue.metadata.get("labels") {
            if let Some(labels_array) = labels.as_array() {
                for label in labels_array {
                    if let Some(name) = label.as_str() {
                        context.keywords.push(name.to_lowercase());
                    }
                }
            }
        }

        // Deduplicate
        context.deduplicate();

        context
    }

    /// Extract context from a generic issue (fallback).
    pub fn from_generic(issue: &Issue) -> Self {
        let mut context = Self::new();

        let description = issue.description.as_deref().unwrap_or("");
        let raw_text = format!("{}\n{}", issue.title, description);
        extract_from_text(&mut context, &raw_text);
        context.raw_text = raw_text;

        context.deduplicate();
        context
    }

    /// Remove duplicate entries.
    fn deduplicate(&mut self) {
        let filenames_set: HashSet<_> = self.filenames.drain(..).collect();
        self.filenames = filenames_set.into_iter().collect();

        let functions_set: HashSet<_> = self.functions.drain(..).collect();
        self.functions = functions_set.into_iter().collect();

        let keywords_set: HashSet<_> = self.keywords.drain(..).collect();
        self.keywords = keywords_set.into_iter().collect();

        let repos_set: HashSet<_> = self.repos.drain(..).collect();
        self.repos = repos_set.into_iter().collect();
    }

    /// Check if context has any useful data.
    pub fn is_empty(&self) -> bool {
        self.filenames.is_empty()
            && self.functions.is_empty()
            && self.keywords.is_empty()
            && self.repos.is_empty()
    }
}

/// Check if a string looks like a file path.
fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || s.contains('.') && !s.starts_with('.')
}

/// Extract file paths and function names from stack trace text.
fn extract_from_stacktrace(context: &mut IssueContext, stacktrace: &str) {
    // Common stack trace file patterns
    // Python: File "path/to/file.py", line 123, in function_name
    // Node.js: at function_name (path/to/file.js:123:45)
    // PHP: #0 /path/to/file.php(123): function_name()
    // Java: at package.Class.method(File.java:123)

    // Python style
    let python_re = Regex::new(r#"File "([^"]+\.py)""#).ok();
    if let Some(re) = python_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                context.filenames.push(m.as_str().to_string());
            }
        }
    }

    // Node.js/JavaScript style
    let node_re = Regex::new(r"\(([^)]+\.[jt]sx?):(\d+)").ok();
    if let Some(re) = node_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                context.filenames.push(m.as_str().to_string());
            }
        }
    }

    // PHP style
    let php_re = Regex::new(r"(/[^(]+\.php)\((\d+)\)").ok();
    if let Some(re) = php_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                context.filenames.push(m.as_str().to_string());
            }
        }
    }

    // Java style
    let java_re = Regex::new(r"\(([^)]+\.java):(\d+)\)").ok();
    if let Some(re) = java_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                context.filenames.push(m.as_str().to_string());
            }
        }
    }

    // Go style: /path/to/file.go:123
    let go_re = Regex::new(r"(/[^\s:]+\.go):(\d+)").ok();
    if let Some(re) = go_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                context.filenames.push(m.as_str().to_string());
            }
        }
    }

    // Rust style: path/to/file.rs:123:45
    let rust_re = Regex::new(r"([^\s]+\.rs):(\d+)").ok();
    if let Some(re) = rust_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                context.filenames.push(m.as_str().to_string());
            }
        }
    }

    // Extract function names from "at function" or "in function" patterns
    let func_re = Regex::new(r"(?:at|in)\s+([a-zA-Z_][a-zA-Z0-9_.:]+)").ok();
    if let Some(re) = func_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                let func = m.as_str();
                // Skip common noise
                if !func.starts_with("http")
                    && !func.starts_with("file")
                    && !func.contains("node_modules")
                {
                    context.functions.push(func.to_string());
                }
            }
        }
    }
}

/// Extract file paths and keywords from general text.
/// Uses pre-compiled regex patterns for efficiency.
fn extract_from_text(context: &mut IssueContext, text: &str) {
    // Extract file paths using pre-compiled regex
    if let Some(re) = PATH_RE.as_ref() {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().trim_start_matches(['/', '\\']);
                if !path.is_empty() {
                    context.filenames.push(path.to_string());
                }
            }
        }
    }

    // Extract repository references (org/repo format) using pre-compiled regex
    if let Some(re) = REPO_RE.as_ref() {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let repo = m.as_str();
                // Filter out common false positives (file paths, URLs)
                if !repo.contains("http")
                    && !repo.contains("www")
                    && !repo.ends_with(".rs")
                    && !repo.ends_with(".js")
                    && !repo.ends_with(".ts")
                    && !repo.ends_with(".php")
                    && !repo.ends_with(".py")
                    && !repo.ends_with(".go")
                    && !repo.starts_with("src/")
                    && !repo.starts_with("lib/")
                    && !repo.starts_with("app/")
                {
                    context.repos.push(repo.to_string());
                }
            }
        }
    }

    // Extract class names (PascalCase) using pre-compiled regex
    if let Some(re) = CLASS_RE.as_ref() {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                context.keywords.push(m.as_str().to_string());
            }
        }
    }

    // Extract error types using pre-compiled regex
    if let Some(re) = ERROR_RE.as_ref() {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                context.keywords.push(m.as_str().to_string());
            }
        }
    }
}

/// Extract file paths from markdown code blocks.
fn extract_from_code_blocks(context: &mut IssueContext, text: &str) {
    // Match markdown code blocks with optional language
    // ```language
    // code
    // ```
    let block_re = Regex::new(r"```(?:\w+)?\s*\n([\s\S]*?)```").ok();

    if let Some(re) = block_re {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let code = m.as_str();
                extract_from_text(context, code);
                extract_from_stacktrace(context, code);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_issue(source: &str, title: &str, description: &str) -> Issue {
        Issue {
            id: "test-1".to_string(),
            short_id: "TEST-1".to_string(),
            source: source.to_string(),
            title: title.to_string(),
            description: if description.is_empty() {
                None
            } else {
                Some(description.to_string())
            },
            url: "https://example.com/test".to_string(),
            priority: crate::types::IssuePriority::Medium,
            status: crate::types::IssueStatus::Open,
            metadata: std::collections::HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn test_context_from_sentry_with_filename() {
        let mut issue = create_test_issue("sentry", "Error in handler", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("src/handlers/auth.rs"));
        issue
            .metadata
            .insert("function".to_string(), json!("authenticate"));

        let context = IssueContext::from_sentry(&issue);

        assert!(context
            .filenames
            .contains(&"src/handlers/auth.rs".to_string()));
        assert!(context.functions.contains(&"authenticate".to_string()));
    }

    #[test]
    fn test_context_from_sentry_with_culprit() {
        let mut issue = create_test_issue("sentry", "Error in handler", "");
        issue.metadata.insert(
            "culprit".to_string(),
            json!("api/routes.ts in handleRequest"),
        );

        let context = IssueContext::from_sentry(&issue);

        assert!(context.filenames.contains(&"api/routes.ts".to_string()));
        assert!(context.functions.contains(&"handleRequest".to_string()));
    }

    #[test]
    fn test_context_from_sentry_with_stacktrace() {
        let mut issue = create_test_issue("sentry", "Error in handler", "");
        issue.metadata.insert(
            "stacktrace".to_string(),
            json!(
                r#"
                File "app/services/user_service.py", line 45, in create_user
                    raise ValueError("Invalid email")
                File "app/controllers/user_controller.py", line 23, in handle
                    return service.create_user(data)
            "#
            ),
        );

        let context = IssueContext::from_sentry(&issue);

        assert!(context
            .filenames
            .iter()
            .any(|f| f.contains("user_service.py")));
        assert!(context
            .filenames
            .iter()
            .any(|f| f.contains("user_controller.py")));
    }

    #[test]
    fn test_context_from_linear() {
        let issue = create_test_issue(
            "linear",
            "Fix authentication bug",
            "The bug is in `src/auth/session.rs` line 45.\n\n```rust\nfn validate_session() {\n    // error here\n}\n```",
        );

        let context = IssueContext::from_linear(&issue);

        assert!(context.filenames.iter().any(|f| f.contains("session.rs")));
    }

    #[test]
    fn test_context_from_linear_with_code_block() {
        let issue = create_test_issue(
            "linear",
            "Error in Router",
            "```\nat Router.handle (src/router/index.ts:123:45)\nat Server.listen (src/server.ts:67:12)\n```",
        );

        let context = IssueContext::from_linear(&issue);

        // Should extract from code blocks
        assert!(!context.filenames.is_empty() || !context.functions.is_empty());
    }

    #[test]
    fn test_context_from_issue_dispatch() {
        let sentry_issue = create_test_issue("sentry", "Sentry Error", "");
        let linear_issue = create_test_issue("linear", "Linear Issue", "");
        let other_issue = create_test_issue("jira", "Jira Issue", "");

        // All should work without errors
        let _ = IssueContext::from_issue(&sentry_issue);
        let _ = IssueContext::from_issue(&linear_issue);
        let _ = IssueContext::from_issue(&other_issue);
    }

    #[test]
    fn test_extract_file_paths() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "The error is in src/main.rs and also affects lib/utils.py",
        );

        assert!(context.filenames.iter().any(|f| f.contains("main.rs")));
        assert!(context.filenames.iter().any(|f| f.contains("utils.py")));
    }

    #[test]
    fn test_extract_class_names() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Check the UserController and AuthService classes",
        );

        assert!(context.keywords.contains(&"UserController".to_string()));
        assert!(context.keywords.contains(&"AuthService".to_string()));
    }

    #[test]
    fn test_extract_error_types() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Got a ValidationError and AuthenticationFailure",
        );

        assert!(context.keywords.contains(&"ValidationError".to_string()));
        assert!(context
            .keywords
            .contains(&"AuthenticationFailure".to_string()));
    }

    #[test]
    fn test_context_deduplication() {
        let mut context = IssueContext::new();
        context.filenames.push("file.rs".to_string());
        context.filenames.push("file.rs".to_string());
        context.functions.push("func".to_string());
        context.functions.push("func".to_string());

        context.deduplicate();

        assert_eq!(context.filenames.len(), 1);
        assert_eq!(context.functions.len(), 1);
    }

    #[test]
    fn test_context_is_empty() {
        let empty = IssueContext::new();
        assert!(empty.is_empty());

        let mut not_empty = IssueContext::new();
        not_empty.filenames.push("test.rs".to_string());
        assert!(!not_empty.is_empty());
    }

    #[test]
    fn test_looks_like_path() {
        assert!(looks_like_path("src/main.rs"));
        assert!(looks_like_path("path/to/file.js"));
        assert!(looks_like_path("file.py"));
        assert!(!looks_like_path(".hidden"));
        assert!(!looks_like_path("nopathhere"));
    }

    #[test]
    fn test_extract_python_stacktrace() {
        let mut context = IssueContext::new();
        let stacktrace = r#"
            Traceback (most recent call last):
              File "/app/services/user.py", line 45, in create
                raise ValueError("Invalid")
              File "/app/api/routes.py", line 23, in handle
                return service.create(data)
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        assert!(context.filenames.iter().any(|f| f.contains("user.py")));
        assert!(context.filenames.iter().any(|f| f.contains("routes.py")));
    }

    #[test]
    fn test_extract_nodejs_stacktrace() {
        let mut context = IssueContext::new();
        let stacktrace = r#"
            Error: Something went wrong
                at processTicksAndRejections (internal/process/task_queues.js:95:5)
                at Router.handle (/app/src/router.ts:45:12)
                at Server.listen (/app/src/server.js:23:8)
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        // Should extract the app files, not internal ones
        assert!(context
            .filenames
            .iter()
            .any(|f| f.contains("router.ts") || f.contains("server.js")));
    }
}
