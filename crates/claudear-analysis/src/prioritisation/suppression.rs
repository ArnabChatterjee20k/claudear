//! Suppression rule evaluation.
//!
//! Extracts issue field values and matches them against user-configured rules.
//! First matching rule wins.

use claudear_core::types::{
    Issue, SuppressionField, SuppressionMatchMode, SuppressionResult, SuppressionRule,
};
use regex_lite::Regex;
use std::collections::HashMap;

/// Maximum allowed regex pattern length to prevent pathological patterns.
const MAX_REGEX_PATTERN_LENGTH: usize = 1024;

/// Pre-compiled regex cache keyed by pattern string.
pub struct RegexCache {
    compiled: HashMap<String, Option<Regex>>,
}

impl RegexCache {
    /// Build a new regex cache from a set of suppression rules.
    ///
    /// Pre-compiles all regex patterns once so they can be reused across multiple
    /// `check_issue_with_cache` calls.
    pub fn new(rules: &[SuppressionRule]) -> Self {
        let mut compiled = HashMap::new();
        for rule in rules {
            if rule.match_mode == SuppressionMatchMode::Regex
                && !compiled.contains_key(&rule.pattern)
            {
                let re = if rule.pattern.len() > MAX_REGEX_PATTERN_LENGTH {
                    tracing::warn!(
                        pattern_len = rule.pattern.len(),
                        max = MAX_REGEX_PATTERN_LENGTH,
                        rule = %rule.name,
                        "Suppression regex pattern exceeds maximum length, skipping"
                    );
                    None
                } else {
                    match Regex::new(&rule.pattern) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            tracing::warn!(
                                pattern = %rule.pattern,
                                error = %e,
                                "Invalid suppression regex"
                            );
                            None
                        }
                    }
                };
                compiled.insert(rule.pattern.clone(), re);
            }
        }
        RegexCache { compiled }
    }

    fn is_match(&self, pattern: &str, value: &str) -> bool {
        self.compiled
            .get(pattern)
            .and_then(|opt| opt.as_ref())
            .is_some_and(|re| re.is_match(value))
    }
}

/// Evaluate all suppression rules against a batch of candidates.
///
/// Returns `(kept, suppressed)` where each suppressed entry includes its result.
/// Pre-compiles all regex patterns once for the entire batch.
pub fn evaluate(
    rules: &[SuppressionRule],
    candidates: Vec<Issue>,
) -> (Vec<Issue>, Vec<(Issue, SuppressionResult)>) {
    let cache = RegexCache::new(rules);
    let mut kept = Vec::new();
    let mut suppressed = Vec::new();

    for issue in candidates {
        let result = check_issue_with_cache(rules, &issue, &cache);
        if result.suppressed {
            suppressed.push((issue, result));
        } else {
            kept.push(issue);
        }
    }

    (kept, suppressed)
}

/// Check a single issue against all suppression rules (first match wins).
///
/// This compiles regexes on every call. Prefer [`evaluate`] for batch processing
/// or [`check_issue_with_cache`] when you have a pre-built cache.
pub fn check_issue(rules: &[SuppressionRule], issue: &Issue) -> SuppressionResult {
    let cache = RegexCache::new(rules);
    check_issue_with_cache(rules, issue, &cache)
}

/// Check a single issue against all suppression rules using a pre-compiled regex cache.
pub fn check_issue_with_cache(
    rules: &[SuppressionRule],
    issue: &Issue,
    cache: &RegexCache,
) -> SuppressionResult {
    for rule in rules {
        // Scope check: if the rule specifies sources, the issue must belong to one.
        if !rule.sources.is_empty()
            && !rule
                .sources
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&issue.source))
        {
            continue;
        }

        let field_value = extract_field(issue, &rule.field);
        let field_value = match field_value {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        if matches_pattern_cached(&field_value, &rule.pattern, rule.match_mode, cache) {
            return SuppressionResult {
                suppressed: true,
                matched_rule: Some(rule.name.clone()),
                reason: Some(if rule.reason.is_empty() {
                    format!("Matched rule '{}'", rule.name)
                } else {
                    rule.reason.clone()
                }),
            };
        }
    }

    SuppressionResult {
        suppressed: false,
        matched_rule: None,
        reason: None,
    }
}

/// Extract the value of a field from an issue for matching.
fn extract_field(issue: &Issue, field: &SuppressionField) -> Option<String> {
    match field {
        SuppressionField::Title => Some(issue.title.clone()),
        SuppressionField::Description => issue.description.clone(),
        SuppressionField::Source => Some(issue.source.clone()),
        SuppressionField::Culprit => issue.get_metadata::<String>("culprit"),
        SuppressionField::Filename => issue.get_metadata::<String>("filename"),
        SuppressionField::ErrorType => issue.get_metadata::<String>("error_type"),
        SuppressionField::Project => issue.get_metadata::<String>("project"),
        SuppressionField::Labels => issue
            .get_metadata::<Vec<String>>("labels")
            .map(|v| v.join(",")),
        SuppressionField::Metadata(key) => issue.metadata.get(key).map(|v| match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string(),
        }),
    }
}

/// Check if `value` matches `pattern` according to `mode`, using the pre-compiled regex cache.
fn matches_pattern_cached(
    value: &str,
    pattern: &str,
    mode: SuppressionMatchMode,
    cache: &RegexCache,
) -> bool {
    match mode {
        SuppressionMatchMode::Contains => {
            let lower = value.to_lowercase();
            let pat = pattern.to_lowercase();
            lower.contains(&pat)
        }
        SuppressionMatchMode::Exact => value.to_lowercase() == pattern.to_lowercase(),
        SuppressionMatchMode::Regex => cache.is_match(pattern, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudear_core::types::{Issue, SuppressionField, SuppressionMatchMode, SuppressionRule};

    fn make_issue(title: &str, source: &str) -> Issue {
        Issue::new("id-1", "SHORT-1", title, "https://example.com", source)
    }

    #[test]
    fn contains_match_case_insensitive() {
        let rules = vec![SuppressionRule {
            name: "flaky".into(),
            field: SuppressionField::Title,
            pattern: "FLAKY".into(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "Known flaky test".into(),
        }];
        let issue = make_issue("Some flaky test failure", "sentry");
        let result = check_issue(&rules, &issue);
        assert!(result.suppressed);
        assert_eq!(result.matched_rule.as_deref(), Some("flaky"));
    }

    #[test]
    fn exact_match_case_insensitive() {
        let rules = vec![SuppressionRule {
            name: "exact".into(),
            field: SuppressionField::Title,
            pattern: "exact title".into(),
            match_mode: SuppressionMatchMode::Exact,
            sources: vec![],
            reason: "".into(),
        }];
        let issue = make_issue("Exact Title", "linear");
        let result = check_issue(&rules, &issue);
        assert!(result.suppressed);
    }

    #[test]
    fn regex_match() {
        let rules = vec![SuppressionRule {
            name: "timeout".into(),
            field: SuppressionField::Title,
            pattern: r"timeout.*\d+ms".into(),
            match_mode: SuppressionMatchMode::Regex,
            sources: vec![],
            reason: "transient timeout".into(),
        }];
        let issue = make_issue("Request timeout after 5000ms", "sentry");
        let result = check_issue(&rules, &issue);
        assert!(result.suppressed);
    }

    #[test]
    fn source_scoping() {
        let rules = vec![SuppressionRule {
            name: "sentry-only".into(),
            field: SuppressionField::Title,
            pattern: "noise".into(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec!["sentry".into()],
            reason: "".into(),
        }];
        let sentry_issue = make_issue("some noise", "sentry");
        let linear_issue = make_issue("some noise", "linear");
        assert!(check_issue(&rules, &sentry_issue).suppressed);
        assert!(!check_issue(&rules, &linear_issue).suppressed);
    }

    #[test]
    fn no_match_returns_not_suppressed() {
        let rules = vec![SuppressionRule {
            name: "nope".into(),
            field: SuppressionField::Title,
            pattern: "xyz".into(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "".into(),
        }];
        let issue = make_issue("real bug", "linear");
        let result = check_issue(&rules, &issue);
        assert!(!result.suppressed);
        assert!(result.matched_rule.is_none());
    }

    #[test]
    fn first_rule_wins() {
        let rules = vec![
            SuppressionRule {
                name: "first".into(),
                field: SuppressionField::Title,
                pattern: "crash".into(),
                match_mode: SuppressionMatchMode::Contains,
                sources: vec![],
                reason: "first reason".into(),
            },
            SuppressionRule {
                name: "second".into(),
                field: SuppressionField::Title,
                pattern: "crash".into(),
                match_mode: SuppressionMatchMode::Contains,
                sources: vec![],
                reason: "second reason".into(),
            },
        ];
        let issue = make_issue("app crash", "sentry");
        let result = check_issue(&rules, &issue);
        assert_eq!(result.matched_rule.as_deref(), Some("first"));
    }

    #[test]
    fn evaluate_splits_correctly() {
        let rules = vec![SuppressionRule {
            name: "noise".into(),
            field: SuppressionField::Title,
            pattern: "noise".into(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "".into(),
        }];
        let issues = vec![
            make_issue("real bug", "linear"),
            make_issue("noisy noise", "sentry"),
            make_issue("another bug", "linear"),
        ];
        let (kept, suppressed) = evaluate(&rules, issues);
        assert_eq!(kept.len(), 2);
        assert_eq!(suppressed.len(), 1);
        assert_eq!(suppressed[0].0.title, "noisy noise");
    }

    #[test]
    fn regex_cache_rejects_oversized_pattern() {
        // Pattern exceeding MAX_REGEX_PATTERN_LENGTH should not match anything
        let long_pattern = "a".repeat(MAX_REGEX_PATTERN_LENGTH + 1);
        let rules = vec![SuppressionRule {
            name: "long".into(),
            field: SuppressionField::Title,
            pattern: long_pattern.clone(),
            match_mode: SuppressionMatchMode::Regex,
            sources: vec![],
            reason: "oversized".into(),
        }];
        let issue = make_issue(&"a".repeat(MAX_REGEX_PATTERN_LENGTH + 1), "sentry");
        let result = check_issue(&rules, &issue);
        assert!(
            !result.suppressed,
            "Oversized regex pattern must be skipped and not match"
        );
    }

    #[test]
    fn metadata_field_extraction() {
        let rules = vec![SuppressionRule {
            name: "err".into(),
            field: SuppressionField::ErrorType,
            pattern: "RateLimitError".into(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "".into(),
        }];
        let mut issue = make_issue("rate limit", "sentry");
        issue.set_metadata("error_type", "RateLimitError");
        assert!(check_issue(&rules, &issue).suppressed);
    }
}
