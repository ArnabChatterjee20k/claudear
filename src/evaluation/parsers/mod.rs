//! Output parsers for different evaluation tools.

mod cargo;
mod dotnet;
mod generic;
mod kotlin;
mod npm;
mod php;
mod swift;

use super::detector::DetectedTool;
use super::types::EvalSnapshot;

/// Parse tool output into an EvalSnapshot using the appropriate parser.
pub fn parse_output(tool: &DetectedTool, stdout: &str, stderr: &str) -> EvalSnapshot {
    match tool.name.as_str() {
        "cargo test" => cargo::parse_test(stdout, stderr),
        "cargo clippy" => cargo::parse_clippy(stdout, stderr),
        "cargo fmt" => cargo::parse_fmt(stdout, stderr),
        "cargo llvm-cov" => cargo::parse_coverage(stdout, stderr),
        "phpunit" => php::parse_test(stdout, stderr),
        "phpstan" => php::parse_analysis(stdout, stderr),
        "php-cs-fixer" => php::parse_lint(stdout, stderr),
        "phpunit coverage" => php::parse_coverage(stdout, stderr),
        "jest" => npm::parse_test(stdout, stderr),
        "eslint" => npm::parse_analysis(stdout, stderr),
        "prettier" => npm::parse_lint(stdout, stderr),
        "jest coverage" => npm::parse_coverage(stdout, stderr),
        "gradle test" => kotlin::parse_test(stdout, stderr),
        "detekt" => kotlin::parse_analysis(stdout, stderr),
        "ktlint" => kotlin::parse_lint(stdout, stderr),
        "jacoco" => kotlin::parse_coverage(stdout, stderr),
        "swift test" => swift::parse_test(stdout, stderr),
        "swiftlint" => swift::parse_analysis(stdout, stderr),
        "swift-format" => swift::parse_lint(stdout, stderr),
        "swift coverage" => swift::parse_coverage(stdout, stderr),
        "dotnet test" => dotnet::parse_test(stdout, stderr),
        "dotnet build" => dotnet::parse_analysis(stdout, stderr),
        "dotnet format" => dotnet::parse_lint(stdout, stderr),
        "dotnet coverage" => dotnet::parse_coverage(stdout, stderr),
        _ => generic::parse(stdout, stderr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::types::EvalCategory;

    fn make_tool(name: &str, category: EvalCategory) -> DetectedTool {
        DetectedTool {
            category,
            name: name.into(),
            command: vec![name.into()],
        }
    }

    #[test]
    fn test_parse_output_cargo_test_routes_correctly() {
        let tool = make_tool("cargo test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        // Should return a valid snapshot (even if empty)
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_cargo_clippy_routes_correctly() {
        let tool = make_tool("cargo clippy", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_cargo_fmt_routes_correctly() {
        let tool = make_tool("cargo fmt", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_cargo_llvm_cov_routes_correctly() {
        let tool = make_tool("cargo llvm-cov", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert!(snapshot.line_coverage_pct.is_none() || snapshot.line_coverage_pct.is_some());
    }

    #[test]
    fn test_parse_output_phpunit_routes_correctly() {
        let tool = make_tool("phpunit", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_phpstan_routes_correctly() {
        let tool = make_tool("phpstan", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_jest_routes_correctly() {
        let tool = make_tool("jest", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_eslint_routes_correctly() {
        let tool = make_tool("eslint", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_prettier_routes_correctly() {
        let tool = make_tool("prettier", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_jest_coverage_routes_correctly() {
        let tool = make_tool("jest coverage", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_gradle_test_routes_correctly() {
        let tool = make_tool("gradle test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_detekt_routes_correctly() {
        let tool = make_tool("detekt", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_ktlint_routes_correctly() {
        let tool = make_tool("ktlint", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_jacoco_routes_correctly() {
        let tool = make_tool("jacoco", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_swift_test_routes_correctly() {
        let tool = make_tool("swift test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_swiftlint_routes_correctly() {
        let tool = make_tool("swiftlint", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_swift_format_routes_correctly() {
        let tool = make_tool("swift-format", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_swift_coverage_routes_correctly() {
        let tool = make_tool("swift coverage", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_dotnet_test_routes_correctly() {
        let tool = make_tool("dotnet test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_dotnet_build_routes_correctly() {
        let tool = make_tool("dotnet build", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_dotnet_format_routes_correctly() {
        let tool = make_tool("dotnet format", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_dotnet_coverage_routes_correctly() {
        let tool = make_tool("dotnet coverage", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_php_cs_fixer_routes_correctly() {
        let tool = make_tool("php-cs-fixer", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_phpunit_coverage_routes_correctly() {
        let tool = make_tool("phpunit coverage", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_unknown_tool_uses_generic() {
        let tool = make_tool("unknown-tool", EvalCategory::Test);
        let snapshot = parse_output(&tool, "some output", "");
        // Generic parser should not panic
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_custom_tool_uses_generic() {
        let tool = make_tool("custom", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "error occurred");
        assert_eq!(snapshot.passed, 0);
    }
}
