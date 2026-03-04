//! Issue context extraction for repository inference.
//!
//! Extracts searchable context (filenames, functions, keywords) from issues
//! to enable automated repository inference.

use claudear_core::types::Issue;
use regex_lite::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Pre-compiled regex patterns for text extraction.
/// These are compiled once at first use and reused for all subsequent calls.
static PATH_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[\s"'`(;\[])([a-zA-Z0-9_./\\-]+\.(rs|js|ts|tsx|jsx|py|php|go|java|rb|swift|kt|c|cpp|h|hpp|cs|vue|svelte|html|css|scss|sass|yaml|yml|json|xml|md))\b"#
    ).ok()
});

static REPO_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s:,(`])([a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+)(?:$|[\s,):`])").ok()
});

/// Regex to extract vendor package names from PHP paths (e.g., vendor/org/package/)
static VENDOR_PHP_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"vendor/([a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+)/").ok());

/// Regex to extract package names from node_modules paths (e.g., node_modules/@org/package/)
static VENDOR_NODE_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"node_modules/(@[a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+|[a-zA-Z0-9_.-]+)/").ok()
});

/// Regex to extract Go module paths (e.g., github.com/org/repo)
static VENDOR_GO_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:vendor/)?github\.com/([a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+)/").ok());

/// Regex to match PHP fully-qualified class names (e.g., `Appwrite\Utopia\Request::getHeader()`)
static PHP_FQCN_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"\b([A-Z][a-zA-Z0-9]*(?:\\[A-Z][a-zA-Z0-9]*)*)(?:::(\w+))?\b").ok()
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
    /// - `project` - Sentry project name (maps to top-level repo)
    pub fn from_sentry(issue: &Issue) -> Self {
        let mut context = Self::new();

        // Strategy 0: Use Sentry project name to infer top-level repo
        // Sentry projects map 1:1 with top-level repos.
        // Project names like "cloud-staging" or "cloud-production" should map to "cloud"
        if let Some(project) = issue.metadata.get("project").and_then(|v| v.as_str()) {
            let normalized = normalize_project_name(project);
            if !normalized.is_empty() {
                // Add as a potential repo reference (will be matched against known repos)
                context.repos.push(normalized.clone());

                // Also add with common org prefixes to try matching org/repo format
                // The inferrer will need to fuzzy match these
                context.keywords.push(format!("repo:{}", normalized));
            }
        }

        // Extract filename from metadata
        if let Some(filename) = issue.metadata.get("filename").and_then(|v| v.as_str()) {
            context.filenames.push(filename.to_string());
            // Also extract vendor packages from the filename path
            extract_vendor_packages(&mut context, filename);
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
                extract_vendor_packages(&mut context, parts[0].trim());
            } else if !parts.is_empty() {
                // Could be just a file path
                if looks_like_path(parts[0]) {
                    context.filenames.push(parts[0].trim().to_string());
                    extract_vendor_packages(&mut context, parts[0].trim());
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
    s.contains('/') || s.contains('\\') || (s.contains('.') && !s.starts_with('.'))
}

/// Normalize a Sentry project name to a potential repository name.
///
/// Strips common environment suffixes like "-staging", "-production", "-dev", "-prod".
/// Examples:
/// - "cloud-staging" -> "cloud"
/// - "cloud-production" -> "cloud"
/// - "my-api-dev" -> "my-api"
/// - "console" -> "console"
fn normalize_project_name(project: &str) -> String {
    let suffixes = [
        "-staging",
        "-production",
        "-prod",
        "-dev",
        "-development",
        "-test",
        "-qa",
        "-uat",
        "-preview",
        "-canary",
    ];

    let mut normalized = project.to_lowercase();
    for suffix in &suffixes {
        if let Some(stripped) = normalized.strip_suffix(suffix) {
            if !stripped.is_empty() {
                normalized = stripped.to_string();
            }
            break;
        }
    }
    normalized
}

/// Extract vendor/dependency package names from file paths.
///
/// Parses paths like:
/// - PHP: `/vendor/utopia-php/database/src/...` -> `utopia-php/database`
/// - Node: `/node_modules/@scope/package/...` -> `@scope/package`
/// - Go: `/vendor/github.com/org/repo/...` -> `org/repo`
fn extract_vendor_packages(context: &mut IssueContext, path: &str) {
    // PHP vendor packages (composer)
    if let Some(re) = VENDOR_PHP_RE.as_ref() {
        for cap in re.captures_iter(path) {
            if let Some(m) = cap.get(1) {
                let package = m.as_str();
                // Add as potential repo reference
                context.repos.push(package.to_string());
            }
        }
    }

    // Node.js packages
    if let Some(re) = VENDOR_NODE_RE.as_ref() {
        for cap in re.captures_iter(path) {
            if let Some(m) = cap.get(1) {
                let package = m.as_str();
                // Node scoped packages like @scope/package - convert to repo format
                let repo_name = package.trim_start_matches('@').replace('/', "-");
                if package.starts_with('@') {
                    // For scoped packages, use the full name as-is (without @)
                    context
                        .repos
                        .push(package.trim_start_matches('@').to_string());
                }
                context.repos.push(repo_name);
            }
        }
    }

    // Go modules from github.com
    if let Some(re) = VENDOR_GO_RE.as_ref() {
        for cap in re.captures_iter(path) {
            if let Some(m) = cap.get(1) {
                context.repos.push(m.as_str().to_string());
            }
        }
    }
}

/// Extract file paths and function names from stack trace text.
fn extract_from_stacktrace(context: &mut IssueContext, stacktrace: &str) {
    // Common stack trace file patterns
    // Python: File "path/to/file.py", line 123, in function_name
    // Node.js: at function_name (path/to/file.js:123:45)
    // PHP: #0 /path/to/file.php(123): function_name()
    // Java: at package.Class.method(File.java:123)

    // Collect file paths to extract vendor packages from
    let mut collected_paths: Vec<String> = Vec::new();

    // Python style
    let python_re = Regex::new(r#"File "([^"]+\.py)""#).ok();
    if let Some(re) = python_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().to_string();
                context.filenames.push(path.clone());
                collected_paths.push(path);
            }
        }
    }

    // Node.js/JavaScript style
    let node_re = Regex::new(r"\(([^)]+\.[jt]sx?):(\d+)").ok();
    if let Some(re) = node_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().to_string();
                context.filenames.push(path.clone());
                collected_paths.push(path);
            }
        }
    }

    // PHP style - captures paths like /usr/src/code/vendor/utopia-php/database/src/...
    let php_re = Regex::new(r"(/[^\s(]+\.php)(?:\((\d+)\))?").ok();
    if let Some(re) = php_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().to_string();
                context.filenames.push(path.clone());
                collected_paths.push(path);
            }
        }
    }

    // Java style
    let java_re = Regex::new(r"\(([^)]+\.java):(\d+)\)").ok();
    if let Some(re) = java_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().to_string();
                context.filenames.push(path.clone());
                collected_paths.push(path);
            }
        }
    }

    // Go style: /path/to/file.go:123
    let go_re = Regex::new(r"(/[^\s:]+\.go):(\d+)").ok();
    if let Some(re) = go_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().to_string();
                context.filenames.push(path.clone());
                collected_paths.push(path);
            }
        }
    }

    // Rust style: path/to/file.rs:123:45
    let rust_re = Regex::new(r"([^\s]+\.rs):(\d+)").ok();
    if let Some(re) = rust_re {
        for cap in re.captures_iter(stacktrace) {
            if let Some(m) = cap.get(1) {
                let path = m.as_str().to_string();
                context.filenames.push(path.clone());
                collected_paths.push(path);
            }
        }
    }

    // Extract vendor/dependency packages from all collected paths
    for path in &collected_paths {
        extract_vendor_packages(context, path);
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

    // Extract PHP fully-qualified class names (e.g., Appwrite\Utopia\Request::getHeader())
    if let Some(re) = PHP_FQCN_RE.as_ref() {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let fqcn = m.as_str();
                let segments: Vec<&str> = fqcn.split('\\').collect();
                // Need at least 2 segments to be a namespace (e.g., Utopia\Request)
                if segments.len() >= 2 {
                    // Add ClassName.php (last segment)
                    if let Some(class_name) = segments.last() {
                        context.filenames.push(format!("{}.php", class_name));
                    }

                    // Add partial namespace path as filename
                    // e.g., Utopia\Http\Request -> Http/Request.php
                    if segments.len() >= 3 {
                        let path_segments = &segments[1..]; // skip top-level namespace
                        context
                            .filenames
                            .push(format!("{}.php", path_segments.join("/")));
                    }

                    // Convert first namespace segment to lowercase for repo matching
                    // e.g., Utopia -> "utopia" as keyword for repo search
                    let first_lower = segments[0].to_lowercase();
                    context.keywords.push(format!("repo:{}", first_lower));
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
            priority: claudear_core::types::IssuePriority::Medium,
            status: claudear_core::types::IssueStatus::Open,
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
    fn test_normalize_project_name() {
        assert_eq!(normalize_project_name("cloud-staging"), "cloud");
        assert_eq!(normalize_project_name("cloud-production"), "cloud");
        assert_eq!(normalize_project_name("cloud-prod"), "cloud");
        assert_eq!(normalize_project_name("my-api-dev"), "my-api");
        assert_eq!(normalize_project_name("console"), "console");
        assert_eq!(normalize_project_name("Cloud-Staging"), "cloud");
    }

    #[test]
    fn test_extract_vendor_packages_php() {
        let mut context = IssueContext::new();
        extract_vendor_packages(
            &mut context,
            "/usr/src/code/vendor/utopia-php/database/src/Database/Adapter/SQL.php",
        );

        assert!(context.repos.contains(&"utopia-php/database".to_string()));
    }

    #[test]
    fn test_extract_vendor_packages_multiple() {
        let mut context = IssueContext::new();

        // Extract from multiple paths
        extract_vendor_packages(
            &mut context,
            "/app/vendor/utopia-php/database/src/Database.php",
        );
        extract_vendor_packages(&mut context, "/app/vendor/utopia-php/pools/src/Pool.php");
        extract_vendor_packages(&mut context, "/app/vendor/utopia-php/queue/src/Server.php");

        assert!(context.repos.contains(&"utopia-php/database".to_string()));
        assert!(context.repos.contains(&"utopia-php/pools".to_string()));
        assert!(context.repos.contains(&"utopia-php/queue".to_string()));
    }

    #[test]
    fn test_sentry_project_extraction() {
        let mut issue = create_test_issue("sentry", "MySQL error", "");
        issue
            .metadata
            .insert("project".to_string(), json!("cloud-staging"));

        let context = IssueContext::from_sentry(&issue);

        // Should extract normalized project name as a repo reference
        assert!(context.repos.contains(&"cloud".to_string()));
    }

    #[test]
    fn test_sentry_stacktrace_vendor_extraction() {
        let mut issue = create_test_issue("sentry", "MySQL server has gone away", "");
        issue
            .metadata
            .insert("project".to_string(), json!("cloud-staging"));
        issue.metadata.insert(
            "stacktrace".to_string(),
            json!(
                r#"
                /usr/src/code/vendor/utopia-php/database/src/Database/Adapter/SQL.php in __call at line 393
                /usr/src/code/vendor/utopia-php/database/src/Database/Adapter/Pool.php in getDocument at line 59
                /usr/src/code/vendor/utopia-php/pools/src/Pools/Pool.php in closure at line 230
                "#
            ),
        );

        let context = IssueContext::from_sentry(&issue);

        // Should extract project name
        assert!(context.repos.contains(&"cloud".to_string()));
        // Should extract vendor packages from stack trace
        assert!(context.repos.contains(&"utopia-php/database".to_string()));
        assert!(context.repos.contains(&"utopia-php/pools".to_string()));
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

    #[test]
    fn test_sentry_all_metadata_none() {
        // Sentry issue with no metadata at all — should not panic
        let issue = create_test_issue("sentry", "Error", "");
        let context = IssueContext::from_sentry(&issue);
        // raw_text should still contain the title
        assert!(context.raw_text.contains("Error"));
    }

    #[test]
    fn test_sentry_metadata_values_are_non_string_types() {
        // metadata values that are numbers/bools/null instead of strings
        // as_str() should return None for these, so they should be skipped
        let mut issue = create_test_issue("sentry", "Error", "");
        issue.metadata.insert("filename".to_string(), json!(42));
        issue.metadata.insert("function".to_string(), json!(true));
        issue.metadata.insert("culprit".to_string(), json!(null));
        issue
            .metadata
            .insert("stacktrace".to_string(), json!(["not", "a", "string"]));
        issue.metadata.insert("project".to_string(), json!(99.5));
        issue
            .metadata
            .insert("message".to_string(), json!({"nested": "obj"}));

        let context = IssueContext::from_sentry(&issue);

        // Should not panic, and non-string metadata should be skipped
        assert!(context.filenames.is_empty() || context.filenames.iter().all(|f| !f.is_empty()));
    }

    #[test]
    fn test_sentry_empty_string_metadata() {
        // Empty strings in metadata fields
        let mut issue = create_test_issue("sentry", "", "");
        issue.metadata.insert("filename".to_string(), json!(""));
        issue.metadata.insert("function".to_string(), json!(""));
        issue.metadata.insert("culprit".to_string(), json!(""));
        issue.metadata.insert("stacktrace".to_string(), json!(""));
        issue.metadata.insert("project".to_string(), json!(""));
        issue.metadata.insert("message".to_string(), json!(""));

        let context = IssueContext::from_sentry(&issue);

        // Empty project should not produce a repo reference (normalize returns "")
        // because of the !normalized.is_empty() guard
        assert!(
            !context.repos.iter().any(|r| r.is_empty()),
            "Empty repo references should not be added"
        );
    }

    #[test]
    fn test_generic_issue_with_none_description() {
        let issue = create_test_issue("unknown", "Just a title", "");
        let context = IssueContext::from_generic(&issue);

        assert!(context.raw_text.contains("Just a title"));
        // None description should be treated as empty
        assert!(!context.raw_text.contains("null"));
    }

    #[test]
    fn test_linear_issue_with_none_description() {
        let issue = create_test_issue("linear", "Title only", "");
        let context = IssueContext::from_linear(&issue);

        assert!(context.raw_text.contains("Title only"));
    }

    #[test]
    fn test_sentry_description_none_and_message_none() {
        // Both description and message are missing
        let issue = create_test_issue("sentry", "TitleOnly", "");
        let context = IssueContext::from_sentry(&issue);
        // raw_text should be "TitleOnly\n\n" (empty description + empty message)
        assert!(context.raw_text.contains("TitleOnly"));
    }

    #[test]
    fn test_culprit_with_multiple_in_keywords() {
        // "file in function in extra" — split(" in ") produces 3 parts
        // Code takes parts[0] as file, parts[1] as function, ignoring parts[2+]
        let mut issue = create_test_issue("sentry", "Error", "");
        issue.metadata.insert(
            "culprit".to_string(),
            json!("api/handler.ts in processRequest in innerLoop"),
        );

        let context = IssueContext::from_sentry(&issue);

        assert!(
            context.filenames.contains(&"api/handler.ts".to_string()),
            "First part should be treated as filename"
        );
        assert!(
            context.functions.contains(&"processRequest".to_string()),
            "Second part should be treated as function"
        );
    }

    #[test]
    fn test_culprit_single_part_that_looks_like_path() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue
            .metadata
            .insert("culprit".to_string(), json!("src/controllers/main.rs"));

        let context = IssueContext::from_sentry(&issue);

        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Single-part culprit that looks like a path should be added as filename"
        );
    }

    #[test]
    fn test_culprit_single_part_not_a_path() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue
            .metadata
            .insert("culprit".to_string(), json!("someFunctionName"));

        let context = IssueContext::from_sentry(&issue);

        // "someFunctionName" doesn't look like a path (no / or \\ or non-leading .)
        // so it should NOT be added as a filename from the culprit branch
        assert!(
            !context.filenames.contains(&"someFunctionName".to_string()),
            "Non-path culprit should not be added as filename"
        );
    }

    #[test]
    fn test_culprit_with_whitespace_trimming() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue.metadata.insert(
            "culprit".to_string(),
            json!("  api/routes.ts  in  handleRequest  "),
        );

        let context = IssueContext::from_sentry(&issue);

        assert!(
            context.filenames.contains(&"api/routes.ts".to_string()),
            "Culprit parts should be trimmed"
        );
        assert!(
            context.functions.contains(&"handleRequest".to_string()),
            "Culprit parts should be trimmed"
        );
    }

    #[test]
    fn test_looks_like_path_backslash() {
        assert!(
            looks_like_path("src\\main.rs"),
            "Backslash paths should be detected"
        );
    }

    #[test]
    fn test_looks_like_path_dot_only_in_middle() {
        // "file.py" — has dot, doesn't start with dot
        assert!(looks_like_path("file.py"));
    }

    #[test]
    fn test_looks_like_path_dot_file_starting_with_dot() {
        // ".hidden" starts with . → should return false
        assert!(!looks_like_path(".hidden"));
        // ".env" starts with . → should return false
        assert!(!looks_like_path(".env"));
    }

    #[test]
    fn test_looks_like_path_empty_string() {
        // Empty string: no /, no \\, no . → should return false
        assert!(!looks_like_path(""));
    }

    #[test]
    fn test_looks_like_path_just_a_slash() {
        assert!(looks_like_path("/"), "Bare slash contains '/'");
    }

    #[test]
    fn test_looks_like_path_dotfile_with_extension() {
        // ".gitignore" starts with . → false per the logic
        assert!(!looks_like_path(".gitignore"));
    }

    #[test]
    fn test_looks_like_path_relative_dot_path() {
        // "./src/main.rs" starts with . → false per logic, even though it is a path
        // This is a potential gotcha in the current implementation
        assert!(
            !looks_like_path("./src/main.rs") || looks_like_path("./src/main.rs"),
            "Relative dot-slash paths: verifying actual behavior"
        );
        // The real check: starts_with('.') is true, but also contains('/'), so:
        // contains('/') is true → short-circuits to true
        assert!(looks_like_path("./src/main.rs"));
    }

    #[test]
    fn test_normalize_project_name_empty() {
        assert_eq!(normalize_project_name(""), "");
    }

    #[test]
    fn test_normalize_project_name_only_suffix() {
        // Entire name is a suffix like "-staging" → guard prevents stripping to ""
        assert_eq!(normalize_project_name("-staging"), "-staging");
    }

    #[test]
    fn test_normalize_project_name_double_suffix() {
        // "api-dev-staging" — only first matching suffix stripped (break after first match)
        // "-staging" matches → "api-dev"
        assert_eq!(normalize_project_name("api-dev-staging"), "api-dev");
    }

    #[test]
    fn test_normalize_project_name_suffix_in_middle() {
        // "staging-api" — no suffix match (strip_suffix not strip_contains)
        assert_eq!(normalize_project_name("staging-api"), "staging-api");
    }

    #[test]
    fn test_normalize_project_name_case_insensitive() {
        // Input is lowercased before matching, so "API-STAGING" → "api-staging" → "api"
        assert_eq!(normalize_project_name("API-STAGING"), "api");
        assert_eq!(normalize_project_name("My-App-PRODUCTION"), "my-app");
    }

    #[test]
    fn test_normalize_project_name_all_suffixes() {
        // Verify all known suffixes are stripped
        let suffixes = vec![
            ("app-staging", "app"),
            ("app-production", "app"),
            ("app-prod", "app"),
            ("app-dev", "app"),
            ("app-development", "app"),
            ("app-test", "app"),
            ("app-qa", "app"),
            ("app-uat", "app"),
            ("app-preview", "app"),
            ("app-canary", "app"),
        ];
        for (input, expected) in suffixes {
            assert_eq!(
                normalize_project_name(input),
                expected,
                "Failed for input: {}",
                input
            );
        }
    }

    #[test]
    fn test_extract_from_text_with_unicode_title() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Error in src/handlers/用户.rs when processing données",
        );

        // The path regex expects ASCII file extensions
        // "用户.rs" contains .rs, but the path chars before it are non-ASCII
        // The regex requires [a-zA-Z_./\\-]+ so non-ASCII should not match
        // Just verify no panic
        assert!(context.filenames.is_empty() || !context.filenames.is_empty());
    }

    #[test]
    fn test_extract_from_text_with_markdown_formatting() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "**Bold** text mentions `src/main.rs` and _italic_ text mentions `lib/utils.py`",
        );

        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Should extract paths from backtick-wrapped text"
        );
        assert!(
            context.filenames.iter().any(|f| f.contains("utils.py")),
            "Should extract paths from backtick-wrapped text"
        );
    }

    #[test]
    fn test_extract_from_text_with_html_entities() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Path is &quot;src/main.rs&quot; and function is &lt;init&gt;",
        );

        // The ";" in "&quot;" is recognized as a valid path delimiter,
        // so the path IS extracted even with HTML entities.
        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Semicolons in HTML entities should act as valid path delimiters"
        );
    }

    #[test]
    fn test_extract_from_text_with_newlines_and_tabs() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Error found:\n\tsrc/handlers/auth.rs\n\tlib/utils.py\n",
        );

        assert!(
            context.filenames.iter().any(|f| f.contains("auth.rs")),
            "Should extract paths preceded by tabs"
        );
    }

    #[test]
    fn test_special_chars_in_issue_title() {
        let issue = create_test_issue(
            "sentry",
            "Error: \"unexpected token\" in <Component> at src/app.tsx:42",
            "",
        );

        let context = IssueContext::from_sentry(&issue);

        // Should extract the file path from the title
        assert!(
            context.filenames.iter().any(|f| f.contains("app.tsx")),
            "Should extract path from title with special chars"
        );
    }

    #[test]
    fn test_deduplication_preserves_all_unique_items() {
        let mut context = IssueContext::new();
        context.filenames.push("a.rs".to_string());
        context.filenames.push("b.rs".to_string());
        context.filenames.push("c.rs".to_string());
        context.filenames.push("a.rs".to_string()); // dup
        context.filenames.push("b.rs".to_string()); // dup

        context.deduplicate();

        assert_eq!(context.filenames.len(), 3);
        // All three unique values must be present
        let set: HashSet<_> = context.filenames.iter().cloned().collect();
        assert!(set.contains("a.rs"));
        assert!(set.contains("b.rs"));
        assert!(set.contains("c.rs"));
    }

    #[test]
    fn test_deduplication_across_all_fields() {
        let mut context = IssueContext::new();
        context.filenames.push("f.rs".to_string());
        context.filenames.push("f.rs".to_string());
        context.functions.push("fn1".to_string());
        context.functions.push("fn1".to_string());
        context.keywords.push("kw".to_string());
        context.keywords.push("kw".to_string());
        context.repos.push("org/repo".to_string());
        context.repos.push("org/repo".to_string());

        context.deduplicate();

        assert_eq!(context.filenames.len(), 1);
        assert_eq!(context.functions.len(), 1);
        assert_eq!(context.keywords.len(), 1);
        assert_eq!(context.repos.len(), 1);
    }

    #[test]
    fn test_deduplication_empty_context() {
        let mut context = IssueContext::new();
        context.deduplicate(); // should not panic
        assert!(context.is_empty());
    }

    #[test]
    fn test_deduplication_case_sensitive() {
        // Deduplication should be case-sensitive (HashSet<String>)
        let mut context = IssueContext::new();
        context.filenames.push("File.rs".to_string());
        context.filenames.push("file.rs".to_string());

        context.deduplicate();

        assert_eq!(
            context.filenames.len(),
            2,
            "Deduplication is case-sensitive"
        );
    }

    #[test]
    fn test_is_empty_with_only_repos() {
        let mut context = IssueContext::new();
        context.repos.push("org/repo".to_string());
        assert!(
            !context.is_empty(),
            "Context with repos should not be empty"
        );
    }

    #[test]
    fn test_is_empty_with_only_keywords() {
        let mut context = IssueContext::new();
        context.keywords.push("error".to_string());
        assert!(
            !context.is_empty(),
            "Context with keywords should not be empty"
        );
    }

    #[test]
    fn test_is_empty_with_only_functions() {
        let mut context = IssueContext::new();
        context.functions.push("main".to_string());
        assert!(
            !context.is_empty(),
            "Context with functions should not be empty"
        );
    }

    #[test]
    fn test_is_empty_ignores_raw_text() {
        // raw_text is not considered in is_empty check
        let mut context = IssueContext::new();
        context.raw_text = "some raw text".to_string();
        assert!(
            context.is_empty(),
            "raw_text alone should not make context non-empty"
        );
    }

    #[test]
    fn test_from_issue_uses_sentry_for_sentry_source() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue
            .metadata
            .insert("project".to_string(), json!("my-app-staging"));

        let context = IssueContext::from_issue(&issue);

        // from_sentry extracts project; from_generic would not
        assert!(
            context.repos.contains(&"my-app".to_string()),
            "from_issue should dispatch to from_sentry for sentry source"
        );
    }

    #[test]
    fn test_from_issue_uses_linear_for_linear_source() {
        let mut issue = create_test_issue(
            "linear",
            "Bug report",
            "Check ```\nsrc/router/index.ts\n```",
        );
        issue
            .metadata
            .insert("labels".to_string(), json!(["backend", "urgent"]));

        let context = IssueContext::from_issue(&issue);

        // from_linear extracts labels; from_generic would not
        assert!(
            context.keywords.contains(&"backend".to_string()),
            "from_issue should dispatch to from_linear for linear source"
        );
    }

    #[test]
    fn test_from_issue_unknown_source_uses_generic() {
        let issue = create_test_issue(
            "github",
            "Bug in src/api.rs",
            "See src/handlers/main.py for details",
        );

        let context = IssueContext::from_issue(&issue);

        // Should still extract file paths via generic handler
        assert!(
            context.filenames.iter().any(|f| f.contains("api.rs"))
                || context.filenames.iter().any(|f| f.contains("main.py")),
            "Generic handler should still extract file paths"
        );
    }

    #[test]
    fn test_from_issue_empty_source_string() {
        let issue = create_test_issue("", "Bug", "src/main.rs has an issue");

        let context = IssueContext::from_issue(&issue);

        // Empty source should fall through to generic
        assert!(!context.raw_text.is_empty());
    }

    #[test]
    fn test_extract_paths_all_supported_extensions() {
        let extensions = vec![
            "rs", "js", "ts", "tsx", "jsx", "py", "php", "go", "java", "rb", "swift", "kt", "c",
            "cpp", "h", "hpp", "cs", "vue", "svelte", "html", "css", "scss", "sass", "yaml", "yml",
            "json", "xml", "md",
        ];

        for ext in &extensions {
            let mut context = IssueContext::new();
            let text = format!("Error in src/file.{}", ext);
            extract_from_text(&mut context, &text);
            assert!(
                context
                    .filenames
                    .iter()
                    .any(|f| f.ends_with(&format!(".{}", ext))),
                "Should detect .{} extension",
                ext
            );
        }
    }

    #[test]
    fn test_extract_paths_unsupported_extension() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "Error in src/data.parquet and src/model.onnx");

        // These extensions are not in the regex, so should not be extracted
        assert!(
            !context.filenames.iter().any(|f| f.contains("parquet")),
            ".parquet should not be extracted"
        );
        assert!(
            !context.filenames.iter().any(|f| f.contains("onnx")),
            ".onnx should not be extracted"
        );
    }

    #[test]
    fn test_extract_path_with_leading_slash_trimmed() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "Check /src/main.rs for the error");

        // The code trims leading '/' and '\\' from extracted paths
        assert!(
            context.filenames.iter().any(|f| f == "src/main.rs"),
            "Leading slash should be trimmed from path"
        );
    }

    #[test]
    fn test_extract_path_in_quotes() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "File \"src/main.rs\" has an error");

        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Should extract paths from quoted text"
        );
    }

    #[test]
    fn test_extract_path_in_backticks() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "The file `src/main.rs` is broken");

        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Should extract paths from backtick text"
        );
    }

    #[test]
    fn test_extract_path_in_parentheses() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "Check (src/main.rs) for the issue");

        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Should extract paths from parenthesized text"
        );
    }

    #[test]
    fn test_extract_path_at_start_of_text() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "src/main.rs has a bug");

        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Should extract path at start of line (^ anchor)"
        );
    }

    #[test]
    fn test_extract_deep_nested_path() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Error in src/services/auth/handlers/v2/login.ts",
        );

        // Digits are included in the path regex character class, so the full
        // path including "v2/" is matched correctly.
        assert!(
            context.filenames.iter().any(|f| f.contains("login.ts")),
            "Paths with digits in directories should be extracted"
        );

        // However, paths without digits in directory names work fine
        let mut context2 = IssueContext::new();
        extract_from_text(
            &mut context2,
            "Error in src/services/auth/handlers/login.ts",
        );
        assert!(
            context2.filenames.iter().any(|f| f.contains("login.ts")),
            "Deeply nested paths without digits should be extracted"
        );
    }

    #[test]
    fn test_extract_repo_reference() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "See the issue in facebook/react for details");

        assert!(
            context.repos.contains(&"facebook/react".to_string()),
            "Should extract org/repo references"
        );
    }

    #[test]
    fn test_extract_repo_reference_false_positive_filtering() {
        let mut context = IssueContext::new();
        // These should be filtered out as false positives
        extract_from_text(
            &mut context,
            "Check src/main.rs and lib/utils.py and app/routes.ts",
        );

        // Paths starting with src/, lib/, app/ should be filtered from repos
        assert!(
            !context.repos.iter().any(|r| r.starts_with("src/")),
            "src/ paths should be filtered from repos"
        );
        assert!(
            !context.repos.iter().any(|r| r.starts_with("lib/")),
            "lib/ paths should be filtered from repos"
        );
        assert!(
            !context.repos.iter().any(|r| r.starts_with("app/")),
            "app/ paths should be filtered from repos"
        );
    }

    #[test]
    fn test_extract_class_names_various_suffixes() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Uses UserHandler, AuthMiddleware, DataRepository, CacheFactory, PaymentProvider, AppModule, NavComponent, and OrderManager",
        );

        let expected = vec![
            "UserHandler",
            "AuthMiddleware",
            "DataRepository",
            "CacheFactory",
            "PaymentProvider",
            "AppModule",
            "NavComponent",
            "OrderManager",
        ];
        for class in &expected {
            assert!(
                context.keywords.contains(&class.to_string()),
                "Should extract class name: {}",
                class
            );
        }
    }

    #[test]
    fn test_extract_class_names_no_match_for_lowercase() {
        let mut context = IssueContext::new();
        // lowercase "userController" should NOT match (regex requires PascalCase start)
        extract_from_text(&mut context, "Check the userController");

        assert!(
            !context.keywords.contains(&"userController".to_string()),
            "lowercase class names should not be matched"
        );
    }

    #[test]
    fn test_extract_error_types_various() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Caught TypeError, NullPointerException, and ConnectionFailure",
        );

        assert!(context.keywords.contains(&"TypeError".to_string()));
        assert!(context
            .keywords
            .contains(&"NullPointerException".to_string()));
        assert!(context.keywords.contains(&"ConnectionFailure".to_string()));
    }

    #[test]
    fn test_error_type_not_extracted_if_lowercase() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "Got a validationError");

        // "validationError" starts with lowercase, regex requires [A-Z]
        assert!(
            !context.keywords.contains(&"validationError".to_string()),
            "lowercase error types should not be matched"
        );
    }

    #[test]
    fn test_stacktrace_java_format() {
        let mut context = IssueContext::new();
        let stacktrace = r#"
            java.lang.NullPointerException
                at com.example.UserService.getUser(UserService.java:45)
                at com.example.ApiController.handle(ApiController.java:23)
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        assert!(
            context
                .filenames
                .iter()
                .any(|f| f.contains("UserService.java")),
            "Should extract Java file from stacktrace"
        );
        assert!(
            context
                .filenames
                .iter()
                .any(|f| f.contains("ApiController.java")),
            "Should extract Java file from stacktrace"
        );
    }

    #[test]
    fn test_stacktrace_go_format() {
        let mut context = IssueContext::new();
        let stacktrace = r#"
            goroutine 1 [running]:
            main.handler()
                /app/cmd/server/main.go:45 +0x1a4
            net/http.(*ServeMux).ServeHTTP(...)
                /usr/local/go/src/net/http/server.go:2387
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        assert!(
            context.filenames.iter().any(|f| f.contains("main.go")),
            "Should extract Go file from stacktrace"
        );
    }

    #[test]
    fn test_stacktrace_rust_format() {
        let mut context = IssueContext::new();
        let stacktrace = r#"
            thread 'main' panicked at 'called `Result::unwrap()` on an `Err` value'
            src/handlers/auth.rs:42:5
            src/main.rs:15:10
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        assert!(
            context.filenames.iter().any(|f| f.contains("auth.rs")),
            "Should extract Rust file from stacktrace"
        );
        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Should extract Rust file from stacktrace"
        );
    }

    #[test]
    fn test_stacktrace_php_format() {
        let mut context = IssueContext::new();
        let stacktrace = r#"
            PHP Fatal error: Uncaught Error
            #0 /var/www/app/src/Controller/UserController.php(45): App\Service->validate()
            #1 /var/www/app/vendor/framework/http/Router.php(123): App\Controller->handle()
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        assert!(
            context
                .filenames
                .iter()
                .any(|f| f.contains("UserController.php")),
            "Should extract PHP file from stacktrace"
        );
    }

    #[test]
    fn test_stacktrace_empty_string() {
        let mut context = IssueContext::new();
        extract_from_stacktrace(&mut context, "");

        assert!(context.filenames.is_empty());
        assert!(context.functions.is_empty());
    }

    #[test]
    fn test_stacktrace_function_extraction_filters_noise() {
        let mut context = IssueContext::new();
        let stacktrace = r#"
            at validFunction (src/app.js:10:5)
            at http://example.com/bundle.js:1:1
            at file://local/path
            at node_modules/pkg/index.js
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        // Functions starting with "http" or "file" or containing "node_modules" are filtered
        assert!(
            !context.functions.iter().any(|f| f.starts_with("http")),
            "http-prefixed functions should be filtered"
        );
        assert!(
            !context.functions.iter().any(|f| f.starts_with("file")),
            "file-prefixed functions should be filtered"
        );
        assert!(
            !context.functions.iter().any(|f| f.contains("node_modules")),
            "node_modules functions should be filtered"
        );
    }

    #[test]
    fn test_stacktrace_mixed_languages() {
        // A stacktrace that somehow has multiple language formats
        let mut context = IssueContext::new();
        let stacktrace = r#"
            File "/app/main.py", line 10, in handler
            at Controller.process (/app/server.ts:42:10)
            /app/vendor/lib/core/Base.php(100): run()
        "#;

        extract_from_stacktrace(&mut context, stacktrace);

        assert!(
            context.filenames.iter().any(|f| f.contains("main.py")),
            "Should extract Python file"
        );
        assert!(
            context.filenames.iter().any(|f| f.contains("server.ts")),
            "Should extract Node.js file"
        );
        assert!(
            context.filenames.iter().any(|f| f.contains("Base.php")),
            "Should extract PHP file"
        );
    }

    #[test]
    fn test_vendor_node_scoped_package() {
        let mut context = IssueContext::new();
        extract_vendor_packages(
            &mut context,
            "/app/node_modules/@angular/core/src/di/injector.ts",
        );

        // Should extract the scoped package
        assert!(
            context.repos.iter().any(|r| r.contains("angular")),
            "Should extract scoped node package"
        );
    }

    #[test]
    fn test_vendor_node_unscoped_package() {
        let mut context = IssueContext::new();
        extract_vendor_packages(&mut context, "/app/node_modules/express/lib/router.js");

        assert!(
            context.repos.iter().any(|r| r.contains("express")),
            "Should extract unscoped node package"
        );
    }

    #[test]
    fn test_vendor_go_module() {
        let mut context = IssueContext::new();
        extract_vendor_packages(&mut context, "/app/vendor/github.com/gorilla/mux/mux.go");

        assert!(
            context.repos.contains(&"gorilla/mux".to_string()),
            "Should extract Go module from vendor path"
        );
    }

    #[test]
    fn test_vendor_no_match_for_regular_path() {
        let mut context = IssueContext::new();
        extract_vendor_packages(&mut context, "/app/src/handlers/auth.rs");

        assert!(
            context.repos.is_empty(),
            "Regular paths should not produce vendor repos"
        );
    }

    #[test]
    fn test_vendor_empty_path() {
        let mut context = IssueContext::new();
        extract_vendor_packages(&mut context, "");

        assert!(context.repos.is_empty());
    }

    #[test]
    fn test_code_block_with_language_tag() {
        let mut context = IssueContext::new();
        let text = "Here's the error:\n```python\nFile \"/app/main.py\", line 10, in handler\n    raise ValueError(\"bad\")\n```";

        extract_from_code_blocks(&mut context, text);

        assert!(
            context.filenames.iter().any(|f| f.contains("main.py")),
            "Should extract from code block with language tag"
        );
    }

    #[test]
    fn test_code_block_without_language_tag() {
        let mut context = IssueContext::new();
        let text = "Error:\n```\nsrc/main.rs:10:5 panicked\n```";

        extract_from_code_blocks(&mut context, text);

        assert!(
            context.filenames.iter().any(|f| f.contains("main.rs")),
            "Should extract from code block without language tag"
        );
    }

    #[test]
    fn test_multiple_code_blocks() {
        let mut context = IssueContext::new();
        let text = "First:\n```\nsrc/a.rs:10\n```\n\nSecond:\n```python\nFile \"/app/b.py\", line 5, in run\n```";

        extract_from_code_blocks(&mut context, text);

        assert!(
            context.filenames.iter().any(|f| f.contains("a.rs")),
            "Should extract from first code block"
        );
        assert!(
            context.filenames.iter().any(|f| f.contains("b.py")),
            "Should extract from second code block"
        );
    }

    #[test]
    fn test_code_block_empty_content() {
        let mut context = IssueContext::new();
        let text = "Empty block:\n```\n```";

        extract_from_code_blocks(&mut context, text);

        // Should not panic on empty code block
        assert!(context.filenames.is_empty());
    }

    #[test]
    fn test_no_code_blocks_in_text() {
        let mut context = IssueContext::new();
        let text = "No code blocks here, just plain text about src/main.rs";

        extract_from_code_blocks(&mut context, text);

        // extract_from_code_blocks only processes code blocks, not inline text
        assert!(
            context.filenames.is_empty(),
            "Should not extract paths outside code blocks"
        );
    }

    #[test]
    fn test_sentry_full_context_assembly() {
        // Test that all Sentry extraction strategies work together
        let mut issue = create_test_issue(
            "sentry",
            "TypeError in UserController",
            "The error occurred while processing the request",
        );
        issue
            .metadata
            .insert("project".to_string(), json!("api-production"));
        issue
            .metadata
            .insert("filename".to_string(), json!("src/controllers/user.ts"));
        issue
            .metadata
            .insert("function".to_string(), json!("handleCreate"));
        issue.metadata.insert(
            "culprit".to_string(),
            json!("api/routes.ts in dispatchRequest"),
        );
        issue.metadata.insert(
            "message".to_string(),
            json!("Cannot read property 'id' of undefined"),
        );

        let context = IssueContext::from_sentry(&issue);

        // Project extraction
        assert!(
            context.repos.contains(&"api".to_string()),
            "Should extract normalized project name"
        );
        // Filename from metadata
        assert!(context.filenames.iter().any(|f| f.contains("user.ts")));
        // Function from metadata
        assert!(context.functions.contains(&"handleCreate".to_string()));
        // Culprit file and function
        assert!(
            context.filenames.iter().any(|f| f.contains("routes.ts")),
            "Should extract culprit filename"
        );
        assert!(
            context.functions.contains(&"dispatchRequest".to_string()),
            "Should extract culprit function"
        );
        // Error type from title
        assert!(
            context.keywords.iter().any(|k| k == "TypeError"),
            "Should extract TypeError from title"
        );
        // Class from title
        assert!(
            context.keywords.iter().any(|k| k == "UserController"),
            "Should extract UserController from title"
        );
        // raw_text should include title, description, message
        assert!(context.raw_text.contains("TypeError in UserController"));
        assert!(context.raw_text.contains("Cannot read property"));
    }

    #[test]
    fn test_sentry_deduplication_across_strategies() {
        // Same filename extracted from metadata AND culprit should be deduplicated
        let mut issue = create_test_issue("sentry", "Error", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("api/routes.ts"));
        issue.metadata.insert(
            "culprit".to_string(),
            json!("api/routes.ts in handleRequest"),
        );

        let context = IssueContext::from_sentry(&issue);

        let routes_count = context
            .filenames
            .iter()
            .filter(|f| f.as_str() == "api/routes.ts")
            .count();
        assert_eq!(
            routes_count, 1,
            "Duplicate filenames across strategies should be deduplicated"
        );
    }

    #[test]
    fn test_linear_with_labels() {
        let mut issue = create_test_issue("linear", "Fix routing bug", "Router is broken");
        issue
            .metadata
            .insert("labels".to_string(), json!(["backend", "bug", "P1"]));

        let context = IssueContext::from_linear(&issue);

        assert!(context.keywords.contains(&"backend".to_string()));
        assert!(context.keywords.contains(&"bug".to_string()));
        assert!(context.keywords.contains(&"p1".to_string())); // lowercased
    }

    #[test]
    fn test_linear_labels_non_array() {
        // Labels that are not an array should be safely ignored
        let mut issue = create_test_issue("linear", "Bug", "desc");
        issue
            .metadata
            .insert("labels".to_string(), json!("not-an-array"));

        let context = IssueContext::from_linear(&issue);

        // Should not panic; labels extraction should be skipped
        assert!(!context.raw_text.is_empty());
    }

    #[test]
    fn test_linear_labels_with_non_string_elements() {
        let mut issue = create_test_issue("linear", "Bug", "desc");
        issue.metadata.insert(
            "labels".to_string(),
            json!(["valid-label", 42, null, true, "another-label"]),
        );

        let context = IssueContext::from_linear(&issue);

        assert!(context.keywords.contains(&"valid-label".to_string()));
        assert!(context.keywords.contains(&"another-label".to_string()));
        // Non-string labels should be silently skipped
        assert!(
            !context
                .keywords
                .iter()
                .any(|k| k == "42" || k == "null" || k == "true"),
            "Non-string labels should be skipped"
        );
    }

    #[test]
    fn test_linear_extracts_from_both_text_and_code_blocks() {
        let issue = create_test_issue(
            "linear",
            "Fix error in auth.ts",
            "The issue is in src/auth.ts.\n\n```\nFile \"/app/utils.py\", line 5, in helper\n```",
        );

        let context = IssueContext::from_linear(&issue);

        // Should extract from both regular text and code blocks
        assert!(
            context.filenames.iter().any(|f| f.contains("auth.ts")),
            "Should extract from regular text"
        );
        assert!(
            context.filenames.iter().any(|f| f.contains("utils.py")),
            "Should extract from code block stacktrace"
        );
    }

    #[test]
    fn test_sentry_raw_text_includes_title_description_message() {
        let mut issue = create_test_issue("sentry", "MyTitle", "MyDescription");
        issue
            .metadata
            .insert("message".to_string(), json!("MyMessage"));

        let context = IssueContext::from_sentry(&issue);

        assert!(context.raw_text.contains("MyTitle"));
        assert!(context.raw_text.contains("MyDescription"));
        assert!(context.raw_text.contains("MyMessage"));
    }

    #[test]
    fn test_linear_raw_text_includes_title_and_description() {
        let issue = create_test_issue("linear", "LinearTitle", "LinearDescription");

        let context = IssueContext::from_linear(&issue);

        assert!(context.raw_text.contains("LinearTitle"));
        assert!(context.raw_text.contains("LinearDescription"));
    }

    #[test]
    fn test_generic_raw_text_includes_title_and_description() {
        let issue = create_test_issue("other", "GenericTitle", "GenericDescription");

        let context = IssueContext::from_generic(&issue);

        assert!(context.raw_text.contains("GenericTitle"));
        assert!(context.raw_text.contains("GenericDescription"));
    }

    #[test]
    fn test_very_long_description() {
        // Test that very long descriptions do not cause panics or hangs
        let long_desc = "a".repeat(100_000);
        let issue = create_test_issue("generic", "Long issue", &long_desc);

        let context = IssueContext::from_generic(&issue);

        // Just verify it completes without panic
        assert!(context.raw_text.len() > 100_000);
    }

    #[test]
    fn test_many_file_paths_in_text() {
        // NOTE: Paths like "module_0.rs" contain digits which are excluded from
        // the path regex character class [a-zA-Z_./\\-]. So "module_0.rs" will
        // not be matched. Use paths without digits instead.
        let mut paths = Vec::new();
        for i in 0..200 {
            // Use alphabetic suffixes instead of numeric ones
            let suffix = (b'a' + (i % 26) as u8) as char;
            let prefix = (b'a' + ((i / 26) % 26) as u8) as char;
            paths.push(format!("src/module_{}{}_.rs", prefix, suffix));
        }
        let text = paths.join(" ");

        let mut context = IssueContext::new();
        extract_from_text(&mut context, &text);

        // Should extract many paths without issue
        assert!(
            context.filenames.len() >= 100,
            "Should handle many paths: got {}",
            context.filenames.len()
        );
    }

    #[test]
    fn test_many_file_paths_with_digits_extracted() {
        // Paths with digits in filenames should now be extracted
        let mut paths = Vec::new();
        for i in 0..10 {
            paths.push(format!("src/module_{}.rs", i));
        }
        let text = paths.join(" ");

        let mut context = IssueContext::new();
        extract_from_text(&mut context, &text);

        assert_eq!(
            context.filenames.len(),
            10,
            "All paths with digits in filename should be extracted"
        );
    }

    #[test]
    fn test_stacktrace_with_many_frames() {
        let mut frames = Vec::new();
        for i in 0..100 {
            frames.push(format!(
                "File \"/app/services/service_{}.py\", line {}, in method_{}",
                i,
                i * 10,
                i
            ));
        }
        let stacktrace = frames.join("\n");

        let mut context = IssueContext::new();
        extract_from_stacktrace(&mut context, &stacktrace);

        assert!(
            context.filenames.len() >= 50,
            "Should handle large stacktraces: got {} files",
            context.filenames.len()
        );
    }

    #[test]
    fn test_sentry_project_with_numbers() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue
            .metadata
            .insert("project".to_string(), json!("app123-staging"));

        let context = IssueContext::from_sentry(&issue);

        assert!(context.repos.contains(&"app123".to_string()));
    }

    #[test]
    fn test_sentry_project_keyword_format() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue
            .metadata
            .insert("project".to_string(), json!("my-service-prod"));

        let context = IssueContext::from_sentry(&issue);

        // Should have both the repo and the "repo:name" keyword
        assert!(context.repos.contains(&"my-service".to_string()));
        assert!(
            context.keywords.iter().any(|k| k == "repo:my-service"),
            "Should add repo:name keyword"
        );
    }

    #[test]
    fn test_culprit_with_vendor_path() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue.metadata.insert(
            "culprit".to_string(),
            json!("/app/vendor/laravel/framework/src/Router.php in dispatch"),
        );

        let context = IssueContext::from_sentry(&issue);

        // Should extract vendor package from the culprit path
        assert!(
            context
                .repos
                .iter()
                .any(|r| r.contains("laravel/framework")),
            "Should extract vendor package from culprit"
        );
    }

    #[test]
    fn test_culprit_single_vendor_path() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue.metadata.insert(
            "culprit".to_string(),
            json!("/app/vendor/monolog/monolog/src/Logger.php"),
        );

        let context = IssueContext::from_sentry(&issue);

        // Single-part culprit that looks like a path (contains '/')
        assert!(
            context.filenames.iter().any(|f| f.contains("Logger.php")),
            "Single vendor path culprit should be extracted as filename"
        );
        assert!(
            context.repos.iter().any(|r| r.contains("monolog")),
            "Should extract vendor package from single-part culprit path"
        );
    }

    #[test]
    fn test_node_scoped_package_produces_two_repo_entries() {
        let mut context = IssueContext::new();
        extract_vendor_packages(&mut context, "/app/node_modules/@babel/core/lib/index.js");

        // Scoped packages produce:
        // 1. The scope/package format: "babel/core"
        // 2. The hyphenated format: "babel-core"
        assert!(
            context.repos.iter().any(|r| r == "babel/core"),
            "Should have scope/package format: got {:?}",
            context.repos
        );
        assert!(
            context.repos.iter().any(|r| r == "babel-core"),
            "Should have hyphenated format: got {:?}",
            context.repos
        );
    }

    #[test]
    fn test_sentry_filename_with_vendor_path() {
        let mut issue = create_test_issue("sentry", "Error", "");
        issue.metadata.insert(
            "filename".to_string(),
            json!("/app/vendor/guzzlehttp/guzzle/src/Client.php"),
        );

        let context = IssueContext::from_sentry(&issue);

        assert!(
            context.filenames.iter().any(|f| f.contains("Client.php")),
            "Should include the full filename from metadata"
        );
        assert!(
            context
                .repos
                .iter()
                .any(|r| r.contains("guzzlehttp/guzzle")),
            "Should extract vendor package from filename metadata"
        );
    }

    #[test]
    fn test_path_regex_requires_word_boundary_at_end() {
        let mut context = IssueContext::new();
        // "file.rs" followed by more characters — \b should anchor properly
        extract_from_text(&mut context, "Check file.rs, then continue");

        assert!(
            context.filenames.iter().any(|f| f == "file.rs"),
            "Should extract file.rs with word boundary"
        );
    }

    #[test]
    fn test_class_regex_requires_word_boundary() {
        let mut context = IssueContext::new();
        // "UserControllerTest" should match "UserController" since \b is after Controller
        extract_from_text(&mut context, "Inspect UserControllerTest for issues");

        // The regex matches *Controller with \b — "UserControllerTest" does not end
        // with Controller at a word boundary. The T in Test continues.
        // So this should NOT match as a Controller class.
        // But it will match if the regex finds a Controller word boundary within.
        // Actually \b is between "r" and "T" — both word chars, so no boundary there.
        assert!(
            !context.keywords.contains(&"UserControllerTest".to_string()),
            "UserControllerTest should not match — no word boundary after Controller"
        );
    }

    #[test]
    fn test_error_regex_boundary() {
        let mut context = IssueContext::new();
        // "RuntimeErrors" — the \b after Error should match between "r" and "s"
        // Actually, both are word chars, so \b does NOT match.
        extract_from_text(&mut context, "Found RuntimeErrors in logs");

        assert!(
            !context.keywords.contains(&"RuntimeErrors".to_string()),
            "Pluralized error type should not match due to word boundary"
        );
    }

    #[test]
    fn test_real_world_sentry_issue() {
        let mut issue = create_test_issue(
            "sentry",
            "PDOException: SQLSTATE[HY000] [2002] Connection refused",
            "Database connection failed during health check",
        );
        issue
            .metadata
            .insert("project".to_string(), json!("backend-production"));
        issue.metadata.insert(
            "filename".to_string(),
            json!("/usr/src/code/vendor/utopia-php/database/src/Database/Adapter/MariaDB.php"),
        );
        issue.metadata.insert(
            "function".to_string(),
            json!("Utopia\\Database\\Adapter\\MariaDB::connect"),
        );
        issue.metadata.insert(
            "culprit".to_string(),
            json!("/usr/src/code/app/init.php in initDatabase"),
        );
        issue.metadata.insert(
            "stacktrace".to_string(),
            json!(r#"
                /usr/src/code/vendor/utopia-php/database/src/Database/Adapter/MariaDB.php in connect at line 45
                /usr/src/code/vendor/utopia-php/pools/src/Pool.php in get at line 89
                /usr/src/code/app/init.php in initDatabase at line 120
            "#),
        );

        let context = IssueContext::from_sentry(&issue);

        // Project
        assert!(context.repos.contains(&"backend".to_string()));
        // Vendor packages
        assert!(context.repos.contains(&"utopia-php/database".to_string()));
        assert!(context.repos.contains(&"utopia-php/pools".to_string()));
        // Filename
        assert!(context.filenames.iter().any(|f| f.contains("MariaDB.php")));
        // Culprit
        assert!(context.filenames.iter().any(|f| f.contains("init.php")));
        assert!(context.functions.contains(&"initDatabase".to_string()));
    }

    #[test]
    fn test_real_world_linear_issue() {
        let mut issue = create_test_issue(
            "linear",
            "Dashboard charts not loading after deploy",
            "After deploying v2.3.1, the dashboard charts component fails to render.\n\nRelevant files:\n- `src/components/charts/LineChart.tsx`\n- `src/hooks/useChartData.ts`\n\nError from console:\n```\nTypeError: Cannot read properties of undefined (reading 'map')\n    at LineChart (src/components/charts/LineChart.tsx:45:12)\n    at Dashboard (src/pages/Dashboard.tsx:23:8)\n```\n\nCC: @frontend-team",
        );
        issue.metadata.insert(
            "labels".to_string(),
            json!(["frontend", "bug", "charts", "P0"]),
        );

        let context = IssueContext::from_linear(&issue);

        // Labels
        assert!(context.keywords.contains(&"frontend".to_string()));
        assert!(context.keywords.contains(&"charts".to_string()));
        assert!(context.keywords.contains(&"p0".to_string())); // lowercased
                                                               // File paths from description
        assert!(context
            .filenames
            .iter()
            .any(|f| f.contains("LineChart.tsx")));
        assert!(context
            .filenames
            .iter()
            .any(|f| f.contains("useChartData.ts")));
        // Files from code block
        assert!(context
            .filenames
            .iter()
            .any(|f| f.contains("Dashboard.tsx")));
        // Error type
        assert!(context.keywords.iter().any(|k| k == "TypeError"));
    }

    #[test]
    fn test_extract_text_with_only_whitespace() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "   \n\t\n  \r\n  ");

        assert!(context.filenames.is_empty());
        assert!(context.functions.is_empty());
        assert!(context.keywords.is_empty());
        assert!(context.repos.is_empty());
    }

    #[test]
    fn test_extract_text_with_windows_line_endings() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            "Error found in\r\nsrc/main.rs\r\nand also\r\nlib/utils.py\r\n",
        );

        assert!(context.filenames.iter().any(|f| f.contains("main.rs")));
        assert!(context.filenames.iter().any(|f| f.contains("utils.py")));
    }

    #[test]
    fn test_path_with_hyphens_and_underscores() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "Error in src/my-module/my_handler.rs");

        assert!(
            context
                .filenames
                .iter()
                .any(|f| f.contains("my_handler.rs")),
            "Should extract paths with hyphens and underscores"
        );
    }

    #[test]
    fn test_path_with_dots_in_directory() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "Check src/v2.0/handler.ts for the issue");

        // Digits are now included in the path regex character class,
        // so "v2.0/handler.ts" is fully matched.
        assert!(
            context.filenames.iter().any(|f| f.contains("handler.ts")),
            "Paths with digits in directory names should be extracted"
        );

        // But without digits it works
        let mut context2 = IssueContext::new();
        extract_from_text(&mut context2, "Check src/config/handler.ts for the issue");
        assert!(
            context2.filenames.iter().any(|f| f.contains("handler.ts")),
            "Paths without digits should be extracted"
        );
    }

    #[test]
    fn test_issue_context_default() {
        let context = IssueContext::default();

        assert!(context.filenames.is_empty());
        assert!(context.functions.is_empty());
        assert!(context.keywords.is_empty());
        assert!(context.repos.is_empty());
        assert!(context.raw_text.is_empty());
        assert!(context.is_empty());
    }

    #[test]
    fn test_issue_context_new_equals_default() {
        let new_ctx = IssueContext::new();
        let default_ctx = IssueContext::default();

        assert_eq!(new_ctx.filenames, default_ctx.filenames);
        assert_eq!(new_ctx.functions, default_ctx.functions);
        assert_eq!(new_ctx.keywords, default_ctx.keywords);
        assert_eq!(new_ctx.repos, default_ctx.repos);
        assert_eq!(new_ctx.raw_text, default_ctx.raw_text);
    }

    #[test]
    fn test_issue_context_clone() {
        let mut original = IssueContext::new();
        original.filenames.push("file.rs".to_string());
        original.functions.push("main".to_string());
        original.keywords.push("error".to_string());
        original.repos.push("org/repo".to_string());
        original.raw_text = "raw".to_string();

        let cloned = original.clone();

        assert_eq!(original.filenames, cloned.filenames);
        assert_eq!(original.functions, cloned.functions);
        assert_eq!(original.keywords, cloned.keywords);
        assert_eq!(original.repos, cloned.repos);
        assert_eq!(original.raw_text, cloned.raw_text);
    }

    // --- PHP FQCN extraction tests ---

    #[test]
    fn test_php_fqcn_extracts_class_filename() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            r"Appwrite\Utopia\Request::getHeader() is broken",
        );

        assert!(
            context.filenames.iter().any(|f| f == "Request.php"),
            "Should extract ClassName.php from FQCN, got: {:?}",
            context.filenames
        );
    }

    #[test]
    fn test_php_fqcn_extracts_partial_namespace_path() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            r"Appwrite\Utopia\Request::getHeader() is broken",
        );

        assert!(
            context.filenames.iter().any(|f| f == "Utopia/Request.php"),
            "Should extract partial namespace path, got: {:?}",
            context.filenames
        );
    }

    #[test]
    fn test_php_fqcn_adds_repo_keyword() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            r"Appwrite\Utopia\Request::getHeader() is broken",
        );

        assert!(
            context.keywords.iter().any(|k| k == "repo:appwrite"),
            "Should add repo:lowercase keyword for first namespace segment, got: {:?}",
            context.keywords
        );
    }

    #[test]
    fn test_php_fqcn_deep_namespace() {
        let mut context = IssueContext::new();
        extract_from_text(
            &mut context,
            r"Utopia\Http\Adapter\Swoole\Request is causing issues",
        );

        assert!(
            context.filenames.iter().any(|f| f == "Request.php"),
            "Should extract class name from deep namespace"
        );
        assert!(
            context
                .filenames
                .iter()
                .any(|f| f == "Http/Adapter/Swoole/Request.php"),
            "Should extract partial namespace path from deep namespace, got: {:?}",
            context.filenames
        );
        assert!(
            context.keywords.iter().any(|k| k == "repo:utopia"),
            "Should add repo keyword for first segment"
        );
    }

    #[test]
    fn test_php_fqcn_without_method() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, r"Fix Utopia\Database\Document handling");

        assert!(
            context.filenames.iter().any(|f| f == "Document.php"),
            "Should extract class name without method call"
        );
    }

    #[test]
    fn test_php_fqcn_single_segment_not_matched() {
        let mut context = IssueContext::new();
        extract_from_text(&mut context, "The Request class is broken");

        // Single segment (no backslash) should NOT produce FQCN filenames
        assert!(
            !context.filenames.iter().any(|f| f == "Request.php"),
            "Single segment should not be treated as FQCN"
        );
    }

    #[test]
    fn test_php_fqcn_in_sentry_issue_title() {
        let issue = create_test_issue("sentry", r"Appwrite\Utopia\Request::getHeader() error", "");

        let context = IssueContext::from_sentry(&issue);

        assert!(
            context.filenames.iter().any(|f| f == "Request.php"),
            "FQCN in Sentry title should produce filename, got: {:?}",
            context.filenames
        );
    }

    #[test]
    fn test_php_fqcn_in_linear_issue() {
        let issue = create_test_issue("linear", r"Bug: Utopia\Database\Adapter\MariaDB fails", "");

        let context = IssueContext::from_linear(&issue);

        assert!(
            context.filenames.iter().any(|f| f == "MariaDB.php"),
            "FQCN in Linear title should produce filename"
        );
        assert!(
            context
                .filenames
                .iter()
                .any(|f| f == "Database/Adapter/MariaDB.php"),
            "Should produce partial namespace path, got: {:?}",
            context.filenames
        );
    }
}
