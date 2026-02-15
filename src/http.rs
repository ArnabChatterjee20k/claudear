//! Shared HTTP response type used by source and client HTTP abstractions.

use crate::error::{Error, Result};

/// HTTP response abstraction for testability.
///
/// Used by source adapters (Linear, Sentry) and API clients (Discord)
/// to abstract over the actual HTTP client for unit testing.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    /// Check if the status is successful (2xx).
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Check if the status is 404 Not Found.
    pub fn is_not_found(&self) -> bool {
        self.status == 404
    }

    /// Parse the body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body)
            .map_err(|e| Error::Other(format!("JSON parse error: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_response_success_200() {
        let resp = HttpResponse {
            status: 200,
            body: String::new(),
        };
        assert!(resp.is_success());
    }

    #[test]
    fn test_http_response_success_299() {
        let resp = HttpResponse {
            status: 299,
            body: String::new(),
        };
        assert!(resp.is_success());
    }

    #[test]
    fn test_http_response_failure_400() {
        let resp = HttpResponse {
            status: 400,
            body: String::new(),
        };
        assert!(!resp.is_success());
    }

    #[test]
    fn test_http_response_failure_500() {
        let resp = HttpResponse {
            status: 500,
            body: String::new(),
        };
        assert!(!resp.is_success());
    }

    #[test]
    fn test_http_response_json_valid() {
        let resp = HttpResponse {
            status: 200,
            body: r#"{"key": "value"}"#.to_string(),
        };
        let parsed: std::collections::HashMap<String, String> = resp.json().unwrap();
        assert_eq!(parsed.get("key").unwrap(), "value");
    }

    #[test]
    fn test_http_response_json_invalid() {
        let resp = HttpResponse {
            status: 200,
            body: "not json".to_string(),
        };
        let result: Result<serde_json::Value> = resp.json();
        assert!(result.is_err());
    }
}
