//! Raw Anthropic API streaming event types.
//!
//! These correspond to the wire format emitted by the Anthropic Messages API
//! (`message_start`, `content_block_delta`, etc.) rather than the higher-level
//! CLI format used by `claude --output-format stream-json`.

use serde::Deserialize;

/// Top-level NDJSON events from the raw Anthropic Messages streaming API.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ApiStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart {},
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        #[serde(default)]
        content_block: Option<ApiContentBlock>,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        #[serde(default)]
        delta: Option<ApiDelta>,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {},
    #[serde(rename = "message_delta")]
    MessageDelta {},
    #[serde(rename = "message_stop")]
    MessageStop {},
    /// Forward-compat: ignore unknown event types.
    #[serde(other)]
    Unknown,
}

/// Content block types within a `content_block_start` event.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ApiContentBlock {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
    },
    /// Forward-compat catch-all.
    #[serde(other)]
    Other,
}

/// Delta types within a `content_block_delta` event.
/// Variant names mirror the upstream wire format (`text_delta`, `input_json_delta`).
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ApiDelta {
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta {
        #[serde(default)]
        partial_json: String,
    },
    /// Forward-compat catch-all.
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_api_text_delta() {
        let line = r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        match event {
            ApiStreamEvent::ContentBlockDelta {
                delta: Some(ApiDelta::TextDelta { text }),
            } => assert_eq!(text, "hello"),
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_api_message_stop() {
        let line = r#"{"type":"message_stop"}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(event, ApiStreamEvent::MessageStop {}));
    }

    #[test]
    fn test_parse_api_tool_use_start() {
        let line = r#"{"type":"content_block_start","content_block":{"type":"tool_use","id":"tu_1","name":"Bash"}}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        match event {
            ApiStreamEvent::ContentBlockStart {
                content_block: Some(ApiContentBlock::ToolUse { id, name }),
            } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "Bash");
            }
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_api_unknown_type_forward_compat() {
        let line = r#"{"type":"some_future_event","data":"anything"}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(event, ApiStreamEvent::Unknown));
    }

    #[test]
    fn test_parse_api_message_start() {
        let line = r#"{"type":"message_start"}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(event, ApiStreamEvent::MessageStart {}));
    }

    #[test]
    fn test_parse_api_content_block_stop() {
        let line = r#"{"type":"content_block_stop"}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(event, ApiStreamEvent::ContentBlockStop {}));
    }

    #[test]
    fn test_parse_api_input_json_delta() {
        let line = r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"key\":"}}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        match event {
            ApiStreamEvent::ContentBlockDelta {
                delta: Some(ApiDelta::InputJsonDelta { partial_json }),
            } => assert_eq!(partial_json, r#"{"key":"#),
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_api_text_block_start() {
        let line = r#"{"type":"content_block_start","content_block":{"type":"text","text":""}}"#;
        let event: ApiStreamEvent = serde_json::from_str(line).unwrap();
        match event {
            ApiStreamEvent::ContentBlockStart {
                content_block: Some(ApiContentBlock::Text { text }),
            } => assert_eq!(text, ""),
            other => panic!("Unexpected event: {:?}", other),
        }
    }
}
