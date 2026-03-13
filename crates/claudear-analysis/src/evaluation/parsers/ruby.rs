//! Ruby output parsers (rspec, rubocop, simplecov).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse rspec test output.
///
/// Summary: "X examples, Y failures, Z pending"
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // "10 examples, 2 failures, 1 pending"
        // or "10 examples, 0 failures"
        if trimmed.contains("example") && trimmed.contains("failure") {
            let total = extract_count_before(trimmed, "example").unwrap_or(0);
            let failures = extract_count_before(trimmed, "failure").unwrap_or(0);
            let pending = extract_count_before(trimmed, "pending").unwrap_or(0);
            snapshot.passed = total.saturating_sub(failures + pending);
            snapshot.failed = failures;
            snapshot.skipped = pending;
        }
        // Individual failure lines
        if trimmed.starts_with("rspec ") && trimmed.contains(":") {
            snapshot.diagnostics.push(Diagnostic {
                file: "rspec".to_string(),
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

/// Parse rubocop JSON analysis output.
///
/// `{"files":[{"offenses":[...]}],"summary":{...}}`
pub fn parse_analysis(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        // Parse from summary
        if let Some(summary) = v.get("summary") {
            snapshot.errors = summary
                .get("offense_count")
                .and_then(|c| c.as_u64())
                .unwrap_or(0) as u32;
        }
        // Parse individual offenses
        if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
            for file in files {
                let file_path = file
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                if let Some(offenses) = file.get("offenses").and_then(|o| o.as_array()) {
                    for offense in offenses {
                        let severity_str = offense
                            .get("severity")
                            .and_then(|s| s.as_str())
                            .unwrap_or("warning");
                        let severity = if severity_str == "error" || severity_str == "fatal" {
                            DiagnosticSeverity::Error
                        } else {
                            DiagnosticSeverity::Warning
                        };
                        let message = offense
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        let line_num = offense
                            .get("location")
                            .and_then(|l| l.get("start_line"))
                            .and_then(|l| l.as_u64())
                            .map(|l| l as u32);
                        let col = offense
                            .get("location")
                            .and_then(|l| l.get("start_column"))
                            .and_then(|c| c.as_u64())
                            .map(|c| c as u32);
                        let code = offense
                            .get("cop_name")
                            .and_then(|c| c.as_str())
                            .map(String::from);

                        match severity {
                            DiagnosticSeverity::Error => snapshot.errors += 1,
                            DiagnosticSeverity::Warning => snapshot.warnings += 1,
                            _ => {}
                        }

                        snapshot.diagnostics.push(Diagnostic {
                            file: file_path.clone(),
                            line: line_num,
                            column: col,
                            severity,
                            code,
                            message,
                        });
                    }
                }
            }
        }
        // Reconcile: if summary had a count but we also counted per-offense,
        // the per-offense counts are more accurate (they include severity).
        // The summary "errors" field was a rough total; overwrite with actual counts.
        if !snapshot.diagnostics.is_empty() {
            snapshot.errors = snapshot
                .diagnostics
                .iter()
                .filter(|d| d.severity == DiagnosticSeverity::Error)
                .count() as u32;
        }
    }

    snapshot
}

/// Parse rubocop lint output (dry-run mode).
///
/// Counts files needing correction.
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with(".rb") && !trimmed.is_empty() {
            snapshot.warnings += 1;
            snapshot.diagnostics.push(Diagnostic {
                file: trimmed.to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Warning,
                code: None,
                message: "File needs correction".to_string(),
            });
        }
    }

    snapshot
}

/// Parse simplecov output.
///
/// "X / Y LOC (85.5%) covered"
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains("LOC") && trimmed.contains("covered") {
            if let Some(pct) = extract_percentage(trimmed) {
                snapshot.line_coverage_pct = Some(pct);
                break;
            }
        }
        // Fallback: "Coverage: 85.5%"
        if trimmed.contains("coverage:") || trimmed.contains("Coverage:") {
            if let Some(pct) = extract_percentage(trimmed) {
                snapshot.line_coverage_pct = Some(pct);
                break;
            }
        }
    }

    snapshot
}

fn extract_count_before(line: &str, keyword: &str) -> Option<u32> {
    let idx = line.find(keyword)?;
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
    fn test_parse_rspec() {
        let stdout = "10 examples, 2 failures, 1 pending";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 7);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.errors, 2);
    }

    #[test]
    fn test_parse_rspec_no_failures() {
        let stdout = "5 examples, 0 failures";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 5);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_rspec_empty() {
        let snap = parse_test("", "");
        assert_eq!(snap.passed, 0);
    }

    #[test]
    fn test_parse_rubocop_json() {
        let stdout = r#"{"files":[{"path":"app.rb","offenses":[{"severity":"warning","message":"Line too long","cop_name":"Layout/LineLength","location":{"start_line":10,"start_column":1}}]}],"summary":{"offense_count":1}}"#;
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(
            snap.diagnostics[0].code.as_deref(),
            Some("Layout/LineLength")
        );
    }

    #[test]
    fn test_parse_rubocop_empty() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_rubocop_invalid_json() {
        let snap = parse_analysis("not json", "");
        assert_eq!(snap.errors, 0);
    }

    #[test]
    fn test_parse_lint_rb_files() {
        let stdout = "app/models/user.rb\napp/controllers/main.rb";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
    }

    #[test]
    fn test_parse_lint_empty() {
        let snap = parse_lint("", "");
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_simplecov_coverage() {
        let stdout = "500 / 600 LOC (83.33%) covered";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 83.33).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_empty() {
        let snap = parse_coverage("", "");
        assert!(snap.line_coverage_pct.is_none());
    }
}
