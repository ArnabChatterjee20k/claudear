//! Blast radius classification.
//!
//! Classifies issues into blast-radius tiers based on file paths, functions,
//! and culprits found in issue metadata, matched against configurable path patterns.

use claudear_config::config::PrioritisationConfig;
use claudear_core::types::{BlastRadius, Issue};

/// Classify the blast radius of an issue based on its metadata.
///
/// Extracts `filename`, `function`, and `culprit` from the issue metadata,
/// then checks against configured path patterns in priority order:
/// Critical > Infrastructure > Cosmetic > Test > Core > Peripheral.
///
/// Returns `Core` when no metadata is available (conservative default).
pub fn classify(issue: &Issue, config: &PrioritisationConfig) -> BlastRadius {
    let signals: Vec<String> = ["filename", "function", "culprit"]
        .iter()
        .filter_map(|key| issue.get_metadata::<String>(key))
        .collect();

    if signals.is_empty() {
        return BlastRadius::Core;
    }

    let haystack: Vec<String> = signals.iter().map(|s| s.to_lowercase()).collect();

    // Check in priority order: most impactful first, then least impactful, then middle.
    if any_match(&haystack, &config.critical_paths) {
        return BlastRadius::Critical;
    }
    if any_match(&haystack, &config.infra_paths) {
        return BlastRadius::Infrastructure;
    }
    if any_match(&haystack, &config.cosmetic_paths) {
        return BlastRadius::Cosmetic;
    }
    if any_match(&haystack, &config.test_paths) {
        return BlastRadius::Test;
    }
    if any_match(&haystack, &config.core_paths) {
        return BlastRadius::Core;
    }

    BlastRadius::Peripheral
}

/// Word-boundary/path-segment matching: splits signals on path separators
/// (`/`, `\`, `.`, `_`, `-`) and matches whole segments against patterns.
/// This prevents false positives like "ci" matching "social" or "core" matching "score".
fn any_match(haystack: &[String], patterns: &[String]) -> bool {
    let lower_patterns: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();
    haystack.iter().any(|h| {
        let segments: Vec<&str> = h.split(&['/', '\\', '.', '_', '-'][..]).collect();
        lower_patterns
            .iter()
            .any(|p| segments.iter().any(|seg| seg == p))
    })
}

/// Map a blast radius to its scoring component (0.0-1.0).
pub fn blast_radius_score(br: BlastRadius) -> f64 {
    match br {
        BlastRadius::Critical => 1.0,
        BlastRadius::Infrastructure => 0.8,
        BlastRadius::Core => 0.6,
        BlastRadius::Peripheral => 0.4,
        BlastRadius::Test => 0.2,
        BlastRadius::Cosmetic => 0.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudear_config::config::PrioritisationConfig;
    use claudear_core::types::Issue;

    fn make_issue_with_file(filename: &str) -> Issue {
        let mut issue = Issue::new("id", "SH-1", "title", "url", "sentry");
        issue.set_metadata("filename", filename);
        issue
    }

    fn default_config() -> PrioritisationConfig {
        PrioritisationConfig::default()
    }

    #[test]
    fn critical_path_detection() {
        let issue = make_issue_with_file("src/auth/login.rs");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Critical);
    }

    #[test]
    fn infra_path_detection() {
        let issue = make_issue_with_file("deploy/docker-compose.yml");
        assert_eq!(
            classify(&issue, &default_config()),
            BlastRadius::Infrastructure
        );
    }

    #[test]
    fn test_path_detection() {
        let issue = make_issue_with_file("src/foo/test_bar.py");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Test);
    }

    #[test]
    fn cosmetic_path_detection() {
        let issue = make_issue_with_file("README.md");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Cosmetic);
    }

    #[test]
    fn core_path_detection() {
        let issue = make_issue_with_file("src/api/routes.rs");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Core);
    }

    #[test]
    fn peripheral_fallback() {
        let issue = make_issue_with_file("src/utils/format.rs");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Peripheral);
    }

    #[test]
    fn no_metadata_defaults_to_core() {
        let issue = Issue::new("id", "SH-1", "title", "url", "sentry");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Core);
    }

    #[test]
    fn critical_wins_over_test() {
        // File path contains both "auth" (critical) and "test" patterns
        let issue = make_issue_with_file("test/auth/integration.rs");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Critical);
    }

    #[test]
    fn culprit_used_as_signal() {
        let mut issue = Issue::new("id", "SH-1", "title", "url", "sentry");
        issue.set_metadata("culprit", "payment_service.charge");
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Critical);
    }

    #[test]
    fn blast_radius_scores() {
        assert_eq!(blast_radius_score(BlastRadius::Critical), 1.0);
        assert_eq!(blast_radius_score(BlastRadius::Cosmetic), 0.1);
    }

    #[test]
    fn segment_matching_prevents_ci_false_positive() {
        // "social" contains the substring "ci" but the segment "social" != "ci"
        let issue = make_issue_with_file("src/social/feed.rs");
        assert_ne!(
            classify(&issue, &default_config()),
            BlastRadius::Infrastructure,
            "\"social\" must not match the infra pattern \"ci\""
        );
    }

    #[test]
    fn segment_matching_prevents_core_false_positive() {
        // "score" contains substring "core" but segment "score" != "core"
        let issue = make_issue_with_file("src/score/engine.rs");
        assert_ne!(
            classify(&issue, &default_config()),
            BlastRadius::Core,
            "\"score\" must not match the core pattern \"core\" via segment matching"
        );
        // Should fall through to Peripheral
        assert_eq!(classify(&issue, &default_config()), BlastRadius::Peripheral);
    }

    #[test]
    fn segment_matching_exact_segment_works() {
        // "ci" as its own segment should match infra
        let issue = make_issue_with_file("ci/build.sh");
        assert_eq!(
            classify(&issue, &default_config()),
            BlastRadius::Infrastructure
        );
    }
}
