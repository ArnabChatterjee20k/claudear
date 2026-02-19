//! Auto-detect available evaluation tools for a project.

use super::types::EvalCategory;
use std::path::Path;

/// A detected evaluation tool with its command.
#[derive(Debug, Clone)]
pub struct DetectedTool {
    pub category: EvalCategory,
    pub name: String,
    pub command: Vec<String>,
}

/// Configuration overrides for evaluation tools.
#[derive(Debug, Clone, Default)]
pub struct ToolOverrides {
    pub custom_test_cmd: Option<String>,
    pub custom_lint_cmd: Option<String>,
    pub custom_analysis_cmd: Option<String>,
    pub custom_coverage_cmd: Option<String>,
}

/// Detect available evaluation tools for a project directory.
pub fn detect_tools(project_dir: &Path, overrides: &ToolOverrides) -> Vec<DetectedTool> {
    let mut tools = Vec::new();

    // Check for custom overrides first
    if let Some(ref cmd) = overrides.custom_test_cmd {
        tools.push(DetectedTool {
            category: EvalCategory::Test,
            name: "custom".into(),
            command: shell_words(cmd),
        });
    }
    if let Some(ref cmd) = overrides.custom_lint_cmd {
        tools.push(DetectedTool {
            category: EvalCategory::Lint,
            name: "custom".into(),
            command: shell_words(cmd),
        });
    }
    if let Some(ref cmd) = overrides.custom_analysis_cmd {
        tools.push(DetectedTool {
            category: EvalCategory::StaticAnalysis,
            name: "custom".into(),
            command: shell_words(cmd),
        });
    }
    if let Some(ref cmd) = overrides.custom_coverage_cmd {
        tools.push(DetectedTool {
            category: EvalCategory::Coverage,
            name: "custom".into(),
            command: shell_words(cmd),
        });
    }

    // Only auto-detect categories not already covered by overrides
    let has_test = tools.iter().any(|t| t.category == EvalCategory::Test);
    let has_lint = tools.iter().any(|t| t.category == EvalCategory::Lint);
    let has_analysis = tools
        .iter()
        .any(|t| t.category == EvalCategory::StaticAnalysis);
    let has_coverage = tools.iter().any(|t| t.category == EvalCategory::Coverage);

    // Rust (Cargo.toml)
    if project_dir.join("Cargo.toml").exists() {
        if !has_test {
            tools.push(DetectedTool {
                category: EvalCategory::Test,
                name: "cargo test".into(),
                command: vec![
                    "cargo".into(),
                    "test".into(),
                    "--".into(),
                    "--format".into(),
                    "json".into(),
                ],
            });
        }
        if !has_analysis && which_exists("cargo") {
            tools.push(DetectedTool {
                category: EvalCategory::StaticAnalysis,
                name: "cargo clippy".into(),
                command: vec![
                    "cargo".into(),
                    "clippy".into(),
                    "--message-format=json".into(),
                    "--".into(),
                    "-D".into(),
                    "warnings".into(),
                ],
            });
        }
        if !has_lint && which_exists("cargo") {
            tools.push(DetectedTool {
                category: EvalCategory::Lint,
                name: "cargo fmt".into(),
                command: vec!["cargo".into(), "fmt".into(), "--check".into()],
            });
        }
        if !has_coverage && which_exists("cargo") && which_exists("cargo-llvm-cov") {
            tools.push(DetectedTool {
                category: EvalCategory::Coverage,
                name: "cargo llvm-cov".into(),
                command: vec!["cargo".into(), "llvm-cov".into(), "--json".into()],
            });
        }
    }

    // PHP (composer.json)
    if project_dir.join("composer.json").exists() {
        let vendor = project_dir.join("vendor/bin");
        if !has_test && vendor.join("phpunit").exists() {
            tools.push(DetectedTool {
                category: EvalCategory::Test,
                name: "phpunit".into(),
                command: vec![
                    "vendor/bin/phpunit".into(),
                    "--log-junit".into(),
                    "-".into(),
                ],
            });
        }
        if !has_analysis && vendor.join("phpstan").exists() {
            tools.push(DetectedTool {
                category: EvalCategory::StaticAnalysis,
                name: "phpstan".into(),
                command: vec![
                    "vendor/bin/phpstan".into(),
                    "analyse".into(),
                    "--error-format=json".into(),
                ],
            });
        }
        if !has_lint && vendor.join("php-cs-fixer").exists() {
            tools.push(DetectedTool {
                category: EvalCategory::Lint,
                name: "php-cs-fixer".into(),
                command: vec![
                    "vendor/bin/php-cs-fixer".into(),
                    "fix".into(),
                    "--dry-run".into(),
                    "--format=json".into(),
                ],
            });
        }
        if !has_coverage && vendor.join("phpunit").exists() {
            tools.push(DetectedTool {
                category: EvalCategory::Coverage,
                name: "phpunit coverage".into(),
                command: vec![
                    "vendor/bin/phpunit".into(),
                    "--coverage-clover".into(),
                    "php://stdout".into(),
                ],
            });
        }
    }

    // JS/TS (package.json)
    if project_dir.join("package.json").exists() {
        if !has_test && (which_exists("npx") || which_exists("jest")) {
            tools.push(DetectedTool {
                category: EvalCategory::Test,
                name: "jest".into(),
                command: vec!["npx".into(), "jest".into(), "--json".into()],
            });
        }
        if !has_analysis && which_exists("npx") {
            tools.push(DetectedTool {
                category: EvalCategory::StaticAnalysis,
                name: "eslint".into(),
                command: vec![
                    "npx".into(),
                    "eslint".into(),
                    "--format".into(),
                    "json".into(),
                    ".".into(),
                ],
            });
        }
        if !has_lint && which_exists("npx") {
            tools.push(DetectedTool {
                category: EvalCategory::Lint,
                name: "prettier".into(),
                command: vec![
                    "npx".into(),
                    "prettier".into(),
                    "--check".into(),
                    ".".into(),
                ],
            });
        }
        if !has_coverage && which_exists("npx") {
            tools.push(DetectedTool {
                category: EvalCategory::Coverage,
                name: "jest coverage".into(),
                command: vec![
                    "npx".into(),
                    "jest".into(),
                    "--coverage".into(),
                    "--coverageReporters=json-summary".into(),
                ],
            });
        }
    }

    // Kotlin (build.gradle or build.gradle.kts)
    if project_dir.join("build.gradle.kts").exists() || project_dir.join("build.gradle").exists() {
        let gradlew = if project_dir.join("gradlew").exists() {
            "./gradlew"
        } else {
            "gradle"
        };
        if !has_test {
            tools.push(DetectedTool {
                category: EvalCategory::Test,
                name: "gradle test".into(),
                command: vec![gradlew.into(), "test".into()],
            });
        }
        if !has_analysis && which_exists(gradlew) {
            tools.push(DetectedTool {
                category: EvalCategory::StaticAnalysis,
                name: "detekt".into(),
                command: vec![gradlew.into(), "detekt".into()],
            });
        }
        if !has_lint && which_exists(gradlew) {
            tools.push(DetectedTool {
                category: EvalCategory::Lint,
                name: "ktlint".into(),
                command: vec![gradlew.into(), "ktlintCheck".into()],
            });
        }
        if !has_coverage && which_exists(gradlew) {
            tools.push(DetectedTool {
                category: EvalCategory::Coverage,
                name: "jacoco".into(),
                command: vec![gradlew.into(), "jacocoTestReport".into()],
            });
        }
    }

    // Swift (Package.swift)
    if project_dir.join("Package.swift").exists() {
        if !has_test && which_exists("swift") {
            tools.push(DetectedTool {
                category: EvalCategory::Test,
                name: "swift test".into(),
                command: vec!["swift".into(), "test".into()],
            });
        }
        if !has_analysis && which_exists("swiftlint") {
            tools.push(DetectedTool {
                category: EvalCategory::StaticAnalysis,
                name: "swiftlint".into(),
                command: vec![
                    "swiftlint".into(),
                    "lint".into(),
                    "--reporter".into(),
                    "json".into(),
                ],
            });
        }
        if !has_lint && which_exists("swift-format") {
            tools.push(DetectedTool {
                category: EvalCategory::Lint,
                name: "swift-format".into(),
                command: vec![
                    "swift-format".into(),
                    "lint".into(),
                    "--recursive".into(),
                    ".".into(),
                ],
            });
        }
        if !has_coverage && which_exists("swift") {
            tools.push(DetectedTool {
                category: EvalCategory::Coverage,
                name: "swift coverage".into(),
                command: vec![
                    "swift".into(),
                    "test".into(),
                    "--enable-code-coverage".into(),
                ],
            });
        }
    }

    // C# (.csproj or .sln)
    let has_csproj = project_dir.read_dir().is_ok_and(|mut entries| {
        entries.any(|e| {
            e.ok().is_some_and(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.ends_with(".csproj") || name.ends_with(".sln")
            })
        })
    });
    if has_csproj && which_exists("dotnet") {
        if !has_test {
            tools.push(DetectedTool {
                category: EvalCategory::Test,
                name: "dotnet test".into(),
                command: vec![
                    "dotnet".into(),
                    "test".into(),
                    "--logger".into(),
                    "trx".into(),
                ],
            });
        }
        if !has_analysis {
            tools.push(DetectedTool {
                category: EvalCategory::StaticAnalysis,
                name: "dotnet build".into(),
                command: vec!["dotnet".into(), "build".into(), "/warnaserror".into()],
            });
        }
        if !has_lint {
            tools.push(DetectedTool {
                category: EvalCategory::Lint,
                name: "dotnet format".into(),
                command: vec![
                    "dotnet".into(),
                    "format".into(),
                    "--verify-no-changes".into(),
                ],
            });
        }
        if !has_coverage {
            tools.push(DetectedTool {
                category: EvalCategory::Coverage,
                name: "dotnet coverage".into(),
                command: vec![
                    "dotnet".into(),
                    "test".into(),
                    "--collect:XPlat Code Coverage".into(),
                ],
            });
        }
    }

    tools
}

fn which_exists(binary: &str) -> bool {
    std::process::Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn shell_words(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_detect_rust_project() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let tools = detect_tools(dir.path(), &ToolOverrides::default());
        // Should detect at least cargo test
        assert!(tools.iter().any(|t| t.name.contains("cargo")));
    }

    #[test]
    fn test_custom_overrides() {
        let dir = TempDir::new().unwrap();
        let overrides = ToolOverrides {
            custom_test_cmd: Some("make test".into()),
            ..Default::default()
        };
        let tools = detect_tools(dir.path(), &overrides);
        assert!(tools
            .iter()
            .any(|t| t.name == "custom" && t.category == EvalCategory::Test));
    }

    #[test]
    fn test_empty_dir_no_tools() {
        let dir = TempDir::new().unwrap();
        let tools = detect_tools(dir.path(), &ToolOverrides::default());
        assert!(tools.is_empty());
    }

    #[test]
    fn test_shell_words() {
        assert_eq!(
            shell_words("cargo test --json"),
            vec!["cargo", "test", "--json"]
        );
    }

    // ─── shell_words edge cases ──────────────────────────────────

    #[test]
    fn test_shell_words_single_word() {
        assert_eq!(shell_words("cargo"), vec!["cargo"]);
    }

    #[test]
    fn test_shell_words_empty_string() {
        let result: Vec<String> = shell_words("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_shell_words_extra_whitespace() {
        assert_eq!(
            shell_words("  cargo   test   --json  "),
            vec!["cargo", "test", "--json"]
        );
    }

    #[test]
    fn test_shell_words_tabs_and_spaces() {
        assert_eq!(
            shell_words("cargo\ttest\t--json"),
            vec!["cargo", "test", "--json"]
        );
    }

    // ─── Custom override tests ──────────────────────────────────

    #[test]
    fn test_custom_lint_override() {
        let dir = TempDir::new().unwrap();
        let overrides = ToolOverrides {
            custom_lint_cmd: Some("eslint --fix .".into()),
            ..Default::default()
        };
        let tools = detect_tools(dir.path(), &overrides);
        assert!(tools
            .iter()
            .any(|t| t.name == "custom" && t.category == EvalCategory::Lint));
        let lint_tool = tools
            .iter()
            .find(|t| t.category == EvalCategory::Lint)
            .unwrap();
        assert_eq!(lint_tool.command, vec!["eslint", "--fix", "."]);
    }

    #[test]
    fn test_custom_analysis_override() {
        let dir = TempDir::new().unwrap();
        let overrides = ToolOverrides {
            custom_analysis_cmd: Some("mypy --strict .".into()),
            ..Default::default()
        };
        let tools = detect_tools(dir.path(), &overrides);
        assert!(tools
            .iter()
            .any(|t| t.name == "custom" && t.category == EvalCategory::StaticAnalysis));
    }

    #[test]
    fn test_custom_coverage_override() {
        let dir = TempDir::new().unwrap();
        let overrides = ToolOverrides {
            custom_coverage_cmd: Some("coverage run".into()),
            ..Default::default()
        };
        let tools = detect_tools(dir.path(), &overrides);
        assert!(tools
            .iter()
            .any(|t| t.name == "custom" && t.category == EvalCategory::Coverage));
    }

    #[test]
    fn test_all_custom_overrides() {
        let dir = TempDir::new().unwrap();
        let overrides = ToolOverrides {
            custom_test_cmd: Some("make test".into()),
            custom_lint_cmd: Some("make lint".into()),
            custom_analysis_cmd: Some("make analyze".into()),
            custom_coverage_cmd: Some("make coverage".into()),
        };
        let tools = detect_tools(dir.path(), &overrides);
        assert_eq!(tools.len(), 4);
        assert!(tools.iter().all(|t| t.name == "custom"));
        assert!(tools.iter().any(|t| t.category == EvalCategory::Test));
        assert!(tools.iter().any(|t| t.category == EvalCategory::Lint));
        assert!(tools
            .iter()
            .any(|t| t.category == EvalCategory::StaticAnalysis));
        assert!(tools.iter().any(|t| t.category == EvalCategory::Coverage));
    }

    // ─── Override prevents auto-detection ──────────────────────────

    #[test]
    fn test_custom_test_override_prevents_rust_autodetect() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let overrides = ToolOverrides {
            custom_test_cmd: Some("my-test-runner".into()),
            ..Default::default()
        };
        let tools = detect_tools(dir.path(), &overrides);
        // Should have the custom test tool, not cargo test
        let test_tools: Vec<_> = tools
            .iter()
            .filter(|t| t.category == EvalCategory::Test)
            .collect();
        assert_eq!(test_tools.len(), 1);
        assert_eq!(test_tools[0].name, "custom");
    }

    // ─── DetectedTool tests ──────────────────────────────────

    #[test]
    fn test_detected_tool_clone() {
        let tool = DetectedTool {
            category: EvalCategory::Test,
            name: "cargo test".into(),
            command: vec!["cargo".into(), "test".into()],
        };
        let cloned = tool.clone();
        assert_eq!(cloned.name, "cargo test");
        assert_eq!(cloned.category, EvalCategory::Test);
        assert_eq!(cloned.command, vec!["cargo", "test"]);
    }

    #[test]
    fn test_detected_tool_debug() {
        let tool = DetectedTool {
            category: EvalCategory::Lint,
            name: "eslint".into(),
            command: vec!["npx".into(), "eslint".into()],
        };
        let dbg = format!("{:?}", tool);
        assert!(dbg.contains("Lint"));
        assert!(dbg.contains("eslint"));
    }

    // ─── ToolOverrides tests ──────────────────────────────────

    #[test]
    fn test_tool_overrides_default() {
        let overrides = ToolOverrides::default();
        assert!(overrides.custom_test_cmd.is_none());
        assert!(overrides.custom_lint_cmd.is_none());
        assert!(overrides.custom_analysis_cmd.is_none());
        assert!(overrides.custom_coverage_cmd.is_none());
    }

    #[test]
    fn test_tool_overrides_clone() {
        let overrides = ToolOverrides {
            custom_test_cmd: Some("make test".into()),
            custom_lint_cmd: None,
            custom_analysis_cmd: Some("mypy .".into()),
            custom_coverage_cmd: None,
        };
        let cloned = overrides.clone();
        assert_eq!(cloned.custom_test_cmd, Some("make test".into()));
        assert!(cloned.custom_lint_cmd.is_none());
        assert_eq!(cloned.custom_analysis_cmd, Some("mypy .".into()));
    }

    #[test]
    fn test_tool_overrides_debug() {
        let overrides = ToolOverrides::default();
        let dbg = format!("{:?}", overrides);
        assert!(dbg.contains("ToolOverrides"));
    }

    // ─── JS/TS project detection ──────────────────────────────────

    #[test]
    fn test_detect_js_project() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name": "test", "version": "1.0.0"}"#,
        )
        .unwrap();
        let tools = detect_tools(dir.path(), &ToolOverrides::default());
        // Should detect at least some JS tools (jest, eslint, prettier)
        // depending on whether npx is available
        // At minimum, the detection logic should not panic
        for tool in &tools {
            assert!(
                tool.category == EvalCategory::Test
                    || tool.category == EvalCategory::Lint
                    || tool.category == EvalCategory::StaticAnalysis
                    || tool.category == EvalCategory::Coverage
            );
        }
    }

    // ─── Rust project detection details ──────────────────────────

    #[test]
    fn test_detect_rust_project_includes_clippy_and_fmt() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let tools = detect_tools(dir.path(), &ToolOverrides::default());

        // Should have cargo test at minimum
        assert!(tools.iter().any(|t| t.name == "cargo test"));

        // Check that cargo test command includes json format flag
        let test_tool = tools.iter().find(|t| t.name == "cargo test").unwrap();
        assert!(test_tool.command.contains(&"--format".to_string()));
        assert!(test_tool.command.contains(&"json".to_string()));
    }

    // ─── Multiple project files ──────────────────────────────────

    #[test]
    fn test_detect_mixed_rust_and_js_project() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name": "test"}"#).unwrap();
        let tools = detect_tools(dir.path(), &ToolOverrides::default());
        // Should detect tools for both Rust and JS
        let has_cargo = tools.iter().any(|t| t.name.contains("cargo"));
        assert!(has_cargo, "Should detect Rust tools");
    }

    // ─── Nonexistent directory ──────────────────────────────────

    #[test]
    fn test_detect_nonexistent_directory() {
        let tools = detect_tools(
            Path::new("/tmp/nonexistent_dir_for_claudear_test_12345"),
            &ToolOverrides::default(),
        );
        // Should not panic and return empty
        assert!(tools.is_empty());
    }

    // ─── which_exists tests ──────────────────────────────────

    #[test]
    fn test_which_exists_for_known_binary() {
        // "ls" should exist on all Unix systems
        assert!(which_exists("ls"));
    }

    #[test]
    fn test_which_exists_for_unknown_binary() {
        assert!(!which_exists(
            "nonexistent_binary_that_does_not_exist_12345"
        ));
    }
}
