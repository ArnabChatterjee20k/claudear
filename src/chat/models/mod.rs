//! Model browsing, searching, and downloading for the chat feature.

pub mod download;
pub mod providers;
pub mod types;

pub use download::DownloadProgress;
pub use providers::HuggingFaceProvider;
pub use types::*;

use std::sync::LazyLock;

/// Shared HTTP client for model provider API calls.
pub(crate) static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
});
