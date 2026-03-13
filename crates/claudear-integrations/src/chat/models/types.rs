//! Types for model browsing and management.

use serde::{Deserialize, Serialize};

/// A model returned from a provider search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub name: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub details: Option<ModelDetails>,
}

/// Details about a model's format and characteristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDetails {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub quantization_level: Option<String>,
}

/// Query parameters for listing/searching models.
#[derive(Debug, Clone, Deserialize)]
pub struct ListModelsQuery {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(alias = "q")]
    pub search: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    20
}

/// Response from a model browse/search operation.
#[derive(Debug, Clone, Serialize)]
pub struct BrowseResponse {
    pub models: Vec<ModelResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Request body for downloading a model.
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadRequest {
    #[serde(default)]
    pub url: Option<String>,
}

/// Response from a model info lookup.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfoResponse {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gguf_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<ModelDetails>,
}
