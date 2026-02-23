//! WhatsApp Cloud API webhook handler.

use super::WebhookHandler;
use crate::ask_reply_inbox;
use crate::config::WhatsAppConfig;
use crate::error::Result;
use crate::secret::OptionalSecretExt;
use crate::types::{Issue, MatchPriority, MatchResult};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use subtle::ConstantTimeEq;

/// Webhook handler for WhatsApp inbound messages.
pub struct WhatsAppWebhookHandler {
    config: WhatsAppConfig,
}

impl WhatsAppWebhookHandler {
    /// Create a new WhatsApp webhook handler.
    pub fn new(config: WhatsAppConfig) -> Self {
        Self { config }
    }

    fn listen_phone_number_id(&self) -> Option<&str> {
        self.config
            .listen_phone_number_id
            .as_deref()
            .or(self.config.phone_number_id.as_deref())
    }

    fn extract_title(text: &str) -> String {
        let first_line = text.lines().next().unwrap_or(text);
        if first_line.len() > 100 {
            format!("{}...", &first_line[..first_line.floor_char_boundary(97)])
        } else {
            first_line.to_string()
        }
    }
}

#[async_trait]
impl WebhookHandler for WhatsAppWebhookHandler {
    fn source_name(&self) -> &str {
        "whatsapp"
    }

    fn verify_signature(&self, payload: &[u8], headers: &HashMap<String, String>) -> bool {
        let secret = match self.config.app_secret.expose_as_deref() {
            Some(s) if !s.is_empty() => s,
            _ => {
                tracing::error!(
                    source = "whatsapp",
                    "No WhatsApp app_secret configured - rejecting webhook for security"
                );
                return false;
            }
        };

        let signature = match headers.get("x-hub-signature-256") {
            Some(v) => v,
            None => return false,
        };
        let sig_hex = match signature.strip_prefix("sha256=") {
            Some(v) => v,
            None => return false,
        };

        let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => return false,
        };
        mac.update(payload);
        let expected_hex = hex::encode(mac.finalize().into_bytes());
        sig_hex.as_bytes().ct_eq(expected_hex.as_bytes()).into()
    }

    async fn parse_payload(&self, payload: &serde_json::Value) -> Result<Option<Issue>> {
        let entries = match payload.get("entry").and_then(|v| v.as_array()) {
            Some(v) => v,
            None => return Ok(None),
        };

        for entry in entries {
            let changes = match entry.get("changes").and_then(|v| v.as_array()) {
                Some(v) => v,
                None => continue,
            };

            for change in changes {
                let value = match change.get("value") {
                    Some(v) => v,
                    None => continue,
                };

                let phone_number_id = value
                    .get("metadata")
                    .and_then(|m| m.get("phone_number_id"))
                    .and_then(|v| v.as_str());
                if let (Some(expected), Some(actual)) =
                    (self.listen_phone_number_id(), phone_number_id)
                {
                    if expected != actual {
                        continue;
                    }
                }

                let messages = match value.get("messages").and_then(|v| v.as_array()) {
                    Some(v) => v,
                    None => continue,
                };

                for msg in messages {
                    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if msg_type != "text" {
                        continue;
                    }

                    let text = match msg
                        .get("text")
                        .and_then(|t| t.get("body"))
                        .and_then(|v| v.as_str())
                    {
                        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
                        _ => continue,
                    };

                    let message_id = match msg.get("id").and_then(|v| v.as_str()) {
                        Some(v) if !v.is_empty() => v.to_string(),
                        _ => continue,
                    };
                    let from = msg
                        .get("from")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let timestamp = msg
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let context_message_id = msg
                        .get("context")
                        .and_then(|c| c.get("id"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let replied_at = timestamp
                        .parse::<i64>()
                        .ok()
                        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
                        .unwrap_or_else(Utc::now);

                    ask_reply_inbox::record_whatsapp_message(
                        ask_reply_inbox::WhatsAppInboundMessage {
                            message_id: message_id.clone(),
                            from: from.clone(),
                            text: text.clone(),
                            replied_at,
                            context_message_id: context_message_id.clone(),
                        },
                    );

                    let short_id = format!("WA-{}", message_id.chars().take(8).collect::<String>());
                    let title = Self::extract_title(&text);
                    let mut issue = Issue::new(&message_id, &short_id, &title, "", "whatsapp");
                    issue.description = Some(text);
                    issue.set_metadata("author_phone", &from);
                    issue.set_metadata("message_id", &message_id);
                    if let Some(phone_number_id) = phone_number_id {
                        issue.set_metadata("phone_number_id", phone_number_id);
                    }

                    return Ok(Some(issue));
                }
            }
        }

        Ok(None)
    }

    fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
        MatchResult::matched("whatsapp_message", MatchPriority::Normal)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("WhatsApp Message Issue: {}\n", issue.title);
        if let Some(desc) = &issue.description {
            context.push_str("\nMessage:\n");
            context.push_str(desc);
            context.push('\n');
        }
        if let Some(phone) = issue.get_metadata::<String>("author_phone") {
            context.push_str(&format!("\nAuthor Phone: {}\n", phone));
        }
        Ok(context)
    }
}
