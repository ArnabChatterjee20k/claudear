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
}
