//! Go output parsers (go test, go vet, gofmt, go test -cover).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse `go test -json ./...` output.
///
/// Each line is a JSON object: `{"Action":"pass"/"fail"/"skip","Test":"TestFoo",...}`
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let action = v.get("Action").and_then(|a| a.as_str()).unwrap_or("");
            let test = v.get("Test").and_then(|t| t.as_str());
            // Only count lines that have a "Test" field (package-level pass/fail excluded)
            if test.is_some() {
                match action {
                    "pass" => snapshot.passed += 1,
                    "fail" => {
                        snapshot.failed += 1;
                        snapshot.diagnostics.push(Diagnostic {
                            file: "go-test".to_string(),
                            line: None,
                            column: None,
                            severity: DiagnosticSeverity::Error,
                            code: None,
                            message: format!("FAIL: {}", test.unwrap_or("unknown")),
                        });
                    }
                    "skip" => snapshot.skipped += 1,
                    _ => {}
                }
            }
        }
    }

    snapshot.errors = snapshot.failed;
    snapshot
}

/// Parse `go vet ./...` output.
///
/// Lines: "file.go:10:5: description"
pub fn parse_analysis(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains(".go:") {
            let parts: Vec<&str> = trimmed.splitn(4, ':').collect();
            if parts.len() >= 4 {
                let file = parts[0].to_string();
                let line_num = parts[1].trim().parse::<u32>().ok();
                let col = parts[2].trim().parse::<u32>().ok();
                let message = parts[3].trim().to_string();
                snapshot.warnings += 1;
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
    }

    snapshot
}

/// Parse `gofmt -l .` output.
///
/// Each output line is a file that needs formatting.
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && trimmed.ends_with(".go") {
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

/// Parse `go test -cover ./...` output.
///
/// Lines: "coverage: 85.5% of statements"
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    let mut total_pct = 0.0f64;
    let mut count = 0u32;

    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains("coverage:") && trimmed.contains("% of statements") {
            if let Some(pct) = extract_coverage_pct(trimmed) {
                total_pct += pct;
                count += 1;
            }
        }
    }

    if count > 0 {
        snapshot.line_coverage_pct = Some(total_pct / count as f64);
    }

    snapshot
}

fn extract_coverage_pct(line: &str) -> Option<f64> {
    let idx = line.find("coverage:")?;
    let after = &line[idx + "coverage:".len()..];
    let trimmed = after.trim();
    let num_str: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_go_test_json() {
        let stdout = r#"{"Action":"pass","Test":"TestAdd","Package":"pkg"}
{"Action":"fail","Test":"TestSub","Package":"pkg"}
{"Action":"skip","Test":"TestMul","Package":"pkg"}
{"Action":"pass","Package":"pkg"}"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 1);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_go_test_empty() {
        let snap = parse_test("", "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_go_vet() {
        let stdout = "main.go:10:5: unreachable code";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "main.go");
        assert_eq!(snap.diagnostics[0].line, Some(10));
    }

    #[test]
    fn test_parse_go_vet_empty() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_gofmt() {
        let stdout = "main.go\nutils.go";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_gofmt_empty() {
        let snap = parse_lint("", "");
        assert_eq!(snap.warnings, 0);
    }

    #[test]
    fn test_parse_go_coverage() {
        let stdout = "ok  \tpkg\t0.5s\tcoverage: 85.5% of statements\nok  \tpkg2\t0.3s\tcoverage: 90.0% of statements";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 87.75).abs() < 0.01);
    }

    #[test]
    fn test_parse_go_coverage_single() {
        let stdout = "coverage: 85.5% of statements";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_go_coverage_empty() {
        let snap = parse_coverage("", "");
        assert!(snap.line_coverage_pct.is_none());
    }
}
