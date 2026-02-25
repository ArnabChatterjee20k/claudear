//! Dart output parsers (dart test, dart analyze, dart format, dart test coverage).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse `dart test` output.
///
/// "+X: All tests passed!" or "+X -Y: Some tests failed."
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // "+10: All tests passed!"
        if trimmed.contains("All tests passed") {
            if let Some(p) = extract_plus_count(trimmed) {
                snapshot.passed = p;
            }
        }
        // "+8 -2: Some tests failed."
        if trimmed.contains("Some tests failed") {
            if let Some(p) = extract_plus_count(trimmed) {
                snapshot.passed = p;
            }
            if let Some(f) = extract_minus_count(trimmed) {
                snapshot.failed = f;
            }
        }
        // Individual failure line
        if trimmed.contains("[E]") || (trimmed.starts_with("-") && trimmed.contains("FAILED")) {
            snapshot.diagnostics.push(Diagnostic {
                file: "dart-test".to_string(),
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

/// Parse `dart analyze` output.
///
/// "info - file.dart:10:5 - msg - code"
/// Summary: "X issues found."
pub fn parse_analysis(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // "info - lib/main.dart:10:5 - Unused import - unused_import"
        // "warning - lib/main.dart:10:5 - msg - code"
        // "error - lib/main.dart:10:5 - msg - code"
        if (trimmed.starts_with("info")
            || trimmed.starts_with("warning")
            || trimmed.starts_with("error"))
            && trimmed.contains(" - ")
            && trimmed.contains(".dart:")
        {
            let parts: Vec<&str> = trimmed.splitn(2, " - ").collect();
            let severity_str = parts[0].trim();
            let severity = match severity_str {
                "error" => DiagnosticSeverity::Error,
                "warning" => DiagnosticSeverity::Warning,
                _ => DiagnosticSeverity::Info,
            };

            match severity {
                DiagnosticSeverity::Error => snapshot.errors += 1,
                DiagnosticSeverity::Warning => snapshot.warnings += 1,
                _ => {}
            }

            // Parse file:line:col from rest
            let rest = parts.get(1).unwrap_or(&"");
            let loc_parts: Vec<&str> = rest.splitn(2, " - ").collect();
            let loc = loc_parts[0].trim();
            let file_parts: Vec<&str> = loc.splitn(3, ':').collect();
            let file = file_parts.first().unwrap_or(&"unknown").to_string();
            let line_num = file_parts.get(1).and_then(|s| s.parse::<u32>().ok());
            let col = file_parts.get(2).and_then(|s| s.parse::<u32>().ok());
            let message = loc_parts.get(1).unwrap_or(&"").trim().to_string();

            snapshot.diagnostics.push(Diagnostic {
                file,
                line: line_num,
                column: col,
                severity,
                code: None,
                message,
            });
        }
        // Summary: "3 issues found."
        if trimmed.contains("issues found") || trimmed.contains("issue found") {
            // Already counted per-line
        }
    }

    snapshot
}

/// Parse `dart format` output.
///
/// Lists files that were changed.
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // dart format with --set-exit-if-changed outputs "Changed X.dart"
        if trimmed.starts_with("Changed ") && trimmed.ends_with(".dart") {
            snapshot.warnings += 1;
            snapshot.diagnostics.push(Diagnostic {
                file: trimmed
                    .strip_prefix("Changed ")
                    .unwrap_or(trimmed)
                    .to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Warning,
                code: None,
                message: "File needs formatting".to_string(),
            });
        }
        // Also handle plain file listing
        if trimmed.ends_with(".dart") && !trimmed.contains(' ') && !trimmed.is_empty() {
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

/// Parse dart test coverage output.
///
/// Parse LCOV or terminal coverage output.
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);

    // Try LCOV format: LF (lines found), LH (lines hit) at end-of-record
    let mut total_lf: u32 = 0;
    let mut total_lh: u32 = 0;
    let mut has_lcov = false;
    for line in combined.lines() {
        let trimmed = line.trim();
        if let Some(lf) = trimmed.strip_prefix("LF:") {
            if let Ok(n) = lf.parse::<u32>() {
                total_lf += n;
                has_lcov = true;
            }
        }
        if let Some(lh) = trimmed.strip_prefix("LH:") {
            if let Ok(n) = lh.parse::<u32>() {
                total_lh += n;
            }
        }
    }
    if has_lcov && total_lf > 0 {
        snapshot.line_coverage_pct = Some(total_lh as f64 / total_lf as f64 * 100.0);
        return snapshot;
    }

    // Fallback: percentage in text
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains("coverage:") || trimmed.contains("Coverage:") {
            if let Some(pct) = extract_percentage(trimmed) {
                snapshot.line_coverage_pct = Some(pct);
                break;
            }
        }
    }

    snapshot
}

fn extract_plus_count(line: &str) -> Option<u32> {
    // "+10" at the start of a token
    for token in line.split_whitespace() {
        if let Some(num) = token.strip_prefix('+') {
            let num_str: String = num.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num_str.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

fn extract_minus_count(line: &str) -> Option<u32> {
    for token in line.split_whitespace() {
        if let Some(num) = token.strip_prefix('-') {
            let num_str: String = num.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num_str.parse::<u32>() {
                if n > 0 {
                    return Some(n);
                }
            }
        }
    }
    None
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
    fn test_parse_dart_test_all_passed() {
        let stdout = "+10: All tests passed!";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 10);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_dart_test_some_failed() {
        let stdout = "+8 -2: Some tests failed.";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 8);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.errors, 2);
    }

    #[test]
    fn test_parse_dart_test_empty() {
        let snap = parse_test("", "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_dart_analyze() {
        let stdout = "error - lib/main.dart:10:5 - Undefined name 'foo' - undefined_identifier\nwarning - lib/utils.dart:20:1 - Unused import - unused_import\ninfo - lib/config.dart:5:1 - Unnecessary cast - unnecessary_cast";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 3);
    }

    #[test]
    fn test_parse_dart_analyze_empty() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_dart_format() {
        let stdout = "Changed lib/main.dart\nChanged lib/utils.dart";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
    }

    #[test]
    fn test_parse_dart_format_empty() {
        let snap = parse_lint("", "");
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_dart_coverage_lcov() {
        let stdout = "SF:lib/main.dart\nDA:1,1\nDA:2,0\nLF:2\nLH:1\nend_of_record";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_dart_coverage_percentage() {
        let stdout = "coverage: 85.5%";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_dart_coverage_empty() {
        let snap = parse_coverage("", "");
        assert!(snap.line_coverage_pct.is_none());
    }
}
