//! Linear API client for webhook management.

use crate::config::LinearConfig;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Client for Linear GraphQL API webhook operations.
pub struct LinearApiClient {
    api_key: String,
    client: reqwest::Client,
}

/// Result of a webhook registration.
#[derive(Debug, Clone)]
pub struct WebhookRegistration {
    /// The webhook ID.
    pub id: String,
    /// The webhook URL.
    pub url: String,
    /// Whether the webhook is enabled.
    pub enabled: bool,
    /// The signing secret for HMAC verification.
    pub secret: String,
}

#[derive(Debug, Serialize)]
struct GraphQLRequest {
    query: String,
    variables: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct WebhookCreateResponse {
    #[serde(rename = "webhookCreate")]
    webhook_create: WebhookCreateResult,
}

#[derive(Debug, Deserialize)]
struct WebhookCreateResult {
    success: bool,
    webhook: Option<WebhookData>,
}

#[derive(Debug, Deserialize)]
struct WebhookData {
    id: String,
    url: String,
    enabled: bool,
    secret: String,
}

#[derive(Debug, Deserialize)]
struct WebhooksResponse {
    webhooks: WebhooksConnection,
}

#[derive(Debug, Deserialize)]
struct WebhooksConnection {
    nodes: Vec<WebhookNode>,
}

#[derive(Debug, Deserialize)]
struct WebhookNode {
    id: String,
    url: String,
    enabled: bool,
}

impl LinearApiClient {
    const API_URL: &'static str = "https://api.linear.app/graphql";

    /// Create a new Linear API client.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Create from a LinearConfig.
    pub fn from_config(config: &LinearConfig) -> Self {
        Self::new(config.api_key.clone())
    }

    /// Register a new webhook with Linear.
    ///
    /// # Arguments
    /// * `url` - The URL where Linear will send webhook events
    /// * `team_id` - Optional team ID to filter events (if None, receives all public team events)
    /// * `resource_types` - List of resource types to subscribe to (e.g., ["Issue"])
    ///
    /// # Returns
    /// The webhook registration result including the signing secret.
    pub async fn create_webhook(
        &self,
        url: &str,
        team_id: Option<&str>,
        resource_types: &[&str],
    ) -> Result<WebhookRegistration> {
        let mutation = r#"
            mutation WebhookCreate($input: WebhookCreateInput!) {
                webhookCreate(input: $input) {
                    success
                    webhook {
                        id
                        url
                        enabled
                        secret
                    }
                }
            }
        "#;

        let resource_types_json: Vec<serde_json::Value> = resource_types
            .iter()
            .map(|r| serde_json::Value::String(r.to_string()))
            .collect();

        let mut input = serde_json::json!({
            "url": url,
            "resourceTypes": resource_types_json,
        });

        if let Some(tid) = team_id {
            input["teamId"] = serde_json::Value::String(tid.to_string());
        } else {
            input["allPublicTeams"] = serde_json::Value::Bool(true);
        }

        let request = GraphQLRequest {
            query: mutation.to_string(),
            variables: serde_json::json!({ "input": input }),
        };

        let response = self
            .client
            .post(Self::API_URL)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to send request to Linear API: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::api(format!(
                "Linear API returned status {}: {}",
                status, body
            )));
        }

        let result: GraphQLResponse<WebhookCreateResponse> = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse Linear API response: {}", e)))?;

        if let Some(errors) = result.errors {
            let error_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
            return Err(Error::api(format!(
                "Linear API errors: {}",
                error_messages.join(", ")
            )));
        }

        let data = result
            .data
            .ok_or_else(|| Error::api("No data in Linear API response"))?;

        if !data.webhook_create.success {
            return Err(Error::api("Webhook creation failed"));
        }

        let webhook = data
            .webhook_create
            .webhook
            .ok_or_else(|| Error::api("No webhook data in response"))?;

        Ok(WebhookRegistration {
            id: webhook.id,
            url: webhook.url,
            enabled: webhook.enabled,
            secret: webhook.secret,
        })
    }

    /// List existing webhooks for the organization.
    pub async fn list_webhooks(&self) -> Result<Vec<(String, String, bool)>> {
        let query = r#"
            query {
                webhooks {
                    nodes {
                        id
                        url
                        enabled
                    }
                }
            }
        "#;

        let request = GraphQLRequest {
            query: query.to_string(),
            variables: serde_json::json!({}),
        };

        let response = self
            .client
            .post(Self::API_URL)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to send request to Linear API: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::api(format!(
                "Linear API returned status {}: {}",
                status, body
            )));
        }

        let result: GraphQLResponse<WebhooksResponse> = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse Linear API response: {}", e)))?;

        if let Some(errors) = result.errors {
            let error_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
            return Err(Error::api(format!(
                "Linear API errors: {}",
                error_messages.join(", ")
            )));
        }

        let data = result
            .data
            .ok_or_else(|| Error::api("No data in Linear API response"))?;

        Ok(data
            .webhooks
            .nodes
            .into_iter()
            .map(|w| (w.id, w.url, w.enabled))
            .collect())
    }

    /// Check if a webhook with the given URL already exists.
    pub async fn webhook_exists(&self, url: &str) -> Result<bool> {
        let webhooks = self.list_webhooks().await?;
        Ok(webhooks.iter().any(|(_, wh_url, _)| wh_url == url))
    }

    /// Delete a webhook by ID.
    pub async fn delete_webhook(&self, webhook_id: &str) -> Result<bool> {
        let mutation = r#"
            mutation WebhookDelete($id: String!) {
                webhookDelete(id: $id) {
                    success
                }
            }
        "#;

        let request = GraphQLRequest {
            query: mutation.to_string(),
            variables: serde_json::json!({ "id": webhook_id }),
        };

        let response = self
            .client
            .post(Self::API_URL)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to send request to Linear API: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::api(format!(
                "Linear API returned status {}: {}",
                status, body
            )));
        }

        #[derive(Debug, Deserialize)]
        struct DeleteResponse {
            #[serde(rename = "webhookDelete")]
            webhook_delete: DeleteResult,
        }

        #[derive(Debug, Deserialize)]
        struct DeleteResult {
            success: bool,
        }

        let result: GraphQLResponse<DeleteResponse> = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse Linear API response: {}", e)))?;

        if let Some(errors) = result.errors {
            let error_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
            return Err(Error::api(format!(
                "Linear API errors: {}",
                error_messages.join(", ")
            )));
        }

        let data = result
            .data
            .ok_or_else(|| Error::api("No data in Linear API response"))?;

        Ok(data.webhook_delete.success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_api_client_new() {
        let client = LinearApiClient::new("lin_api_key".to_string());
        assert_eq!(client.api_key, "lin_api_key");
    }

    #[test]
    fn test_linear_api_client_from_config() {
        let config = LinearConfig {
            enabled: true,
            api_key: "lin_config_key".to_string(),
            trigger_labels: vec![],
            trigger_states: vec![],
            team_id: None,
            project_id: None,
            webhook_secret: None,
        };
        let client = LinearApiClient::from_config(&config);
        assert_eq!(client.api_key, "lin_config_key");
    }

    #[test]
    fn test_webhook_registration_fields() {
        let registration = WebhookRegistration {
            id: "wh_123".to_string(),
            url: "https://example.com/webhook".to_string(),
            enabled: true,
            secret: "secret_abc".to_string(),
        };
        assert_eq!(registration.id, "wh_123");
        assert_eq!(registration.url, "https://example.com/webhook");
        assert!(registration.enabled);
        assert_eq!(registration.secret, "secret_abc");
    }

    #[test]
    fn test_graphql_request_serialization() {
        let request = GraphQLRequest {
            query: "query { test }".to_string(),
            variables: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("query"));
        assert!(json.contains("variables"));
    }
}
