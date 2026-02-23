//! Webhook auto-configuration orchestrator.

use crate::config::{
    Config, GitHubConfig, GitLabConfig, JiraConfig, LinearConfig, SentryConfig, SlackSourceConfig,
    TelegramConfig, WhatsAppConfig,
};
use crate::env_writer::update_env_file;
use crate::error::{Error, Result};
use crate::webhook::linear_api::LinearApiClient;
use crate::webhook::sentry_api::SentryApiClient;
use serde::Deserialize;
use std::collections::HashMap;
use uuid::Uuid;

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

#[derive(Debug, Deserialize)]
struct GitHubHookListItem {
    #[serde(default)]
    config: GitHubHookConfig,
}

#[derive(Debug, Default, Deserialize)]
struct GitHubHookConfig {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabHookListItem {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramApiResponse {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackApiOkResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

/// Orchestrates webhook auto-configuration for supported webhook services.
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
    /// 1. Create webhooks for enabled auto-configurable services
    ///    (Linear, Sentry, GitHub, GitLab, Jira, Telegram, Slack, WhatsApp)
    /// 2. Emit notes for enabled services that currently require manual setup
    /// 2. Write the returned secrets to the .env file
    ///
    /// # Arguments
    /// * `base_url` - The public URL where webhooks will be received
    pub async fn configure(&self, base_url: &str) -> Result<WebhookSetupResult> {
        tracing::info!("Starting webhook auto-configuration...");
        tracing::info!("Base URL: {}", base_url);

        let mut result = WebhookSetupResult::default();
        let mut env_updates: HashMap<String, String> = HashMap::new();
        let mut configured_any = false;
        let mut attempted_auto_config = false;
        let mut failure_warnings: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        // Configure Linear webhook
        if let Some(linear_config) = self.config.linear() {
            if linear_config.enabled {
                attempted_auto_config = true;
                match self.configure_linear(linear_config, base_url).await {
                    Ok((webhook_id, secret)) => {
                        result.linear_configured = true;
                        result.linear_webhook_id = Some(webhook_id);
                        result.linear_secret = Some(secret.clone());
                        env_updates.insert("LINEAR_WEBHOOK_SECRET".to_string(), secret);
                        configured_any = true;
                        tracing::info!("Linear webhook configured successfully");
                    }
                    Err(e) => {
                        let warning = format!("Failed to configure Linear webhook: {}", e);
                        tracing::warn!("{}", warning);
                        failure_warnings.push(warning);
                    }
                }
            }
        }

        // Configure Sentry webhooks
        if let Some(ref sentry_config) = self.config.issues.sentry {
            if sentry_config.enabled {
                attempted_auto_config = true;
                match self.configure_sentry(sentry_config, base_url).await {
                    Ok((count, secret)) => {
                        result.sentry_configured = true;
                        result.sentry_project_count = count;
                        if let Some(s) = secret {
                            result.sentry_secret = Some(s.clone());
                            env_updates.insert("SENTRY_CLIENT_SECRET".to_string(), s);
                        }
                        configured_any = true;
                        tracing::info!("Sentry webhooks configured for {} project(s)", count);
                    }
                    Err(e) => {
                        let warning = format!("Failed to configure Sentry webhooks: {}", e);
                        tracing::warn!("{}", warning);
                        failure_warnings.push(warning);
                    }
                }
            }
        }

        // Configure Jira issue webhooks (/webhook/jira)
        if let Some(jira_config) = self.config.jira() {
            if jira_config.enabled {
                if !jira_config.base_url.trim().is_empty() && !jira_config.api_token.is_empty() {
                    attempted_auto_config = true;
                    match self.configure_jira(jira_config, base_url).await {
                        Ok(created) => {
                            configured_any = true;
                            let note = if created {
                                "Jira issue webhook configured".to_string()
                            } else {
                                "Jira issue webhook already configured".to_string()
                            };
                            tracing::info!("{}", note);
                            notes.push(note);
                        }
                        Err(e) => {
                            let warning = format!("Failed to configure Jira webhook: {}", e);
                            tracing::warn!("{}", warning);
                            failure_warnings.push(warning);
                        }
                    }
                } else if jira_config.base_url.trim().is_empty() {
                    notes.push(
                        "Jira is enabled but jira.base_url is empty; skipping Jira webhook auto-setup."
                            .to_string(),
                    );
                } else {
                    notes.push(
                        "Jira is enabled but jira.api_token is empty; skipping Jira webhook auto-setup."
                            .to_string(),
                    );
                }
            }
        }

        // Configure Telegram webhook (/webhook/telegram)
        if self.config.notifiers.telegram.source_enabled {
            let telegram_config = &self.config.notifiers.telegram;
            let has_bot_token = telegram_config
                .bot_token
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty());
            if has_bot_token {
                attempted_auto_config = true;
                match self.configure_telegram(telegram_config, base_url).await {
                    Ok(generated_secret) => {
                        configured_any = true;
                        if let Some(secret) = generated_secret {
                            env_updates.insert("TELEGRAM_WEBHOOK_SECRET".to_string(), secret);
                        }
                        notes.push("Telegram webhook configured (Bot API setWebhook)".to_string());
                    }
                    Err(e) => {
                        let warning = format!("Failed to configure Telegram webhook: {}", e);
                        tracing::warn!("{}", warning);
                        failure_warnings.push(warning);
                    }
                }
            } else {
                notes.push(
                    "Telegram source is enabled but telegram.bot_token is missing; skipping Telegram webhook auto-setup."
                        .to_string(),
                );
            }
        }

        // Configure Slack Events API webhook (manifest API -> /webhook/slack)
        if let Some(slack_config) = self.config.issues.slack.as_ref() {
            let has_app_id = slack_config
                .app_id
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty());
            let has_manifest_token = slack_config
                .app_config_token
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty());
            let has_signing_secret = slack_config
                .signing_secret
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty());

            if has_app_id && has_manifest_token && has_signing_secret {
                attempted_auto_config = true;
                match self.configure_slack(slack_config, base_url).await {
                    Ok(updated) => {
                        configured_any = true;
                        let note = if updated {
                            "Slack Events API callback configured via apps.manifest.update"
                                .to_string()
                        } else {
                            "Slack Events API callback already configured".to_string()
                        };
                        tracing::info!("{}", note);
                        notes.push(note);
                    }
                    Err(e) => {
                        let warning = format!("Failed to configure Slack webhook: {}", e);
                        tracing::warn!("{}", warning);
                        failure_warnings.push(warning);
                    }
                }
            } else {
                let mut missing = Vec::new();
                if !has_app_id {
                    missing.push("slack.app_id");
                }
                if !has_manifest_token {
                    missing.push("slack.app_config_token");
                }
                if !has_signing_secret {
                    missing.push("slack.signing_secret");
                }
                notes.push(format!(
                    "Slack source is configured, but webhook auto-setup requires {}.",
                    missing.join(", ")
                ));
            }
        }

        // Configure WhatsApp webhook subscription (WABA subscribed_apps -> /webhook/whatsapp)
        if self.config.notifiers.whatsapp.source_enabled {
            let wa = &self.config.notifiers.whatsapp;
            let has_access_token = wa
                .access_token
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty());
            let has_business_account_id = wa
                .business_account_id
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty());
            let has_app_secret = wa
                .app_secret
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty());

            if has_access_token && has_business_account_id && has_app_secret {
                attempted_auto_config = true;
                match self.configure_whatsapp(wa, base_url).await {
                    Ok(generated_verify_token) => {
                        configured_any = true;
                        if let Some(token) = generated_verify_token {
                            env_updates.insert("WHATSAPP_WEBHOOK_VERIFY_TOKEN".to_string(), token);
                        }
                        notes.push(
                            "WhatsApp webhook subscription configured (WABA subscribed_apps)"
                                .to_string(),
                        );
                    }
                    Err(e) => {
                        let warning = format!("Failed to configure WhatsApp webhook: {}", e);
                        tracing::warn!("{}", warning);
                        failure_warnings.push(warning);
                    }
                }
            } else {
                let mut missing = Vec::new();
                if !has_access_token {
                    missing.push("whatsapp.access_token");
                }
                if !has_business_account_id {
                    missing.push("whatsapp.business_account_id");
                }
                if !has_app_secret {
                    missing.push("whatsapp.app_secret");
                }
                notes.push(format!(
                    "WhatsApp source is enabled, but webhook auto-setup requires {}.",
                    missing.join(", ")
                ));
            }
        }

        // Configure GitLab issue webhooks (group hooks -> /webhook/gitlab)
        if let Some(gitlab_config) = self.config.gitlab() {
            if gitlab_config.enabled {
                if !gitlab_config.groups.is_empty() && gitlab_config.token.is_some() {
                    attempted_auto_config = true;
                    match self.configure_gitlab(gitlab_config, base_url).await {
                        Ok((group_count, generated_secret)) => {
                            configured_any = true;
                            if let Some(secret) = generated_secret {
                                env_updates.insert("GITLAB_WEBHOOK_SECRET".to_string(), secret);
                            }
                            let note = format!(
                                "GitLab issue webhooks configured for {} group(s)",
                                group_count
                            );
                            tracing::info!("{}", note);
                            notes.push(note);
                        }
                        Err(e) => {
                            let warning = format!("Failed to configure GitLab webhooks: {}", e);
                            tracing::warn!("{}", warning);
                            failure_warnings.push(warning);
                        }
                    }
                } else if gitlab_config.groups.is_empty() {
                    notes.push(
                        "GitLab is enabled but no groups are configured; skipping GitLab webhook auto-setup."
                            .to_string(),
                    );
                } else if gitlab_config.token.is_none() {
                    notes.push(
                        "GitLab is enabled but no token is configured; skipping GitLab webhook auto-setup."
                            .to_string(),
                    );
                }
            }
        }

        // Configure GitHub review webhooks (repo hooks -> /webhook/github)
        let github_config = self.config.github();
        if !github_config.repos.is_empty() && github_config.token.is_some() {
            attempted_auto_config = true;
            match self.configure_github(github_config, base_url).await {
                Ok((repo_count, generated_secret)) => {
                    configured_any = true;
                    if let Some(secret) = generated_secret {
                        env_updates.insert("GITHUB_WEBHOOK_SECRET".to_string(), secret);
                    }
                    let note = format!(
                        "GitHub review webhooks configured for {} repo(s)",
                        repo_count
                    );
                    tracing::info!("{}", note);
                    notes.push(note);
                }
                Err(e) => {
                    let warning = format!("Failed to configure GitHub webhooks: {}", e);
                    tracing::warn!("{}", warning);
                    failure_warnings.push(warning);
                }
            }
        } else if !github_config.repos.is_empty() && github_config.token.is_none() {
            if self.config.github_app().is_configured() {
                notes.push(
                    "GitHub repos are configured but no PAT is set; GitHub App webhook setup is managed separately (manifest/app settings) and is not auto-configured here yet.".to_string(),
                );
            } else {
                notes.push(
                    "GitHub repos are configured but no GitHub token is available; skipping GitHub webhook auto-setup.".to_string(),
                );
            }
        }

        // Write secrets to .env file
        if !env_updates.is_empty() {
            tracing::info!("Writing secrets to {}", self.env_path.display());
            update_env_file(&self.env_path, &env_updates)?;
        }

        result.warnings.extend(failure_warnings.clone());
        result.warnings.extend(notes);

        if !configured_any {
            if attempted_auto_config && !failure_warnings.is_empty() {
                return Err(Error::config(format!(
                    "Failed to configure any webhooks: {}",
                    failure_warnings.join("; ")
                )));
            }
            if !attempted_auto_config && result.warnings.is_empty() {
                return Err(Error::config(
                    "No webhook sources are enabled. Enable Linear, Sentry, GitHub (with repos), GitLab (with groups), Jira, Telegram source, Slack source, or WhatsApp source in your configuration."
                ));
            }
            if !attempted_auto_config {
                return Err(Error::config(format!(
                    "No auto-configurable webhook services are enabled. {}",
                    result.warnings.join("; ")
                )));
            }
        }

        Ok(result)
    }

    async fn configure_github(
        &self,
        config: &GitHubConfig,
        base_url: &str,
    ) -> Result<(usize, Option<String>)> {
        let token = config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitHub token is required for webhook auto-setup"))?
            .expose()
            .to_string();
        if token.is_empty() {
            return Err(Error::config(
                "GitHub token is required for webhook auto-setup",
            ));
        }
        if config.repos.is_empty() {
            return Err(Error::config(
                "GitHub repos must be configured for webhook auto-setup",
            ));
        }

        let callback_url = format!("{}/webhook/github", base_url.trim_end_matches('/'));
        let secret = config
            .webhook_secret
            .as_ref()
            .map(|s| s.expose().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "ghwh_{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                )
            });
        let generated_secret = config
            .webhook_secret
            .as_ref()
            .map(|s| s.expose().is_empty())
            .unwrap_or(true)
            .then(|| secret.clone());

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut configured_count = 0usize;
        let mut repo_failures: Vec<String> = Vec::new();

        for repo in &config.repos {
            match self
                .ensure_github_repo_webhook(&client, &token, repo, &callback_url, &secret)
                .await
            {
                Ok(_) => configured_count += 1,
                Err(e) => repo_failures.push(format!("{} ({})", repo, e)),
            }
        }

        if configured_count == 0 {
            return Err(Error::api(format!(
                "GitHub webhook setup failed for all repos: {}",
                repo_failures.join("; ")
            )));
        }

        if !repo_failures.is_empty() {
            tracing::warn!(
                "GitHub webhook setup partially succeeded: configured {} repo(s), failed for {}",
                configured_count,
                repo_failures.join("; ")
            );
        }

        Ok((configured_count, generated_secret))
    }

    async fn configure_jira(&self, config: &JiraConfig, base_url: &str) -> Result<bool> {
        let callback_url = format!("{}/webhook/jira", base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let auth_header = Self::jira_auth_header(config);

        if self
            .ensure_jira_webhook(&client, &auth_header, &config.base_url, &callback_url)
            .await?
        {
            return Ok(true);
        }

        Ok(false)
    }

    async fn configure_telegram(
        &self,
        config: &TelegramConfig,
        base_url: &str,
    ) -> Result<Option<String>> {
        let token = config
            .bot_token
            .as_ref()
            .ok_or_else(|| Error::config("Telegram bot_token is required for webhook auto-setup"))?
            .expose()
            .trim()
            .to_string();
        if token.is_empty() {
            return Err(Error::config(
                "Telegram bot_token is required for webhook auto-setup",
            ));
        }

        let callback_url = format!("{}/webhook/telegram", base_url.trim_end_matches('/'));
        let secret = std::env::var("TELEGRAM_WEBHOOK_SECRET")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "tgwh_{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                )
            });
        let generated_secret = std::env::var("TELEGRAM_WEBHOOK_SECRET")
            .ok()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
            .then(|| secret.clone());

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let url = format!("https://api.telegram.org/bot{}/setWebhook", token);
        let payload = serde_json::json!({
            "url": callback_url,
            "allowed_updates": ["message"],
            "secret_token": secret,
        });
        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| Error::api(format!("Telegram setWebhook request failed: {}", e)))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::api(format!(
                "Telegram setWebhook returned {}: {}",
                status, body
            )));
        }

        let parsed: TelegramApiResponse = serde_json::from_str(&body).map_err(|e| {
            Error::api(format!(
                "Failed to parse Telegram setWebhook response: {}",
                e
            ))
        })?;
        if !parsed.ok {
            return Err(Error::api(format!(
                "Telegram setWebhook failed: {}",
                parsed
                    .description
                    .unwrap_or_else(|| "unknown error".to_string())
            )));
        }

        Ok(generated_secret)
    }

    async fn configure_slack(&self, config: &SlackSourceConfig, base_url: &str) -> Result<bool> {
        let app_id = config
            .app_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::config("Slack app_id is required for webhook auto-setup"))?;
        let app_config_token = config
            .app_config_token
            .as_ref()
            .map(|s| s.expose().trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::config("Slack app_config_token is required for webhook auto-setup")
            })?;

        let callback_url = format!("{}/webhook/slack", base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let export_resp = client
            .post("https://slack.com/api/apps.manifest.export")
            .header("Authorization", format!("Bearer {}", app_config_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "app_id": app_id }))
            .send()
            .await
            .map_err(|e| Error::api(format!("Slack apps.manifest.export request failed: {}", e)))?;
        let export_status = export_resp.status();
        let export_body = export_resp.text().await.unwrap_or_default();
        if !export_status.is_success() {
            return Err(Error::api(format!(
                "Slack apps.manifest.export returned {}: {}",
                export_status, export_body
            )));
        }

        let export_json: serde_json::Value = serde_json::from_str(&export_body)
            .map_err(|e| Error::api(format!("Failed to parse Slack export response: {}", e)))?;
        let ok_resp: SlackApiOkResponse = serde_json::from_value(export_json.clone())
            .map_err(|e| Error::api(format!("Failed to parse Slack ok/error fields: {}", e)))?;
        if !ok_resp.ok {
            return Err(Error::api(format!(
                "Slack apps.manifest.export failed: {}",
                ok_resp.error.unwrap_or_else(|| "unknown error".to_string())
            )));
        }

        let mut manifest = match export_json.get("manifest") {
            Some(serde_json::Value::Object(obj)) => serde_json::Value::Object(obj.clone()),
            Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s)
                .map_err(|e| Error::api(format!("Failed to parse Slack manifest JSON: {}", e)))?,
            _ => {
                return Err(Error::api(
                    "Slack apps.manifest.export response missing manifest".to_string(),
                ))
            }
        };

        let changed = Self::ensure_slack_events_subscription(&mut manifest, &callback_url)?;
        if !changed {
            return Ok(false);
        }

        let update_resp = client
            .post("https://slack.com/api/apps.manifest.update")
            .header("Authorization", format!("Bearer {}", app_config_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "app_id": app_id,
                "manifest": serde_json::to_string(&manifest)
                    .map_err(|e| Error::api(format!("Failed to serialize Slack manifest: {}", e)))?
            }))
            .send()
            .await
            .map_err(|e| Error::api(format!("Slack apps.manifest.update request failed: {}", e)))?;
        let update_status = update_resp.status();
        let update_body = update_resp.text().await.unwrap_or_default();
        if !update_status.is_success() {
            return Err(Error::api(format!(
                "Slack apps.manifest.update returned {}: {}",
                update_status, update_body
            )));
        }

        let update_json: SlackApiOkResponse = serde_json::from_str(&update_body)
            .map_err(|e| Error::api(format!("Failed to parse Slack update response: {}", e)))?;
        if !update_json.ok {
            return Err(Error::api(format!(
                "Slack apps.manifest.update failed: {}",
                update_json
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            )));
        }

        Ok(true)
    }

    async fn configure_whatsapp(
        &self,
        config: &WhatsAppConfig,
        base_url: &str,
    ) -> Result<Option<String>> {
        let access_token = config
            .access_token
            .as_ref()
            .map(|s| s.expose().trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::config("WhatsApp access_token is required for webhook auto-setup")
            })?;
        let business_account_id = config
            .business_account_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::config("WhatsApp business_account_id is required for webhook auto-setup")
            })?;

        let callback_url = format!("{}/webhook/whatsapp", base_url.trim_end_matches('/'));
        let verify_token = config
            .webhook_verify_token
            .as_ref()
            .map(|s| s.expose().trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("WHATSAPP_WEBHOOK_VERIFY_TOKEN")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| {
                format!(
                    "wawv_{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                )
            });
        let generated_verify_token = config
            .webhook_verify_token
            .as_ref()
            .map(|s| s.expose().trim().is_empty())
            .unwrap_or(true)
            .then(|| verify_token.clone());

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let url = format!(
            "https://graph.facebook.com/v21.0/{}/subscribed_apps",
            business_account_id
        );

        let resp = client
            .post(&url)
            .bearer_auth(access_token)
            .json(&serde_json::json!({
                "override_callback_uri": callback_url,
                "verify_token": verify_token,
            }))
            .send()
            .await
            .map_err(|e| Error::api(format!("WhatsApp subscribed_apps request failed: {}", e)))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::api(format!(
                "WhatsApp subscribed_apps returned {}: {}",
                status, body
            )));
        }

        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| Error::api(format!("Failed to parse WhatsApp response: {}", e)))?;
        if parsed
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
        {
            return Ok(generated_verify_token);
        }

        Err(Error::api(format!(
            "WhatsApp subscribed_apps failed: {}",
            body
        )))
    }

    async fn configure_gitlab(
        &self,
        config: &GitLabConfig,
        base_url: &str,
    ) -> Result<(usize, Option<String>)> {
        let token = config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token is required for webhook auto-setup"))?
            .expose()
            .to_string();
        if token.is_empty() {
            return Err(Error::config(
                "GitLab token is required for webhook auto-setup",
            ));
        }

        let groups: Vec<&str> = config
            .groups
            .iter()
            .map(|g| g.trim())
            .filter(|g| !g.is_empty())
            .collect();
        if groups.is_empty() {
            return Err(Error::config(
                "GitLab groups must be configured for webhook auto-setup",
            ));
        }

        let callback_url = format!("{}/webhook/gitlab", base_url.trim_end_matches('/'));
        let secret = config
            .webhook_secret
            .as_ref()
            .map(|s| s.expose().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "glwh_{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                )
            });
        let generated_secret = config
            .webhook_secret
            .as_ref()
            .map(|s| s.expose().is_empty())
            .unwrap_or(true)
            .then(|| secret.clone());

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut configured_count = 0usize;
        let mut group_failures: Vec<String> = Vec::new();

        for group in groups {
            match self
                .ensure_gitlab_group_webhook(
                    &client,
                    &token,
                    &config.base_url,
                    group,
                    &callback_url,
                    &secret,
                )
                .await
            {
                Ok(_) => configured_count += 1,
                Err(e) => group_failures.push(format!("{} ({})", group, e)),
            }
        }

        if configured_count == 0 {
            return Err(Error::api(format!(
                "GitLab webhook setup failed for all groups: {}",
                group_failures.join("; ")
            )));
        }

        if !group_failures.is_empty() {
            tracing::warn!(
                "GitLab webhook setup partially succeeded: configured {} group(s), failed for {}",
                configured_count,
                group_failures.join("; ")
            );
        }

        Ok((configured_count, generated_secret))
    }

    fn ensure_slack_events_subscription(
        manifest: &mut serde_json::Value,
        callback_url: &str,
    ) -> Result<bool> {
        let root = manifest
            .as_object_mut()
            .ok_or_else(|| Error::api("Slack manifest is not a JSON object"))?;

        let settings = root
            .entry("settings")
            .or_insert_with(|| serde_json::json!({}));
        if !settings.is_object() {
            *settings = serde_json::json!({});
        }
        let settings_obj = settings
            .as_object_mut()
            .ok_or_else(|| Error::api("Slack manifest.settings is not an object"))?;

        let event_subs = settings_obj
            .entry("event_subscriptions")
            .or_insert_with(|| serde_json::json!({}));
        if !event_subs.is_object() {
            *event_subs = serde_json::json!({});
        }
        let event_subs_obj = event_subs
            .as_object_mut()
            .ok_or_else(|| Error::api("Slack manifest.settings.event_subscriptions invalid"))?;

        let mut changed = false;
        if event_subs_obj.get("request_url").and_then(|v| v.as_str()) != Some(callback_url) {
            event_subs_obj.insert(
                "request_url".to_string(),
                serde_json::Value::String(callback_url.to_string()),
            );
            changed = true;
        }

        let bot_events = event_subs_obj
            .entry("bot_events")
            .or_insert_with(|| serde_json::json!([]));
        if !bot_events.is_array() {
            *bot_events = serde_json::json!([]);
            changed = true;
        }
        let arr = bot_events.as_array_mut().ok_or_else(|| {
            Error::api("Slack manifest.settings.event_subscriptions.bot_events invalid")
        })?;

        for event in [
            "message.channels",
            "message.groups",
            "message.im",
            "message.mpim",
        ] {
            if !arr.iter().any(|v| v.as_str() == Some(event)) {
                arr.push(serde_json::Value::String(event.to_string()));
                changed = true;
            }
        }

        Ok(changed)
    }

    async fn ensure_jira_webhook(
        &self,
        client: &reqwest::Client,
        auth_header: &str,
        jira_base_url: &str,
        callback_url: &str,
    ) -> Result<bool> {
        let hooks_url = format!(
            "{}/rest/webhooks/1.0/webhook",
            jira_base_url.trim_end_matches('/')
        );

        let list_resp = client
            .get(&hooks_url)
            .header("Authorization", auth_header)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| Error::api(format!("Jira list webhooks request failed: {}", e)))?;

        if list_resp.status().is_success() {
            let body = list_resp.text().await.unwrap_or_default();
            let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
                Error::api(format!("Failed to parse Jira webhook list response: {}", e))
            })?;
            let exists = parsed.as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("url")
                        .and_then(|v| v.as_str())
                        .is_some_and(|u| u == callback_url)
                })
            });
            if exists {
                return Ok(false);
            }
        } else {
            let status = list_resp.status();
            let body = list_resp.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "Jira list webhooks returned {}: {}",
                status, body
            )));
        }

        let payload = serde_json::json!({
            "name": "claudear",
            "url": callback_url,
            "events": ["jira:issue_created", "jira:issue_updated"],
            "excludeBody": false
        });

        let create_resp = client
            .post(&hooks_url)
            .header("Authorization", auth_header)
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| Error::api(format!("Jira create webhook request failed: {}", e)))?;

        if create_resp.status().is_success() {
            return Ok(true);
        }

        let status = create_resp.status();
        let body = create_resp.text().await.unwrap_or_default();
        if status.as_u16() == 400
            && (body.contains("already exists")
                || body.contains("already registered")
                || body.contains(callback_url))
        {
            return Ok(false);
        }

        Err(Error::api(format!(
            "Jira create webhook returned {}: {}",
            status, body
        )))
    }

    async fn ensure_github_repo_webhook(
        &self,
        client: &reqwest::Client,
        token: &str,
        repo: &str,
        callback_url: &str,
        secret: &str,
    ) -> Result<()> {
        let hooks_url = format!("https://api.github.com/repos/{}/hooks", repo);
        let list_resp = client
            .get(&hooks_url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "claudear")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::api(format!("GitHub list hooks request failed: {}", e)))?;

        if !list_resp.status().is_success() {
            let status = list_resp.status();
            let body = list_resp.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "GitHub list hooks returned {}: {}",
                status, body
            )));
        }

        let hooks: Vec<GitHubHookListItem> = list_resp
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse GitHub hooks response: {}", e)))?;

        if hooks
            .iter()
            .any(|h| h.config.url.as_deref().is_some_and(|u| u == callback_url))
        {
            return Ok(());
        }

        let payload = serde_json::json!({
            "name": "web",
            "active": true,
            "events": ["pull_request_review", "pull_request_review_comment"],
            "config": {
                "url": callback_url,
                "content_type": "json",
                "secret": secret,
                "insecure_ssl": "0"
            }
        });

        let create_resp = client
            .post(&hooks_url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "claudear")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&payload)
            .send()
            .await
            .map_err(|e| Error::api(format!("GitHub create hook request failed: {}", e)))?;

        if create_resp.status().is_success() {
            return Ok(());
        }

        let status = create_resp.status();
        let body = create_resp.text().await.unwrap_or_default();
        if status.as_u16() == 422
            && (body.contains("Hook already exists") || body.contains("already exists"))
        {
            return Ok(());
        }

        Err(Error::api(format!(
            "GitHub create hook returned {}: {}",
            status, body
        )))
    }

    async fn ensure_gitlab_group_webhook(
        &self,
        client: &reqwest::Client,
        token: &str,
        gitlab_base_url: &str,
        group: &str,
        callback_url: &str,
        secret: &str,
    ) -> Result<()> {
        let encoded_group: String =
            url::form_urlencoded::byte_serialize(group.as_bytes()).collect();
        let hooks_url = format!(
            "{}/api/v4/groups/{}/hooks",
            gitlab_base_url.trim_end_matches('/'),
            encoded_group
        );

        let list_resp = client
            .get(&hooks_url)
            .header("PRIVATE-TOKEN", token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| Error::api(format!("GitLab list hooks request failed: {}", e)))?;

        if !list_resp.status().is_success() {
            let status = list_resp.status();
            let body = list_resp.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "GitLab list hooks returned {}: {}",
                status, body
            )));
        }

        let hooks: Vec<GitLabHookListItem> = list_resp
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse GitLab hooks response: {}", e)))?;

        if hooks
            .iter()
            .any(|h| h.url.as_deref().is_some_and(|u| u == callback_url))
        {
            return Ok(());
        }

        let payload = serde_json::json!({
            "url": callback_url,
            "token": secret,
            "issues_events": true,
            "enable_ssl_verification": true
        });

        let create_resp = client
            .post(&hooks_url)
            .header("PRIVATE-TOKEN", token)
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| Error::api(format!("GitLab create hook request failed: {}", e)))?;

        if create_resp.status().is_success() {
            return Ok(());
        }

        let status = create_resp.status();
        let body = create_resp.text().await.unwrap_or_default();
        if (status.as_u16() == 400 || status.as_u16() == 409)
            && (body.contains("already been taken")
                || body.contains("Hook already exists")
                || body.contains("has already been taken"))
        {
            return Ok(());
        }

        Err(Error::api(format!(
            "GitLab create hook returned {}: {}",
            status, body
        )))
    }

    fn jira_auth_header(config: &JiraConfig) -> String {
        if config.auth_mode.eq_ignore_ascii_case("bearer") {
            format!("Bearer {}", config.api_token.expose())
        } else {
            let credentials = format!("{}:{}", config.email, config.api_token.expose());
            let encoded = {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes())
            };
            format!("Basic {}", encoded)
        }
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
            .linear()
            .is_some_and(|c| c.enabled && c.webhook_secret.is_none());

        let sentry_needs = self
            .config
            .issues
            .sentry
            .as_ref()
            .is_some_and(|c| c.enabled && c.client_secret.is_none());

        let github = self.config.github();
        let github_needs =
            !github.repos.is_empty() && github.token.is_some() && github.webhook_secret.is_none();

        let gitlab_needs = self.config.gitlab().is_some_and(|c| {
            c.enabled && !c.groups.is_empty() && c.token.is_some() && c.webhook_secret.is_none()
        });

        let jira_needs = self
            .config
            .jira()
            .is_some_and(|c| c.enabled && !c.base_url.trim().is_empty() && !c.api_token.is_empty());

        let telegram_needs = self.config.notifiers.telegram.source_enabled
            && self
                .config
                .notifiers
                .telegram
                .bot_token
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty());

        let slack_needs = self.config.issues.slack.as_ref().is_some_and(|c| {
            c.app_id.as_ref().is_some_and(|v| !v.trim().is_empty())
                && c.app_config_token
                    .as_ref()
                    .is_some_and(|s| !s.expose().trim().is_empty())
                && c.signing_secret
                    .as_ref()
                    .is_some_and(|s| !s.expose().trim().is_empty())
        });

        let whatsapp = &self.config.notifiers.whatsapp;
        let whatsapp_needs = whatsapp.source_enabled
            && whatsapp
                .access_token
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty())
            && whatsapp
                .business_account_id
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty())
            && whatsapp
                .app_secret
                .as_ref()
                .is_some_and(|s| !s.expose().trim().is_empty());

        linear_needs
            || sentry_needs
            || github_needs
            || gitlab_needs
            || jira_needs
            || telegram_needs
            || slack_needs
            || whatsapp_needs
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
        println!("Warnings / Notes:");
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
            workspace: "/tmp/repos".into(),
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
            scm: crate::config::ScmConfig::default(),
            issues: crate::config::IssuesConfig::default(),
            notifiers: crate::config::NotifiersConfig::default(),
            ask: crate::config::AskConfig::default(),
            retry: crate::config::RetryConfig::default(),
            regression: crate::config::RegressionConfig::default(),
            cascade: crate::config::CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: crate::config::LearningConfig::default(),
            prioritisation: crate::config::PrioritisationConfig::default(),
            code_index: crate::config::CodeIndexConfig::default(),
            evaluation: crate::config::EvaluationConfig::default(),
            storage_dir: "/tmp/claudear-storage".into(),
            dashboard: crate::config::DashboardConfig::default(),
            tenant_id: None,
            database_url: None,
            redis_url: None,
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test".into(),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test".into(),
            trigger_labels: vec![],
            trigger_states: vec![],
            team_id: None,
            project_id: None,
            webhook_secret: Some("secret".into()), // Has secret
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
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sentry-secret".into()),
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
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: None,
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("linear-secret".into()),
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: false,
            webhook_secret: None,
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.workspace = "/custom/dir".into();
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "key-123".into(),
            webhook_secret: Some("secret".into()),
            ..Default::default()
        });

        let configurator = WebhookConfigurator::new(config, "/tmp/.env");

        assert_eq!(configurator.config.webhook_port, 5555);
        assert_eq!(
            configurator.config.workspace,
            std::path::PathBuf::from("/custom/dir")
        );
        assert!(configurator.config.issues.linear.is_some());
        assert_eq!(
            configurator
                .config
                .issues
                .linear
                .as_ref()
                .unwrap()
                .api_key
                .expose(),
            "key-123"
        );
    }

    #[test]
    fn test_needs_configuration_linear_disabled_no_secret() {
        let mut config = test_config();
        config.issues.linear = Some(crate::config::LinearConfig {
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: false,
            webhook_secret: Some("has-secret".into()),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: None,
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sentry-secret".into()),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("lin-secret".into()),
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sen-secret".into()),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("secret".into()),
            ..Default::default()
        });
        // No sentry at all
        config.issues.sentry = None;
        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(
            !configurator.needs_configuration(),
            "Only Linear enabled with secret should not need config"
        );
    }

    #[test]
    fn test_needs_configuration_only_sentry_enabled_and_configured() {
        let mut config = test_config();
        config.issues.linear = None;
        config.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("secret".into()),
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
        config.issues.linear = None;
        config.issues.sentry = Some(crate::config::SentryConfig {
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
    fn test_needs_configuration_github_repos_no_webhook_secret() {
        let mut config = test_config();
        config.scm.github.token = Some("ghp_test".into());
        config.scm.github.repos = vec!["owner/repo".to_string()];
        config.scm.github.webhook_secret = None;

        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(configurator.needs_configuration());
    }

    #[test]
    fn test_needs_configuration_github_repos_with_webhook_secret() {
        let mut config = test_config();
        config.scm.github.token = Some("ghp_test".into());
        config.scm.github.repos = vec!["owner/repo".to_string()];
        config.scm.github.webhook_secret = Some("gh-secret".into());

        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(!configurator.needs_configuration());
    }

    #[test]
    fn test_needs_configuration_gitlab_groups_no_webhook_secret() {
        let mut config = test_config();
        config.scm.gitlab = Some(crate::config::GitLabConfig {
            enabled: true,
            token: Some("glpat_test".into()),
            groups: vec!["group/subgroup".to_string()],
            webhook_secret: None,
            ..Default::default()
        });

        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(configurator.needs_configuration());
    }

    #[test]
    fn test_needs_configuration_gitlab_groups_with_webhook_secret() {
        let mut config = test_config();
        config.scm.gitlab = Some(crate::config::GitLabConfig {
            enabled: true,
            token: Some("glpat_test".into()),
            groups: vec!["group/subgroup".to_string()],
            webhook_secret: Some("gl-secret".into()),
            ..Default::default()
        });

        let configurator = WebhookConfigurator::new(config, "/tmp/.env");
        assert!(!configurator.needs_configuration());
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
        config1.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: None,
            ..Default::default()
        });
        config1.issues.sentry = Some(crate::config::SentryConfig {
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
        config2.issues.linear = Some(crate::config::LinearConfig {
            enabled: false,
            webhook_secret: None,
            ..Default::default()
        });
        config2.issues.sentry = Some(crate::config::SentryConfig {
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
        config3.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("secret".into()),
            ..Default::default()
        });
        config3.issues.sentry = Some(crate::config::SentryConfig {
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
        config4.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            webhook_secret: Some("lin-secret".into()),
            ..Default::default()
        });
        config4.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            client_secret: Some("sen-secret".into()),
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
    async fn test_configure_slack_source_only_returns_manual_setup_note() {
        let mut config = test_config();
        config.issues.slack = Some(crate::config::SlackSourceConfig {
            bot_token: Some("xoxb-test".into()),
            channel_id: Some("C123".to_string()),
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-slack-manual.env");

        let result = configurator.configure("https://example.com").await;
        assert!(
            result.is_err(),
            "configure should error without auto-configurable sources"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("No auto-configurable webhook services are enabled"));
        assert!(err_msg.contains("Slack source is configured"));
    }

    #[tokio::test]
    async fn test_configure_jira_enabled_attempts_auto_setup() {
        let mut config = test_config();
        config.issues.jira = Some(crate::config::JiraConfig {
            enabled: true,
            base_url: "https://example.atlassian.net".to_string(),
            api_token: "jira-token".into(),
            ..Default::default()
        });
        let configurator = WebhookConfigurator::new(config, "/tmp/test-jira-manual.env");

        let result = configurator.configure("https://example.com").await;
        assert!(
            result.is_err(),
            "configure should error when Jira API setup fails"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to configure any webhooks"));
        assert!(err_msg.contains("Jira"));
    }

    #[tokio::test]
    async fn test_configure_linear_disabled_sentry_disabled_returns_error() {
        // Both sources present but disabled => error about no sources enabled
        let mut config = test_config();
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: false,
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: false,
            ..Default::default()
        });
        config.issues.sentry = None;
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
        config.issues.linear = None;
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "invalid-api-key".into(),
            trigger_labels: vec![],
            trigger_states: vec![],
            team_id: None,
            project_id: None,
            webhook_secret: None,
            ..Default::default()
        });
        config.issues.sentry = None;
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
        config.issues.linear = None;
        config.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            auth_token: "invalid-token".into(),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "invalid-linear-key".into(),
            webhook_secret: None,
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            auth_token: "invalid-sentry-token".into(),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test-key".into(),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "test-key".into(),
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: true,
            api_key: "bad-key".into(),
            webhook_secret: None,
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
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
        config.issues.linear = Some(crate::config::LinearConfig {
            enabled: false,
            ..Default::default()
        });
        config.issues.sentry = Some(crate::config::SentryConfig {
            enabled: true,
            auth_token: "bad-token".into(),
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
