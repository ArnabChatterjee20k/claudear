//! Jira webhook handler.

use super::WebhookHandler;
use crate::source::{IssueSource, JiraSource};
use async_trait::async_trait;
use claudear_config::config::JiraConfig;
use claudear_core::error::Result;
use claudear_core::types::{Issue, MatchResult};
use std::collections::HashMap;

/// Webhook handler for Jira issue events.
///
/// This handler parses lightweight Jira webhook payloads and then fetches the
/// canonical issue from the Jira REST API using the existing `JiraSource`
/// mapping/criteria logic.
pub struct JiraWebhookHandler {
    source: JiraSource,
}

impl JiraWebhookHandler {
    /// Create a new Jira webhook handler.
    pub fn new(config: JiraConfig) -> Self {
        Self {
            source: JiraSource::new(config),
        }
    }
}

#[async_trait]
impl WebhookHandler for JiraWebhookHandler {
    fn source_name(&self) -> &str {
        "jira"
    }

    fn verify_signature(&self, _payload: &[u8], _headers: &HashMap<String, String>) -> bool {
        // Jira admin webhooks do not use a shared signing secret in the current config
        // model, so we accept requests and rely on endpoint secrecy + source filtering.
        true
    }

    async fn parse_payload(&self, payload: &serde_json::Value) -> Result<Option<Issue>> {
        let event = payload
            .get("webhookEvent")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Only issue lifecycle events are relevant to the Jira issue source.
        if !matches!(event, "jira:issue_created" | "jira:issue_updated") {
            return Ok(None);
        }

        let issue_key = match payload
            .get("issue")
            .and_then(|v| v.get("key"))
            .and_then(|v| v.as_str())
        {
            Some(v) if !v.trim().is_empty() => v.trim(),
            _ => return Ok(None),
        };

        let issue = self.source.get_issue(issue_key).await?;
        Ok(Some(issue))
    }

    fn matches_criteria(&self, issue: &Issue) -> MatchResult {
        self.source.matches_criteria(issue)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        self.source.build_issue_context(issue).await
    }
}
