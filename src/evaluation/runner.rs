//! Execute evaluation tools and compute deltas.

use super::detector::{self, DetectedTool, ToolOverrides};
use super::parsers;
use super::types::{EvalDelta, EvalSnapshot, EvaluationResult};
use crate::config::EvaluationConfig;
use crate::error::Result;
use std::path::Path;
use std::time::Instant;
use tokio::process::Command;

/// Maximum raw output to store per tool (10KB).
const MAX_RAW_OUTPUT: usize = 10 * 1024;

/// Main evaluator that orchestrates tool detection, execution, and delta computation.
pub struct CodeQualityEvaluator;

impl CodeQualityEvaluator {
    /// Run baseline evaluation (before fix).
    pub async fn run_baseline(
        project_dir: &Path,
        config: &EvaluationConfig,
    ) -> Result<Vec<EvalSnapshot>> {
        if !config.enabled {
            return Ok(Vec::new());
        }

        let overrides = ToolOverrides {
            custom_test_cmd: config.custom_test_cmd.clone(),
            custom_lint_cmd: config.custom_lint_cmd.clone(),
            custom_analysis_cmd: config.custom_analysis_cmd.clone(),
            custom_coverage_cmd: config.custom_coverage_cmd.clone(),
        };

        let tools = detector::detect_tools(project_dir, &overrides);
        let tools = filter_by_config(&tools, config);

        let mut snapshots = Vec::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(config.total_timeout_secs);

        for tool in &tools {
            if Instant::now() >= deadline {
                tracing::warn!("Evaluation total timeout reached, skipping remaining tools");
                break;
            }
            match run_tool(project_dir, tool, config.tool_timeout_secs).await {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(e) => {
                    tracing::warn!(tool = %tool.name, error = %e, "Evaluation tool failed");
                }
            }
        }

        Ok(snapshots)
    }

    /// Run after-fix evaluation and compute deltas.
    pub async fn run_after_and_compute_deltas(
        project_dir: &Path,
        config: &EvaluationConfig,
        before_snapshots: Vec<EvalSnapshot>,
        attempt_id: i64,
        repo: &str,
    ) -> Result<EvaluationResult> {
        if !config.enabled || before_snapshots.is_empty() {
            return Ok(EvaluationResult::new(
                attempt_id,
                repo.to_string(),
                Vec::new(),
            ));
        }

        let overrides = ToolOverrides {
            custom_test_cmd: config.custom_test_cmd.clone(),
            custom_lint_cmd: config.custom_lint_cmd.clone(),
            custom_analysis_cmd: config.custom_analysis_cmd.clone(),
            custom_coverage_cmd: config.custom_coverage_cmd.clone(),
        };

        let tools = detector::detect_tools(project_dir, &overrides);
        let tools = filter_by_config(&tools, config);

        let mut deltas = Vec::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(config.total_timeout_secs);

        for before in before_snapshots {
            // Find matching tool for after run
            let matching_tool = tools.iter().find(|t| t.name == before.tool_name);
            let Some(tool) = matching_tool else {
                continue;
            };

            if Instant::now() >= deadline {
                tracing::warn!("Evaluation total timeout reached");
                break;
            }

            match run_tool(project_dir, tool, config.tool_timeout_secs).await {
                Ok(after) => {
                    deltas.push(EvalDelta::compute(before, after));
                }
                Err(e) => {
                    tracing::warn!(tool = %tool.name, error = %e, "After-evaluation tool failed");
                }
            }
        }

        Ok(EvaluationResult::new(attempt_id, repo.to_string(), deltas))
    }
}

/// Run a single tool and parse its output into a snapshot.
async fn run_tool(
    project_dir: &Path,
    tool: &DetectedTool,
    timeout_secs: u64,
) -> Result<EvalSnapshot> {
    let start = Instant::now();

    let mut cmd = Command::new(&tool.command[0]);
    if tool.command.len() > 1 {
        cmd.args(&tool.command[1..]);
    }
    cmd.current_dir(project_dir);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output())
        .await
        .map_err(|_| {
            crate::error::Error::Other(format!(
                "Evaluation tool '{}' timed out after {}s",
                tool.name, timeout_secs
            ))
        })?
        .map_err(|e| {
            crate::error::Error::Other(format!(
                "Failed to run evaluation tool '{}': {}",
                tool.name, e
            ))
        })?;

    let duration = start.elapsed().as_secs_f64();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{}\n{}", stdout, stderr);
    let raw_output = truncate_output(&combined, MAX_RAW_OUTPUT);

    let mut snapshot = parsers::parse_output(tool, &stdout, &stderr);
    snapshot.exit_code = output.status.code().unwrap_or(-1);
    snapshot.duration_secs = duration;
    snapshot.raw_output = raw_output;
    snapshot.tool_name = tool.name.clone();
    snapshot.category = tool.category;

    Ok(snapshot)
}

fn filter_by_config<'a>(
    tools: &'a [DetectedTool],
    config: &EvaluationConfig,
) -> Vec<&'a DetectedTool> {
    tools
        .iter()
        .filter(|t| match t.category {
            super::types::EvalCategory::Test => config.test_delta,
            super::types::EvalCategory::Lint => config.lint_delta,
            super::types::EvalCategory::StaticAnalysis => config.static_analysis_delta,
            super::types::EvalCategory::Coverage => config.coverage_delta,
        })
        .collect()
}

fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i < max_bytes.saturating_sub(20))
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        format!("{}...[truncated]", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_output_short() {
        let s = "short output";
        assert_eq!(truncate_output(s, 100), s);
    }

    #[test]
    fn test_truncate_output_long() {
        let s = "a".repeat(20_000);
        let result = truncate_output(&s, 10240);
        assert!(result.len() <= 10260);
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_filter_by_config() {
        let tools = vec![
            DetectedTool {
                category: super::super::types::EvalCategory::Test,
                name: "test".into(),
                command: vec!["test".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Coverage,
                name: "cov".into(),
                command: vec!["cov".into()],
            },
        ];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: true,
            coverage_delta: false,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "test");
    }

    #[test]
    fn test_filter_by_config_all_enabled() {
        let tools = vec![
            DetectedTool {
                category: super::super::types::EvalCategory::Test,
                name: "test".into(),
                command: vec!["test".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Lint,
                name: "lint".into(),
                command: vec!["lint".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::StaticAnalysis,
                name: "analysis".into(),
                command: vec!["analysis".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Coverage,
                name: "cov".into(),
                command: vec!["cov".into()],
            },
        ];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: true,
            lint_delta: true,
            static_analysis_delta: true,
            coverage_delta: true,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert_eq!(filtered.len(), 4);
    }

    #[test]
    fn test_filter_by_config_all_disabled() {
        let tools = vec![DetectedTool {
            category: super::super::types::EvalCategory::Test,
            name: "test".into(),
            command: vec!["test".into()],
        }];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: false,
            lint_delta: false,
            static_analysis_delta: false,
            coverage_delta: false,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_by_config_empty_tools() {
        let tools: Vec<DetectedTool> = vec![];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: true,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_by_config_only_lint() {
        let tools = vec![
            DetectedTool {
                category: super::super::types::EvalCategory::Test,
                name: "test".into(),
                command: vec!["test".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Lint,
                name: "lint".into(),
                command: vec!["lint".into()],
            },
        ];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: false,
            lint_delta: true,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "lint");
    }

    #[test]
    fn test_filter_by_config_only_static_analysis() {
        let tools = vec![DetectedTool {
            category: super::super::types::EvalCategory::StaticAnalysis,
            name: "clippy".into(),
            command: vec!["clippy".into()],
        }];
        let config = EvaluationConfig {
            enabled: true,
            static_analysis_delta: true,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "clippy");
    }

    #[test]
    fn test_truncate_output_empty() {
        assert_eq!(truncate_output("", 100), "");
    }

    #[test]
    fn test_truncate_output_exact_limit() {
        let s = "a".repeat(100);
        assert_eq!(truncate_output(&s, 100), s);
    }

    #[test]
    fn test_truncate_output_one_over_limit() {
        let s = "a".repeat(101);
        let result = truncate_output(&s, 100);
        assert!(result.ends_with("...[truncated]"));
        assert!(result.len() < 101 + 14); // 14 = len("...[truncated]")
    }

    #[test]
    fn test_truncate_output_unicode_boundary() {
        // Multi-byte unicode characters
        let s = "\u{1f600}".repeat(50); // each emoji is 4 bytes
        let result = truncate_output(&s, 100);
        // Should not panic and should be valid UTF-8
        assert!(result.is_char_boundary(0));
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_truncate_output_very_small_max() {
        let s = "hello world this is a test string";
        let result = truncate_output(&s, 5);
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_truncate_output_max_raw_output_constant() {
        assert_eq!(MAX_RAW_OUTPUT, 10 * 1024);
    }

    #[tokio::test]
    async fn test_run_baseline_disabled() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = EvaluationConfig {
            enabled: false,
            ..Default::default()
        };
        let result = CodeQualityEvaluator::run_baseline(dir.path(), &config)
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_run_after_and_compute_deltas_disabled() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = EvaluationConfig {
            enabled: false,
            ..Default::default()
        };
        let result = CodeQualityEvaluator::run_after_and_compute_deltas(
            dir.path(),
            &config,
            vec![],
            1,
            "org/repo",
        )
        .await
        .unwrap();
        assert!(result.deltas.is_empty());
        assert!(!result.overall_improved);
    }

    #[tokio::test]
    async fn test_run_after_and_compute_deltas_empty_before() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = EvaluationConfig {
            enabled: true,
            ..Default::default()
        };
        let result = CodeQualityEvaluator::run_after_and_compute_deltas(
            dir.path(),
            &config,
            vec![],
            42,
            "org/repo",
        )
        .await
        .unwrap();
        assert!(result.deltas.is_empty());
        assert_eq!(result.attempt_id, 42);
        assert_eq!(result.repo, "org/repo");
    }

    #[test]
    fn test_truncate_output_zero_max() {
        let s = "hello";
        let result = truncate_output(s, 0);
        // max_bytes=0, saturating_sub(20)=0, so end=0
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_truncate_output_max_less_than_20() {
        let s = "a".repeat(50);
        let result = truncate_output(&s, 10);
        // max_bytes=10, saturating_sub(20)=0, so end=0
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_truncate_output_large_content() {
        let s = "abcdefghij".repeat(10000);
        let result = truncate_output(&s, MAX_RAW_OUTPUT);
        assert!(result.len() <= MAX_RAW_OUTPUT + 20);
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_truncate_output_exactly_at_limit_not_truncated() {
        let s = "a".repeat(MAX_RAW_OUTPUT);
        let result = truncate_output(&s, MAX_RAW_OUTPUT);
        assert_eq!(result.len(), MAX_RAW_OUTPUT);
        assert!(!result.contains("[truncated]"));
    }

    #[test]
    fn test_truncate_output_one_byte_over_limit() {
        let s = "a".repeat(MAX_RAW_OUTPUT + 1);
        let result = truncate_output(&s, MAX_RAW_OUTPUT);
        assert!(result.ends_with("...[truncated]"));
        assert!(result.len() < MAX_RAW_OUTPUT + 20);
    }

    #[test]
    fn test_filter_by_config_only_coverage() {
        let tools = vec![
            DetectedTool {
                category: super::super::types::EvalCategory::Test,
                name: "test".into(),
                command: vec!["test".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Coverage,
                name: "cov".into(),
                command: vec!["cov".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Lint,
                name: "lint".into(),
                command: vec!["lint".into()],
            },
        ];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: false,
            lint_delta: false,
            static_analysis_delta: false,
            coverage_delta: true,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "cov");
    }

    #[test]
    fn test_filter_by_config_test_and_lint_only() {
        let tools = vec![
            DetectedTool {
                category: super::super::types::EvalCategory::Test,
                name: "cargo test".into(),
                command: vec!["cargo".into(), "test".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Lint,
                name: "cargo fmt".into(),
                command: vec!["cargo".into(), "fmt".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::StaticAnalysis,
                name: "clippy".into(),
                command: vec!["cargo".into(), "clippy".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Coverage,
                name: "llvm-cov".into(),
                command: vec!["cargo".into(), "llvm-cov".into()],
            },
        ];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: true,
            lint_delta: true,
            static_analysis_delta: false,
            coverage_delta: false,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "cargo test");
        assert_eq!(filtered[1].name, "cargo fmt");
    }

    #[tokio::test]
    async fn test_run_baseline_disabled_with_snapshots() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = EvaluationConfig {
            enabled: false,
            test_delta: true,
            lint_delta: true,
            ..Default::default()
        };
        // Even with deltas enabled, if overall enabled=false, should return empty
        let result = CodeQualityEvaluator::run_baseline(dir.path(), &config)
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_run_after_disabled_with_before_snapshots() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = EvaluationConfig {
            enabled: false,
            ..Default::default()
        };
        // Pass non-empty before_snapshots, but config is disabled
        let before = vec![super::super::types::EvalSnapshot::default()];
        let result = CodeQualityEvaluator::run_after_and_compute_deltas(
            dir.path(),
            &config,
            before,
            1,
            "org/repo",
        )
        .await
        .unwrap();
        assert!(result.deltas.is_empty());
        assert!(!result.overall_improved);
    }

    #[tokio::test]
    async fn test_run_after_enabled_but_empty_before() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = EvaluationConfig {
            enabled: true,
            test_delta: true,
            ..Default::default()
        };
        // enabled=true but before_snapshots is empty -> early return
        let result = CodeQualityEvaluator::run_after_and_compute_deltas(
            dir.path(),
            &config,
            vec![],
            99,
            "my/repo",
        )
        .await
        .unwrap();
        assert!(result.deltas.is_empty());
        assert_eq!(result.attempt_id, 99);
        assert_eq!(result.repo, "my/repo");
        assert!(!result.overall_improved);
        assert_eq!(result.summary, "No evaluation tools ran.");
    }

    #[test]
    fn test_truncate_output_multibyte_chars_not_split() {
        // CJK characters are 3 bytes each
        let s = "\u{4f60}\u{597d}".repeat(1000); // 6000 bytes
        let result = truncate_output(&s, 100);
        // Should be valid UTF-8 - accessing chars should not panic
        let _ = result.chars().count();
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_truncate_output_emoji_chars_not_split() {
        // Each emoji is 4 bytes
        let s = "\u{1f600}".repeat(500); // 2000 bytes
        let result = truncate_output(&s, 50);
        let _ = result.chars().count();
        assert!(result.ends_with("...[truncated]"));
    }

    #[test]
    fn test_filter_by_config_duplicate_categories() {
        let tools = vec![
            DetectedTool {
                category: super::super::types::EvalCategory::Test,
                name: "cargo test".into(),
                command: vec!["cargo".into(), "test".into()],
            },
            DetectedTool {
                category: super::super::types::EvalCategory::Test,
                name: "npm test".into(),
                command: vec!["npm".into(), "test".into()],
            },
        ];
        let config = EvaluationConfig {
            enabled: true,
            test_delta: true,
            ..Default::default()
        };
        let filtered = filter_by_config(&tools, &config);
        // Both test tools should be included
        assert_eq!(filtered.len(), 2);
    }
}
