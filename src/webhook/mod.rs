//! Webhook handlers and HTTP server.

mod configurator;
mod linear;
mod linear_api;
mod sentry;
mod sentry_api;
mod server;

pub use configurator::{print_setup_result, WebhookConfigurator, WebhookSetupResult};
pub use linear::LinearWebhookHandler;
pub use linear_api::{LinearApiClient, WebhookRegistration};
pub use sentry::SentryWebhookHandler;
pub use sentry_api::{SentryApiClient, SentryWebhookRegistration};
pub use server::WebhookServer;

use crate::error::Result;
use crate::types::{Issue, MatchResult};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Trait for webhook handlers.
#[async_trait]
pub trait WebhookHandler: Send + Sync {
    /// Source name this handler is for.
    fn source_name(&self) -> &str;

    /// Verify the webhook signature/authenticity.
    fn verify_signature(&self, payload: &[u8], headers: &HashMap<String, String>) -> bool;

    /// Parse the webhook payload into an Issue.
    /// Returns None if this webhook should be ignored.
    async fn parse_payload(&self, payload: &serde_json::Value) -> Result<Option<Issue>>;

    /// Check if the parsed issue matches processing criteria.
    fn matches_criteria(&self, issue: &Issue) -> MatchResult;

    /// Build context for Claude from the issue.
    async fn build_issue_context(&self, issue: &Issue) -> Result<String>;
}

/// Registry for webhook handlers.
pub struct WebhookHandlerRegistry {
    handlers: HashMap<String, Arc<dyn WebhookHandler>>,
}

impl WebhookHandlerRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler.
    pub fn register(&mut self, handler: Arc<dyn WebhookHandler>) {
        self.handlers
            .insert(handler.source_name().to_string(), handler);
    }

    /// Get a handler by source name.
    pub fn get(&self, source_name: &str) -> Option<&Arc<dyn WebhookHandler>> {
        self.handlers.get(source_name)
    }

    /// Get all registered handlers.
    pub fn get_all(&self) -> Vec<&Arc<dyn WebhookHandler>> {
        self.handlers.values().collect()
    }

    /// Check if a handler is registered.
    pub fn has(&self, source_name: &str) -> bool {
        self.handlers.contains_key(source_name)
    }
}

impl Default for WebhookHandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
