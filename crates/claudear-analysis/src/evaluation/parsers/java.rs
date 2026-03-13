//! Java output parsers (Maven surefire, Maven build, google-java-format, JaCoCo).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse Maven surefire test output.
///
/// Summary: "Tests run: X, Failures: Y, Errors: Z, Skipped: W"
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Tests run:") && trimmed.contains("Failures:") {
            let total = extract_maven_count(trimmed, "Tests run:").unwrap_or(0);
            let failures = extract_maven_count(trimmed, "Failures:").unwrap_or(0);
            let errors = extract_maven_count(trimmed, "Errors:").unwrap_or(0);
            let skipped = extract_maven_count(trimmed, "Skipped:").unwrap_or(0);
            snapshot.passed = total.saturating_sub(failures + errors + skipped);
            snapshot.failed = failures + errors;
            snapshot.skipped = skipped;
            snapshot.errors = snapshot.failed;
        }
        // Individual failure: "[ERROR] testName(com.package.TestClass): message"
        if trimmed.starts_with("[ERROR]") && trimmed.contains("Test") {
            snapshot.diagnostics.push(Diagnostic {
                file: "maven-test".to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Error,
                code: None,
                message: trimmed.to_string(),
            });
        }
    }

    snapshot
}

/// Parse Maven build output for analysis.
///
/// [ERROR] and [WARNING] lines.
pub fn parse_analysis(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[ERROR]") {
            snapshot.errors += 1;
            snapshot.diagnostics.push(Diagnostic {
                file: "maven".to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Error,
                code: None,
                message: trimmed.to_string(),
            });
        } else if trimmed.starts_with("[WARNING]") {
            snapshot.warnings += 1;
            snapshot.diagnostics.push(Diagnostic {
                file: "maven".to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Warning,
                code: None,
                message: trimmed.to_string(),
            });
        }
    }

    snapshot
}

/// Parse google-java-format lint output.
///
/// Lists files that differ from the formatted version.
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && trimmed.ends_with(".java") {
            snapshot.warnings += 1;
            snapshot.diagnostics.push(Diagnostic {
                file: trimmed.to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Warning,
                code: None,
                message: "File needs formatting".to_string(),
            });
        }
    }

    snapshot
}

/// Parse JaCoCo XML coverage output.
///
/// `<counter type="LINE" missed="X" covered="Y"/>`
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains("type=\"LINE\"")
            && trimmed.contains("missed=")
            && trimmed.contains("covered=")
        {
            let missed = extract_xml_attr(trimmed, "missed");
            let covered = extract_xml_attr(trimmed, "covered");
            if let (Some(m), Some(c)) = (missed, covered) {
                let total = m + c;
                if total > 0.0 {
                    snapshot.line_coverage_pct = Some(c / total * 100.0);
                }
            }
        }
        if trimmed.contains("type=\"BRANCH\"")
            && trimmed.contains("missed=")
            && trimmed.contains("covered=")
        {
            let missed = extract_xml_attr(trimmed, "missed");
            let covered = extract_xml_attr(trimmed, "covered");
            if let (Some(m), Some(c)) = (missed, covered) {
                let total = m + c;
                if total > 0.0 {
                    snapshot.branch_coverage_pct = Some(c / total * 100.0);
                }
            }
        }
    }

    snapshot
}

fn extract_maven_count(line: &str, prefix: &str) -> Option<u32> {
    let idx = line.find(prefix)?;
    let after = &line[idx + prefix.len()..];
    let num_str: String = after
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse().ok()
}

fn extract_xml_attr(line: &str, attr: &str) -> Option<f64> {
    let needle = format!("{}=\"", attr);
    let idx = line.find(&needle)?;
    let after = &line[idx + needle.len()..];
    let val_str: String = after.chars().take_while(|c| *c != '"').collect();
    val_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_maven_test() {
        let stdout = "Tests run: 10, Failures: 2, Errors: 1, Skipped: 1";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 6);
        assert_eq!(snap.failed, 3);
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.errors, 3);
    }

    #[test]
    fn test_parse_maven_test_all_passed() {
        let stdout = "Tests run: 5, Failures: 0, Errors: 0, Skipped: 0";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 5);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_maven_test_empty() {
        let snap = parse_test("", "");
        assert_eq!(snap.passed, 0);
    }

    #[test]
    fn test_parse_maven_analysis() {
        let stdout = "[ERROR] Some build error\n[WARNING] Deprecated API\n[INFO] Build done";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_maven_analysis_empty() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_google_java_format() {
        let stdout = "src/Main.java\nsrc/Utils.java";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_lint_empty() {
        let snap = parse_lint("", "");
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_jacoco_coverage() {
        let stdout = r#"<counter type="LINE" missed="15" covered="85"/>
<counter type="BRANCH" missed="10" covered="40"/>"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_empty() {
        let snap = parse_coverage("", "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }
}
