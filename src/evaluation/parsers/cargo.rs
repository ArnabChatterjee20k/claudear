//! Cargo output parsers (Rust).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse `cargo test -- --format json` output.
pub fn parse_test(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("type").and_then(|t| t.as_str()) == Some("test") {
                if let Some(event) = v.get("event").and_then(|e| e.as_str()) {
                    match event {
                        "ok" => snapshot.passed += 1,
                        "failed" => {
                            snapshot.failed += 1;
                            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                            snapshot.diagnostics.push(Diagnostic {
                                file: name.to_string(),
                                line: None,
                                column: None,
                                severity: DiagnosticSeverity::Error,
                                code: None,
                                message: format!("Test '{}' failed", name),
                            });
                        }
                        "ignored" => snapshot.skipped += 1,
                        _ => {}
                    }
                }
            }
        }
    }

    snapshot.errors = snapshot.failed;
    snapshot
}

/// Parse `cargo clippy --message-format=json` output.
pub fn parse_clippy(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("reason").and_then(|r| r.as_str()) == Some("compiler-message") {
                if let Some(message) = v.get("message") {
                    let level = message
                        .get("level")
                        .and_then(|l| l.as_str())
                        .unwrap_or("unknown");
                    let msg = message
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    let code = message
                        .get("code")
                        .and_then(|c| c.get("code"))
                        .and_then(|c| c.as_str())
                        .map(String::from);

                    let (file, line_num, col) = extract_span(message);

                    let severity = match level {
                        "error" => DiagnosticSeverity::Error,
                        "warning" => DiagnosticSeverity::Warning,
                        _ => DiagnosticSeverity::Info,
                    };

                    match level {
                        "error" => snapshot.errors += 1,
                        "warning" => snapshot.warnings += 1,
                        _ => {}
                    }

                    // Skip "aborting due to" summary messages
                    if !msg.starts_with("aborting due to") {
                        snapshot.diagnostics.push(Diagnostic {
                            file,
                            line: line_num,
                            column: col,
                            severity,
                            code,
                            message: msg,
                        });
                    }
                }
            }
        }
    }

    snapshot
}

/// Parse `cargo fmt --check` output.
pub fn parse_fmt(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    // cargo fmt --check outputs "Diff in /path/to/file.rs at line X:"
    for line in stdout.lines().chain(stderr.lines()) {
        let trimmed = line.trim();
        if trimmed.starts_with("Diff in ") {
            snapshot.warnings += 1;
            let file = trimmed
                .strip_prefix("Diff in ")
                .unwrap_or("")
                .split(" at line ")
                .next()
                .unwrap_or("unknown")
                .to_string();
            snapshot.diagnostics.push(Diagnostic {
                file,
                line: None,
                column: None,
                severity: DiagnosticSeverity::Warning,
                code: None,
                message: trimmed.to_string(),
            });
        }
    }

    snapshot
}

/// Parse `cargo llvm-cov --json` output.
pub fn parse_coverage(stdout: &str, _stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

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

    snapshot
}

fn extract_span(message: &serde_json::Value) -> (String, Option<u32>, Option<u32>) {
    if let Some(spans) = message.get("spans").and_then(|s| s.as_array()) {
        if let Some(span) = spans.first() {
            let file = span
                .get("file_name")
                .and_then(|f| f.as_str())
                .unwrap_or("unknown")
                .to_string();
            let line = span
                .get("line_start")
                .and_then(|l| l.as_u64())
                .map(|l| l as u32);
            let col = span
                .get("column_start")
                .and_then(|c| c.as_u64())
                .map(|c| c as u32);
            return (file, line, col);
        }
    }
    ("unknown".to_string(), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cargo_test_json() {
        let stdout = r#"{ "type": "test", "event": "ok", "name": "test_foo" }
{ "type": "test", "event": "failed", "name": "test_bar" }
{ "type": "test", "event": "ignored", "name": "test_baz" }
{ "type": "suite", "event": "ok" }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 1);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_clippy_json() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused variable","code":{"code":"unused_variables"},"spans":[{"file_name":"src/main.rs","line_start":10,"column_start":5}]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "src/main.rs");
    }

    #[test]
    fn test_parse_fmt() {
        let stdout = "Diff in src/main.rs at line 10:\n  some diff content\n";
        let snap = parse_fmt(stdout, "");
        assert_eq!(snap.warnings, 1);
    }

    #[test]
    fn test_parse_coverage_json() {
        let stdout =
            r#"{"data":[{"totals":{"lines":{"percent":85.5},"branches":{"percent":72.3}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.5).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 72.3).abs() < 0.01);
    }
}
