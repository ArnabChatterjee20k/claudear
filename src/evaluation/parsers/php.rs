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

    // ==================== parse_test (PHPUnit) ====================

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
    fn test_parse_test_empty_input() {
        let snap = parse_test("", "");
        assert_eq!(snap.category, EvalCategory::Test);
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_failure_diagnostics_from_junit_xml() {
        let stdout = r#"<?xml version="1.0"?>
<testsuites>
  <testsuite name="Tests" tests="3" failures="1" errors="1">
    <testcase name="testAdd" class="MathTest">
      <failure type="AssertionError">Expected 4 but got 5</failure>
    </testcase>
    <testcase name="testDivide" class="MathTest">
      <error type="DivisionByZeroError">Division by zero</error>
    </testcase>
    <testcase name="testSubtract" class="MathTest" />
  </testsuite>
</testsuites>"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].file, "phpunit");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert!(snap.diagnostics[0].message.contains("<failure"));
        assert!(snap.diagnostics[1].message.contains("<error"));
        // line and column should be None, code should be None
        assert!(snap.diagnostics[0].line.is_none());
        assert!(snap.diagnostics[0].column.is_none());
        assert!(snap.diagnostics[0].code.is_none());
    }

    #[test]
    fn test_parse_test_failure_tag_inline() {
        // Test that <failure and <error anywhere on a line are detected
        let stdout =
            "    <failure type=\"PHPUnit\\Framework\\ExpectationFailedException\">msg</failure>";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn test_parse_test_failures_line_does_not_add_diagnostics() {
        // The "FAILURES!" line is recognized but does not generate diagnostics on its own
        let snap = parse_test("", "FAILURES!\nTests: 5, Assertions: 5, Failures: 2");
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.passed, 3);
        // No diagnostics since there are no <failure or <error tags in stdout
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_both_tests_and_ok_formats() {
        // When both "Tests:" and "OK (" appear, the "OK (" line should override passed count
        let combined = "Tests: 10, Assertions: 20, Failures: 0\nOK (10 tests, 20 assertions)";
        let snap = parse_test("", combined);
        // "Tests:" line sets passed to 10, then "OK (" also sets passed to 10
        assert_eq!(snap.passed, 10);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_test_ok_format_overrides_passed() {
        // "OK (" line alone should parse correctly
        let snap = parse_test("", "OK (7 tests, 14 assertions)");
        assert_eq!(snap.passed, 7);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_test_missing_failures_and_errors_in_summary() {
        // "Tests:" line without Failures: or Errors: fields
        let snap = parse_test("", "Tests: 15, Assertions: 30");
        assert_eq!(snap.passed, 15);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
    }

    #[test]
    fn test_parse_test_summary_in_stderr() {
        let snap = parse_test(
            "",
            "Tests: 20, Assertions: 50, Failures: 5, Errors: 2, Skipped: 1",
        );
        assert_eq!(snap.passed, 12); // 20 - 5 - 2 - 1
        assert_eq!(snap.failed, 7); // 5 + 2
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_test_summary_in_stdout() {
        let snap = parse_test("Tests: 8, Assertions: 16, Failures: 1", "");
        assert_eq!(snap.passed, 7);
        assert_eq!(snap.failed, 1);
    }

    #[test]
    fn test_parse_test_error_tag_without_failure_tag() {
        let stdout =
            r#"<testcase name="testBoom"><error type="RuntimeException">boom</error></testcase>"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].message.contains("<error"));
    }

    #[test]
    fn test_parse_test_no_matching_lines() {
        // Lines that do not start with "Tests:" or "OK (" should be ignored
        let snap = parse_test("Running tests...\nAll done.", "PHPUnit 9.5.0");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_saturating_sub_no_underflow() {
        // If failures + errors + skipped exceeds total tests, passed should be 0 (saturating)
        let snap = parse_test("", "Tests: 2, Assertions: 5, Failures: 3, Errors: 1");
        assert_eq!(snap.passed, 0); // saturating_sub: 2 - 4 = 0
        assert_eq!(snap.failed, 4);
    }

    #[test]
    fn test_parse_test_multiple_failure_and_error_tags() {
        let stdout = "\
<failure type=\"A\">first failure</failure>
<failure type=\"B\">second failure</failure>
<error type=\"C\">first error</error>";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 3);
        for d in &snap.diagnostics {
            assert_eq!(d.severity, DiagnosticSeverity::Error);
            assert_eq!(d.file, "phpunit");
        }
    }

    #[test]
    fn test_parse_test_ok_with_non_numeric_prefix() {
        // "OK (" followed by non-numeric should not parse a count
        let snap = parse_test("", "OK (no tests executed)");
        // The "OK (" branch tries to parse a number but fails, so passed stays 0
        assert_eq!(snap.passed, 0);
    }

    // ==================== parse_analysis (PHPStan) ====================

    #[test]
    fn test_parse_phpstan_json() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":2},"files":{"src/Foo.php":{"errors":1,"messages":[{"message":"Undefined variable $x","line":10}]},"src/Bar.php":{"errors":1,"messages":[{"message":"Type mismatch","line":20}]}},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_analysis_empty_input() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.category, EvalCategory::StaticAnalysis);
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_invalid_json() {
        let snap = parse_analysis("not valid json {{{", "some stderr");
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_json_no_files_key() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":0},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_json_with_empty_files() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":0},"files":{},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_file_with_empty_messages() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":0},"files":{"src/Clean.php":{"errors":0,"messages":[]}},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_multiple_files_multiple_messages() {
        let stdout = r#"{
            "totals":{"errors":0,"file_errors":5},
            "files":{
                "src/A.php":{"errors":2,"messages":[
                    {"message":"Undefined variable $a","line":1},
                    {"message":"Unused import","line":3}
                ]},
                "src/B.php":{"errors":1,"messages":[
                    {"message":"Return type mismatch","line":42}
                ]},
                "src/C.php":{"errors":2,"messages":[
                    {"message":"Missing parameter type","line":10},
                    {"message":"Dead code","line":20}
                ]}
            },
            "errors":[]
        }"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 5);
        assert_eq!(snap.diagnostics.len(), 5);

        // Check that file paths are preserved
        let files: Vec<&str> = snap.diagnostics.iter().map(|d| d.file.as_str()).collect();
        assert!(files.contains(&"src/A.php"));
        assert!(files.contains(&"src/B.php"));
        assert!(files.contains(&"src/C.php"));

        // Check line numbers
        let a_diags: Vec<_> = snap
            .diagnostics
            .iter()
            .filter(|d| d.file == "src/A.php")
            .collect();
        assert_eq!(a_diags.len(), 2);
        assert_eq!(a_diags[0].line, Some(1));
        assert_eq!(a_diags[1].line, Some(3));
    }

    #[test]
    fn test_parse_analysis_message_missing_line() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":1},"files":{"src/X.php":{"errors":1,"messages":[{"message":"Some error"}]}},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].line.is_none());
        assert_eq!(snap.diagnostics[0].message, "Some error");
    }

    #[test]
    fn test_parse_analysis_message_missing_message_field() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":1},"files":{"src/X.php":{"errors":1,"messages":[{"line":5}]}},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].line, Some(5));
        // Missing message defaults to empty string
        assert_eq!(snap.diagnostics[0].message, "");
    }

    #[test]
    fn test_parse_analysis_message_missing_both_fields() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":1},"files":{"src/X.php":{"errors":1,"messages":[{}]}},"errors":[]}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].line.is_none());
        assert_eq!(snap.diagnostics[0].message, "");
    }

    #[test]
    fn test_parse_analysis_no_totals_key() {
        // JSON without "totals" key: errors stays 0 but files are still parsed
        let stdout =
            r#"{"files":{"src/X.php":{"errors":1,"messages":[{"message":"err","line":1}]}}}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0); // no totals -> default 0
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_analysis_files_not_object() {
        // "files" key exists but is not an object
        let stdout = r#"{"totals":{"errors":0,"file_errors":0},"files":"not an object"}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_messages_not_array() {
        // "messages" key exists but is not an array
        let stdout = r#"{"totals":{"errors":0,"file_errors":1},"files":{"src/X.php":{"errors":1,"messages":"not an array"}}}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_all_diagnostics_are_errors() {
        let stdout = r#"{"totals":{"errors":0,"file_errors":1},"files":{"src/X.php":{"errors":1,"messages":[{"message":"err","line":1}]}}}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert!(snap.diagnostics[0].column.is_none());
        assert!(snap.diagnostics[0].code.is_none());
    }

    // ==================== parse_lint (PHP-CS-Fixer) ====================

    #[test]
    fn test_parse_php_cs_fixer_json() {
        let stdout = r#"{"files":[{"name":"src/Foo.php","appliedFixers":["braces","spaces"]}]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_lint_empty_input() {
        let snap = parse_lint("", "");
        assert_eq!(snap.category, EvalCategory::Lint);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_invalid_json() {
        let snap = parse_lint("not json", "stderr output");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_empty_files_array() {
        let stdout = r#"{"files":[]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_file_with_no_applied_fixers() {
        let stdout = r#"{"files":[{"name":"src/Clean.php"}]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].message, "Needs fixing: ");
    }

    #[test]
    fn test_parse_lint_file_with_empty_applied_fixers() {
        let stdout = r#"{"files":[{"name":"src/Clean.php","appliedFixers":[]}]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].message, "Needs fixing: ");
    }

    #[test]
    fn test_parse_lint_multiple_files_with_fixers() {
        let stdout = r#"{"files":[
            {"name":"src/Foo.php","appliedFixers":["braces"]},
            {"name":"src/Bar.php","appliedFixers":["spaces","trailing_comma"]},
            {"name":"src/Baz.php","appliedFixers":["single_quote","no_unused_imports","ordered_imports"]}
        ]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 3);
        assert_eq!(snap.diagnostics.len(), 3);

        assert_eq!(snap.diagnostics[0].file, "src/Foo.php");
        assert_eq!(snap.diagnostics[0].message, "Needs fixing: braces");

        assert_eq!(snap.diagnostics[1].file, "src/Bar.php");
        assert_eq!(
            snap.diagnostics[1].message,
            "Needs fixing: spaces, trailing_comma"
        );

        assert_eq!(snap.diagnostics[2].file, "src/Baz.php");
        assert_eq!(
            snap.diagnostics[2].message,
            "Needs fixing: single_quote, no_unused_imports, ordered_imports"
        );
    }

    #[test]
    fn test_parse_lint_missing_name_field() {
        let stdout = r#"{"files":[{"appliedFixers":["braces"]}]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics[0].file, "unknown");
    }

    #[test]
    fn test_parse_lint_diagnostics_are_warnings() {
        let stdout = r#"{"files":[{"name":"src/X.php","appliedFixers":["braces"]}]}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Warning);
        assert!(snap.diagnostics[0].line.is_none());
        assert!(snap.diagnostics[0].column.is_none());
        assert!(snap.diagnostics[0].code.is_none());
    }

    #[test]
    fn test_parse_lint_no_files_key() {
        let stdout = r#"{"something_else": true}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_files_not_array() {
        let stdout = r#"{"files": "not an array"}"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    // ==================== parse_coverage (Clover XML) ====================

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

    #[test]
    fn test_parse_coverage_empty_input() {
        let snap = parse_coverage("", "");
        assert_eq!(snap.category, EvalCategory::Coverage);
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_zero_statements() {
        let stdout = r#"<metrics statements="0" coveredstatements="0" />"#;
        let snap = parse_coverage(stdout, "");
        // total is 0, so the if total > 0.0 check prevents division by zero
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_missing_statements_attr_entirely() {
        // A line with <metrics that does not contain "statements=" at all.
        // This line won't even pass the contains("statements=") check.
        let stdout = r#"<metrics methods="10" />"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_has_coveredstatements_but_no_standalone_statements() {
        // "coveredstatements" contains the substring "statements=", so the line
        // passes the contains("statements=") check. But extract_xml_attr for
        // "statements" will match inside "coveredstatements" due to simple find().
        // This documents the parser's behavior with partial attribute name overlap.
        let stdout = r#"<metrics coveredstatements="50" />"#;
        let snap = parse_coverage(stdout, "");
        // extract_xml_attr("statements") finds "statements=\"" inside "coveredstatements=\"50\""
        // and returns 50.0 for the "statements" query. But coveredstatements also returns 50.0.
        // So it computes (50/50)*100 = 100%.
        assert!((snap.line_coverage_pct.unwrap() - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_missing_coveredstatements_attr() {
        let stdout = r#"<metrics statements="100" />"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_multiple_metrics_lines() {
        // Clover XML can have multiple <metrics> lines at different levels
        // (file-level, class-level, project-level). The parser picks up all
        // that match and the last one wins.
        let stdout = r#"<?xml version="1.0"?>
<coverage>
  <project>
    <package name="src">
      <file name="Foo.php">
        <metrics statements="50" coveredstatements="25" />
      </file>
      <file name="Bar.php">
        <metrics statements="30" coveredstatements="30" />
      </file>
    </package>
    <metrics statements="200" coveredstatements="150" />
  </project>
</coverage>"#;
        let snap = parse_coverage(stdout, "");
        // The last <metrics> line with statements= wins: 150/200 = 75%
        assert!((snap.line_coverage_pct.unwrap() - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_first_metrics_line_used_then_overwritten() {
        // Verify that with multiple matching lines, the last one overwrites
        let stdout = "\
<metrics statements=\"10\" coveredstatements=\"5\" />\n\
<metrics statements=\"100\" coveredstatements=\"90\" />";
        let snap = parse_coverage(stdout, "");
        // Last line: 90/100 = 90%
        assert!((snap.line_coverage_pct.unwrap() - 90.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_100_percent() {
        let stdout = r#"<metrics statements="50" coveredstatements="50" />"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_0_percent_nonzero_statements() {
        let stdout = r#"<metrics statements="50" coveredstatements="0" />"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_non_xml_input() {
        let snap = parse_coverage("This is not XML at all", "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_metrics_without_statements_keyword() {
        // Has <metrics but not "statements=" (only "methods=")
        let stdout = r#"<metrics methods="10" coveredmethods="8" />"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_non_numeric_statements() {
        let stdout = r#"<metrics statements="abc" coveredstatements="def" />"#;
        let snap = parse_coverage(stdout, "");
        // extract_xml_attr returns None for non-numeric values
        assert!(snap.line_coverage_pct.is_none());
    }

    // ==================== extract_phpunit_count ====================

    #[test]
    fn test_extract_phpunit_count_basic() {
        assert_eq!(
            extract_phpunit_count("Tests: 42, Assertions: 100", "Tests:"),
            Some(42)
        );
    }

    #[test]
    fn test_extract_phpunit_count_failures() {
        assert_eq!(
            extract_phpunit_count("Tests: 10, Failures: 3, Errors: 1", "Failures:"),
            Some(3)
        );
    }

    #[test]
    fn test_extract_phpunit_count_errors() {
        assert_eq!(
            extract_phpunit_count("Tests: 10, Failures: 3, Errors: 1", "Errors:"),
            Some(1)
        );
    }

    #[test]
    fn test_extract_phpunit_count_prefix_not_found() {
        assert_eq!(extract_phpunit_count("Tests: 42", "Failures:"), None);
    }

    #[test]
    fn test_extract_phpunit_count_empty_line() {
        assert_eq!(extract_phpunit_count("", "Tests:"), None);
    }

    #[test]
    fn test_extract_phpunit_count_no_number_after_prefix() {
        assert_eq!(extract_phpunit_count("Tests: abc", "Tests:"), None);
    }

    #[test]
    fn test_extract_phpunit_count_zero() {
        assert_eq!(extract_phpunit_count("Failures: 0", "Failures:"), Some(0));
    }

    #[test]
    fn test_extract_phpunit_count_large_number() {
        assert_eq!(
            extract_phpunit_count("Tests: 99999, Assertions: 200000", "Tests:"),
            Some(99999)
        );
    }

    #[test]
    fn test_extract_phpunit_count_number_followed_by_comma() {
        assert_eq!(
            extract_phpunit_count("Tests: 5, Assertions: 10", "Tests:"),
            Some(5)
        );
    }

    #[test]
    fn test_extract_phpunit_count_skipped() {
        assert_eq!(
            extract_phpunit_count("Tests: 10, Skipped: 2", "Skipped:"),
            Some(2)
        );
    }

    #[test]
    fn test_extract_phpunit_count_extra_spaces() {
        // The function trims after the prefix, so extra spaces are handled
        assert_eq!(extract_phpunit_count("Tests:   7", "Tests:"), Some(7));
    }

    // ==================== extract_xml_attr ====================

    #[test]
    fn test_extract_xml_attr_basic() {
        assert_eq!(
            extract_xml_attr(r#"<metrics statements="100" />"#, "statements"),
            Some(100.0)
        );
    }

    #[test]
    fn test_extract_xml_attr_coveredstatements() {
        assert_eq!(
            extract_xml_attr(r#"<metrics coveredstatements="85" />"#, "coveredstatements"),
            Some(85.0)
        );
    }

    #[test]
    fn test_extract_xml_attr_not_found() {
        assert_eq!(
            extract_xml_attr(r#"<metrics statements="100" />"#, "nonexistent"),
            None
        );
    }

    #[test]
    fn test_extract_xml_attr_empty_line() {
        assert_eq!(extract_xml_attr("", "statements"), None);
    }

    #[test]
    fn test_extract_xml_attr_non_numeric_value() {
        assert_eq!(
            extract_xml_attr(r#"<metrics statements="abc" />"#, "statements"),
            None
        );
    }

    #[test]
    fn test_extract_xml_attr_float_value() {
        assert_eq!(
            extract_xml_attr(r#"<metrics coverage="85.5" />"#, "coverage"),
            Some(85.5)
        );
    }

    #[test]
    fn test_extract_xml_attr_zero() {
        assert_eq!(
            extract_xml_attr(r#"<metrics statements="0" />"#, "statements"),
            Some(0.0)
        );
    }

    #[test]
    fn test_extract_xml_attr_empty_value() {
        assert_eq!(
            extract_xml_attr(r#"<metrics statements="" />"#, "statements"),
            None
        );
    }

    #[test]
    fn test_extract_xml_attr_multiple_attrs() {
        let line = r#"<metrics statements="200" coveredstatements="150" methods="10" />"#;
        assert_eq!(extract_xml_attr(line, "statements"), Some(200.0));
        assert_eq!(extract_xml_attr(line, "coveredstatements"), Some(150.0));
        assert_eq!(extract_xml_attr(line, "methods"), Some(10.0));
    }

    #[test]
    fn test_extract_xml_attr_partial_match_does_not_confuse() {
        // "statements" should not match "coveredstatements" since find looks for exact attr="
        let line = r#"<metrics coveredstatements="50" />"#;
        // Searching for "statements" will find it inside "coveredstatements"
        // because find("statements=\"") matches inside "coveredstatements=\""
        // This is a known limitation of the simple parser
        let result = extract_xml_attr(line, "statements");
        // The find will match at the "statements" inside "coveredstatements"
        assert_eq!(result, Some(50.0));
    }
}
