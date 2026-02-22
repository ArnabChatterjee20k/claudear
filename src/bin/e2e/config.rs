//! Config generation for E2E daemon instances.
//!
//! Builds a typed `Config` struct and serializes to TOML for the daemon process.

use anyhow::{Context, Result};
use claudear::config::Config;
use std::path::{Path, PathBuf};

/// Generate a TOML config for a scenario daemon.
pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    pub fn new(work_dir: &Path, db_path: &Path, port: u16) -> Self {
        let mut config = Config::default();
        config.work_dir = work_dir.to_path_buf();
        config.db_path = db_path.to_path_buf();
        config.webhook_port = port;
        config.poll_interval_ms = 5_000; // Fast polling for E2E
        config.max_issues_per_cycle = 1;
        config.max_concurrent = 1;
        config.processing_delay_ms = 0;
        // Enable learning features (matches bash generate_config)
        config.learning = claudear::config::LearningConfig {
            auto_extract_learnings: true,
            diff_analysis: true,
            qa_promotion: true,
            repo_knowledge: true,
            review_classification: true,
            strategy_fingerprinting: true,
            quality_scoring: true,
            cluster_detection: true,
            cross_repo_correlation: true,
            ..Default::default()
        };
        config.prioritisation = Default::default();
        config.code_index = claudear::CodeIndexConfig {
            enabled: false,
            ..Default::default()
        };
        config.evaluation = claudear::EvaluationConfig {
            enabled: false,
            ..Default::default()
        };
        Self { config }
    }

    /// Override paths for Docker container (db, work_dir, repos, bind address).
    pub fn docker_paths(mut self) -> Self {
        self.config.db_path = std::path::PathBuf::from("/app/data/claudear.db");
        self.config.work_dir = std::path::PathBuf::from("/app/project");
        self.config.bind_address = "0.0.0.0".to_string();
        self
    }

    pub fn claude_timeout(mut self, secs: u64) -> Self {
        self.config.claude_timeout_secs = secs;
        self
    }

    pub fn auto_discover_paths(mut self, paths: Vec<String>) -> Self {
        self.config.auto_discover_paths = paths;
        self
    }

    pub fn skip_permissions(mut self) -> Self {
        self.config.claude.skip_permissions = true;
        self
    }

    pub fn github(mut self, token: &str, repo: &str) -> Self {
        self.config.github.token = Some(token.to_string());
        self.config.github.auto_resolve_on_merge = false;
        self.config.github.review_trigger = "/claudear".to_string();
        self.config.known_orgs = vec![repo.split('/').next().unwrap_or("unknown").to_string()];
        self
    }

    pub fn gitlab(mut self, token: &str, base_url: &str, group: &str) -> Self {
        self.config.gitlab = Some(claudear::config::GitLabConfig {
            enabled: true,
            token: Some(token.to_string()),
            base_url: base_url.to_string(),
            groups: vec![group.to_string()],
            auto_resolve_on_merge: false,
            review_trigger: "/claudear".to_string(),
            ..Default::default()
        });
        self.config.known_orgs = vec![group.to_string()];
        self
    }

    pub fn linear(mut self, api_key: &str, team_id: &str) -> Self {
        self.config.linear = Some(claudear::config::LinearConfig {
            enabled: true,
            api_key: api_key.to_string(),
            team_id: Some(team_id.to_string()),
            trigger_labels: vec!["claudear-e2e".to_string()],
            trigger_states: vec!["backlog".to_string(), "todo".to_string()],
            ..Default::default()
        });
        self
    }

    pub fn jira(mut self, base_url: &str, email: &str, api_token: &str, project_key: &str) -> Self {
        self.config.jira = Some(claudear::config::JiraConfig {
            enabled: true,
            base_url: base_url.to_string(),
            email: email.to_string(),
            api_token: api_token.to_string(),
            auth_mode: "basic".to_string(),
            project_keys: vec![project_key.to_string()],
            trigger_labels: vec!["claudear-e2e".to_string()],
            trigger_statuses: vec!["To Do".to_string(), "Backlog".to_string()],
            ..Default::default()
        });
        self
    }

    pub fn discord_source(mut self, bot_token: &str, channel_id: &str) -> Self {
        self.config.discord.bot_token = Some(bot_token.to_string());
        self.config.discord.channel_id = Some(channel_id.to_string());
        self.config.discord.source_enabled = true;
        self.config.discord.listen_channel_id = Some(channel_id.to_string());
        self
    }

    pub fn discord_notifier(mut self, webhook_url: &str) -> Self {
        self.config.discord.webhook_url = Some(webhook_url.to_string());
        self
    }

    pub fn slack_source(mut self, bot_token: &str, channel_id: &str) -> Self {
        self.config.slack.bot_token = Some(bot_token.to_string());
        self.config.slack.channel_id = Some(channel_id.to_string());
        self.config.slack.source_enabled = true;
        self.config.slack.listen_channel_id = Some(channel_id.to_string());
        self
    }

    pub fn slack_user_id(mut self, user_id: &str) -> Self {
        self.config.slack.user_id = Some(user_id.to_string());
        self
    }

    pub fn slack_notifier(mut self, bot_token: &str, channel_id: &str) -> Self {
        self.config.slack.bot_token = Some(bot_token.to_string());
        self.config.slack.channel_id = Some(channel_id.to_string());
        self
    }

    pub fn instructions(mut self, instructions: &str) -> Self {
        self.config.claude.instructions = Some(instructions.to_string());
        self
    }

    pub fn ask(mut self, enabled: bool) -> Self {
        self.config.ask.enabled = enabled;
        if enabled {
            self.config.ask.wait_timeout_secs = 300;
            self.config.ask.poll_interval_secs = 5;
            self.config.ask.best_effort_on_timeout = true;
        }
        self
    }

    pub fn retry(mut self, max_retries: u32) -> Self {
        self.config.retry = claudear::RetryConfig {
            max_retries,
            base_delay_ms: 1_000, // Fast retries for E2E
            max_delay_ms: 5_000,
        };
        self
    }

    pub fn regression(mut self, enabled: bool) -> Self {
        self.config.regression = claudear::config::RegressionConfig {
            enabled,
            check_interval_secs: Some(10),
            monitoring_duration_secs: Some(10),
            ..Default::default()
        };
        self
    }

    pub fn cascade(mut self, rules: Vec<claudear::CascadeConfig>) -> Self {
        if let Some(first) = rules.into_iter().next() {
            self.config.cascade = first;
        }
        self
    }

    pub fn cascade_rule(mut self, upstream: &str, downstream: &str, trigger: &str) -> Self {
        self.config.cascade.enabled = true;
        self.config
            .cascade
            .rules
            .push(claudear::config::CascadeRule {
                upstream: upstream.to_string(),
                downstream: downstream.to_string(),
                trigger: if trigger == "release" {
                    claudear::config::CascadeTrigger::Release
                } else {
                    claudear::config::CascadeTrigger::Merge
                },
                target_branch: None,
                version_update: true,
                instructions: None,
            });
        self
    }

    /// Write the config to a TOML file and return the path.
    pub fn write_to(self, dir: &Path, name: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).context("create config dir")?;
        let path = dir.join(format!("{}.toml", name));
        let toml_str = toml::to_string_pretty(&self.config).context("serialize config to TOML")?;
        std::fs::write(&path, toml_str).context("write config file")?;
        Ok(path)
    }
}
