//! Python output parsers (pytest, mypy/ruff, ruff format/black, pytest-cov).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse `pytest --tb=short -q` output.
///
/// Pytest summary line: "X passed, Y failed, Z skipped in T.Ts"
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // Summary line: "5 passed, 2 failed, 1 skipped in 1.23s"
        // or: "5 passed in 0.5s"
        if trimmed.contains(" passed") && trimmed.contains(" in ") {
            if let Some(p) = extract_count(trimmed, "passed") {
                snapshot.passed = p;
            }
            if let Some(f) = extract_count(trimmed, "failed") {
                snapshot.failed = f;
            }
            if let Some(s) = extract_count(trimmed, "skipped") {
                snapshot.skipped = s;
            }
        }
        // Individual FAILED lines: "FAILED test_foo.py::test_bar - AssertionError"
        if trimmed.starts_with("FAILED ") {
            snapshot.diagnostics.push(Diagnostic {
                file: "pytest".to_string(),
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

/// Parse mypy or ruff check output.
///
/// mypy: "file.py:10: error: msg [code]"
/// ruff: "file.py:10:5: E501 msg"
pub fn parse_analysis(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // mypy format: "file.py:10: error: msg [code]"
        if trimmed.contains(": error:") || trimmed.contains(": warning:") {
            let is_error = trimmed.contains(": error:");
            let parts: Vec<&str> = trimmed.splitn(3, ':').collect();
            let file = parts.first().unwrap_or(&"unknown").to_string();
            let line_num = parts.get(1).and_then(|s| s.trim().parse::<u32>().ok());

            if is_error {
                snapshot.errors += 1;
            } else {
                snapshot.warnings += 1;
            }

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
        // ruff format: "file.py:10:5: E501 Line too long"
        if trimmed.contains(".py:") {
            let parts: Vec<&str> = trimmed.splitn(4, ':').collect();
            if parts.len() >= 4 {
                let rest = parts[3].trim();
                if rest.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    let file = parts[0].to_string();
                    let line_num = parts[1].trim().parse::<u32>().ok();
                    let col = parts[2].trim().parse::<u32>().ok();
                    snapshot.warnings += 1;
                    snapshot.diagnostics.push(Diagnostic {
                        file,
                        line: line_num,
                        column: col,
                        severity: DiagnosticSeverity::Warning,
                        code: None,
                        message: rest.to_string(),
                    });
                }
            }
        }
    }

    snapshot
}

/// Parse ruff format / black lint output.
///
/// ruff format: "would reformat file.py"
/// black: "Would reformat file.py"
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("would reformat") || trimmed.starts_with("Would reformat") {
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

/// Parse pytest-cov output.
///
/// Coverage table has "TOTAL ... 85%" line.
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("TOTAL") {
            if let Some(pct) = extract_percentage(trimmed) {
                snapshot.line_coverage_pct = Some(pct);
                break;
            }
        }
    }

    snapshot
}

fn extract_count(line: &str, keyword: &str) -> Option<u32> {
    let idx = line.find(keyword)?;
    // Walk backward from keyword to find the number
    let before = &line[..idx];
    let num_str: String = before
        .trim()
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if num_str.is_empty() {
        return None;
    }
    num_str.parse().ok()
}

fn extract_percentage(line: &str) -> Option<f64> {
    for (i, c) in line.chars().enumerate() {
        if c == '%' {
            let chars: Vec<char> = line.chars().collect();
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
    fn test_parse_pytest_output() {
        let stdout = "5 passed, 2 failed, 1 skipped in 1.23s";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 5);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.errors, 2);
    }

    #[test]
    fn test_parse_pytest_all_passed() {
        let stdout = "10 passed in 0.5s";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 10);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_pytest_failed_lines() {
        let stdout = "FAILED test_foo.py::test_bar - AssertionError\n3 passed, 1 failed in 0.5s";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].message.contains("FAILED"));
    }

    #[test]
    fn test_parse_pytest_empty() {
        let snap = parse_test("", "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_mypy_output() {
        let stdout = "src/main.py:10: error: Incompatible types [assignment]\nsrc/utils.py:5: warning: Unused import [import]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_ruff_check_output() {
        let stdout = "src/main.py:10:5: E501 Line too long (120 > 88 characters)";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_analysis_empty() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_ruff_format_lint() {
        let stdout = "would reformat src/main.py\nwould reformat src/utils.py";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
    }

    #[test]
    fn test_parse_black_lint() {
        let stdout = "Would reformat src/main.py";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
    }

    #[test]
    fn test_parse_lint_empty() {
        let snap = parse_lint("", "");
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_pytest_cov() {
        let stdout =
            "Name    Stmts   Miss  Cover\n------\nsrc/main.py  100  15  85%\nTOTAL  200  30  85%";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_empty() {
        let snap = parse_coverage("", "");
        assert!(snap.line_coverage_pct.is_none());
    }
}
