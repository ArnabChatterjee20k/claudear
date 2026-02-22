//! .NET output parsers (dotnet test, dotnet build, dotnet format).

use crate::evaluation::types::{Diagnostic, DiagnosticSeverity, EvalCategory, EvalSnapshot};

/// Parse `dotnet test --logger trx` output.
///
/// dotnet test console output includes summary lines like:
/// "Passed!  - Failed:     0, Passed:    42, Skipped:     0, Total:    42"
/// "Failed!  - Failed:     2, Passed:    40, Skipped:     0, Total:    42"
pub fn parse_test(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Test,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // Parse the summary line
        if (trimmed.starts_with("Passed!") || trimmed.starts_with("Failed!"))
            && trimmed.contains("Total:")
        {
            snapshot.passed = extract_dotnet_count(trimmed, "Passed:").unwrap_or(0);
            snapshot.failed = extract_dotnet_count(trimmed, "Failed:").unwrap_or(0);
            snapshot.skipped = extract_dotnet_count(trimmed, "Skipped:").unwrap_or(0);
            snapshot.errors = snapshot.failed;
        }
        // Also capture individual test failure details
        if trimmed.contains("Failed ") && trimmed.contains("[") {
            snapshot.diagnostics.push(Diagnostic {
                file: "dotnet-test".to_string(),
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

/// Parse `dotnet build /warnaserror` output.
///
/// MSBuild outputs lines like:
/// "path/File.cs(10,5): error CS1002: ; expected [project.csproj]"
/// "path/File.cs(10,5): warning CS0168: The variable 'x' is declared but never used [project.csproj]"
pub fn parse_analysis(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::StaticAnalysis,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        if let Some(diag) = parse_msbuild_diagnostic(trimmed) {
            match diag.severity {
                DiagnosticSeverity::Error => snapshot.errors += 1,
                DiagnosticSeverity::Warning => snapshot.warnings += 1,
                _ => {}
            }
            snapshot.diagnostics.push(diag);
        }
    }

    snapshot
}

/// Parse `dotnet format --verify-no-changes` output.
///
/// dotnet format outputs file paths that need formatting.
pub fn parse_lint(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Lint,
        ..Default::default()
    };

    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // dotnet format outputs lines like "Formatted file - path/File.cs"
        // or warning-style output about files needing formatting
        if trimmed.starts_with("Formatted code file")
            || trimmed.contains(".cs") && trimmed.contains("would be formatted")
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

/// Parse `dotnet test --collect:"XPlat Code Coverage"` output.
///
/// Coverage data is written to XML files, but we try to extract summary from console output.
pub fn parse_coverage(stdout: &str, stderr: &str) -> EvalSnapshot {
    let mut snapshot = EvalSnapshot {
        category: EvalCategory::Coverage,
        ..Default::default()
    };

    // Try Cobertura XML format if present in stdout
    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let trimmed = line.trim();
        // Cobertura XML: <coverage line-rate="0.85" branch-rate="0.72" ...>
        if trimmed.contains("<coverage") && trimmed.contains("line-rate=") {
            if let Some(line_rate) = extract_xml_attr_val(trimmed, "line-rate") {
                snapshot.line_coverage_pct = Some(line_rate * 100.0);
            }
            if let Some(branch_rate) = extract_xml_attr_val(trimmed, "branch-rate") {
                snapshot.branch_coverage_pct = Some(branch_rate * 100.0);
            }
        }
    }

    snapshot
}

fn extract_dotnet_count(line: &str, prefix: &str) -> Option<u32> {
    let idx = line.find(prefix)?;
    let after = &line[idx + prefix.len()..];
    let num_str: String = after
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse().ok()
}

fn parse_msbuild_diagnostic(line: &str) -> Option<Diagnostic> {
    // Format: "path/File.cs(10,5): error CS1002: ; expected [project.csproj]"
    // or:     "path/File.cs(10,5): warning CS0168: description [project.csproj]"
    let (severity_str, severity) = if line.contains("): error ") {
        ("): error ", DiagnosticSeverity::Error)
    } else if line.contains("): warning ") {
        ("): warning ", DiagnosticSeverity::Warning)
    } else {
        return None;
    };

    let sev_idx = line.find(severity_str)?;
    let file_part = &line[..sev_idx];
    let rest = &line[sev_idx + severity_str.len()..];

    // Parse file and location: "path/File.cs(10,5)"
    let (file, line_num, col) = if let Some(paren_idx) = file_part.find('(') {
        let file = file_part[..paren_idx].to_string();
        let loc = &file_part[paren_idx + 1..];
        let loc = loc.strip_suffix(')').unwrap_or(loc);
        let parts: Vec<&str> = loc.split(',').collect();
        let line_num = parts.first().and_then(|s| s.trim().parse::<u32>().ok());
        let col = parts.get(1).and_then(|s| s.trim().parse::<u32>().ok());
        (file, line_num, col)
    } else {
        (file_part.to_string(), None, None)
    };

    // Parse code and message: "CS1002: ; expected [project.csproj]"
    let message_part = rest.split('[').next().unwrap_or(rest).trim();
    let (code, message) = if let Some(colon_idx) = message_part.find(": ") {
        let code = message_part[..colon_idx].trim().to_string();
        let msg = message_part[colon_idx + 2..].trim().to_string();
        (Some(code), msg)
    } else {
        (None, message_part.to_string())
    };

    Some(Diagnostic {
        file,
        line: line_num,
        column: col,
        severity,
        code,
        message,
    })
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
    fn test_parse_dotnet_test_passed() {
        let stdout = "Passed!  - Failed:     0, Passed:    42, Skipped:     3, Total:    45";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 42);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 3);
    }

    #[test]
    fn test_parse_dotnet_test_failed() {
        let stdout = "Failed!  - Failed:     2, Passed:    40, Skipped:     0, Total:    42";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 40);
        assert_eq!(snap.failed, 2);
    }

    #[test]
    fn test_parse_msbuild_error() {
        let stdout = "src/Foo.cs(10,5): error CS1002: ; expected [src/proj.csproj]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "src/Foo.cs");
        assert_eq!(snap.diagnostics[0].line, Some(10));
        assert_eq!(snap.diagnostics[0].code.as_deref(), Some("CS1002"));
    }

    #[test]
    fn test_parse_msbuild_warning() {
        let stdout =
            "src/Bar.cs(20,1): warning CS0168: The variable 'x' is declared but never used [proj.csproj]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn test_parse_cobertura_coverage() {
        let stdout = r#"<coverage line-rate="0.85" branch-rate="0.72" version="1.0">"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 85.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 72.0).abs() < 0.01);
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
    fn test_parse_test_individual_failure_diagnostic() {
        // Lines with "Failed " and "[" should produce diagnostics
        let stdout = "  Failed SomeNamespace.TestClass.MyTest [10 ms]";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(snap.diagnostics[0].file, "dotnet-test");
        assert!(snap.diagnostics[0].message.contains("Failed SomeNamespace"));
    }

    #[test]
    fn test_parse_test_multiple_individual_failures() {
        let stdout = "  Failed TestA.Foo [5 ms]\n  Failed TestB.Bar [12 ms]";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_test_failure_without_bracket_no_diagnostic() {
        // "Failed " present but no "[" -> no diagnostic
        let stdout = "Failed something without bracket";
        let snap = parse_test(stdout, "");
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_test_summary_in_stderr() {
        let snap = parse_test(
            "",
            "Failed!  - Failed:     3, Passed:    17, Skipped:     1, Total:    21",
        );
        assert_eq!(snap.passed, 17);
        assert_eq!(snap.failed, 3);
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.errors, 3);
    }

    #[test]
    fn test_parse_test_summary_line_not_passed_or_failed_ignored() {
        // A line that contains "Total:" but doesn't start with "Passed!" or "Failed!"
        let stdout = "Running - Failed:  0, Passed:  5, Skipped:  0, Total:  5";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 0);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 0);
    }

    #[test]
    fn test_parse_test_summary_and_individual_failures_combined() {
        let stdout = "  Failed TestA.Foo [5 ms]\n\
                       Failed!  - Failed:     1, Passed:     9, Skipped:     0, Total:    10";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 9);
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_test_passed_summary_with_whitespace() {
        let stdout = "  Passed!  - Failed:     0, Passed:   100, Skipped:     5, Total:   105  ";
        let snap = parse_test(stdout, "");
        assert_eq!(snap.passed, 100);
        assert_eq!(snap.failed, 0);
        assert_eq!(snap.skipped, 5);
        assert_eq!(snap.errors, 0);
    }

    // ---------------------------------------------------------------
    // parse_analysis (MSBuild): additional coverage
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_analysis_empty_input() {
        let snap = parse_analysis("", "");
        assert_eq!(snap.category, EvalCategory::StaticAnalysis);
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_multiple_errors_and_warnings() {
        let stdout = "src/A.cs(1,1): error CS0001: error one [p.csproj]\n\
                       src/B.cs(2,3): warning CS0002: warning one [p.csproj]\n\
                       src/C.cs(10,5): error CS0003: error two [p.csproj]\n\
                       src/D.cs(20,1): warning CS0004: warning two [p.csproj]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 2);
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 4);
    }

    #[test]
    fn test_parse_analysis_lines_without_markers_ignored() {
        let stdout = "Build started...\n\
                       Determining projects to restore...\n\
                       All projects are up-to-date for restore.\n\
                       Build succeeded.";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_file_without_parentheses() {
        // "): error " is required, so a line like "File.cs: error CS0001: desc [proj]"
        // does NOT match because it doesn't have "): error " (paren before colon)
        let stdout = "File.cs: error CS0001: desc [proj.csproj]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_analysis_mixed_errors_and_warnings_in_same_output() {
        let stdout = "src/Foo.cs(5,2): error CS1001: missing [proj.csproj]\n\
                       some unrelated log line\n\
                       src/Bar.cs(15,8): warning CS0168: unused [proj.csproj]";
        let snap = parse_analysis(stdout, "");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].file, "src/Foo.cs");
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(snap.diagnostics[1].file, "src/Bar.cs");
        assert_eq!(snap.diagnostics[1].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn test_parse_analysis_from_stderr() {
        let snap = parse_analysis("", "src/X.cs(1,1): error CS9999: bad [proj.csproj]");
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].file, "src/X.cs");
    }

    #[test]
    fn test_parse_analysis_diagnostic_fields() {
        let stdout = "path/to/File.cs(42,7): warning CS0168: The variable 'x' is declared but never used [project.csproj]";
        let snap = parse_analysis(stdout, "");
        let d = &snap.diagnostics[0];
        assert_eq!(d.file, "path/to/File.cs");
        assert_eq!(d.line, Some(42));
        assert_eq!(d.column, Some(7));
        assert_eq!(d.severity, DiagnosticSeverity::Warning);
        assert_eq!(d.code.as_deref(), Some("CS0168"));
        assert_eq!(d.message, "The variable 'x' is declared but never used");
    }

    // ---------------------------------------------------------------
    // parse_lint (dotnet format): additional coverage
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_lint_empty_input() {
        let snap = parse_lint("", "");
        assert_eq!(snap.category, EvalCategory::Lint);
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_formatted_code_file_lines() {
        let stdout = "Formatted code file path/to/File.cs\n\
                       Formatted code file path/to/Other.cs";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
        assert_eq!(snap.diagnostics[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(snap.diagnostics[0].message, "File needs formatting");
    }

    #[test]
    fn test_parse_lint_would_be_formatted_cs_lines() {
        let stdout = "  src/Foo.cs would be formatted\n\
                         src/Bar.cs would be formatted";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 2);
        assert_eq!(snap.diagnostics.len(), 2);
    }

    #[test]
    fn test_parse_lint_lines_without_cs_ignored() {
        // A "would be formatted" line without ".cs" should be ignored
        let stdout = "  src/Foo.txt would be formatted\n\
                       Some other log line\n\
                       Build succeeded.";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    #[test]
    fn test_parse_lint_multiple_formatting_issues() {
        let stdout = "Formatted code file src/A.cs\n\
                       Formatted code file src/B.cs\n\
                       Formatted code file src/C.cs\n\
                       src/D.cs would be formatted\n\
                       src/E.cs would be formatted";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 5);
        assert_eq!(snap.diagnostics.len(), 5);
    }

    #[test]
    fn test_parse_lint_from_stderr() {
        let snap = parse_lint("", "Formatted code file src/X.cs");
        assert_eq!(snap.warnings, 1);
        assert_eq!(snap.diagnostics.len(), 1);
    }

    #[test]
    fn test_parse_lint_cs_without_would_be_formatted_ignored() {
        // Contains ".cs" but not "would be formatted" and doesn't start with
        // "Formatted code file" -> should be ignored
        let stdout = "Restoring src/Program.cs references...";
        let snap = parse_lint(stdout, "");
        assert_eq!(snap.warnings, 0);
        assert!(snap.diagnostics.is_empty());
    }

    // ---------------------------------------------------------------
    // parse_coverage (Cobertura XML): additional coverage
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_coverage_empty_input() {
        let snap = parse_coverage("", "");
        assert_eq!(snap.category, EvalCategory::Coverage);
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_only_line_rate_no_branch_rate() {
        let stdout = r#"<coverage line-rate="0.95" version="1.0">"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 95.0).abs() < 0.01);
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_only_branch_rate_no_line_rate() {
        // The condition requires both "<coverage" and "line-rate=", so a line
        // with branch-rate but no line-rate won't match the outer condition.
        let stdout = r#"<coverage branch-rate="0.60" version="1.0">"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_line_without_coverage_tag_ignored() {
        let stdout = "line-rate=\"0.99\" branch-rate=\"0.80\"";
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!(snap.branch_coverage_pct.is_none());
    }

    #[test]
    fn test_parse_coverage_missing_line_rate_value() {
        // Has "<coverage" and "line-rate=" but malformed value
        let stdout = r#"<coverage line-rate="abc" branch-rate="0.50">"#;
        let snap = parse_coverage(stdout, "");
        assert!(snap.line_coverage_pct.is_none());
        assert!((snap.branch_coverage_pct.unwrap() - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_from_stderr() {
        let snap = parse_coverage(
            "",
            r#"<coverage line-rate="0.70" branch-rate="0.55" version="1.0">"#,
        );
        assert!((snap.line_coverage_pct.unwrap() - 70.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 55.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_zero_rates() {
        let stdout = r#"<coverage line-rate="0.0" branch-rate="0.0">"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 0.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_coverage_full_rates() {
        let stdout = r#"<coverage line-rate="1.0" branch-rate="1.0">"#;
        let snap = parse_coverage(stdout, "");
        assert!((snap.line_coverage_pct.unwrap() - 100.0).abs() < 0.01);
        assert!((snap.branch_coverage_pct.unwrap() - 100.0).abs() < 0.01);
    }

    // ---------------------------------------------------------------
    // extract_dotnet_count: helper function tests
    // ---------------------------------------------------------------

    #[test]
    fn test_extract_dotnet_count_normal() {
        let line = "Passed!  - Failed:     0, Passed:    42, Skipped:     3, Total:    45";
        assert_eq!(extract_dotnet_count(line, "Passed:"), Some(42));
        assert_eq!(extract_dotnet_count(line, "Failed:"), Some(0));
        assert_eq!(extract_dotnet_count(line, "Skipped:"), Some(3));
        assert_eq!(extract_dotnet_count(line, "Total:"), Some(45));
    }

    #[test]
    fn test_extract_dotnet_count_empty_line() {
        assert_eq!(extract_dotnet_count("", "Passed:"), None);
    }

    #[test]
    fn test_extract_dotnet_count_prefix_not_found() {
        assert_eq!(extract_dotnet_count("some text", "Passed:"), None);
    }

    #[test]
    fn test_extract_dotnet_count_no_number_after_prefix() {
        // "Passed:" followed by non-digit characters only
        assert_eq!(extract_dotnet_count("Passed: abc", "Passed:"), None);
    }

    #[test]
    fn test_extract_dotnet_count_multiple_numbers_takes_first() {
        // Should take the first contiguous run of digits after the prefix
        assert_eq!(
            extract_dotnet_count("Passed: 10 20 30", "Passed:"),
            Some(10)
        );
    }

    // ---------------------------------------------------------------
    // parse_msbuild_diagnostic: helper function tests
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_msbuild_diagnostic_error_full_format() {
        let line = "src/Foo.cs(10,5): error CS1002: ; expected [src/proj.csproj]";
        let diag = parse_msbuild_diagnostic(line).unwrap();
        assert_eq!(diag.file, "src/Foo.cs");
        assert_eq!(diag.line, Some(10));
        assert_eq!(diag.column, Some(5));
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code.as_deref(), Some("CS1002"));
        assert_eq!(diag.message, "; expected");
    }

    #[test]
    fn test_parse_msbuild_diagnostic_warning_full_format() {
        let line = "src/Bar.cs(20,1): warning CS0168: The variable 'x' is unused [proj.csproj]";
        let diag = parse_msbuild_diagnostic(line).unwrap();
        assert_eq!(diag.file, "src/Bar.cs");
        assert_eq!(diag.line, Some(20));
        assert_eq!(diag.column, Some(1));
        assert_eq!(diag.severity, DiagnosticSeverity::Warning);
        assert_eq!(diag.code.as_deref(), Some("CS0168"));
        assert_eq!(diag.message, "The variable 'x' is unused");
    }

    #[test]
    fn test_parse_msbuild_diagnostic_no_error_or_warning_returns_none() {
        assert!(parse_msbuild_diagnostic("Build succeeded.").is_none());
        assert!(parse_msbuild_diagnostic("").is_none());
        assert!(parse_msbuild_diagnostic("some random text").is_none());
    }

    #[test]
    fn test_parse_msbuild_diagnostic_no_parentheses_in_file_path() {
        // The file part has no parentheses so (file, None, None) branch is taken
        // This needs "): error " in the line, meaning the paren must come from
        // the format. Let's construct a line that takes the else branch:
        // No '(' before "): error " means file_part has no '('.
        // Actually for this branch to be reached, file_part = line[..sev_idx]
        // where sev_idx is position of "): error ". So the line must have
        // "): error " but the part before it has no '('.
        // Example: "some_file): error CS0001: desc [p.csproj]"
        // file_part = "some_file" (no parens) but wait, sev_idx points to "): error "
        // so file_part = "some_file" -- actually file_part = line[..sev_idx] = "some_file"
        let line = "some_file): error CS0001: desc [proj.csproj]";
        let diag = parse_msbuild_diagnostic(line).unwrap();
        assert_eq!(diag.file, "some_file");
        assert_eq!(diag.line, None);
        assert_eq!(diag.column, None);
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code.as_deref(), Some("CS0001"));
    }

    #[test]
    fn test_parse_msbuild_diagnostic_no_project_brackets() {
        // Message without [project.csproj] portion
        let line = "src/Foo.cs(1,1): error CS0001: some error";
        let diag = parse_msbuild_diagnostic(line).unwrap();
        assert_eq!(diag.code.as_deref(), Some("CS0001"));
        assert_eq!(diag.message, "some error");
    }

    #[test]
    fn test_parse_msbuild_diagnostic_message_without_code_colon() {
        // Message part that has no ": " separator -> code is None
        let line = "src/Foo.cs(1,1): error something [proj.csproj]";
        let diag = parse_msbuild_diagnostic(line).unwrap();
        assert!(diag.code.is_none());
        assert_eq!(diag.message, "something");
    }

    #[test]
    fn test_parse_msbuild_diagnostic_only_line_number_no_column() {
        // Parenthesized part with only one number (no comma)
        let line = "src/Foo.cs(42): error CS0001: desc [proj.csproj]";
        let diag = parse_msbuild_diagnostic(line).unwrap();
        assert_eq!(diag.file, "src/Foo.cs");
        assert_eq!(diag.line, Some(42));
        assert_eq!(diag.column, None);
    }

    // ---------------------------------------------------------------
    // extract_xml_attr_val: helper function tests
    // ---------------------------------------------------------------

    #[test]
    fn test_extract_xml_attr_val_normal() {
        let line = r#"<coverage line-rate="0.85" branch-rate="0.72">"#;
        assert!((extract_xml_attr_val(line, "line-rate").unwrap() - 0.85).abs() < 0.001);
        assert!((extract_xml_attr_val(line, "branch-rate").unwrap() - 0.72).abs() < 0.001);
    }

    #[test]
    fn test_extract_xml_attr_val_missing_attr() {
        let line = r#"<coverage line-rate="0.85">"#;
        assert!(extract_xml_attr_val(line, "branch-rate").is_none());
    }

    #[test]
    fn test_extract_xml_attr_val_empty_line() {
        assert!(extract_xml_attr_val("", "line-rate").is_none());
    }

    #[test]
    fn test_extract_xml_attr_val_non_numeric_value() {
        let line = r#"<coverage line-rate="abc">"#;
        assert!(extract_xml_attr_val(line, "line-rate").is_none());
    }

    #[test]
    fn test_extract_xml_attr_val_empty_value() {
        let line = r#"<coverage line-rate="">"#;
        assert!(extract_xml_attr_val(line, "line-rate").is_none());
    }

    #[test]
    fn test_extract_xml_attr_val_integer_value() {
        let line = r#"<coverage line-rate="1">"#;
        assert!((extract_xml_attr_val(line, "line-rate").unwrap() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_extract_xml_attr_val_zero() {
        let line = r#"<coverage line-rate="0">"#;
        assert!((extract_xml_attr_val(line, "line-rate").unwrap() - 0.0).abs() < 0.001);
    }
}
