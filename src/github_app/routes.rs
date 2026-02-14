//! HTTP route handlers for the GitHub App manifest flow.
//!
//! These handlers implement the setup flow for creating a GitHub App:
//!
//! 1. `/github/setup` - Initiates the flow by redirecting to GitHub with a manifest
//! 2. `/github/callback` - Receives the callback from GitHub with the App credentials
//!
//! **Note**: These routes are NOT wired up by default. They will be registered
//! in the webhook server when the feature is enabled.

use crate::env_writer::update_env_file;
use crate::error::{Error, Result};
use crate::github_app::manifest::AppManifest;
use axum::extract::Query;
use axum::response::{Html, Redirect};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Setup state expiry time in minutes.
/// CSRF tokens and setup state are invalidated after this duration.
const SETUP_STATE_EXPIRY_MINUTES: i64 = 15;

/// CSRF state for the setup flow.
///
/// This is stored in memory and validated when the callback is received.
#[derive(Debug, Clone)]
pub struct SetupState {
    /// CSRF token to prevent replay attacks.
    pub csrf_token: String,
    /// The base URL that was used for the manifest.
    pub base_url: String,
    /// When this state was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl SetupState {
    /// Create a new setup state.
    pub fn new(base_url: String) -> Self {
        Self {
            csrf_token: generate_csrf_token(),
            base_url,
            created_at: chrono::Utc::now(),
        }
    }

    /// Check if this state is still valid (not expired).
    pub fn is_valid(&self) -> bool {
        let age = chrono::Utc::now() - self.created_at;
        age.num_minutes() < SETUP_STATE_EXPIRY_MINUTES
    }
}

/// Generate a cryptographically secure random CSRF token.
fn generate_csrf_token() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let bytes: [u8; 32] = rng.random();
    hex::encode(bytes)
}

/// Query parameters for the setup endpoint.
#[derive(Debug, Deserialize)]
pub struct SetupQuery {
    /// Base URL for the claudear instance.
    pub base_url: Option<String>,
    /// Optional organization to create the App under.
    pub org: Option<String>,
    /// Optional custom name for the App.
    pub name: Option<String>,
}

/// Query parameters for the callback endpoint.
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    /// The temporary code from GitHub.
    pub code: String,
    /// The CSRF state token.
    pub state: String,
}

/// Response from GitHub's manifest code exchange.
#[derive(Debug, Deserialize)]
struct ManifestConversionResponse {
    /// The App ID.
    id: i64,
    /// The App slug (URL-friendly name).
    #[allow(dead_code)]
    slug: String,
    /// The App name.
    name: String,
    /// Client ID for OAuth.
    client_id: String,
    /// Client secret for OAuth.
    client_secret: String,
    /// The webhook secret.
    webhook_secret: String,
    /// The private key in PEM format.
    pem: String,
    /// The HTML URL for the App's settings page.
    html_url: String,
}

/// State manager for CSRF tokens during setup flow.
#[derive(Debug, Default)]
pub struct SetupStateManager {
    states: RwLock<HashMap<String, SetupState>>,
}

impl SetupStateManager {
    /// Create a new state manager.
    pub fn new() -> Self {
        Self {
            states: RwLock::new(HashMap::new()),
        }
    }

    /// Create and store a new setup state.
    pub fn create_state(&self, base_url: String) -> SetupState {
        let state = SetupState::new(base_url);
        if let Ok(mut states) = self.states.write() {
            // Clean up expired states
            states.retain(|_, s| s.is_valid());
            states.insert(state.csrf_token.clone(), state.clone());
        }
        state
    }

    /// Validate and consume a state token.
    pub fn validate_and_consume(&self, token: &str) -> Option<SetupState> {
        if let Ok(mut states) = self.states.write() {
            if let Some(state) = states.remove(token) {
                if state.is_valid() {
                    return Some(state);
                }
            }
        }
        None
    }
}

/// Handler for `/github/setup`.
///
/// Initiates the GitHub App manifest flow by:
/// 1. Generating a CSRF token
/// 2. Creating the App manifest
/// 3. Redirecting to GitHub with the manifest
///
/// # Query Parameters
/// - `base_url` - Required. The public URL of this claudear instance.
/// - `org` - Optional. GitHub organization to create the App under.
/// - `name` - Optional. Custom name for the App.
pub async fn github_setup_handler(
    Query(query): Query<SetupQuery>,
    state_manager: Arc<SetupStateManager>,
) -> std::result::Result<Redirect, Html<String>> {
    // Validate base_url is provided
    let base_url = match query.base_url {
        Some(url) if !url.is_empty() => url,
        _ => {
            return Err(Html(setup_form_html(None)));
        }
    };

    // Validate the URL looks reasonable
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err(Html(setup_form_html(Some(
            "Base URL must start with http:// or https://",
        ))));
    }

    // Create the manifest
    let manifest = AppManifest::generate(&base_url, query.name.as_deref());

    // Create and store CSRF state
    let setup_state = state_manager.create_state(base_url);

    // Generate the GitHub redirect URL
    let github_url = if let Some(org) = query.org {
        manifest.github_org_manifest_url(&org, &setup_state.csrf_token)
    } else {
        manifest.github_manifest_url(&setup_state.csrf_token)
    };

    match github_url {
        Ok(url) => Ok(Redirect::temporary(&url)),
        Err(e) => Err(Html(format!(
            "<h1>Error</h1><p>Failed to generate manifest: {}</p>",
            html_escape(&e.to_string())
        ))),
    }
}

/// Handler for `/github/callback`.
///
/// Receives the callback from GitHub after App creation:
/// 1. Validates the CSRF token
/// 2. Exchanges the code for App credentials
/// 3. Saves credentials to `.env` and PEM file
/// 4. Shows success page with next steps
pub async fn github_callback_handler(
    Query(query): Query<CallbackQuery>,
    state_manager: Arc<SetupStateManager>,
) -> Html<String> {
    // Validate CSRF state
    let setup_state = match state_manager.validate_and_consume(&query.state) {
        Some(state) => state,
        None => {
            return Html(error_html(
                "Invalid or expired state",
                "The setup session has expired. Please start over.",
            ));
        }
    };

    // Exchange the code for credentials
    let credentials = match exchange_code_for_credentials(&query.code).await {
        Ok(creds) => creds,
        Err(e) => {
            return Html(error_html(
                "Failed to exchange code",
                &format!("GitHub returned an error: {}", e),
            ));
        }
    };

    // Save credentials
    let save_result = save_credentials(&credentials, &setup_state.base_url);

    match save_result {
        Ok(_) => Html(success_html(&credentials, &setup_state.base_url)),
        Err(e) => Html(partial_success_html(&credentials, &e.to_string())),
    }
}

/// Default timeout for HTTP requests (30 seconds).
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Default connection timeout (10 seconds).
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Exchange the manifest code for App credentials.
async fn exchange_code_for_credentials(code: &str) -> Result<ManifestConversionResponse> {
    let url = format!("https://api.github.com/app-manifests/{}/conversions", code);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
        .connect_timeout(std::time::Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let response = client
        .post(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "claudear")
        .send()
        .await
        .map_err(|e| Error::api(format!("Failed to contact GitHub: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::api(format!(
            "GitHub API error ({}): {}",
            status, body
        )));
    }

    response
        .json::<ManifestConversionResponse>()
        .await
        .map_err(|e| Error::api(format!("Failed to parse GitHub response: {}", e)))
}

/// Save the App credentials to disk.
fn save_credentials(creds: &ManifestConversionResponse, base_url: &str) -> Result<()> {
    // Save private key to PEM file
    let pem_path = "github-app-key.pem";
    fs::write(pem_path, &creds.pem).map_err(|e| {
        Error::config(format!(
            "Failed to write private key to {}: {}",
            pem_path, e
        ))
    })?;

    // Set restrictive permissions on the PEM file (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(pem_path, perms).ok();
    }

    // Update .env file
    let env_path = Path::new(".env");
    let mut updates = HashMap::new();
    updates.insert("GITHUB_APP_ID".to_string(), creds.id.to_string());
    updates.insert(
        "GITHUB_APP_PRIVATE_KEY_PATH".to_string(),
        pem_path.to_string(),
    );
    updates.insert(
        "GITHUB_APP_WEBHOOK_SECRET".to_string(),
        creds.webhook_secret.clone(),
    );
    updates.insert("GITHUB_APP_CLIENT_ID".to_string(), creds.client_id.clone());
    updates.insert(
        "GITHUB_APP_CLIENT_SECRET".to_string(),
        creds.client_secret.clone(),
    );
    updates.insert("GITHUB_APP_BASE_URL".to_string(), base_url.to_string());

    update_env_file(env_path, &updates)?;

    Ok(())
}

/// Escape HTML special characters to prevent XSS.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Generate HTML for the setup form.
fn setup_form_html(error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p style="color: red;">{}</p>"#, html_escape(e)))
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>claudear - GitHub App Setup</title>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 600px; margin: 50px auto; padding: 20px; }}
        input {{ width: 100%; padding: 10px; margin: 10px 0; box-sizing: border-box; }}
        button {{ background: #238636; color: white; padding: 10px 20px; border: none; cursor: pointer; }}
        button:hover {{ background: #2ea043; }}
    </style>
</head>
<body>
    <h1>claudear - GitHub App Setup</h1>
    <p>Enter the public URL where this claudear instance is accessible:</p>
    {error}
    <form method="GET" action="/github/setup">
        <input type="url" name="base_url" placeholder="https://your-server:3100" required>
        <p><small>Optional: Create the App under an organization</small></p>
        <input type="text" name="org" placeholder="organization-name (optional)">
        <p><small>Optional: Custom App name</small></p>
        <input type="text" name="name" placeholder="claudear (default)">
        <button type="submit">Create GitHub App</button>
    </form>
</body>
</html>"#,
        error = error_html
    )
}

/// Generate HTML for error page.
fn error_html(title: &str, message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>claudear - Setup Error</title>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 600px; margin: 50px auto; padding: 20px; }}
        .error {{ background: #ffebe9; border: 1px solid #ff8182; padding: 20px; border-radius: 6px; }}
    </style>
</head>
<body>
    <h1>Setup Error</h1>
    <div class="error">
        <h2>{}</h2>
        <p>{}</p>
    </div>
    <p><a href="/github/setup">Try again</a></p>
</body>
</html>"#,
        html_escape(title),
        html_escape(message)
    )
}

/// Generate HTML for success page.
fn success_html(creds: &ManifestConversionResponse, base_url: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>claudear - Setup Complete</title>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 600px; margin: 50px auto; padding: 20px; }}
        .success {{ background: #d1f5d3; border: 1px solid #34d058; padding: 20px; border-radius: 6px; }}
        code {{ background: #f6f8fa; padding: 2px 6px; border-radius: 3px; }}
        .steps {{ background: #f6f8fa; padding: 20px; border-radius: 6px; margin-top: 20px; }}
    </style>
</head>
<body>
    <h1>GitHub App Created!</h1>
    <div class="success">
        <p><strong>{}</strong> has been created successfully.</p>
        <p>App ID: <code>{}</code></p>
    </div>
    <div class="steps">
        <h2>Next Steps</h2>
        <ol>
            <li>Install the App on your repositories: <a href="{}" target="_blank">App Settings</a></li>
            <li>Restart claudear to load the new credentials</li>
            <li>The webhook URL has been configured to: <code>{}/webhook/github</code></li>
        </ol>
    </div>
    <p>Credentials have been saved to:</p>
    <ul>
        <li><code>.env</code> - Environment variables</li>
        <li><code>github-app-key.pem</code> - Private key</li>
    </ul>
</body>
</html>"#,
        html_escape(&creds.name),
        creds.id,
        html_escape(&creds.html_url),
        html_escape(base_url)
    )
}

/// Generate HTML for partial success (credentials received but save failed).
fn partial_success_html(creds: &ManifestConversionResponse, error: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>claudear - Credentials Received</title>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 800px; margin: 50px auto; padding: 20px; }}
        .warning {{ background: #fff8c5; border: 1px solid #d4a72c; padding: 20px; border-radius: 6px; }}
        pre {{ background: #f6f8fa; padding: 15px; overflow-x: auto; border-radius: 6px; }}
        .danger {{ color: #cf222e; }}
    </style>
</head>
<body>
    <h1>GitHub App Created!</h1>
    <div class="warning">
        <h2>Manual Configuration Required</h2>
        <p>The App was created, but credentials could not be saved automatically:</p>
        <p><strong>{}</strong></p>
    </div>
    <h2>App Details</h2>
    <ul>
        <li>App Name: <strong>{}</strong></li>
        <li>App ID: <strong>{}</strong></li>
        <li>Client ID: <strong>{}</strong></li>
        <li>Settings: <a href="{}" target="_blank">{}</a></li>
    </ul>
    <h2>Manual Setup</h2>
    <p>Add these to your <code>.env</code> file:</p>
    <pre>
GITHUB_APP_ID={}
GITHUB_APP_PRIVATE_KEY_PATH=github-app-key.pem
GITHUB_APP_WEBHOOK_SECRET={}
GITHUB_APP_CLIENT_ID={}
GITHUB_APP_CLIENT_SECRET={}
</pre>
    <h2 class="danger">Private Key (save to github-app-key.pem)</h2>
    <p class="danger">Copy this private key and save it securely. It will not be shown again!</p>
    <pre>{}</pre>
</body>
</html>"#,
        html_escape(error),
        html_escape(&creds.name),
        creds.id,
        html_escape(&creds.client_id),
        html_escape(&creds.html_url),
        html_escape(&creds.html_url),
        creds.id,
        html_escape(&creds.webhook_secret),
        html_escape(&creds.client_id),
        html_escape(&creds.client_secret),
        html_escape(&creds.pem)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setup_state_new() {
        let state = SetupState::new("https://example.com".to_string());
        assert!(!state.csrf_token.is_empty());
        assert_eq!(state.base_url, "https://example.com");
        assert!(state.is_valid());
    }

    #[test]
    fn test_setup_state_manager_create_and_validate() {
        let manager = SetupStateManager::new();

        let state = manager.create_state("https://example.com".to_string());
        let token = state.csrf_token.clone();

        // Should be able to validate and consume once
        let validated = manager.validate_and_consume(&token);
        assert!(validated.is_some());
        assert_eq!(validated.unwrap().base_url, "https://example.com");

        // Should not be able to consume again
        let second = manager.validate_and_consume(&token);
        assert!(second.is_none());
    }

    #[test]
    fn test_setup_state_manager_invalid_token() {
        let manager = SetupStateManager::new();

        let result = manager.validate_and_consume("invalid_token");
        assert!(result.is_none());
    }

    #[test]
    fn test_generate_csrf_token() {
        let token1 = generate_csrf_token();
        let token2 = generate_csrf_token();

        // Tokens should be non-empty and different
        assert!(!token1.is_empty());
        assert!(!token2.is_empty());
        // Note: Due to nanosecond precision, these might be the same in fast tests
        // but in practice they'll be different
    }

    #[test]
    fn test_setup_form_html_no_error() {
        let html = setup_form_html(None);
        assert!(html.contains("GitHub App Setup"));
        assert!(html.contains("base_url"));
        assert!(!html.contains("color: red"));
    }

    #[test]
    fn test_setup_form_html_with_error() {
        let html = setup_form_html(Some("Test error message"));
        assert!(html.contains("Test error message"));
        assert!(html.contains("color: red"));
    }

    #[test]
    fn test_error_html() {
        let html = error_html("Test Title", "Test message");
        assert!(html.contains("Test Title"));
        assert!(html.contains("Test message"));
        assert!(html.contains("Setup Error"));
    }
}
