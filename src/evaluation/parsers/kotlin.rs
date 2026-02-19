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

fn parse_detekt_line(
    line: &str,
) -> Option<(String, Option<u32>, Option<u32>, String, Option<String>)> {
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

fn parse_ktlint_line(
    line: &str,
) -> Option<(String, Option<u32>, Option<u32>, String, Option<String>)> {
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
}
