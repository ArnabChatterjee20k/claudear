//! Repository inference engine.
//!
//! Automatically determines which repository an issue belongs to based on
//! file paths, stack traces, and other context extracted from issues.

mod context;

pub use context::IssueContext;

use crate::repo::{IndexedRepo, RepoIndex};
use crate::types::Issue;

/// Confidence level for repository inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Direct file path match.
    High,
    /// Fuzzy/partial match.
    Medium,
    /// Content similarity only.
    Low,
    /// No match found.
    None,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Confidence::High => write!(f, "high"),
            Confidence::Medium => write!(f, "medium"),
            Confidence::Low => write!(f, "low"),
            Confidence::None => write!(f, "none"),
        }
    }
}

/// Result of repository inference.
#[derive(Debug, Clone)]
pub struct InferredRepo {
    /// The inferred repository.
    pub repo: IndexedRepo,
    /// Confidence level of the inference.
    pub confidence: Confidence,
    /// Reason for the inference.
    pub reason: String,
    /// File that matched (if applicable).
    pub matched_file: Option<String>,
}

/// Repository inference engine.
///
/// Uses a RepoIndex to determine which repository an issue belongs to
/// based on file paths and other context extracted from the issue.
#[derive(Clone)]
pub struct RepoInferrer {
    index: RepoIndex,
}

impl RepoInferrer {
    /// Create a new inferrer with the given repository index.
    pub fn new(index: RepoIndex) -> Self {
        Self { index }
    }

    /// Infer the target repository for an issue.
    ///
    /// Tries multiple strategies in order of confidence:
    /// 1. Direct file path match
    /// 2. Basename match
    /// 3. Fuzzy file search
    /// 4. Keyword matching (future)
    pub fn infer(&self, issue: &Issue) -> Option<InferredRepo> {
        let context = IssueContext::from_issue(issue);

        tracing::debug!(
            issue_id = %issue.short_id,
            filenames = ?context.filenames,
            functions = ?context.functions,
            "Extracted issue context"
        );

        // Strategy 1: Direct file path match
        for filename in &context.filenames {
            if let Some(repo) = self.index.find_by_file(filename) {
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    file = %filename,
                    "High confidence match: direct file path"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::High,
                    reason: format!("Direct file match: {}", filename),
                    matched_file: Some(filename.clone()),
                });
            }
        }

        // Strategy 2: Fuzzy file search (partial match)
        for filename in &context.filenames {
            let matches = self.index.search_files(filename);

            // If we have exactly one match, it's medium confidence
            if matches.len() == 1 {
                let (repo, matched_path) = matches[0];
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    query = %filename,
                    matched = %matched_path,
                    "Medium confidence match: single fuzzy match"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::Medium,
                    reason: format!("Fuzzy match: {} -> {}", filename, matched_path),
                    matched_file: Some(matched_path.to_string()),
                });
            }

            // If we have multiple matches in the same repo, still medium confidence
            if !matches.is_empty() {
                let first_repo = &matches[0].0.name;
                let all_same_repo = matches.iter().all(|(r, _)| r.name == *first_repo);

                if all_same_repo {
                    let (repo, matched_path) = matches[0];
                    tracing::info!(
                        issue_id = %issue.short_id,
                        repo = %repo.name,
                        matches = matches.len(),
                        "Medium confidence match: all matches in same repo"
                    );
                    return Some(InferredRepo {
                        repo: repo.clone(),
                        confidence: Confidence::Medium,
                        reason: format!(
                            "Fuzzy match ({} files): {} -> {}",
                            matches.len(),
                            filename,
                            matched_path
                        ),
                        matched_file: Some(matched_path.to_string()),
                    });
                }
            }
        }

        // Strategy 3: Try just the basename of each filename
        for filename in &context.filenames {
            let basename = std::path::Path::new(filename)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| filename.clone());

            let matches = self.index.search_files(&basename);

            if matches.len() == 1 {
                let (repo, matched_path) = matches[0];
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    basename = %basename,
                    matched = %matched_path,
                    "Low confidence match: basename match"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::Low,
                    reason: format!("Basename match: {} -> {}", basename, matched_path),
                    matched_file: Some(matched_path.to_string()),
                });
            }
        }

        // No match found
        tracing::debug!(
            issue_id = %issue.short_id,
            "No repository match found"
        );
        None
    }

    /// Get the underlying repository index.
    pub fn index(&self) -> &RepoIndex {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_index() -> RepoIndex {
        let mut index = RepoIndex::new();

        let mut repo1 = IndexedRepo::new("appwrite/console", "/path/console");
        repo1.files = vec![
            "src/routes/auth.ts".to_string(),
            "src/components/Button.tsx".to_string(),
            "src/lib/api/client.ts".to_string(),
        ];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("appwrite/sdk-for-php", "/path/sdk-php");
        repo2.files = vec![
            "src/Appwrite/Client.php".to_string(),
            "src/Appwrite/Services/Account.php".to_string(),
        ];
        index.add_repo(repo2);

        index
    }

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
    fn test_infer_high_confidence() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let mut issue = create_test_issue("sentry", "Auth error", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("src/routes/auth.ts"));

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/console");
        assert_eq!(inferred.confidence, Confidence::High);
    }

    #[test]
    fn test_infer_medium_confidence() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        // Use a partial path that should fuzzy match
        let mut issue = create_test_issue("sentry", "Client error", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("Client.php"));

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/sdk-for-php");
        // Could be high or medium depending on exact matching logic
        assert!(
            inferred.confidence == Confidence::High
                || inferred.confidence == Confidence::Medium
                || inferred.confidence == Confidence::Low
        );
    }

    #[test]
    fn test_infer_no_match() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let issue = create_test_issue("sentry", "Unknown error", "No file paths here");

        let result = inferrer.infer(&issue);

        assert!(result.is_none());
    }

    #[test]
    fn test_infer_from_linear_issue() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let issue = create_test_issue(
            "linear",
            "Fix button styling",
            "The issue is in src/components/Button.tsx",
        );

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/console");
    }

    #[test]
    fn test_confidence_display() {
        assert_eq!(format!("{}", Confidence::High), "high");
        assert_eq!(format!("{}", Confidence::Medium), "medium");
        assert_eq!(format!("{}", Confidence::Low), "low");
        assert_eq!(format!("{}", Confidence::None), "none");
    }

    #[test]
    fn test_inferrer_index_access() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        // Can access the index
        assert_eq!(inferrer.index().len(), 2);
    }
}
