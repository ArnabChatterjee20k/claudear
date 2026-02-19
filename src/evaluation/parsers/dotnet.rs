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
}
