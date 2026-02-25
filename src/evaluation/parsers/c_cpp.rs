//! C/C++ output parsers (ctest/make test, cppcheck, clang-format, gcov).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse ctest or `make test` output.
///
/// ctest: "X% tests passed, Y tests failed out of Z"
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // ctest summary: "100% tests passed, 0 tests failed out of 10"
        if trimmed.contains("tests passed") && trimmed.contains("tests failed out of") {
            if let Some(failed) = extract_number_before(trimmed, " tests failed") {
                snapshot.failed = failed;
            }
            if let Some(total) = extract_number_after(trimmed, "out of ") {
                snapshot.passed = total.saturating_sub(snapshot.failed);
            }
        }
        // Individual failures
        if trimmed.contains("***Failed") || trimmed.contains("FAILED") {
            snapshot.diagnostics.push(Diagnostic {
                file: "ctest".to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Error,
                code: None,
                message: trimmed.to_string(),
            });
        }
    }

    snapshot.errors = snapshot.failed;
    snapshot
}

/// Parse cppcheck output.
///
/// Format: "[file.c:10]: (error) msg" or "file.c:10:5: error: msg [id]"
pub fn parse_analysis(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // Format 1: "[file.c:10]: (error) msg"
        if trimmed.starts_with('[') && trimmed.contains("]: (") {
            let is_error = trimmed.contains("(error)");
            if is_error {
                snapshot.errors += 1;
            } else {
                snapshot.warnings += 1;
            }
            // Parse file:line from inside brackets
            let bracket_end = trimmed.find(']').unwrap_or(0);
            let inside = &trimmed[1..bracket_end];
            let parts: Vec<&str> = inside.splitn(2, ':').collect();
            let file = parts.first().unwrap_or(&"unknown").to_string();
            let line_num = parts.get(1).and_then(|s| s.trim().parse::<u32>().ok());

            snapshot.diagnostics.push(Diagnostic {
                file,
                line: line_num,
                column: None,
                severity: if is_error {
                    DiagnosticSeverity::Error
                } else {
                    DiagnosticSeverity::Warning
                },
                code: None,
                message: trimmed.to_string(),
            });
            continue;
        }
        // Format 2: "file.c:10:5: error: msg [id]"
        if (trimmed.contains(": error:") || trimmed.contains(": warning:"))
            && (trimmed.contains(".c:") || trimmed.contains(".cpp:") || trimmed.contains(".h:"))
        {
            let is_error = trimmed.contains(": error:");
            if is_error {
                snapshot.errors += 1;
            } else {
                snapshot.warnings += 1;
            }
            let parts: Vec<&str> = trimmed.splitn(4, ':').collect();
            let file = parts.first().unwrap_or(&"unknown").to_string();
            let line_num = parts.get(1).and_then(|s| s.trim().parse::<u32>().ok());
            let col = parts.get(2).and_then(|s| s.trim().parse::<u32>().ok());

            snapshot.diagnostics.push(Diagnostic {
                file,
                line: line_num,
                column: col,
                severity: if is_error {
                    DiagnosticSeverity::Error
                } else {
                    DiagnosticSeverity::Warning
                },
                code: None,
                message: trimmed.to_string(),
            });
        }
    }

    snapshot
}

/// Parse clang-format lint output.
///
/// Lists file references that need formatting.
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && (trimmed.ends_with(".c")
                || trimmed.ends_with(".cpp")
                || trimmed.ends_with(".h")
                || trimmed.ends_with(".hpp")
                || trimmed.ends_with(".cc")
                || trimmed.contains(".c:")
                || trimmed.contains(".cpp:")
                || trimmed.contains(".h:"))
        {
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

/// Parse gcov output.
///
/// "Lines executed:85.50% of 200"
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Lines executed:") {
            if let Some(pct) = extract_gcov_pct(trimmed) {
                snapshot.line_coverage_pct = Some(pct);
                break;
            }
        }
    }

    snapshot
}

fn extract_number_before(line: &str, suffix: &str) -> Option<u32> {
    let idx = line.find(suffix)?;
    let before = &line[..idx];
    let num_str: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    num_str.parse().ok()
}

fn extract_number_after(line: &str, prefix: &str) -> Option<u32> {
    let idx = line.find(prefix)?;
    let after = &line[idx + prefix.len()..];
    let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    num_str.parse().ok()
}

fn extract_gcov_pct(line: &str) -> Option<f64> {
    let idx = line.find("Lines executed:")?;
    let after = &line[idx + "Lines executed:".len()..];
    let num_str: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ctest() {
        let stdout = "100% tests passed, 0 tests failed out of 10";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 10);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_ctest_with_failures() {
        let stdout = "80% tests passed, 2 tests failed out of 10";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 8);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.errors, 2);
    }

    #[test]
    fn test_parse_test_empty() {
        let snap = parse_test("", "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_cppcheck_bracket_format() {
        let stderr = "[main.c:10]: (error) Null pointer dereference\n[utils.c:20]: (warning) Unused variable";
        let snap = parse_analysis("", stderr);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_cppcheck_colon_format() {
        let stdout = "main.c:10:5: error: msg [nullPointer]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics[0].file, "main.c");
    }

    #[test]
    fn test_parse_analysis_empty() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_clang_format() {
        let stdout = "src/main.c\nsrc/utils.h";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
    }

    #[test]
    fn test_parse_lint_empty() {
        let snap = parse_lint("", "");
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_gcov() {
        let stdout = "Lines executed:85.50% of 200";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_empty() {
        let snap = parse_coverage("", "");
        assert!(snap.line_coverage_pct.is_none());
    }
}
