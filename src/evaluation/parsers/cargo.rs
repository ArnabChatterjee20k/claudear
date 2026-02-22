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

    #[test]
    fn test_parse_test_empty_input() {
        let snap = parse_test("", "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
        assert_eq!(snap.category, EvalCategory::Test);
    }

    #[test]
    fn test_parse_test_invalid_json_lines_skipped() {
        let stdout = "this is not json\n{bad json\n";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_non_test_type_ignored() {
        let stdout = r#"{ "type": "suite", "event": "started", "test_count": 3 }
{ "type": "suite", "event": "ok", "passed": 3, "failed": 0 }
{ "type": "bench", "event": "ok", "name": "bench_foo" }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
    }

    #[test]
    fn test_parse_test_unknown_event_type() {
        let stdout = r#"{ "type": "test", "event": "started", "name": "test_foo" }
{ "type": "test", "event": "timeout", "name": "test_bar" }"#;
        let snap = parse_test(stdout, "");
        // Unknown events like "started" and "timeout" are silently ignored
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_failed_includes_name_in_diagnostic() {
        let stdout = r#"{ "type": "test", "event": "failed", "name": "tests::my_failing_test" }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "tests::my_failing_test");
        assert_eq!(
            snap.diagnostics[0].message,
            "Test 'tests::my_failing_test' failed"
        );
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert!(snap.diagnostics[0].line.is_none());
        assert!(snap.diagnostics[0].column.is_none());
        assert!(snap.diagnostics[0].code.is_none());
    }

    #[test]
    fn test_parse_test_failed_missing_name_uses_unknown() {
        let stdout = r#"{ "type": "test", "event": "failed" }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "unknown");
        assert_eq!(snap.diagnostics[0].message, "Test 'unknown' failed");
    }

    #[test]
    fn test_parse_test_multiple_mixed_results() {
        let stdout = r#"{ "type": "test", "event": "ok", "name": "test_a" }
{ "type": "test", "event": "ok", "name": "test_b" }
{ "type": "test", "event": "ok", "name": "test_c" }
{ "type": "test", "event": "failed", "name": "test_d" }
{ "type": "test", "event": "failed", "name": "test_e" }
{ "type": "test", "event": "ignored", "name": "test_f" }
{ "type": "test", "event": "ignored", "name": "test_g" }
{ "type": "test", "event": "ignored", "name": "test_h" }
{ "type": "suite", "event": "ok" }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 3);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.skipped, 3);
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].message, "Test 'test_d' failed");
        assert_eq!(snap.diagnostics[1].message, "Test 'test_e' failed");
    }

    #[test]
    fn test_parse_test_mixed_valid_and_invalid_lines() {
        let stdout = r#"not json at all
{ "type": "test", "event": "ok", "name": "test_good" }
{invalid json here}
{ "type": "test", "event": "failed", "name": "test_bad" }
random text"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 1);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_test_json_without_type_field() {
        let stdout = r#"{ "event": "ok", "name": "test_foo" }
{ "something": "else" }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
    }

    #[test]
    fn test_parse_test_json_without_event_field() {
        let stdout = r#"{ "type": "test", "name": "test_foo" }"#;
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
    }

    // -------------------------------------------------------
    // parse_clippy: additional coverage
    // -------------------------------------------------------

    #[test]
    fn test_parse_clippy_empty_input() {
        let snap = parse_clippy("", "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
        assert_eq!(snap.category, EvalCategory::StaticAnalysis);
    }

    #[test]
    fn test_parse_clippy_invalid_json_lines() {
        let stdout = "this is not valid json\n{also not valid}\n";
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_clippy_non_compiler_message_ignored() {
        let stdout = r#"{"reason":"compiler-artifact","target":{"name":"my_crate"}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_clippy_error_level() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"error","message":"cannot find value `x` in this scope","code":{"code":"E0425"},"spans":[{"file_name":"src/lib.rs","line_start":5,"column_start":12}]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 0);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(snap.diagnostics[0].file, "src/lib.rs");
        assert_eq!(snap.diagnostics[0].line, Some(5));
        assert_eq!(snap.diagnostics[0].column, Some(12));
        assert_eq!(snap.diagnostics[0].code, Some("E0425".to_string()));
        assert_eq!(
            snap.diagnostics[0].message,
            "cannot find value `x` in this scope"
        );
    }

    #[test]
    fn test_parse_clippy_warning_level() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused variable: `y`","code":{"code":"unused_variables"},"spans":[{"file_name":"src/main.rs","line_start":10,"column_start":5}]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn test_parse_clippy_info_level() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"note","message":"this is a note","code":null,"spans":[{"file_name":"src/main.rs","line_start":1,"column_start":1}]}}"#;
        let snap = parse_clippy(stdout, "");
        // "note" is not "error" or "warning", so it maps to Info severity
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn test_parse_clippy_unknown_level_maps_to_info() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"help","message":"consider using a reference","code":null,"spans":[{"file_name":"src/foo.rs","line_start":3,"column_start":1}]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn test_parse_clippy_aborting_message_excluded() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 3 previous errors","code":null,"spans":[]}}
{"reason":"compiler-message","message":{"level":"error","message":"real error here","code":{"code":"E0001"},"spans":[{"file_name":"src/main.rs","line_start":1,"column_start":1}]}}"#;
        let snap = parse_clippy(stdout, "");
        // The "aborting due to" message should be excluded from diagnostics
        // but still counted as an error
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].message, "real error here");
    }

    #[test]
    fn test_parse_clippy_message_with_no_spans_uses_unknown() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused import","code":{"code":"unused_imports"},"spans":[]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "unknown");
        assert!(snap.diagnostics[0].line.is_none());
        assert!(snap.diagnostics[0].column.is_none());
    }

    #[test]
    fn test_parse_clippy_message_with_no_code() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"warning","message":"some warning","spans":[{"file_name":"src/main.rs","line_start":1,"column_start":1}]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].code.is_none());
    }

    #[test]
    fn test_parse_clippy_message_with_null_code() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","code":null,"spans":[{"file_name":"src/main.rs","line_start":7,"column_start":3}]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].code.is_none());
    }

    #[test]
    fn test_parse_clippy_multiple_messages() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused variable: `a`","code":{"code":"unused_variables"},"spans":[{"file_name":"src/main.rs","line_start":1,"column_start":5}]}}
{"reason":"compiler-message","message":{"level":"warning","message":"unused variable: `b`","code":{"code":"unused_variables"},"spans":[{"file_name":"src/main.rs","line_start":2,"column_start":5}]}}
{"reason":"compiler-message","message":{"level":"error","message":"cannot find type `Foo`","code":{"code":"E0412"},"spans":[{"file_name":"src/lib.rs","line_start":10,"column_start":1}]}}
{"reason":"compiler-artifact","target":{"name":"my_crate"}}
{"reason":"compiler-message","message":{"level":"error","message":"aborting due to previous error","code":null,"spans":[]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.errors, 2); // "cannot find type" + "aborting due to"
                                    // "aborting due to" is excluded from diagnostics
        assert_eq!(snap.diagnostics.len(), 3);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(snap.diagnostics[1].severity, DiagnosticSeverity::Warning);
        assert_eq!(snap.diagnostics[2].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn test_parse_clippy_compiler_message_without_message_field() {
        // reason is compiler-message but there's no "message" key
        let stdout = r#"{"reason":"compiler-message","other":"data"}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_clippy_mixed_valid_and_invalid_json() {
        let stdout = r#"not json
{"reason":"compiler-message","message":{"level":"warning","message":"unused","code":null,"spans":[]}}
{broken json
{"reason":"compiler-message","message":{"level":"error","message":"real error","code":null,"spans":[]}}"#;
        let snap = parse_clippy(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    // -------------------------------------------------------
    // parse_fmt: additional coverage
    // -------------------------------------------------------

    #[test]
    fn test_parse_fmt_empty_input() {
        let snap = parse_fmt("", "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
        assert_eq!(snap.category, EvalCategory::Lint);
    }

    #[test]
    fn test_parse_fmt_multiple_diff_lines() {
        let stdout = "Diff in src/main.rs at line 10:\n  some diff\nDiff in src/lib.rs at line 20:\n  another diff\nDiff in src/util.rs at line 5:\n  yet another\n";
        let snap = parse_fmt(stdout, "");
        assert_eq!(snap.warnings, 3);
        assert_eq!(snap.diagnostics.len(), 3);
        assert_eq!(snap.diagnostics[0].file, "src/main.rs");
        assert_eq!(snap.diagnostics[1].file, "src/lib.rs");
        assert_eq!(snap.diagnostics[2].file, "src/util.rs");
        for d in &snap.diagnostics {
            assert_eq!(d.severity, DiagnosticSeverity::Warning);
            assert!(d.line.is_none());
            assert!(d.column.is_none());
            assert!(d.code.is_none());
        }
    }

    #[test]
    fn test_parse_fmt_non_diff_lines_ignored() {
        let stdout =
            "Checking formatting...\nAll files formatted correctly!\nSome other output line\n";
        let snap = parse_fmt(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_fmt_stderr_with_diff() {
        // parse_fmt chains stdout and stderr, so "Diff in" in stderr should be picked up
        let snap = parse_fmt("", "Diff in src/error.rs at line 42:\n  diff content\n");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "src/error.rs");
        assert_eq!(
            snap.diagnostics[0].message,
            "Diff in src/error.rs at line 42:"
        );
    }

    #[test]
    fn test_parse_fmt_both_stdout_and_stderr() {
        let stdout = "Diff in src/a.rs at line 1:\n";
        let stderr = "Diff in src/b.rs at line 2:\n";
        let snap = parse_fmt(stdout, stderr);
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].file, "src/a.rs");
        assert_eq!(snap.diagnostics[1].file, "src/b.rs");
    }

    #[test]
    fn test_parse_fmt_diff_line_without_at_line() {
        // "Diff in " prefix but no " at line " part
        let stdout = "Diff in src/main.rs\n";
        let snap = parse_fmt(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        // split(" at line ").next() returns "src/main.rs"
        assert_eq!(snap.diagnostics[0].file, "src/main.rs");
    }

    #[test]
    fn test_parse_fmt_whitespace_trimmed() {
        let stdout = "  Diff in src/main.rs at line 10:  \n";
        let snap = parse_fmt(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics[0].file, "src/main.rs");
    }

    // -------------------------------------------------------
    // parse_coverage: additional coverage
    // -------------------------------------------------------

    #[test]
    fn test_parse_coverage_empty_input() {
        let snap = parse_coverage("", "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
        assert_eq!(snap.category, EvalCategory::Coverage);
    }

    #[test]
    fn test_parse_coverage_invalid_json() {
        let snap = parse_coverage("this is not json", "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_json_without_data_array() {
        let stdout = r#"{"totals":{"lines":{"percent":90.0}}}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_json_with_empty_data_array() {
        let stdout = r#"{"data":[]}"#;
        let snap = parse_coverage(stdout, "");
        // pointer "/data/0/totals" will fail because data[0] doesn't exist
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_only_lines_no_branches() {
        let stdout = r#"{"data":[{"totals":{"lines":{"percent":92.3}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 92.3).abs() < 0.01);
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_only_branches_no_lines() {
        let stdout = r#"{"data":[{"totals":{"branches":{"percent":67.8}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!((snap.branch_coverage_pct.unwrap() - 67.8).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_totals_without_lines_or_branches() {
        let stdout = r#"{"data":[{"totals":{"functions":{"percent":100.0}}}]}"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_lines_without_percent() {
        let stdout = r#"{"data":[{"totals":{"lines":{"count":100,"covered":85}}}]}"#;
        let snap = parse_coverage(stdout, "");
        // "percent" key is missing, so line_coverage_pct should be None
        assert!(snap.line_coverage_pct.is_none());
    }

    // -------------------------------------------------------
    // extract_span: additional coverage
    // -------------------------------------------------------

    #[test]
    fn test_extract_span_empty_spans_array() {
        let msg: serde_json::Value = serde_json::from_str(r#"{"spans":[]}"#).unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "unknown");
        assert!(line.is_none());
        assert!(col.is_none());
    }

    #[test]
    fn test_extract_span_no_spans_key() {
        let msg: serde_json::Value = serde_json::from_str(r#"{"level":"error"}"#).unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "unknown");
        assert!(line.is_none());
        assert!(col.is_none());
    }

    #[test]
    fn test_extract_span_spans_not_array() {
        let msg: serde_json::Value = serde_json::from_str(r#"{"spans":"not an array"}"#).unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "unknown");
        assert!(line.is_none());
        assert!(col.is_none());
    }

    #[test]
    fn test_extract_span_with_missing_fields() {
        // Span object without file_name, line_start, column_start
        let msg: serde_json::Value =
            serde_json::from_str(r#"{"spans":[{"other":"data"}]}"#).unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "unknown"); // falls back to "unknown"
        assert!(line.is_none());
        assert!(col.is_none());
    }

    #[test]
    fn test_extract_span_with_partial_fields() {
        // Span with file_name but no line/column
        let msg: serde_json::Value =
            serde_json::from_str(r#"{"spans":[{"file_name":"src/foo.rs"}]}"#).unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "src/foo.rs");
        assert!(line.is_none());
        assert!(col.is_none());
    }

    #[test]
    fn test_extract_span_with_all_fields() {
        let msg: serde_json::Value = serde_json::from_str(
            r#"{"spans":[{"file_name":"src/main.rs","line_start":42,"column_start":10}]}"#,
        )
        .unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "src/main.rs");
        assert_eq!(line, Some(42));
        assert_eq!(col, Some(10));
    }

    #[test]
    fn test_extract_span_uses_first_span_only() {
        let msg: serde_json::Value = serde_json::from_str(
            r#"{"spans":[{"file_name":"first.rs","line_start":1,"column_start":1},{"file_name":"second.rs","line_start":99,"column_start":99}]}"#,
        )
        .unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "first.rs");
        assert_eq!(line, Some(1));
        assert_eq!(col, Some(1));
    }

    #[test]
    fn test_extract_span_null_message() {
        let msg: serde_json::Value = serde_json::from_str(r#"null"#).unwrap();
        let (file, line, col) = extract_span(&msg);
        assert_eq!(file, "unknown");
        assert!(line.is_none());
        assert!(col.is_none());
    }
}
