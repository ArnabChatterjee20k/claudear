//! Node.js/npm output parsers (Jest, ESLint, Prettier).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse Jest test output (`--json` flag).
pub fn parse_test(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    // Jest JSON: {"numPassedTests":10,"numFailedTests":2,"numPendingTests":1,...,"testResults":[...]}
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        snapshot.passed = v
            .get("numPassedTests")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        snapshot.failed = v
            .get("numFailedTests")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        snapshot.skipped = v
            .get("numPendingTests")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        snapshot.errors = snapshot.failed;

        // Extract failure details from testResults
        if let Some(results) = v.get("testResults").and_then(|r| r.as_array()) {
            for result in results {
                let test_file = result
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                if let Some(assertions) = result.get("assertionResults").and_then(|a| a.as_array())
                {
                    for assertion in assertions {
                        let status = assertion
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        if status == "failed" {
                            let title = assertion
                                .get("fullName")
                                .and_then(|n| n.as_str())
                                .unwrap_or("unknown test");
                            let messages = assertion
                                .get("failureMessages")
                                .and_then(|m| m.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str())
                                        .collect::<Vec<_>>()
                                        .join("; ")
                                })
                                .unwrap_or_default();
                            snapshot.diagnostics.push(Diagnostic {
                                file: test_file.clone(),
                                line: None,
                                column: None,
                                severity: DiagnosticSeverity::Error,
                                code: None,
                                message: format!("{}: {}", title, messages),
                            });
                        }
                    }
                }
            }
        }
    }

    snapshot
}

/// Parse ESLint analysis output (`--format json`).
pub fn parse_analysis(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    // ESLint JSON: [{"filePath":"...","messages":[{"ruleId":"...","severity":2,"message":"...","line":1,"column":1}],"errorCount":1,"warningCount":0}]
    if let Ok(files) = serde_json::from_str::<Vec<serde_json::Value>>(stdout) {
        for file in &files {
            let file_path = file
                .get("filePath")
                .and_then(|p| p.as_str())
                .unwrap_or("unknown")
                .to_string();

            snapshot.errors += file.get("errorCount").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
            snapshot.warnings += file
                .get("warningCount")
                .and_then(|c| c.as_u64())
                .unwrap_or(0) as u32;

            if let Some(messages) = file.get("messages").and_then(|m| m.as_array()) {
                for msg in messages {
                    let severity_num = msg.get("severity").and_then(|s| s.as_u64()).unwrap_or(0);
                    let severity = match severity_num {
                        2 => DiagnosticSeverity::Error,
                        1 => DiagnosticSeverity::Warning,
                        _ => DiagnosticSeverity::Info,
                    };
                    let line = msg.get("line").and_then(|l| l.as_u64()).map(|l| l as u32);
                    let column = msg.get("column").and_then(|c| c.as_u64()).map(|c| c as u32);
                    let message = msg
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    let rule_id = msg.get("ruleId").and_then(|r| r.as_str()).map(String::from);

                    snapshot.diagnostics.push(Diagnostic {
                        file: file_path.clone(),
                        line,
                        column,
                        severity,
                        code: rule_id,
                        message,
                    });
                }
            }
        }
    }

    snapshot
}

/// Parse Prettier lint output (`--check`).
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    // Prettier --check outputs file paths that need formatting to stdout.
    // Lines like: "Checking formatting...\n[warn] src/foo.js\n[warn] Code style issues found"
    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[warn]") {
            let content = trimmed.strip_prefix("[warn]").unwrap_or("").trim();
            // Skip summary lines
            if !content.starts_with("Code style")
                && !content.starts_with("All matched")
                && !content.is_empty()
            {
                snapshot.warnings += 1;
                snapshot.diagnostics.push(Diagnostic {
                    file: content.to_string(),
                    line: None,
                    column: None,
                    severity: DiagnosticSeverity::Warning,
                    code: None,
                    message: "File needs formatting".to_string(),
                });
            }
        }
    }

    snapshot
}

/// Parse Jest coverage output (`--coverage --coverageReporters=json-summary`).
pub fn parse_coverage(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    // json-summary outputs to coverage/coverage-summary.json, but some content
    // may appear in stdout. Also parse Jest JSON output for coverage.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        // json-summary format: {"total":{"lines":{"pct":85.5},"branches":{"pct":72.3},...}}
        if let Some(total) = v.get("total") {
            snapshot.line_coverage_pct = total.pointer("/lines/pct").and_then(|p| p.as_f64());
            snapshot.branch_coverage_pct = total.pointer("/branches/pct").and_then(|p| p.as_f64());
        }
    }

    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jest_json() {
        let stdout =
            r#"{"numPassedTests":10,"numFailedTests":2,"numPendingTests":1,"testResults":[]}"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 10);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.skipped, 1);
    }

    #[test]
    fn test_parse_eslint_json() {
        let stdout = r#"[{"filePath":"src/foo.js","messages":[{"ruleId":"no-unused-vars","severity":2,"message":"'x' is defined but never used","line":5,"column":7}],"errorCount":1,"warningCount":0}]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "src/foo.js");
    }

    #[test]
    fn test_parse_prettier_check() {
        let stdout =
            "[warn] src/foo.js\n[warn] src/bar.ts\n[warn] Code style issues found in 2 files.";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_jest_coverage_summary() {
        let stdout = r#"{"total":{"lines":{"total":200,"covered":170,"skipped":0,"pct":85},"branches":{"total":50,"covered":40,"skipped":0,"pct":80}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_test_empty_string() {
        let snap = parse_test("", "");
        assert_eq!(snap.category, EvalCategory::Test);
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_invalid_json() {
        let snap = parse_test("{not valid json!!!", "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_missing_fields() {
        // Valid JSON object but no test-count keys at all
        let snap = parse_test(r#"{"success": true}"#, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
    }

    #[test]
    fn test_parse_test_partial_fields() {
        // Only numPassedTests present, others missing
        let snap = parse_test(r#"{"numPassedTests": 7}"#, "");
        assert_eq!(snap.passed, 7);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
    }

    #[test]
    fn test_parse_test_with_failed_assertions() {
        let stdout = r#"{
            "numPassedTests": 3,
            "numFailedTests": 1,
            "numPendingTests": 0,
            "testResults": [
                {
                    "name": "src/__tests__/math.test.js",
                    "assertionResults": [
                        {
                            "status": "passed",
                            "fullName": "add should sum two numbers"
                        },
                        {
                            "status": "failed",
                            "fullName": "divide should throw on zero",
                            "failureMessages": ["Expected exception but got 0", "Stack trace here"]
                        }
                    ]
                }
            ]
        }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 3);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        let diag = &snap.diagnostics[0];
        assert_eq!(diag.file, "src/__tests__/math.test.js");
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert!(diag.message.contains("divide should throw on zero"));
        assert!(diag.message.contains("Expected exception but got 0"));
        assert!(diag.message.contains("; "));
        assert!(diag.message.contains("Stack trace here"));
        assert!(diag.line.is_none());
        assert!(diag.column.is_none());
        assert!(diag.code.is_none());
    }

    #[test]
    fn test_parse_test_multiple_failed_assertions_across_files() {
        let stdout = r#"{
            "numPassedTests": 0,
            "numFailedTests": 2,
            "numPendingTests": 0,
            "testResults": [
                {
                    "name": "file1.test.js",
                    "assertionResults": [
                        {"status": "failed", "fullName": "test A", "failureMessages": ["msg A"]}
                    ]
                },
                {
                    "name": "file2.test.js",
                    "assertionResults": [
                        {"status": "failed", "fullName": "test B", "failureMessages": ["msg B"]}
                    ]
                }
            ]
        }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].file, "file1.test.js");
        assert_eq!(snap.diagnostics[1].file, "file2.test.js");
    }

    #[test]
    fn test_parse_test_test_results_no_assertion_results() {
        // testResults present but individual result has no assertionResults key
        let stdout = r#"{
            "numPassedTests": 5,
            "numFailedTests": 0,
            "numPendingTests": 0,
            "testResults": [
                {"name": "foo.test.js"}
            ]
        }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 5);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_failed_assertion_no_failure_messages() {
        // Status is "failed" but failureMessages is missing
        let stdout = r#"{
            "numPassedTests": 0,
            "numFailedTests": 1,
            "numPendingTests": 0,
            "testResults": [
                {
                    "name": "test.js",
                    "assertionResults": [
                        {"status": "failed", "fullName": "broken test"}
                    ]
                }
            ]
        }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].message.contains("broken test"));
        // With no failureMessages, the message part after ": " should be empty
        assert!(snap.diagnostics[0].message.ends_with(": "));
    }

    #[test]
    fn test_parse_test_failed_assertion_empty_failure_messages() {
        let stdout = r#"{
            "numPassedTests": 0,
            "numFailedTests": 1,
            "numPendingTests": 0,
            "testResults": [
                {
                    "name": "test.js",
                    "assertionResults": [
                        {"status": "failed", "fullName": "broken", "failureMessages": []}
                    ]
                }
            ]
        }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].message.starts_with("broken: "));
    }

    #[test]
    fn test_parse_test_assertion_missing_name_and_status() {
        let stdout = r#"{
            "numPassedTests": 0,
            "numFailedTests": 0,
            "numPendingTests": 0,
            "testResults": [
                {
                    "assertionResults": [
                        {}
                    ]
                }
            ]
        }"#;
        let snap = parse_test(stdout, "");
        // status defaults to "" which is not "failed", so no diagnostic created
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_only_passed_assertions_no_diagnostics() {
        let stdout = r#"{
            "numPassedTests": 2,
            "numFailedTests": 0,
            "numPendingTests": 0,
            "testResults": [
                {
                    "name": "good.test.js",
                    "assertionResults": [
                        {"status": "passed", "fullName": "works fine"},
                        {"status": "passed", "fullName": "also works"}
                    ]
                }
            ]
        }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 2);
        assert_eq!(snap.failed, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_stderr_is_ignored() {
        // stderr content should not affect parsing
        let snap = parse_test(r#"{"numPassedTests":1}"#, "some stderr garbage");
        assert_eq!(snap.passed, 1);
    }

    #[test]
    fn test_parse_analysis_empty_array() {
        let snap = parse_analysis("[]", "");
        assert_eq!(snap.category, EvalCategory::StaticAnalysis);
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_invalid_json() {
        let snap = parse_analysis("not json", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_empty_string() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_multiple_files_mixed_errors_warnings() {
        let stdout = r#"[
            {
                "filePath": "src/a.js",
                "messages": [
                    {"ruleId": "no-unused-vars", "severity": 2, "message": "x is unused", "line": 1, "column": 5},
                    {"ruleId": "semi", "severity": 1, "message": "missing semicolon", "line": 3, "column": 10}
                ],
                "errorCount": 1,
                "warningCount": 1
            },
            {
                "filePath": "src/b.js",
                "messages": [
                    {"ruleId": "eqeqeq", "severity": 2, "message": "use ===", "line": 7, "column": 3}
                ],
                "errorCount": 1,
                "warningCount": 0
            }
        ]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 3);

        // First diagnostic: error in a.js
        assert_eq!(snap.diagnostics[0].file, "src/a.js");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(snap.diagnostics[0].line, Some(1));
        assert_eq!(snap.diagnostics[0].column, Some(5));
        assert_eq!(snap.diagnostics[0].code, Some("no-unused-vars".to_string()));
        assert_eq!(snap.diagnostics[0].message, "x is unused");

        // Second diagnostic: warning in a.js
        assert_eq!(snap.diagnostics[1].severity, DiagnosticSeverity::Warning);
        assert_eq!(snap.diagnostics[1].line, Some(3));
        assert_eq!(snap.diagnostics[1].column, Some(10));

        // Third diagnostic: error in b.js
        assert_eq!(snap.diagnostics[2].file, "src/b.js");
        assert_eq!(snap.diagnostics[2].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn test_parse_analysis_severity_zero_is_info() {
        let stdout = r#"[{
            "filePath": "src/c.js",
            "messages": [
                {"severity": 0, "message": "info level message"}
            ],
            "errorCount": 0,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Info);
        assert_eq!(snap.diagnostics[0].message, "info level message");
    }

    #[test]
    fn test_parse_analysis_severity_1_is_warning() {
        let stdout = r#"[{
            "filePath": "w.js",
            "messages": [{"severity": 1, "message": "a warning"}],
            "errorCount": 0,
            "warningCount": 1
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn test_parse_analysis_severity_2_is_error() {
        let stdout = r#"[{
            "filePath": "e.js",
            "messages": [{"severity": 2, "message": "an error"}],
            "errorCount": 1,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn test_parse_analysis_unknown_severity_defaults_to_info() {
        let stdout = r#"[{
            "filePath": "x.js",
            "messages": [{"severity": 99, "message": "unknown severity"}],
            "errorCount": 0,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn test_parse_analysis_missing_fields_in_messages() {
        // Message with no ruleId, no line, no column, no message
        let stdout = r#"[{
            "filePath": "m.js",
            "messages": [
                {"severity": 2}
            ],
            "errorCount": 1,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].code, None);
        assert_eq!(snap.diagnostics[0].line, None);
        assert_eq!(snap.diagnostics[0].column, None);
        assert_eq!(snap.diagnostics[0].message, "");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn test_parse_analysis_missing_severity_defaults_to_info() {
        let stdout = r#"[{
            "filePath": "n.js",
            "messages": [{"message": "no severity"}],
            "errorCount": 0,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        // severity defaults to 0 -> Info
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn test_parse_analysis_file_with_no_messages_key() {
        let stdout = r#"[{
            "filePath": "clean.js",
            "errorCount": 0,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_file_with_no_error_warning_counts() {
        // messages present but no errorCount/warningCount
        let stdout = r#"[{
            "filePath": "nocount.js",
            "messages": [
                {"severity": 2, "message": "err"},
                {"severity": 1, "message": "warn"}
            ]
        }]"#;
        let snap = parse_analysis(stdout, "");
        // errorCount/warningCount default to 0 since they are missing
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        // But diagnostics are still created from messages
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_analysis_missing_file_path() {
        let stdout = r#"[{
            "messages": [{"severity": 2, "message": "err"}],
            "errorCount": 1,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics[0].file, "unknown");
    }

    #[test]
    fn test_parse_analysis_empty_messages_array() {
        let stdout = r#"[{
            "filePath": "clean.js",
            "messages": [],
            "errorCount": 0,
            "warningCount": 0
        }]"#;
        let snap = parse_analysis(stdout, "");
        assert!(snap.diagnostics.is_empty());
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_analysis_stderr_is_ignored() {
        let snap = parse_analysis("[]", "some stderr content");
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_empty_input() {
        let snap = parse_lint("", "");
        assert_eq!(snap.category, EvalCategory::Lint);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_only_summary_line_code_style() {
        let stdout = "[warn] Code style issues found in 3 files.";
        let snap = parse_lint(stdout, "");
        // Summary line should be skipped
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_all_matched_files_line_skipped() {
        let stdout = "[warn] All matched files use Prettier code style!";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_empty_after_warn_prefix() {
        // "[warn]" followed by nothing (or just spaces)
        let stdout = "[warn]   ";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_warn_prefix_only() {
        let stdout = "[warn]";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_multiple_file_paths() {
        let stdout = "[warn] src/foo.js\n[warn] src/bar.ts\n[warn] src/baz.css";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 3);
        assert_eq!(snap.diagnostics.len(), 3);
        assert_eq!(snap.diagnostics[0].file, "src/foo.js");
        assert_eq!(snap.diagnostics[1].file, "src/bar.ts");
        assert_eq!(snap.diagnostics[2].file, "src/baz.css");
        for d in &snap.diagnostics {
            assert_eq!(d.severity, DiagnosticSeverity::Warning);
            assert_eq!(d.message, "File needs formatting");
            assert!(d.line.is_none());
            assert!(d.column.is_none());
            assert!(d.code.is_none());
        }
    }

    #[test]
    fn test_parse_lint_stderr_content_contributes() {
        // stderr content with [warn] lines should also be picked up
        let snap = parse_lint("", "[warn] src/from_stderr.js");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "src/from_stderr.js");
    }

    #[test]
    fn test_parse_lint_mixed_stdout_and_stderr() {
        let snap = parse_lint("[warn] src/a.js", "[warn] src/b.js");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics[0].file, "src/a.js");
        assert_eq!(snap.diagnostics[1].file, "src/b.js");
    }

    #[test]
    fn test_parse_lint_lines_without_warn_prefix_ignored() {
        let stdout = "Checking formatting...\nsrc/foo.js\n[warn] src/bar.js\nDone.";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics[0].file, "src/bar.js");
    }

    #[test]
    fn test_parse_lint_full_prettier_output() {
        let stdout = "Checking formatting...\n\
                      [warn] src/foo.js\n\
                      [warn] src/bar.ts\n\
                      [warn] Code style issues found in 2 files.\n\
                      [warn] All matched files use Prettier code style!";
        let snap = parse_lint(stdout, "");
        // Only the two file paths, not the summary lines
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_lint_warn_with_leading_whitespace() {
        // Lines with leading whitespace before [warn]
        let stdout = "  [warn] src/indented.js";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics[0].file, "src/indented.js");
    }

    #[test]
    fn test_parse_coverage_empty_string() {
        let snap = parse_coverage("", "");
        assert_eq!(snap.category, EvalCategory::Coverage);
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_invalid_json() {
        let snap = parse_coverage("not json {{{", "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_json_without_total_key() {
        let stdout = r#"{"src/foo.js": {"lines": {"pct": 90}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_total_with_only_lines() {
        let stdout = r#"{"total": {"lines": {"pct": 92.5}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 92.5).abs() < 0.01);
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_total_with_only_branches() {
        let stdout = r#"{"total": {"branches": {"pct": 75.0}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!((snap.branch_coverage_pct.unwrap() - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_total_empty_object() {
        let stdout = r#"{"total": {}}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_total_lines_missing_pct() {
        let stdout = r#"{"total": {"lines": {"total": 100, "covered": 80}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_zero_percent() {
        let stdout = r#"{"total": {"lines": {"pct": 0}, "branches": {"pct": 0}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 0.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_hundred_percent() {
        let stdout = r#"{"total": {"lines": {"pct": 100}, "branches": {"pct": 100}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 100.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_fractional_values() {
        let stdout = r#"{"total": {"lines": {"pct": 87.654}, "branches": {"pct": 63.21}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 87.654).abs() < 0.001);
        assert!((snap.branch_coverage_pct.unwrap() - 63.21).abs() < 0.001);
    }

    #[test]
    fn test_parse_coverage_stderr_is_ignored() {
        let snap = parse_coverage(r#"{"total": {"lines": {"pct": 50}}}"#, "some stderr noise");
        assert!((snap.line_coverage_pct.unwrap() - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_default_fields_untouched() {
        // Verify that only coverage fields are set; other snapshot fields stay default
        let stdout = r#"{"total": {"lines": {"pct": 80}, "branches": {"pct": 70}}}"#;
        let snap = parse_coverage(stdout, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }
}
