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
