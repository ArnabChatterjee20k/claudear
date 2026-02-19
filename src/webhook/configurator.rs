//! Webhook auto-configuration orchestrator.

use crate::config::{Config, LinearConfig, SentryConfig};
use crate::env_writer::update_env_file;
use crate::error::{Error, Result};
use crate::webhook::linear_api::LinearApiClient;
use crate::webhook::sentry_api::SentryApiClient;
use std::collections::HashMap;

/// Result of webhook auto-configuration.
#[derive(Debug, Default)]
pub struct WebhookSetupResult {
    /// Linear webhook was configured.
    pub linear_configured: bool,
    /// Linear webhook ID.
    pub linear_webhook_id: Option<String>,
    /// Linear webhook secret (for display only - also written to .env).
    pub linear_secret: Option<String>,
    /// Sentry webhooks were configured (one per project).
    pub sentry_configured: bool,
    /// Number of Sentry projects configured.
    pub sentry_project_count: usize,
    /// Sentry webhook secret (for display only - also written to .env).
    /// Note: If multiple projects, they may have different secrets. We use the first one.
    pub sentry_secret: Option<String>,
    /// Errors encountered during setup (non-fatal).
    pub warnings: Vec<String>,
}

/// Orchestrates webhook auto-configuration for Linear and Sentry.
pub struct WebhookConfigurator {
    config: Config,
    env_path: std::path::PathBuf,
}

impl WebhookConfigurator {
    /// Create a new webhook configurator.
    ///
    /// # Arguments
    /// * `config` - The application configuration
    /// * `env_path` - Path to the .env file for writing secrets
    pub fn new(config: Config, env_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            config,
            env_path: env_path.into(),
        }
    }

    /// Run the webhook auto-configuration.
    ///
    /// This will:
    /// 1. Create webhooks for enabled sources (Linear, Sentry)
    /// 2. Write the returned secrets to the .env file
    ///
    /// # Arguments
    /// * `base_url` - The public URL where webhooks will be received
    pub async fn configure(&self, base_url: &str) -> Result<WebhookSetupResult> {
        tracing::info!("Starting webhook auto-configuration...");
        tracing::info!("Base URL: {}", base_url);

        let mut result = WebhookSetupResult::default();
        let mut env_updates: HashMap<String, String> = HashMap::new();

        // Configure Linear webhook
        if let Some(ref linear_config) = self.config.linear {
            if linear_config.enabled {
                match self.configure_linear(linear_config, base_url).await {
                    Ok((webhook_id, secret)) => {
                        result.linear_configured = true;
                        result.linear_webhook_id = Some(webhook_id);
                        result.linear_secret = Some(secret.clone());
                        env_updates.insert("LINEAR_WEBHOOK_SECRET".to_string(), secret);
                        tracing::info!("Linear webhook configured successfully");
                    }
                    Err(e) => {
                        let warning = format!("Failed to configure Linear webhook: {}", e);
                        tracing::warn!("{}", warning);
                        result.warnings.push(warning);
                    }
                }
            }
        }

        // Configure Sentry webhooks
        if let Some(ref sentry_config) = self.config.sentry {
            if sentry_config.enabled {
                match self.configure_sentry(sentry_config, base_url).await {
                    Ok((count, secret)) => {
                        result.sentry_configured = true;
                        result.sentry_project_count = count;
                        if let Some(s) = secret {
                            result.sentry_secret = Some(s.clone());
                            env_updates.insert("SENTRY_CLIENT_SECRET".to_string(), s);
                        }
                        tracing::info!("Sentry webhooks configured for {} project(s)", count);
                    }
                    Err(e) => {
                        let warning = format!("Failed to configure Sentry webhooks: {}", e);
                        tracing::warn!("{}", warning);
                        result.warnings.push(warning);
                    }
                }
            }
        }

        // Write secrets to .env file
        if !env_updates.is_empty() {
            tracing::info!("Writing secrets to {}", self.env_path.display());
            update_env_file(&self.env_path, &env_updates)?;
        }

        if !result.linear_configured && !result.sentry_configured {
            if result.warnings.is_empty() {
                return Err(Error::config(
                    "No webhook sources are enabled. Enable Linear or Sentry in your configuration."
                ));
            } else {
                return Err(Error::config(format!(
                    "Failed to configure any webhooks: {}",
                    result.warnings.join("; ")
                )));
            }
        }

        Ok(result)
    }

    async fn configure_linear(
        &self,
        config: &LinearConfig,
        base_url: &str,
    ) -> Result<(String, String)> {
        let client = LinearApiClient::from_config(config);
        let webhook_url = format!("{}/webhook/linear", base_url.trim_end_matches('/'));

        // Check if webhook already exists
        if client.webhook_exists(&webhook_url).await? {
            return Err(Error::api(format!(
                "A webhook with URL {} already exists in Linear. \
                Delete it manually if you want to reconfigure.",
                webhook_url
            )));
        }

        tracing::info!("Creating Linear webhook: {}", webhook_url);

        let registration = client
            .create_webhook(&webhook_url, config.team_id.as_deref(), &["Issue"])
            .await?;

        Ok((registration.id, registration.secret))
    }

    async fn configure_sentry(
        &self,
        config: &SentryConfig,
        base_url: &str,
    ) -> Result<(usize, Option<String>)> {
        let client = SentryApiClient::from_config(config);
        let webhook_url = format!("{}/webhook/sentry", base_url.trim_end_matches('/'));

        tracing::info!("Creating Sentry webhooks: {}", webhook_url);

        let registrations = client
            .create_webhooks_for_projects(
                &config.project_slugs,
                &webhook_url,
                &["event.created", "event.alert"],
            )
            .await?;

        let count = registrations.len();
        let secret = registrations.first().map(|r| r.secret.clone());

        // Log warning if multiple projects have different secrets
        if registrations.len() > 1 {
            let first_secret = &registrations[0].secret;
            let different = registrations
                .iter()
                .skip(1)
                .any(|r| &r.secret != first_secret);
            if different {
                tracing::warn!(
                    "Sentry webhooks have different secrets for different projects. \
                    Only the first secret will be saved to .env. \
                    You may need to handle this manually."
                );
            }
        }

        Ok((count, secret))
    }

    /// Check if webhooks need to be configured.
    pub fn needs_configuration(&self) -> bool {
        let linear_needs = self
            .config
            .linear
            .as_ref()
            .is_some_and(|c| c.enabled && c.webhook_secret.is_none());

        let sentry_needs = self
            .config
            .sentry
            .as_ref()
            .is_some_and(|c| c.enabled && c.client_secret.is_none());

        linear_needs || sentry_needs
    }
}

/// Print the result of webhook configuration to the console.
pub fn print_setup_result(result: &WebhookSetupResult) {
    println!("\n=== Webhook Configuration Complete ===\n");

    if result.linear_configured {
        println!("Linear:");
        println!("  Status: Configured");
        if let Some(ref id) = result.linear_webhook_id {
            println!("  Webhook ID: {}", id);
        }
        if let Some(ref secret) = result.linear_secret {
            println!("  Secret: {} (saved to .env)", mask_secret(secret));
        }
        println!();
    }

    if result.sentry_configured {
        println!("Sentry:");
        println!("  Status: Configured");
        println!("  Projects: {}", result.sentry_project_count);
        if let Some(ref secret) = result.sentry_secret {
            println!("  Secret: {} (saved to .env)", mask_secret(secret));
        }
        println!();
    }

    if !result.warnings.is_empty() {
        println!("Warnings:");
        for warning in &result.warnings {
            println!("  - {}", warning);
        }
        println!();
    }

    println!("Webhook secrets have been saved to your .env file.");
    println!("The webhook server will now start and begin receiving events.");
}

fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.is_empty() {
        return "****".to_string();
    }
    if chars.len() <= 12 {
        "*".repeat(chars.len())
    } else {
        let prefix: String = chars[..3].iter().collect();
        let suffix: String = chars[chars.len() - 3..].iter().collect();
        format!("{}...{}", prefix, suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            work_dir: "/tmp/repos".into(),
            known_orgs: vec!["test-org".to_string()],
            auto_discover_paths: vec![],
            poll_interval_ms: 300_000,
            webhook_port: 3100,
            db_path: "/tmp/test.db".into(),
            max_issues_per_cycle: 5,
            max_concurrent: 1,
            processing_delay_ms: 5000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            claude_timeout_secs: 21600,
            claude: crate::config::ClaudeConfig::default(),
            discord: crate::config::DiscordConfig::default(),
            slack: crate::config::SlackConfig::default(),
            email: crate::config::EmailConfig::default(),
            sms: crate::config::SmsConfig::default(),
            push: crate::config::PushConfig::default(),
            ask: crate::config::AskConfig::default(),
            github: crate::config::GitHubConfig::default(),
            github_app: crate::config::GitHubAppConfig::default(),
            retry: crate::config::RetryConfig::default(),
            linear: None,
            sentry: None,
            jira: None,
            gitlab: None,
            regression: crate::config::RegressionConfig::default(),
            cascade: crate::config::CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: crate::config::LearningConfig::default(),
            prioritisation: crate::config::PrioritisationConfig::default(),
            code_index: crate::config::CodeIndexConfig::default(),
        }
    }

    #[test]
    fn test_mask_secret_long() {
        assert_eq!(mask_secret("1234567890abcdef"), "123...def");
    }

    #[test]
    fn test_mask_secret_short() {
        assert_eq!(mask_secret("1234"), "****");
    }

    #[test]
    fn test_mask_secret_exactly_12() {
        assert_eq!(mask_secret("123456789012"), "************");
    }

    #[test]
    fn test_mask_secret_13_chars() {
        assert_eq!(mask_secret("1234567890abc"), "123...abc");
    }

    #[test]
    fn test_webhook_setup_result_default() {
        let result = WebhookSetupResult::default();
        assert!(!result.linear_configured);
        assert!(!result.sentry_configured);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_needs_configuration_no_sources() {
        let config = test_config();
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(!configurator.needs_configuration());
    }

    #[test]
    fn test_needs_configuration_linear_no_secret() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test".to_string(),
            trigger_labels: vec![],
            trigger_states: vec![],
            team_id: None,
            project_id: None,
            webhook_secret: None, // No secret
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(configurator.needs_configuration());
    }

    #[test]
    fn test_needs_configuration_linear_with_secret() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test".to_string(),
            trigger_labels: vec![],
            trigger_states: vec![],
            team_id: None,
            project_id: None,
            webhook_secret: Some("secret".to_string()), // Has secret
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(!configurator.needs_configuration());
    }

    // ── mask_secret edge cases ──

    #[test]
    fn test_mask_secret_empty_string() {
        // Empty input now returns "****" instead of an empty string.
        let result = mask_secret("");
        assert_eq!(
            result, "****",
            "empty secret should produce a masked placeholder"
        );
    }

    #[test]
    fn test_mask_secret_single_char() {
        assert_eq!(mask_secret("a"), "*");
    }

    #[test]
    fn test_mask_secret_two_chars() {
        assert_eq!(mask_secret("ab"), "**");
    }

    #[test]
    fn test_mask_secret_three_chars() {
        assert_eq!(mask_secret("abc"), "***");
    }

    #[test]
    fn test_mask_secret_unicode_multibyte() {
        // Uses chars() not bytes(), so multi-byte characters should be
        // handled correctly — each CJK character counts as 1.
        let secret = "\u{65E5}\u{672C}\u{8A9E}\u{30C6}\u{30B9}\u{30C8}\u{6587}\u{5B57}\u{5217}\u{306E}\u{4F8B}\u{3067}\u{3059}\u{FF01}\u{5168}\u{90E8}\u{30DE}\u{30B9}\u{30AF}";
        let chars: Vec<char> = secret.chars().collect();
        assert_eq!(chars.len(), 19, "should be 19 unicode chars");
        // 19 > 12, so should use prefix...suffix format
        let masked = mask_secret(secret);
        let prefix: String = chars[..3].iter().collect();
        let suffix: String = chars[chars.len() - 3..].iter().collect();
        assert_eq!(masked, format!("{}...{}", prefix, suffix));
    }

    #[test]
    fn test_mask_secret_exactly_13_chars() {
        // 13 > 12, so should show prefix...suffix
        assert_eq!(mask_secret("abcdefghijklm"), "abc...klm");
    }

    #[test]
    fn test_mask_secret_exactly_12_chars_boundary() {
        // 12 <= 12, so should be all asterisks
        assert_eq!(mask_secret("abcdefghijkl"), "************");
    }

    // ── needs_configuration with Sentry config ──

    #[test]
    fn test_needs_configuration_sentry_enabled_no_secret() {
        let mut config = test_config();
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            configurator.needs_configuration(),
            "Sentry enabled without client_secret should need config"
        );
    }

    #[test]
    fn test_needs_configuration_sentry_enabled_with_secret() {
        let mut config = test_config();
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sentry-secret".to_string()),
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Sentry enabled with client_secret should not need config"
        );
    }

    #[test]
    fn test_needs_configuration_sentry_disabled() {
        let mut config = test_config();
        config.sentry = Some(crate::config::SentryConfig {
            enabled: false,
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Sentry disabled should not need config even without secret"
        );
    }

    #[test]
    fn test_needs_configuration_both_linear_and_sentry_need_config() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: None,
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            configurator.needs_configuration(),
            "Both sources needing config should return true"
        );
    }

    #[test]
    fn test_needs_configuration_linear_has_secret_sentry_does_not() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("linear-secret".to_string()),
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            configurator.needs_configuration(),
            "Should still need config if Sentry lacks secret"
        );
    }

    #[test]
    fn test_needs_configuration_neither_enabled() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: false,
            webhook_secret: None,
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: false,
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Neither enabled should not need config"
        );
    }

    // ── WebhookSetupResult ──

    #[test]
    fn test_webhook_setup_result_default_all_fields() {
        let result = WebhookSetupResult::default();
        assert!(!result.linear_configured);
        assert!(result.linear_webhook_id.is_none());
        assert!(result.linear_secret.is_none());
        assert!(!result.sentry_configured);
        assert_eq!(result.sentry_project_count, 0);
        assert!(result.sentry_secret.is_none());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_webhook_setup_result_set_all_fields() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-123".to_string()),
            linear_secret: Some("lin-secret".to_string()),
            sentry_configured: true,
            sentry_project_count: 3,
            sentry_secret: Some("sen-secret".to_string()),
            warnings: vec!["warn1".to_string(), "warn2".to_string()],
        };
        assert!(result.linear_configured);
        assert_eq!(result.linear_webhook_id.as_deref(), Some("wh-123"));
        assert_eq!(result.linear_secret.as_deref(), Some("lin-secret"));
        assert!(result.sentry_configured);
        assert_eq!(result.sentry_project_count, 3);
        assert_eq!(result.sentry_secret.as_deref(), Some("sen-secret"));
        assert_eq!(result.warnings.len(), 2);
        assert_eq!(result.warnings[0], "warn1");
        assert_eq!(result.warnings[1], "warn2");
    }
}
