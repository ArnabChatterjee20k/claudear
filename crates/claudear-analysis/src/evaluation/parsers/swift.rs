//! Swift output parsers (swift test, SwiftLint, swift-format).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse `swift test` output.
///
/// Swift test outputs lines like:
/// "Test Suite 'All tests' passed at ..."
/// "Test Case '-[Module.TestClass testMethod]' passed (0.001 seconds)."
/// "Test Case '-[Module.TestClass testMethod]' failed (0.001 seconds)."
/// Summary: "Executed X tests, with Y failures (Z unexpected) in T seconds"
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Test Case") && trimmed.contains("passed") {
            snapshot.passed += 1;
        } else if trimmed.starts_with("Test Case") && trimmed.contains("failed") {
            snapshot.failed += 1;
            snapshot.diagnostics.push(Diagnostic {
                file: "swift-test".to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Error,
                code: None,
                message: trimmed.to_string(),
            });
        }
        // Also check for summary line
        if trimmed.starts_with("Executed") && trimmed.contains("tests") {
            // "Executed 42 tests, with 2 failures (1 unexpected) in 0.5 seconds"
            if let Some(total) = extract_number_after(trimmed, "Executed ") {
                let failures = extract_number_after(trimmed, "with ").unwrap_or(0);
                // Reconcile with per-test counts if they differ
                if snapshot.passed + snapshot.failed == 0 {
                    snapshot.passed = total.saturating_sub(failures);
                    snapshot.failed = failures;
                }
            }
        }
    }

    snapshot.errors = snapshot.failed;
    snapshot
}

/// Parse SwiftLint analysis output (`--reporter json`).
pub fn parse_analysis(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    // SwiftLint JSON: [{"file":"...","line":10,"character":5,"severity":"Warning","type":"Identifier Name","reason":"...","rule_id":"identifier_name"}]
    if let Ok(issues) = serde_json::from_str::<Vec<serde_json::Value>>(stdout) {
        for issue in &issues {
            let file = issue
                .get("file")
                .and_then(|f| f.as_str())
                .unwrap_or("unknown")
                .to_string();
            let line = issue.get("line").and_then(|l| l.as_u64()).map(|l| l as u32);
            let column = issue
                .get("character")
                .and_then(|c| c.as_u64())
                .map(|c| c as u32);
            let severity_str = issue
                .get("severity")
                .and_then(|s| s.as_str())
                .unwrap_or("warning");
            let severity = if severity_str.eq_ignore_ascii_case("error") {
                DiagnosticSeverity::Error
            } else {
                DiagnosticSeverity::Warning
            };
            let message = issue
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            let code = issue
                .get("rule_id")
                .and_then(|r| r.as_str())
                .map(String::from);

            match severity {
                DiagnosticSeverity::Error => snapshot.errors += 1,
                DiagnosticSeverity::Warning => snapshot.warnings += 1,
                _ => {}
            }

            snapshot.diagnostics.push(Diagnostic {
                file,
                line,
                column,
                severity,
                code,
                message,
            });
        }
    }

    snapshot
}

/// Parse swift-format lint output.
///
/// swift-format lint outputs lines like:
/// "path/File.swift:10:5: warning: description"
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains(".swift:")
            && (trimmed.contains("warning:") || trimmed.contains("error:"))
        {
            snapshot.warnings += 1;
            let parts: Vec<&str> = trimmed.splitn(4, ':').collect();
            let file = parts.first().unwrap_or(&"unknown").to_string();
            let line_num = parts.get(1).and_then(|s| s.trim().parse::<u32>().ok());
            let col = parts.get(2).and_then(|s| s.trim().parse::<u32>().ok());
            let message = parts.get(3).unwrap_or(&"").trim().to_string();

            snapshot.diagnostics.push(Diagnostic {
                file,
                line: line_num,
                column: col,
                severity: DiagnosticSeverity::Warning,
                code: None,
                message,
            });
        }
    }

    snapshot
}

/// Parse swift test coverage output (`--enable-code-coverage`).
///
/// Swift produces an llvm-cov compatible JSON when used with
/// `swift test --enable-code-coverage`. The actual coverage data
/// is in a profdata/json file rather than stdout, so we do best-effort
/// parsing of any coverage summary lines.
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    // Try to parse llvm-cov JSON format if present
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(totals) = v.pointer("/data/0/totals") {
            if let Some(lines) = totals.get("lines") {
                snapshot.line_coverage_pct = lines.get("percent").and_then(|p| p.as_f64());
            }
            if let Some(branches) = totals.get("branches") {
                snapshot.branch_coverage_pct = branches.get("percent").and_then(|p| p.as_f64());
            }
        }
    }

    // Fallback: parse percentage from console output
    let combined = format!("{}\n{}", stdout, stderr);
    if snapshot.line_coverage_pct.is_none() {
        for line in combined.lines() {
            let trimmed = line.trim();
            if trimmed.contains("coverage:") || trimmed.contains("Coverage:") {
                if let Some(pct) = extract_percentage(trimmed) {
                    snapshot.line_coverage_pct = Some(pct);
                    break;
                }
            }
        }
    }

    snapshot
}

fn extract_number_after(line: &str, prefix: &str) -> Option<u32> {
    let idx = line.find(prefix)?;
    let after = &line[idx + prefix.len()..];
    let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        return None;
    }
    num_str.parse().ok()
}

fn extract_percentage(line: &str) -> Option<f64> {
    // Look for patterns like "85.5%" or "85.5 %"
    let chars: Vec<char> = line.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '%' {
            // Walk backward to find the number
            let end = i;
            let mut start = end;
            while start > 0 && (chars[start - 1].is_ascii_digit() || chars[start - 1] == '.') {
                start -= 1;
            }
            if start < end {
                let num_str: String = chars[start..end].iter().collect();
                if let Ok(pct) = num_str.parse::<f64>() {
                    return Some(pct);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_swift_test() {
        let stdout = r#"Test Case '-[MyTests.FooTests testA]' passed (0.001 seconds).
Test Case '-[MyTests.FooTests testB]' failed (0.002 seconds).
Test Case '-[MyTests.FooTests testC]' passed (0.001 seconds).
Executed 3 tests, with 1 failures (0 unexpected) in 0.004 seconds"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 2);
        assert_eq!(snap.failed, 1);
    }

    #[test]
    fn test_parse_swiftlint_json() {
        let stdout = r#"[{"file":"Sources/Foo.swift","line":10,"character":5,"severity":"Warning","type":"Identifier Name","reason":"Variable name should be longer","rule_id":"identifier_name"}]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].code.as_deref(), Some("identifier_name"));
    }

    #[test]
    fn test_parse_swift_format() {
        let stdout = "Sources/Foo.swift:10:5: warning: line too long";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_swift_coverage() {
        let stdout =
            r#"{"data":[{"totals":{"lines":{"percent":90.0},"branches":{"percent":75.0}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 90.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage() {
        assert!((extract_percentage("coverage: 85.5%").unwrap() - 85.5).abs() < 0.01);
        assert!(extract_percentage("no percentage here").is_none());
    }

    #[test]
    fn test_parse_test_empty_input() {
        let snap = parse_test("", "");
        assert_eq!(snap.category, EvalCategory::Test);
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_only_summary_line() {
        // No per-test lines so passed+failed == 0 => reconciliation kicks in
        let stdout = "Executed 10 tests, with 3 failures (1 unexpected) in 0.5 seconds";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 7);
        assert_eq!(snap.failed, 3);
        assert_eq!(snap.errors, 3);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_multiple_failed_with_diagnostics() {
        let stdout = r#"Test Case '-[M.T testA]' failed (0.01 seconds).
Test Case '-[M.T testB]' failed (0.02 seconds).
Test Case '-[M.T testC]' passed (0.01 seconds).
Executed 3 tests, with 2 failures (0 unexpected) in 0.04 seconds"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 1);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.diagnostics.len(), 2);
        // Each diagnostic should capture the failed test case line
        assert!(snap.diagnostics[0].message.contains("testA"));
        assert!(snap.diagnostics[1].message.contains("testB"));
        for d in &snap.diagnostics {
            assert_eq!(d.file, "swift-test");
            assert_eq!(d.severity, DiagnosticSeverity::Error);
            assert!(d.line.is_none());
            assert!(d.column.is_none());
            assert!(d.code.is_none());
        }
    }

    #[test]
    fn test_parse_test_summary_reconciliation_when_per_test_zero() {
        // If only the summary is present (no individual Test Case lines),
        // the reconciliation branch (passed+failed == 0) fills in counts.
        let stdout = "Executed 42 tests, with 5 failures (2 unexpected) in 1.0 seconds";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 37); // 42 - 5
        assert_eq!(snap.failed, 5);
        assert_eq!(snap.errors, 5);
    }

    #[test]
    fn test_parse_test_summary_not_reconciled_when_per_test_nonzero() {
        // When per-test lines already provided counts, the summary
        // reconciliation branch is skipped.
        let stdout = r#"Test Case '-[M.T testA]' passed (0.01 seconds).
Test Case '-[M.T testB]' failed (0.01 seconds).
Executed 2 tests, with 1 failures (0 unexpected) in 0.02 seconds"#;
        let snap = parse_test(stdout, "");
        // counts come from per-test lines, not overwritten by summary
        assert_eq!(snap.passed, 1);
        assert_eq!(snap.failed, 1);
    }

    #[test]
    fn test_parse_test_stderr_content() {
        // stderr is combined with stdout, so test case lines there also count
        let stderr = "Test Case '-[M.T testErr]' failed (0.01 seconds).";
        let snap = parse_test("", stderr);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_test_mixed_stdout_stderr() {
        let stdout = "Test Case '-[M.T testA]' passed (0.01 seconds).";
        let stderr = "Test Case '-[M.T testB]' failed (0.01 seconds).";
        let snap = parse_test(stdout, stderr);
        assert_eq!(snap.passed, 1);
        assert_eq!(snap.failed, 1);
    }

    #[test]
    fn test_parse_analysis_empty_json_array() {
        let snap = parse_analysis("[]", "");
        assert_eq!(snap.category, EvalCategory::StaticAnalysis);
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_invalid_json_does_not_panic() {
        let snap = parse_analysis("this is not json at all", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_invalid_json_partial() {
        let snap = parse_analysis("[{\"file\":\"x.swift\"", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_error_severity() {
        let stdout = r#"[{"file":"Err.swift","line":1,"character":1,"severity":"Error","type":"Force Cast","reason":"Force casts should be avoided","rule_id":"force_cast"}]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 0);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(snap.diagnostics[0].file, "Err.swift");
        assert_eq!(snap.diagnostics[0].line, Some(1));
        assert_eq!(snap.diagnostics[0].column, Some(1));
        assert_eq!(snap.diagnostics[0].code.as_deref(), Some("force_cast"));
        assert_eq!(snap.diagnostics[0].message, "Force casts should be avoided");
    }

    #[test]
    fn test_parse_analysis_missing_fields() {
        // All optional fields missing: no file, line, character, severity, reason, rule_id
        let stdout = r#"[{}]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        let d = &snap.diagnostics[0];
        assert_eq!(d.file, "unknown"); // default when file missing
        assert!(d.line.is_none());
        assert!(d.column.is_none());
        assert_eq!(d.severity, DiagnosticSeverity::Warning); // default severity
        assert!(d.code.is_none());
        assert_eq!(d.message, ""); // default when reason missing
        assert_eq!(snap.warnings, 1); // defaults to warning
        assert_eq!(snap.errors, 0);
    }

    #[test]
    fn test_parse_analysis_multiple_mixed_severities() {
        let stdout = r#"[
            {"file":"A.swift","line":10,"character":5,"severity":"Warning","reason":"warn1","rule_id":"r1"},
            {"file":"B.swift","line":20,"character":3,"severity":"Error","reason":"err1","rule_id":"r2"},
            {"file":"C.swift","line":30,"severity":"warning","reason":"warn2","rule_id":"r3"},
            {"file":"D.swift","severity":"error","reason":"err2","rule_id":"r4"}
        ]"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.diagnostics.len(), 4);
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.errors, 2);
        // Verify each diagnostic got the right severity
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(snap.diagnostics[1].severity, DiagnosticSeverity::Error);
        assert_eq!(snap.diagnostics[2].severity, DiagnosticSeverity::Warning); // case-insensitive
        assert_eq!(snap.diagnostics[3].severity, DiagnosticSeverity::Error); // case-insensitive
                                                                             // Missing column defaults to None
        assert!(snap.diagnostics[2].column.is_none());
        assert!(snap.diagnostics[3].line.is_none());
    }

    #[test]
    fn test_parse_analysis_empty_input() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
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
    fn test_parse_lint_error_line() {
        let stdout = "Sources/Bar.swift:20:1: error: trailing whitespace";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1); // both error: and warning: count as warnings
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "Sources/Bar.swift");
        assert_eq!(snap.diagnostics[0].line, Some(20));
        assert_eq!(snap.diagnostics[0].column, Some(1));
        // Message is everything after the 4th split (splitn 4), the " error: trailing whitespace"
        assert!(snap.diagnostics[0].message.contains("trailing whitespace"));
    }

    #[test]
    fn test_parse_lint_warning_and_error_lines() {
        let stdout = "A.swift:1:2: warning: too long\nB.swift:3:4: error: bad indent";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].file, "A.swift");
        assert_eq!(snap.diagnostics[0].line, Some(1));
        assert_eq!(snap.diagnostics[0].column, Some(2));
        assert_eq!(snap.diagnostics[1].file, "B.swift");
        assert_eq!(snap.diagnostics[1].line, Some(3));
        assert_eq!(snap.diagnostics[1].column, Some(4));
    }

    #[test]
    fn test_parse_lint_lines_without_swift_ignored() {
        let stdout = "src/main.rs:10:5: warning: this is not swift\nsome random output\n";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_multiple_issues() {
        let stdout = r#"A.swift:1:1: warning: w1
B.swift:2:2: warning: w2
C.swift:3:3: error: e1
D.swift:4:4: warning: w3"#;
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 4);
        assert_eq!(snap.diagnostics.len(), 4);
    }

    #[test]
    fn test_parse_lint_malformed_lines() {
        // Lines that contain .swift: and warning: but with non-numeric line/col parts
        let stdout = "Foo.swift:abc:xyz: warning: something odd";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "Foo.swift");
        assert!(snap.diagnostics[0].line.is_none()); // "abc" can't parse
        assert!(snap.diagnostics[0].column.is_none()); // "xyz" can't parse
    }

    #[test]
    fn test_parse_lint_line_without_warning_or_error_ignored() {
        // Contains .swift: but neither warning: nor error:
        let stdout = "Foo.swift:10:5: note: some note";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_stderr_content() {
        let stderr = "X.swift:5:1: warning: from stderr";
        let snap = parse_lint("", stderr);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "X.swift");
    }

    #[test]
    fn test_parse_coverage_empty_input() {
        let snap = parse_coverage("", "");
        assert_eq!(snap.category, EvalCategory::Coverage);
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_valid_llvm_cov_json() {
        let stdout =
            r#"{"data":[{"totals":{"lines":{"percent":92.3},"branches":{"percent":78.1}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 92.3).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 78.1).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_llvm_cov_json_lines_only() {
        // branches key missing
        let stdout = r#"{"data":[{"totals":{"lines":{"percent":50.0}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 50.0).abs() < 0.01);
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_llvm_cov_json_branches_only() {
        // lines key missing
        let stdout = r#"{"data":[{"totals":{"branches":{"percent":60.0}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!((snap.branch_coverage_pct.unwrap() - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_invalid_json_fallback_to_console() {
        // Not valid JSON, falls through to console parsing
        let stdout = "not json\ncoverage: 77.5%\nmore output";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 77.5).abs() < 0.01);
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_console_with_capital_coverage() {
        let stdout = "Coverage: 88.8%";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 88.8).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_console_without_percentage() {
        let stdout = "coverage: no percentage here";
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_console_no_coverage_keyword() {
        let stdout = "total lines: 500";
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_multiple_percentage_patterns_first_wins() {
        let stdout = "coverage: 60.0%\ncoverage: 70.0%";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_stderr_fallback() {
        let stderr = "Coverage: 42.0%";
        let snap = parse_coverage("", stderr);
        assert!((snap.line_coverage_pct.unwrap() - 42.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_json_without_data_key() {
        // Valid JSON but no data/totals path, falls back to console
        let stdout = r#"{"summary": "no coverage data"}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_json_with_data_but_no_totals() {
        let stdout = r#"{"data":[{"files":[]}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_json_takes_precedence_over_console() {
        // If JSON parsing succeeds and sets line_coverage_pct, console fallback is skipped
        let stdout = r#"{"data":[{"totals":{"lines":{"percent":99.0}}}]}"#;
        let stderr = "coverage: 50.0%"; // should be ignored
        let snap = parse_coverage(stdout, stderr);
        assert!((snap.line_coverage_pct.unwrap() - 99.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_number_after_basic() {
        assert_eq!(
            extract_number_after("Executed 42 tests", "Executed "),
            Some(42)
        );
    }

    #[test]
    fn test_extract_number_after_prefix_not_found() {
        assert_eq!(extract_number_after("some random line", "Executed "), None);
    }

    #[test]
    fn test_extract_number_after_empty_prefix() {
        // Empty prefix matches at index 0, so it reads from the start of the line
        assert_eq!(extract_number_after("123abc", ""), Some(123));
    }

    #[test]
    fn test_extract_number_after_no_number_after_prefix() {
        assert_eq!(
            extract_number_after("Executed abc tests", "Executed "),
            None
        );
    }

    #[test]
    fn test_extract_number_after_number_at_end() {
        assert_eq!(extract_number_after("with 7", "with "), Some(7));
    }

    #[test]
    fn test_extract_number_after_zero() {
        assert_eq!(extract_number_after("with 0 failures", "with "), Some(0));
    }

    #[test]
    fn test_extract_number_after_large_number() {
        assert_eq!(
            extract_number_after("Executed 999999 tests", "Executed "),
            Some(999999)
        );
    }

    #[test]
    fn test_extract_number_after_empty_line() {
        assert_eq!(extract_number_after("", "Executed "), None);
    }

    #[test]
    fn test_extract_percentage_integer() {
        assert!((extract_percentage("85%").unwrap() - 85.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage_one_decimal() {
        assert!((extract_percentage("85.5%").unwrap() - 85.5).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage_two_decimals() {
        assert!((extract_percentage("85.55%").unwrap() - 85.55).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage_no_percent_sign() {
        assert!(extract_percentage("85.5").is_none());
    }

    #[test]
    fn test_extract_percentage_space_before_percent() {
        // "85.5 %" - space before %, the '%' is found but walking backward
        // from '%' hits ' ' which is neither digit nor '.', so start==end==index of '%'
        // minus the space. Actually the space breaks the backward walk so it will
        // not find a number if there's a space before %.
        assert!(extract_percentage("85.5 %").is_none());
    }

    #[test]
    fn test_extract_percentage_embedded_in_text() {
        assert!((extract_percentage("Line coverage: 92.3% of lines").unwrap() - 92.3).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage_hundred() {
        assert!((extract_percentage("100%").unwrap() - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage_zero() {
        assert!((extract_percentage("0%").unwrap() - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage_multiple_percent_signs_first_wins() {
        // The function iterates and returns on the first valid match
        let result = extract_percentage("50% and 80%").unwrap();
        assert!((result - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_percentage_empty_string() {
        assert!(extract_percentage("").is_none());
    }

    #[test]
    fn test_extract_percentage_percent_sign_only() {
        // '%' at index 0, no digits before it
        assert!(extract_percentage("%").is_none());
    }
}
