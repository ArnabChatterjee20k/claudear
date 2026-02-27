//! Model providers for browsing available GGUF models.

use super::types::{BrowseResponse, ModelDetails, ModelResponse};
use super::HTTP_CLIENT;

/// Search HuggingFace for GGUF models.
pub struct HuggingFaceProvider;

impl HuggingFaceProvider {
    const API_BASE: &'static str = "https://huggingface.co/api/models";

    /// Search for GGUF models on HuggingFace.
    pub async fn search(
        query: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<BrowseResponse, reqwest::Error> {
        let mut url = format!(
            "{}?filter=gguf&sort=downloads&direction=-1&limit={}",
            Self::API_BASE,
            limit
        );

        if !query.is_empty() {
            url.push_str(&format!("&search={}", urlencoding::encode(query)));
        }

        // HuggingFace uses numeric offset-based pagination via the `p` query parameter
        // within the Link header. Our cursor is the offset string.
        if let Some(cursor_val) = cursor {
            if let Some(offset) = parse_cursor_offset(cursor_val) {
                url.push_str(&format!("&offset={}", offset));
            }
        }

        let response = HTTP_CLIENT.get(&url).send().await?.error_for_status()?;

        // Extract next cursor from Link header
        let next_cursor = response
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .and_then(extract_cursor_from_link_header);

        let hf_models: Vec<HfModelEntry> = response.json().await?;

        let models = hf_models
            .into_iter()
            .map(|m| {
                let param_size = extract_param_size(&m.model_id);
                let family = extract_model_family(&m.model_id);
                let quant = m
                    .siblings
                    .as_ref()
                    .and_then(|s| {
                        s.iter()
                            .find(|f| f.rfilename.ends_with(".gguf"))
                            .map(|f| &f.rfilename)
                    })
                    .and_then(|name| extract_quantization(name));

                let gguf_size = m.siblings.as_ref().and_then(|s| {
                    s.iter()
                        .find(|f| f.rfilename.ends_with(".gguf"))
                        .and_then(|f| f.size)
                });

                ModelResponse {
                    name: m.model_id,
                    size: gguf_size,
                    digest: None,
                    modified_at: m.last_modified,
                    details: Some(ModelDetails {
                        format: Some("gguf".to_string()),
                        family,
                        parameter_size: param_size,
                        quantization_level: quant,
                    }),
                }
            })
            .collect();

        Ok(BrowseResponse {
            models,
            next_cursor,
        })
    }
}

/// HuggingFace API model entry (subset of fields we need).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HfModelEntry {
    #[serde(alias = "id")]
    model_id: String,
    #[serde(default)]
    last_modified: Option<String>,
    #[serde(default)]
    siblings: Option<Vec<HfSibling>>,
}

#[derive(serde::Deserialize)]
struct HfSibling {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
}

/// Extract parameter size from model name (e.g., "7B", "13B", "70B").
pub fn extract_param_size(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    // Look for patterns like "7b", "13b", "70b", "1.5b", "0.5b"
    let re = regex_lite::Regex::new(r"(\d+\.?\d*)[bB]").ok()?;
    re.captures(&lower).map(|c| {
        let num = &c[1];
        format!("{}B", num.to_uppercase())
    })
}

/// Extract model family from the model name.
pub fn extract_model_family(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let families = [
        "llama",
        "mistral",
        "qwen",
        "phi",
        "gemma",
        "codellama",
        "deepseek",
        "starcoder",
        "yi",
        "falcon",
        "mpt",
        "opt",
        "bloom",
        "pythia",
        "stablelm",
        "solar",
        "tinyllama",
        "orca",
        "vicuna",
        "wizardcoder",
        "codestral",
        "command-r",
        "granite",
    ];
    for family in &families {
        if lower.contains(family) {
            return Some(family.to_string());
        }
    }
    // Use the first part of the name (before /)
    name.split('/')
        .next_back()
        .and_then(|n| n.split('-').next().map(|s| s.to_lowercase()))
}

/// Extract quantization level from a GGUF filename.
pub fn extract_quantization(filename: &str) -> Option<String> {
    let lower = filename.to_lowercase();
    // Common GGUF quantization patterns: q4_k_m, q5_k_s, q8_0, etc.
    let re = regex_lite::Regex::new(r"(q\d+_[a-z0-9_]+)").ok()?;
    re.captures(&lower).map(|c| c[1].to_uppercase())
}

/// Extract next cursor from HuggingFace Link header.
///
/// Example Link header: `<https://huggingface.co/api/models?...&offset=20>; rel="next"`
pub fn extract_cursor_from_link_header(link: &str) -> Option<String> {
    // Find the "next" link
    for part in link.split(',') {
        if part.contains("rel=\"next\"") {
            // Extract URL between < and >
            let start = part.find('<')? + 1;
            let end = part.find('>')?;
            let url = &part[start..end];
            // Extract offset parameter
            if let Some(offset_start) = url.find("offset=") {
                let offset_str = &url[offset_start + 7..];
                let offset_end = offset_str.find('&').unwrap_or(offset_str.len());
                return Some(offset_str[..offset_end].to_string());
            }
        }
    }
    None
}

/// Parse a cursor string as a numeric offset.
pub fn parse_cursor_offset(cursor: &str) -> Option<u32> {
    cursor.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_param_size() {
        assert_eq!(
            extract_param_size("Qwen/Qwen2.5-Coder-7B-Instruct-GGUF"),
            Some("7B".to_string())
        );
        assert_eq!(
            extract_param_size("meta-llama/Llama-3-13B-GGUF"),
            Some("13B".to_string())
        );
        assert_eq!(
            extract_param_size("model-1.5b-instruct"),
            Some("1.5B".to_string())
        );
        assert_eq!(extract_param_size("no-params-here"), None);
    }

    #[test]
    fn test_extract_model_family() {
        assert_eq!(
            extract_model_family("Qwen/Qwen2.5-Coder-7B"),
            Some("qwen".to_string())
        );
        assert_eq!(
            extract_model_family("meta-llama/Llama-3-8B"),
            Some("llama".to_string())
        );
        assert_eq!(
            extract_model_family("mistralai/Mistral-7B"),
            Some("mistral".to_string())
        );
    }

    #[test]
    fn test_extract_quantization() {
        assert_eq!(
            extract_quantization("model-q4_k_m.gguf"),
            Some("Q4_K_M".to_string())
        );
        assert_eq!(
            extract_quantization("model-q8_0.gguf"),
            Some("Q8_0".to_string())
        );
        assert_eq!(
            extract_quantization("model-q5_k_s.gguf"),
            Some("Q5_K_S".to_string())
        );
        assert_eq!(extract_quantization("model.gguf"), None);
    }

    #[test]
    fn test_extract_cursor_from_link_header() {
        let link = r#"<https://huggingface.co/api/models?filter=gguf&sort=downloads&limit=20&offset=20>; rel="next""#;
        assert_eq!(
            extract_cursor_from_link_header(link),
            Some("20".to_string())
        );

        let no_next = r#"<https://huggingface.co/api/models?offset=0>; rel="prev""#;
        assert_eq!(extract_cursor_from_link_header(no_next), None);
    }

    #[test]
    fn test_parse_cursor_offset() {
        assert_eq!(parse_cursor_offset("20"), Some(20));
        assert_eq!(parse_cursor_offset("100"), Some(100));
        assert_eq!(parse_cursor_offset("abc"), None);
    }
}
