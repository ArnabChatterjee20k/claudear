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
}
