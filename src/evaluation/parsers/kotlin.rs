//! Kotlin/Gradle output parsers (Gradle test, Detekt, ktlint, JaCoCo).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse `gradle test` output.
///
/// Gradle test output is complex XML in build/reports. We parse the console
/// summary lines instead: "X tests completed, Y failed, Z skipped"
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // Gradle outputs: "X tests completed, Y failed" or "X tests completed, Y failed, Z skipped"
        if trimmed.contains("tests completed") {
            if let Some(total) = extract_number_before(trimmed, "tests completed") {
                let failed = extract_number_before(trimmed, "failed").unwrap_or(0);
                let skipped = extract_number_before(trimmed, "skipped").unwrap_or(0);
                snapshot.passed = total.saturating_sub(failed + skipped);
                snapshot.failed = failed;
                snapshot.skipped = skipped;
                snapshot.errors = failed;
            }
        }
        // Also capture individual test failure lines
        if trimmed.contains("FAILED") && !trimmed.contains("BUILD FAILED") {
            snapshot.diagnostics.push(Diagnostic {
                file: "gradle".to_string(),
                line: None,
                column: None,
                severity: DiagnosticSeverity::Error,
                code: None,
                message: trimmed.to_string(),
            });
        }
    }

    snapshot
}

/// Parse Detekt static analysis output.
///
/// Detekt outputs findings in format: "File.kt:10:5: Description [RuleName]"
pub fn parse_analysis(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // Detekt format: "path/File.kt:10:5: Description [RuleName]"
        if let Some((file, line_num, col, message, code)) = parse_detekt_line(trimmed) {
            snapshot.warnings += 1;
            snapshot.diagnostics.push(Diagnostic {
                file,
                line: line_num,
                column: col,
                severity: DiagnosticSeverity::Warning,
                code,
                message,
            });
        }
    }

    // Also check for summary line
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Overall debt:") || trimmed.contains("weighted issues") {
            // detekt summary, already counted above
        }
    }

    snapshot
}

/// Parse ktlint lint output.
///
/// ktlint outputs: "path/File.kt:10:5: Description (rule-name)"
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // ktlint format similar to detekt but with parentheses for rule
        if trimmed.contains(".kt:") || trimmed.contains(".kts:") {
            if let Some((file, line_num, col, message, code)) = parse_ktlint_line(trimmed) {
                snapshot.warnings += 1;
                snapshot.diagnostics.push(Diagnostic {
                    file,
                    line: line_num,
                    column: col,
                    severity: DiagnosticSeverity::Warning,
                    code,
                    message,
                });
            }
        }
    }

    snapshot
}

/// Parse JaCoCo coverage report.
///
/// JaCoCo generates XML reports. We look for the summary counters.
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    // JaCoCo XML: <counter type="LINE" missed="X" covered="Y"/>
    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains("<counter") && trimmed.contains("type=\"LINE\"") {
            let missed = extract_xml_attr_val(trimmed, "missed");
            let covered = extract_xml_attr_val(trimmed, "covered");
            if let (Some(m), Some(c)) = (missed, covered) {
                let total = m + c;
                if total > 0.0 {
                    snapshot.line_coverage_pct = Some((c / total) * 100.0);
                }
            }
        }
        if trimmed.contains("<counter") && trimmed.contains("type=\"BRANCH\"") {
            let missed = extract_xml_attr_val(trimmed, "missed");
            let covered = extract_xml_attr_val(trimmed, "covered");
            if let (Some(m), Some(c)) = (missed, covered) {
                let total = m + c;
                if total > 0.0 {
                    snapshot.branch_coverage_pct = Some((c / total) * 100.0);
                }
            }
        }
    }

    snapshot
}

fn extract_number_before(line: &str, keyword: &str) -> Option<u32> {
    let idx = line.find(keyword)?;
    let before = line[..idx].trim();
    // Get the last "word" before the keyword, which should be the number
    // Handle both "X tests completed, Y failed" patterns
    let num_str = before.rsplit(|c: char| !c.is_ascii_digit()).next()?;
    if num_str.is_empty() {
        return None;
    }
    num_str.parse().ok()
}

type LintMatch = (String, Option<u32>, Option<u32>, String, Option<String>);

fn parse_detekt_line(line: &str) -> Option<LintMatch> {
    // Format: "path/File.kt:10:5: Description [RuleName]"
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 4 {
        return None;
    }
    let file = parts[0].to_string();
    if !file.ends_with(".kt") && !file.ends_with(".kts") {
        return None;
    }
    let line_num = parts[1].trim().parse::<u32>().ok();
    let col = parts[2].trim().parse::<u32>().ok();
    let rest = parts[3].trim();

    let (message, code) = if let Some(bracket_start) = rest.rfind('[') {
        let msg = rest[..bracket_start].trim().to_string();
        let code = rest[bracket_start + 1..]
            .strip_suffix(']')
            .map(|s| s.to_string());
        (msg, code)
    } else {
        (rest.to_string(), None)
    };

    Some((file, line_num, col, message, code))
}

fn parse_ktlint_line(line: &str) -> Option<LintMatch> {
    // Format: "path/File.kt:10:5: Description (rule-name)"
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 4 {
        return None;
    }
    let file = parts[0].to_string();
    let line_num = parts[1].trim().parse::<u32>().ok();
    let col = parts[2].trim().parse::<u32>().ok();
    let rest = parts[3].trim();

    let (message, code) = if let Some(paren_start) = rest.rfind('(') {
        let msg = rest[..paren_start].trim().to_string();
        let code = rest[paren_start + 1..]
            .strip_suffix(')')
            .map(|s| s.to_string());
        (msg, code)
    } else {
        (rest.to_string(), None)
    };

    Some((file, line_num, col, message, code))
}

fn extract_xml_attr_val(line: &str, attr: &str) -> Option<f64> {
    let needle = format!("{}=\"", attr);
    let idx = line.find(&needle)?;
    let after = &line[idx + needle.len()..];
    let val_str: String = after.chars().take_while(|c| *c != '"').collect();
    val_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gradle_test() {
        let stdout = "BUILD SUCCESSFUL\n10 tests completed, 2 failed, 1 skipped";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 7);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.skipped, 1);
    }

    #[test]
    fn test_parse_detekt() {
        let stdout = "src/Foo.kt:10:5: Variable should be named using camelCase [NamingConvention]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(
            snap.diagnostics[0].code.as_deref(),
            Some("NamingConvention")
        );
    }

    #[test]
    fn test_parse_ktlint() {
        let stdout =
            "src/Foo.kt:10:5: Missing newline after '{' (standard:no-blank-line-before-rbrace)";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_jacoco_coverage() {
        let stdout = r#"<counter type="LINE" missed="15" covered="85"/>"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_test_empty_input() {
        let snap = parse_test("", "");
        assert_eq!(snap.category, EvalCategory::Test);
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_no_completed_line_with_failures() {
        // Only FAILED lines, no "tests completed" summary
        let stdout = "com.example.FooTest > testBar FAILED\ncom.example.BazTest > testQux FAILED";
        let snap = parse_test(stdout, "");
        // No "tests completed" line, so counts stay at 0
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        // But the FAILED lines should be captured as diagnostics
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(snap.diagnostics[0].file, "gradle");
        assert!(snap.diagnostics[0]
            .message
            .contains("com.example.FooTest > testBar FAILED"));
        assert!(snap.diagnostics[1]
            .message
            .contains("com.example.BazTest > testQux FAILED"));
    }

    #[test]
    fn test_parse_test_build_failed_not_diagnostic() {
        // "BUILD FAILED" should NOT be added as a diagnostic
        let stdout = "BUILD FAILED in 5s\ncom.example.FooTest > testBar FAILED";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        // Only the test failure, not the BUILD FAILED
        assert!(snap.diagnostics[0]
            .message
            .contains("com.example.FooTest > testBar FAILED"));
        assert!(!snap
            .diagnostics
            .iter()
            .any(|d| d.message.contains("BUILD FAILED")));
    }

    #[test]
    fn test_parse_test_skipped_only() {
        let stdout = "5 tests completed, 0 failed, 5 skipped";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 5);
        assert_eq!(snap.errors, 0);
    }

    #[test]
    fn test_parse_test_stderr_content() {
        // Test that stderr is also parsed
        let stderr = "3 tests completed, 1 failed";
        let snap = parse_test("", stderr);
        assert_eq!(snap.passed, 2);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.errors, 1);
    }

    #[test]
    fn test_parse_test_multiple_failed_diagnostics() {
        let stdout = "com.example.ATest > test1 FAILED\n\
                       com.example.BTest > test2 FAILED\n\
                       com.example.CTest > test3 FAILED\n\
                       3 tests completed, 3 failed";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.failed, 3);
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.diagnostics.len(), 3);
        for diag in &snap.diagnostics {
            assert_eq!(diag.severity, DiagnosticSeverity::Error);
            assert_eq!(diag.file, "gradle");
            assert!(diag.code.is_none());
            assert!(diag.line.is_none());
            assert!(diag.column.is_none());
        }
    }

    #[test]
    fn test_parse_test_no_failed_keyword() {
        // "tests completed" without "failed" keyword
        let stdout = "10 tests completed";
        let snap = parse_test(stdout, "");
        // extract_number_before for "failed" returns None -> unwrap_or(0)
        assert_eq!(snap.passed, 10);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
    }

    // ---- parse_analysis edge cases ----

    #[test]
    fn test_parse_analysis_empty_input() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.category, EvalCategory::StaticAnalysis);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_multiple_detekt_findings() {
        let stdout = "src/Foo.kt:10:5: Variable naming [NamingConvention]\n\
                       src/Bar.kt:20:1: Too long function [MaxLineLength]\n\
                       src/Baz.kt:30:3: Empty block [EmptyFunctionBlock]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 3);
        assert_eq!(snap.diagnostics.len(), 3);
        assert_eq!(snap.diagnostics[0].file, "src/Foo.kt");
        assert_eq!(snap.diagnostics[0].line, Some(10));
        assert_eq!(snap.diagnostics[0].column, Some(5));
        assert_eq!(
            snap.diagnostics[0].code.as_deref(),
            Some("NamingConvention")
        );
        assert_eq!(snap.diagnostics[1].file, "src/Bar.kt");
        assert_eq!(snap.diagnostics[1].line, Some(20));
        assert_eq!(snap.diagnostics[1].code.as_deref(), Some("MaxLineLength"));
        assert_eq!(snap.diagnostics[2].file, "src/Baz.kt");
        assert_eq!(snap.diagnostics[2].line, Some(30));
        assert_eq!(
            snap.diagnostics[2].code.as_deref(),
            Some("EmptyFunctionBlock")
        );
    }

    #[test]
    fn test_parse_analysis_non_kotlin_file_ignored() {
        // Lines referencing non-.kt files should be ignored by parse_detekt_line
        let stdout = "src/Foo.java:10:5: Some issue [SomeRule]\n\
                       src/Bar.py:20:1: Another issue [AnotherRule]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_line_without_proper_format() {
        // Lines that don't have enough colon-separated parts
        let stdout = "Overall debt: 2h 30min\n\
                       - 15 weighted issues found\n\
                       Some random text";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_detekt_line_no_bracket_code() {
        // Detekt finding without [RuleName] bracket
        let stdout = "src/Foo.kt:10:5: Some description without rule code";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].code.is_none());
        assert_eq!(
            snap.diagnostics[0].message,
            "Some description without rule code"
        );
    }

    #[test]
    fn test_parse_analysis_kts_file() {
        let stdout = "build.gradle.kts:5:1: Unused import [UnusedImport]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics[0].file, "build.gradle.kts");
        assert_eq!(snap.diagnostics[0].code.as_deref(), Some("UnusedImport"));
    }

    #[test]
    fn test_parse_analysis_from_stderr() {
        let stderr = "src/Main.kt:1:1: Wildcard import [WildcardImport]";
        let snap = parse_analysis("", stderr);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics[0].file, "src/Main.kt");
    }

    // ---- parse_lint edge cases ----

    #[test]
    fn test_parse_lint_empty_input() {
        let snap = parse_lint("", "");
        assert_eq!(snap.category, EvalCategory::Lint);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_kts_extension() {
        let stdout = "build.gradle.kts:5:1: Missing newline (standard:final-newline)";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "build.gradle.kts");
        assert_eq!(snap.diagnostics[0].line, Some(5));
        assert_eq!(snap.diagnostics[0].column, Some(1));
        assert_eq!(
            snap.diagnostics[0].code.as_deref(),
            Some("standard:final-newline")
        );
    }

    #[test]
    fn test_parse_lint_multiple_issues() {
        let stdout = "src/Foo.kt:10:5: Missing newline (standard:no-blank-line-before-rbrace)\n\
                       src/Bar.kt:20:1: Unexpected indentation (standard:indent)\n\
                       src/Baz.kt:30:8: Unused import (standard:no-unused-imports)";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 3);
        assert_eq!(snap.diagnostics.len(), 3);
        assert_eq!(snap.diagnostics[0].file, "src/Foo.kt");
        assert_eq!(snap.diagnostics[1].file, "src/Bar.kt");
        assert_eq!(snap.diagnostics[2].file, "src/Baz.kt");
    }

    #[test]
    fn test_parse_lint_lines_without_kt_ignored() {
        // Lines not containing .kt: or .kts: should be skipped
        let stdout = "Summary: 5 errors found\n\
                       Running ktlint on 10 files\n\
                       src/Foo.java:10:5: Some issue (rule)";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_no_parenthesized_code() {
        // ktlint line without (rule-name) parenthesized code
        let stdout = "src/Foo.kt:10:5: Description without parenthesized rule";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.diagnostics[0].code.is_none());
        assert_eq!(
            snap.diagnostics[0].message,
            "Description without parenthesized rule"
        );
    }

    #[test]
    fn test_parse_lint_insufficient_parts() {
        // A line that contains .kt: but doesn't have 4 colon-separated parts
        let stdout = "src/Foo.kt:10";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    // ---- parse_coverage edge cases ----

    #[test]
    fn test_parse_coverage_empty_input() {
        let snap = parse_coverage("", "");
        assert_eq!(snap.category, EvalCategory::Coverage);
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_line_and_branch() {
        let stdout = r#"<counter type="LINE" missed="20" covered="80"/>
<counter type="BRANCH" missed="10" covered="40"/>"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 80.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_branch_only() {
        let stdout = r#"<counter type="BRANCH" missed="5" covered="45"/>"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!((snap.branch_coverage_pct.unwrap() - 90.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_zero_total_no_division_by_zero() {
        let stdout = r#"<counter type="LINE" missed="0" covered="0"/>"#;
        let snap = parse_coverage(stdout, "");
        // total is 0, so coverage should remain None (no division by zero)
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_missing_attributes() {
        // Missing "covered" attribute
        let stdout = r#"<counter type="LINE" missed="10"/>"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_missing_missed_attribute() {
        // Missing "missed" attribute
        let stdout = r#"<counter type="LINE" covered="80"/>"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_from_stderr() {
        let stderr = r#"<counter type="LINE" missed="30" covered="70"/>"#;
        let snap = parse_coverage("", stderr);
        assert!((snap.line_coverage_pct.unwrap() - 70.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_non_counter_lines_ignored() {
        let stdout = "Some random XML\n\
                       <report name=\"My Project\">\n\
                       <counter type=\"LINE\" missed=\"10\" covered=\"90\"/>\n\
                       </report>";
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 90.0).abs() < 0.01);
    }

    // ---- extract_number_before edge cases ----

    #[test]
    fn test_extract_number_before_keyword_not_found() {
        assert_eq!(
            extract_number_before("no such keyword here", "missing"),
            None
        );
    }

    #[test]
    fn test_extract_number_before_no_number_before_keyword() {
        // Keyword is present but no number before it
        assert_eq!(
            extract_number_before("tests completed", "tests completed"),
            None
        );
    }

    #[test]
    fn test_extract_number_before_basic() {
        assert_eq!(
            extract_number_before("10 tests completed, 2 failed", "tests completed"),
            Some(10)
        );
        assert_eq!(
            extract_number_before("10 tests completed, 2 failed", "failed"),
            Some(2)
        );
    }

    #[test]
    fn test_extract_number_before_with_comma_separated() {
        assert_eq!(
            extract_number_before("10 tests completed, 2 failed, 3 skipped", "skipped"),
            Some(3)
        );
    }

    #[test]
    fn test_extract_number_before_empty_string() {
        assert_eq!(extract_number_before("", "anything"), None);
    }

    #[test]
    fn test_extract_number_before_keyword_at_start() {
        // Keyword at the very start, nothing before it
        assert_eq!(extract_number_before("failed something", "failed"), None);
    }

    // ---- extract_xml_attr_val edge cases ----

    #[test]
    fn test_extract_xml_attr_val_basic() {
        assert_eq!(
            extract_xml_attr_val(
                r#"<counter type="LINE" missed="15" covered="85"/>"#,
                "missed"
            ),
            Some(15.0)
        );
        assert_eq!(
            extract_xml_attr_val(
                r#"<counter type="LINE" missed="15" covered="85"/>"#,
                "covered"
            ),
            Some(85.0)
        );
    }

    #[test]
    fn test_extract_xml_attr_val_not_found() {
        assert_eq!(
            extract_xml_attr_val(r#"<counter type="LINE"/>"#, "missed"),
            None
        );
    }

    #[test]
    fn test_extract_xml_attr_val_empty_value() {
        // Attribute with empty value should fail to parse as f64
        assert_eq!(
            extract_xml_attr_val(r#"<counter missed=""/>"#, "missed"),
            None
        );
    }

    #[test]
    fn test_extract_xml_attr_val_non_numeric() {
        assert_eq!(
            extract_xml_attr_val(r#"<counter missed="abc"/>"#, "missed"),
            None
        );
    }

    #[test]
    fn test_extract_xml_attr_val_floating_point() {
        assert_eq!(
            extract_xml_attr_val(r#"<counter missed="3.25"/>"#, "missed"),
            Some(3.25)
        );
    }

    #[test]
    fn test_extract_xml_attr_val_empty_line() {
        assert_eq!(extract_xml_attr_val("", "missed"), None);
    }
}
