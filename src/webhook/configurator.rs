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
            bind_address: "127.0.0.1".to_string(),
            db_path: "/tmp/test.db".into(),
            max_issues_per_cycle: 5,
            max_concurrent: 1,
            processing_delay_ms: 5000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            agent: crate::config::AgentConfig::default(),
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
            evaluation: crate::config::EvaluationConfig::default(),
            storage_dir: "/tmp/claudear-storage".into(),
            dashboard: crate::config::DashboardConfig::default(),
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

    #[test]
    fn test_configurator_new_stores_config_and_path() {
        let config = test_config();
        let configurator = WebhookConfigurator::new(config.clone(), "/tmp/test.env");

        assert_eq!(
            configurator.env_path,
            std::path::PathBuf::from("/tmp/test.env")
        );
        assert_eq!(configurator.config.webhook_port, config.webhook_port);
    }

    #[test]
    fn test_configurator_new_with_pathbuf() {
        let config = test_config();
        let path = std::path::PathBuf::from("/home/user/.env");
        let configurator = WebhookConfigurator::new(config, path.clone());

        assert_eq!(configurator.env_path, path);
    }

    #[test]
    fn test_configurator_new_with_string() {
        let config = test_config();
        let configurator = WebhookConfigurator::new(config, String::from("/var/app/.env"));

        assert_eq!(
            configurator.env_path,
            std::path::PathBuf::from("/var/app/.env")
        );
    }

    #[test]
    fn test_configurator_new_with_relative_path() {
        let config = test_config();
        let configurator = WebhookConfigurator::new(config, ".env");

        assert_eq!(configurator.env_path, std::path::PathBuf::from(".env"));
    }

    #[test]
    fn test_configurator_new_with_empty_path() {
        let config = test_config();
        let configurator = WebhookConfigurator::new(config, "");

        assert_eq!(configurator.env_path, std::path::PathBuf::from(""));
    }

    #[test]
    fn test_configurator_new_preserves_all_config_fields() {
        let mut config = test_config();
        config.webhook_port = 5555;
        config.work_dir = "/custom/dir".into();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "key-123".to_string(),
            webhook_secret: Some("secret".to_string()),
            ..Default::default()
        });

        let configurator = WebhookConfigurator::new(config, "/tmp/.env");

        assert_eq!(configurator.config.webhook_port, 5555);
        assert_eq!(
            configurator.config.work_dir,
            std::path::PathBuf::from("/custom/dir")
        );
        assert!(configurator.config.linear.is_some());
        assert_eq!(
            configurator.config.linear.as_ref().unwrap().api_key,
            "key-123"
        );
    }

    #[test]
    fn test_needs_configuration_linear_disabled_no_secret() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: false,
            webhook_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Linear disabled should not need config even without secret"
        );
    }

    #[test]
    fn test_needs_configuration_linear_disabled_with_secret() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: false,
            webhook_secret: Some("has-secret".to_string()),
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Linear disabled with secret should not need config"
        );
    }

    #[test]
    fn test_needs_configuration_sentry_has_secret_linear_does_not() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: None,
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sentry-secret".to_string()),
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            configurator.needs_configuration(),
            "Should need config if Linear lacks secret"
        );
    }

    #[test]
    fn test_needs_configuration_both_have_secrets() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("lin-secret".to_string()),
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sen-secret".to_string()),
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Both with secrets should not need config"
        );
    }

    #[test]
    fn test_needs_configuration_only_linear_enabled_and_configured() {
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("secret".to_string()),
            ..Default::default()
        });
        // No sentry at all
        config.sentry = None;
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Only Linear enabled with secret should not need config"
        );
    }

    #[test]
    fn test_needs_configuration_only_sentry_enabled_and_configured() {
        let mut config = test_config();
        config.linear = None;
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("secret".to_string()),
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Only Sentry enabled with secret should not need config"
        );
    }

    #[test]
    fn test_needs_configuration_only_sentry_enabled_no_secret() {
        let mut config = test_config();
        config.linear = None;
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            configurator.needs_configuration(),
            "Only Sentry enabled without secret should need config"
        );
    }

    #[test]
    fn test_mask_secret_six_chars() {
        // 6 <= 12, so should be all asterisks
        assert_eq!(mask_secret("abcdef"), "******");
    }

    #[test]
    fn test_mask_secret_eleven_chars() {
        assert_eq!(mask_secret("abcdefghijk"), "***********");
    }

    #[test]
    fn test_mask_secret_fourteen_chars() {
        // 14 > 12, so should show prefix...suffix
        assert_eq!(mask_secret("abcdefghijklmn"), "abc...lmn");
    }

    #[test]
    fn test_mask_secret_very_long_string() {
        let secret = "a".repeat(1000);
        let result = mask_secret(&secret);
        assert_eq!(result, "aaa...aaa");
    }

    #[test]
    fn test_mask_secret_with_special_chars() {
        let secret = "!@#$%^&*()_+-=[]{}";
        // 18 chars > 12, so should show prefix...suffix
        let chars: Vec<char> = secret.chars().collect();
        let prefix: String = chars[..3].iter().collect();
        let suffix: String = chars[chars.len() - 3..].iter().collect();
        let result = mask_secret(secret);
        assert_eq!(result, format!("{}...{}", prefix, suffix));
    }

    #[test]
    fn test_mask_secret_with_spaces() {
        let secret = "secret with spaces here";
        // 23 chars > 12, so should show prefix...suffix
        let result = mask_secret(secret);
        assert_eq!(result, "sec...ere");
    }

    #[test]
    fn test_mask_secret_all_asterisks() {
        let secret = "****";
        // 4 <= 12, so should be all asterisks
        assert_eq!(mask_secret(secret), "****");
    }

    #[test]
    fn test_mask_secret_numeric_string() {
        let secret = "1234567890123456";
        // 16 > 12, so prefix...suffix
        assert_eq!(mask_secret(secret), "123...456");
    }

    #[test]
    fn test_webhook_setup_result_debug_format() {
        let result = WebhookSetupResult::default();
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("WebhookSetupResult"));
        assert!(debug_str.contains("linear_configured: false"));
        assert!(debug_str.contains("sentry_configured: false"));
    }

    #[test]
    fn test_webhook_setup_result_debug_with_values() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-999".to_string()),
            linear_secret: Some("secret-val".to_string()),
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: vec!["warning-1".to_string()],
        };
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("linear_configured: true"));
        assert!(debug_str.contains("wh-999"));
        assert!(debug_str.contains("warning-1"));
    }

    #[test]
    fn test_webhook_setup_result_warnings_can_be_added() {
        let mut result = WebhookSetupResult::default();
        assert!(result.warnings.is_empty());

        result.warnings.push("warning 1".to_string());
        result.warnings.push("warning 2".to_string());

        assert_eq!(result.warnings.len(), 2);
    }

    #[test]
    fn test_webhook_setup_result_partial_configuration() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-abc".to_string()),
            linear_secret: Some("lin-secret-val".to_string()),
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: vec![],
        };

        assert!(result.linear_configured);
        assert!(!result.sentry_configured);
        assert!(result.linear_webhook_id.is_some());
        assert!(result.sentry_secret.is_none());
    }

    #[test]
    fn test_webhook_setup_result_sentry_only() {
        let result = WebhookSetupResult {
            linear_configured: false,
            linear_webhook_id: None,
            linear_secret: None,
            sentry_configured: true,
            sentry_project_count: 5,
            sentry_secret: Some("sen-secret-val".to_string()),
            warnings: vec![],
        };

        assert!(!result.linear_configured);
        assert!(result.sentry_configured);
        assert_eq!(result.sentry_project_count, 5);
    }

    #[test]
    fn test_print_setup_result_linear_only() {
        // Capture that this does not panic with linear-only config
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-123".to_string()),
            linear_secret: Some("secret1234567890".to_string()),
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: vec![],
        };

        // Should not panic
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_sentry_only() {
        let result = WebhookSetupResult {
            linear_configured: false,
            linear_webhook_id: None,
            linear_secret: None,
            sentry_configured: true,
            sentry_project_count: 3,
            sentry_secret: Some("sentry-secret-1234567890".to_string()),
            warnings: vec![],
        };

        // Should not panic
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_both_configured() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-456".to_string()),
            linear_secret: Some("linear-secret-1234567890".to_string()),
            sentry_configured: true,
            sentry_project_count: 2,
            sentry_secret: Some("sentry-secret-1234567890".to_string()),
            warnings: vec![],
        };

        // Should not panic
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_with_warnings() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-789".to_string()),
            linear_secret: Some("secret1234567890".to_string()),
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: vec![
                "Failed to configure Sentry: API error".to_string(),
                "Rate limit exceeded".to_string(),
            ],
        };

        // Should not panic
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_nothing_configured_no_warnings() {
        let result = WebhookSetupResult::default();

        // Should not panic, even with no configuration
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_with_no_webhook_id() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: None, // No webhook ID
            linear_secret: Some("secret1234567890".to_string()),
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: vec![],
        };

        // Should not panic even without webhook ID
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_with_no_secrets() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-abc".to_string()),
            linear_secret: None, // No secret
            sentry_configured: true,
            sentry_project_count: 1,
            sentry_secret: None, // No secret
            warnings: vec![],
        };

        // Should not panic even without secrets
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_with_short_secrets() {
        // Secrets that are <= 12 chars should be fully masked
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-abc".to_string()),
            linear_secret: Some("short".to_string()), // 5 chars, masked as *****
            sentry_configured: true,
            sentry_project_count: 1,
            sentry_secret: Some("ab".to_string()), // 2 chars, masked as **
            warnings: vec![],
        };

        // Should not panic
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_with_empty_secret_strings() {
        let result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-abc".to_string()),
            linear_secret: Some("".to_string()), // Empty secret
            sentry_configured: true,
            sentry_project_count: 1,
            sentry_secret: Some("".to_string()), // Empty secret
            warnings: vec![],
        };

        // Should not panic (mask_secret("") returns "****")
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result_many_warnings() {
        let result = WebhookSetupResult {
            linear_configured: false,
            linear_webhook_id: None,
            linear_secret: None,
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: (0..10).map(|i| format!("Warning number {}", i)).collect(),
        };

        // Should not panic with many warnings
        print_setup_result(&result);
    }

    #[test]
    fn test_print_setup_result() {
        // Comprehensive test: verify print_setup_result does not panic for various configurations
        // and exercises all branches (linear, sentry, warnings, secrets, missing IDs)

        // Both configured with secrets
        let full_result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: Some("wh-full".to_string()),
            linear_secret: Some("abcdef1234567890".to_string()),
            sentry_configured: true,
            sentry_project_count: 2,
            sentry_secret: Some("1234567890abcdef".to_string()),
            warnings: vec!["Some warning".to_string()],
        };
        print_setup_result(&full_result); // should not panic

        // No configured sources, just warnings
        let empty_result = WebhookSetupResult {
            linear_configured: false,
            linear_webhook_id: None,
            linear_secret: None,
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: vec!["warning A".to_string(), "warning B".to_string()],
        };
        print_setup_result(&empty_result); // should not panic

        // Linear without webhook ID or secret
        let partial_result = WebhookSetupResult {
            linear_configured: true,
            linear_webhook_id: None,
            linear_secret: None,
            sentry_configured: false,
            sentry_project_count: 0,
            sentry_secret: None,
            warnings: vec![],
        };
        print_setup_result(&partial_result); // should not panic
    }

    #[test]
    fn test_needs_configuration_edge_cases() {
        // Edge case 1: Both sources enabled, both missing secrets
        let mut config1 = test_config();
        config1.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: None,
            ..Default::default()
        });
        config1.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: None,
            ..Default::default()
        });
        let c1 = WebhookConfigurator::new(config1, "/tmp/.env");
        assert!(
            c1.needs_configuration(),
            "both enabled, both missing secrets"
        );

        // Edge case 2: Both sources present but disabled
        let mut config2 = test_config();
        config2.linear = Some(crate::config::LinearConfig {
            enabled: false,
            webhook_secret: None,
            ..Default::default()
        });
        config2.sentry = Some(crate::config::SentryConfig {
            enabled: false,
            client_secret: None,
            ..Default::default()
        });
        let c2 = WebhookConfigurator::new(config2, "/tmp/.env");
        assert!(
            !c2.needs_configuration(),
            "both disabled should not need config"
        );

        // Edge case 3: One enabled with secret, one enabled without
        let mut config3 = test_config();
        config3.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("secret".to_string()),
            ..Default::default()
        });
        config3.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: None,
            ..Default::default()
        });
        let c3 = WebhookConfigurator::new(config3, "/tmp/.env");
        assert!(
            c3.needs_configuration(),
            "one missing secret should need config"
        );

        // Edge case 4: Both enabled, both have secrets
        let mut config4 = test_config();
        config4.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("lin-secret".to_string()),
            ..Default::default()
        });
        config4.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sen-secret".to_string()),
            ..Default::default()
        });
        let c4 = WebhookConfigurator::new(config4, "/tmp/.env");
        assert!(
            !c4.needs_configuration(),
            "both have secrets should not need config"
        );

        // Edge case 5: No sources configured at all
        let config5 = test_config(); // linear=None, sentry=None
        let c5 = WebhookConfigurator::new(config5, "/tmp/.env");
        assert!(
            !c5.needs_configuration(),
            "no sources means no config needed"
        );
    }

    #[test]
    fn test_print_setup_result_sentry_zero_projects() {
        let result = WebhookSetupResult {
            linear_configured: false,
            linear_webhook_id: None,
            linear_secret: None,
            sentry_configured: true,
            sentry_project_count: 0,
            sentry_secret: Some("secret1234567890".to_string()),
            warnings: vec![],
        };

        // Should not panic even with 0 projects
        print_setup_result(&result);
    }

    #[tokio::test]
    async fn test_configure_no_sources_enabled_returns_error() {
        // No linear or sentry configured at all => error about no sources enabled
        let config = test_config(); // linear=None, sentry=None
        let configurator = WebhookConfigurator::new(config, "/tmp/test-no-sources.env");

        let result = configurator.configure("https://example.com").await;
        assert!(result.is_err(), "configure with no sources should error");

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No webhook sources are enabled"),
            "Error should mention no sources are enabled, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_linear_disabled_sentry_disabled_returns_error() {
        // Both sources present but disabled => error about no sources enabled
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: false,
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: false,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-disabled.env");

        let result = configurator.configure("https://example.com").await;
        assert!(
            result.is_err(),
            "configure with disabled sources should error"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No webhook sources are enabled"),
            "Error should mention no sources are enabled, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_linear_disabled_sentry_none_returns_error() {
        // Linear present but disabled, sentry absent => error
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: false,
            ..Default::default()
        });
        config.sentry = None;
        let configurator = WebhookConfigurator::new(config, "/tmp/test-lin-disabled.env");

        let result = configurator.configure("https://example.com").await;
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No webhook sources are enabled"),
            "Expected no-sources error, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_sentry_disabled_linear_none_returns_error() {
        // Sentry present but disabled, linear absent => error
        let mut config = test_config();
        config.linear = None;
        config.sentry = Some(crate::config::SentryConfig {
            enabled: false,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-sen-disabled.env");

        let result = configurator.configure("https://example.com").await;
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No webhook sources are enabled"),
            "Expected no-sources error, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_linear_enabled_but_fails_api_returns_error_with_warnings() {
        // Linear enabled with a dummy API key that will fail when trying to connect.
        // This exercises the warning-accumulation path (lines 75-80) and then the
        // "Failed to configure any webhooks" error path (lines 117-122).
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "invalid-api-key".to_string(),
            trigger_labels: vec![],
            trigger_states: vec![],
            team_id: None,
            project_id: None,
            webhook_secret: None,
            ..Default::default()
        });
        config.sentry = None;
        let configurator = WebhookConfigurator::new(config, "/tmp/test-lin-fail.env");

        let result = configurator.configure("https://example.com").await;
        assert!(
            result.is_err(),
            "configure should error when Linear API fails"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to configure any webhooks"),
            "Error should mention failure to configure, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_sentry_enabled_but_fails_api_returns_error_with_warnings() {
        // Sentry enabled with dummy credentials that will fail.
        // Exercises the sentry warning-accumulation path (lines 97-101) and then the
        // "Failed to configure any webhooks" error path (lines 117-122).
        let mut config = test_config();
        config.linear = None;
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            auth_token: "invalid-token".to_string(),
            org_slug: "nonexistent-org".to_string(),
            project_slugs: vec!["fake-project".to_string()],
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-sen-fail.env");

        let result = configurator.configure("https://example.com").await;
        assert!(
            result.is_err(),
            "configure should error when Sentry API fails"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to configure any webhooks"),
            "Error should mention failure to configure, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_both_enabled_both_fail_returns_combined_warnings() {
        // Both sources enabled with invalid credentials -- both will fail,
        // and the error message should include warnings from both.
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "invalid-linear-key".to_string(),
            webhook_secret: None,
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            auth_token: "invalid-sentry-token".to_string(),
            org_slug: "nonexistent-org".to_string(),
            project_slugs: vec!["fake-project".to_string()],
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-both-fail.env");

        let result = configurator.configure("https://example.com").await;
        assert!(
            result.is_err(),
            "configure should error when both APIs fail"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to configure any webhooks"),
            "Error should mention failure, got: {}",
            err_msg
        );
        // The error should contain both warnings joined by ";"
        assert!(
            err_msg.contains("Linear") || err_msg.contains("Sentry") || err_msg.contains(";"),
            "Error should contain warning info from at least one source, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_base_url_trailing_slash_is_handled() {
        // Even though the API call will fail, this verifies configure()
        // does not panic with a trailing-slash base_url.
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test-key".to_string(),
            webhook_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-trailing.env");

        let result = configurator.configure("https://example.com/").await;
        // Will fail at the API level, but should not panic
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_configure_base_url_no_trailing_slash() {
        // Verifies configure() works without trailing slash
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test-key".to_string(),
            webhook_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-no-trailing.env");

        let result = configurator.configure("https://example.com").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_configure_linear_enabled_sentry_disabled_linear_fails() {
        // Linear enabled but fails, Sentry present but disabled.
        // Only the Linear warning should appear.
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "bad-key".to_string(),
            webhook_secret: None,
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: false,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-mixed.env");

        let result = configurator.configure("https://example.com").await;
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to configure any webhooks"),
            "Should get failure error, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_sentry_enabled_linear_disabled_sentry_fails() {
        // Sentry enabled but fails, Linear present but disabled.
        let mut config = test_config();
        config.linear = Some(crate::config::LinearConfig {
            enabled: false,
            ..Default::default()
        });
        config.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            auth_token: "bad-token".to_string(),
            org_slug: "fake-org".to_string(),
            project_slugs: vec!["fake".to_string()],
            client_secret: None,
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-mixed2.env");

        let result = configurator.configure("https://example.com").await;
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to configure any webhooks"),
            "Should get failure error, got: {}",
            err_msg
        );
    }
}
