//! PHP output parsers (PHPUnit, PHPStan, PHP-CS-Fixer).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse PHPUnit test output (JUnit XML from `--log-junit -`).
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    // PHPUnit JUnit XML: count <testcase> elements and <failure>/<error> children.
    // Simplified: count test results from summary line in stderr or stdout.
    // PHPUnit outputs lines like: "Tests: 42, Assertions: 100, Failures: 2, Errors: 1"
    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Tests:") || trimmed.starts_with("OK (") {
            if let Some(tests) = extract_phpunit_count(trimmed, "Tests:") {
                // Total tests reported; failures/errors subtract from passed
                let failures = extract_phpunit_count(trimmed, "Failures:").unwrap_or(0);
                let errors = extract_phpunit_count(trimmed, "Errors:").unwrap_or(0);
                let skipped = extract_phpunit_count(trimmed, "Skipped:").unwrap_or(0);
                snapshot.passed = tests.saturating_sub(failures + errors + skipped);
                snapshot.failed = failures + errors;
                snapshot.skipped = skipped;
                snapshot.errors = errors;
                snapshot.warnings = 0;
            }
            // "OK (42 tests, 100 assertions)" format
            if trimmed.starts_with("OK (") {
                if let Some(n) = trimmed
                    .strip_prefix("OK (")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u32>().ok())
                {
                    snapshot.passed = n;
                }
            }
        }
        // Also detect individual failure lines
        if trimmed.starts_with("FAILURES!") {
            // Already captured via summary line above
        }
    }

    // Generate diagnostics for failures from JUnit XML <failure> tags
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.contains("<failure") || trimmed.contains("<error") {
            snapshot.diagnostics.push(Diagnostic {
                file: "phpunit".to_string(),
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

/// Parse PHPStan analysis output (`--error-format=json`).
pub fn parse_analysis(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    // PHPStan JSON format: {"totals":{"errors":0,"file_errors":5},"files":{...},"errors":[]}
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(totals) = v.get("totals") {
            snapshot.errors = totals
                .get("file_errors")
                .and_then(|e| e.as_u64())
                .unwrap_or(0) as u32;
        }
        if let Some(files) = v.get("files").and_then(|f| f.as_object()) {
            for (file_path, file_data) in files {
                if let Some(messages) = file_data.get("messages").and_then(|m| m.as_array()) {
                    for msg in messages {
                        let line = msg.get("line").and_then(|l| l.as_u64()).map(|l| l as u32);
                        let message = msg
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        snapshot.diagnostics.push(Diagnostic {
                            file: file_path.clone(),
                            line,
                            column: None,
                            severity: DiagnosticSeverity::Error,
                            code: None,
                            message,
                        });
                    }
                }
            }
        }
    }

    snapshot
}

/// Parse PHP-CS-Fixer lint output (`--dry-run --format=json`).
pub fn parse_lint(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    // PHP-CS-Fixer JSON: {"files":[{"name":"src/Foo.php","appliedFixers":["braces"]}]}
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
            snapshot.warnings = files.len() as u32;
            for file in files {
                let name = file
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let fixers = file
                    .get("appliedFixers")
                    .and_then(|f| f.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                snapshot.diagnostics.push(Diagnostic {
                    file: name,
                    line: None,
                    column: None,
                    severity: DiagnosticSeverity::Warning,
                    code: None,
                    message: format!("Needs fixing: {}", fixers),
                });
            }
        }
    }

    snapshot
}

/// Parse PHPUnit coverage output (Clover XML from `--coverage-clover`).
pub fn parse_coverage(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    // Clover XML: look for <metrics ... coveredstatements="X" statements="Y" />
    // Simple regex-free XML scraping for the project-level metrics line.
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.contains("<metrics") && trimmed.contains("statements=") {
            let statements = extract_xml_attr(trimmed, "statements");
            let covered = extract_xml_attr(trimmed, "coveredstatements");
            if let (Some(total), Some(cov)) = (statements, covered) {
                if total > 0.0 {
                    snapshot.line_coverage_pct = Some((cov / total) * 100.0);
                }
            }
        }
    }

    snapshot
}

fn extract_phpunit_count(line: &str, prefix: &str) -> Option<u32> {
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
    fn test_parse_phpunit_summary() {
        let stderr = "Tests: 42, Assertions: 100, Failures: 2, Errors: 1, Skipped: 3";
        let snap = parse_test("", stderr);
        assert_eq!(snap.passed, 36);
        assert_eq!(snap.failed, 3);
        assert_eq!(snap.skipped, 3);
    }

    #[test]
    fn test_parse_phpunit_ok() {
        let stderr = "OK (42 tests, 100 assertions)";
        let snap = parse_test("", stderr);
        assert_eq!(snap.passed, 42);
    }

    #[test]
    fn test_parse_phpstan_json() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":2},"files":{"src/Foo.php":{"errors":1,"messages":[{"message":"Undefined variable $x","line":10}]},"src/Bar.php":{"errors":1,"messages":[{"message":"Type mismatch","line":20}]}},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_php_cs_fixer_json() {
        let stdout = r#"{"files":[{"name":"src/Foo.php","appliedFixers":["braces","spaces"]}]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_coverage_clover() {
        let stdout = r#"<?xml version="1.0"?>
<coverage>
  <project>
    <metrics statements="100" coveredstatements="85" />
  </project>
</coverage>"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.0).abs() < 0.01);
    }
}
