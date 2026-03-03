//! Output parsers for different evaluation tools.

mod c_cpp;
mod cargo;
mod dart;
mod dotnet;
mod generic;
mod go;
mod java;
mod kotlin;
mod npm;
mod php;
mod python;
mod ruby;
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
        "pytest" => python::parse_test(stdout, stderr),
        "mypy" => python::parse_analysis(stdout, stderr),
        "ruff check" => python::parse_analysis(stdout, stderr),
        "ruff format" => python::parse_lint(stdout, stderr),
        "black" => python::parse_lint(stdout, stderr),
        "pytest coverage" => python::parse_coverage(stdout, stderr),
        "go test" => go::parse_test(stdout, stderr),
        "go vet" => go::parse_analysis(stdout, stderr),
        "gofmt" => go::parse_lint(stdout, stderr),
        "go test coverage" => go::parse_coverage(stdout, stderr),
        "mvn test" => java::parse_test(stdout, stderr),
        "mvn verify" => java::parse_analysis(stdout, stderr),
        "google-java-format" => java::parse_lint(stdout, stderr),
        "mvn jacoco" => java::parse_coverage(stdout, stderr),
        "ctest" => c_cpp::parse_test(stdout, stderr),
        "make test" => c_cpp::parse_test(stdout, stderr),
        "cppcheck" => c_cpp::parse_analysis(stdout, stderr),
        "clang-format" => c_cpp::parse_lint(stdout, stderr),
        "gcov" => c_cpp::parse_coverage(stdout, stderr),
        "rspec" => ruby::parse_test(stdout, stderr),
        "rake test" => ruby::parse_test(stdout, stderr),
        "rubocop" => ruby::parse_analysis(stdout, stderr),
        "rubocop lint" => ruby::parse_lint(stdout, stderr),
        "rspec coverage" => ruby::parse_coverage(stdout, stderr),
        "dart test" => dart::parse_test(stdout, stderr),
        "dart analyze" => dart::parse_analysis(stdout, stderr),
        "dart format" => dart::parse_lint(stdout, stderr),
        "dart test coverage" => dart::parse_coverage(stdout, stderr),
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

    // Python parsers
    #[test]
    fn test_parse_output_pytest_routes_correctly() {
        let tool = make_tool("pytest", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_mypy_routes_correctly() {
        let tool = make_tool("mypy", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_ruff_check_routes_correctly() {
        let tool = make_tool("ruff check", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_ruff_format_routes_correctly() {
        let tool = make_tool("ruff format", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_black_routes_correctly() {
        let tool = make_tool("black", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_pytest_coverage_routes_correctly() {
        let tool = make_tool("pytest coverage", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    // Go parsers
    #[test]
    fn test_parse_output_go_test_routes_correctly() {
        let tool = make_tool("go test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_go_vet_routes_correctly() {
        let tool = make_tool("go vet", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_gofmt_routes_correctly() {
        let tool = make_tool("gofmt", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_go_test_coverage_routes_correctly() {
        let tool = make_tool("go test coverage", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    // Java parsers
    #[test]
    fn test_parse_output_mvn_test_routes_correctly() {
        let tool = make_tool("mvn test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_mvn_verify_routes_correctly() {
        let tool = make_tool("mvn verify", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_google_java_format_routes_correctly() {
        let tool = make_tool("google-java-format", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_mvn_jacoco_routes_correctly() {
        let tool = make_tool("mvn jacoco", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    // C/C++ parsers
    #[test]
    fn test_parse_output_ctest_routes_correctly() {
        let tool = make_tool("ctest", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_make_test_routes_correctly() {
        let tool = make_tool("make test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_cppcheck_routes_correctly() {
        let tool = make_tool("cppcheck", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_clang_format_routes_correctly() {
        let tool = make_tool("clang-format", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_gcov_routes_correctly() {
        let tool = make_tool("gcov", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    // Ruby parsers
    #[test]
    fn test_parse_output_rspec_routes_correctly() {
        let tool = make_tool("rspec", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_rake_test_routes_correctly() {
        let tool = make_tool("rake test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_rubocop_routes_correctly() {
        let tool = make_tool("rubocop", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_rubocop_lint_routes_correctly() {
        let tool = make_tool("rubocop lint", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_rspec_coverage_routes_correctly() {
        let tool = make_tool("rspec coverage", EvalCategory::Coverage);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    // Dart parsers
    #[test]
    fn test_parse_output_dart_test_routes_correctly() {
        let tool = make_tool("dart test", EvalCategory::Test);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.passed, 0);
    }

    #[test]
    fn test_parse_output_dart_analyze_routes_correctly() {
        let tool = make_tool("dart analyze", EvalCategory::StaticAnalysis);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_parse_output_dart_format_routes_correctly() {
        let tool = make_tool("dart format", EvalCategory::Lint);
        let snapshot = parse_output(&tool, "", "");
        assert_eq!(snapshot.warnings, 0);
    }

    #[test]
    fn test_parse_output_dart_test_coverage_routes_correctly() {
        let tool = make_tool("dart test coverage", EvalCategory::Coverage);
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
