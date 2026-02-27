//! Dashboard API endpoints.
//!
//! Provides REST API for the analytics dashboard.

pub mod auth;
pub(crate) mod embedded;
mod routes;

pub use routes::{create_api_router, create_api_router_full, create_api_router_with_dashboard};

use crate::config::Config;
use crate::error::Result;
use crate::storage::FixAttemptTracker;
use axum::http;
use sentry::integrations::tower::{NewSentryLayer, SentryHttpLayer};
use std::path::PathBuf;
use std::sync::Arc;
use tower_cookies::CookieManagerLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

/// API server configuration.
pub struct ApiServer {
    config: Config,
    tracker: Arc<dyn FixAttemptTracker>,
    port: u16,
    dashboard_dir: Option<PathBuf>,
    config_path: PathBuf,
}

impl ApiServer {
    /// Create a new API server.
    pub fn new(config: Config, tracker: Arc<dyn FixAttemptTracker>, config_path: PathBuf) -> Self {
        let port = config.webhook_port; // Reuse webhook port for now
        Self {
            config,
            tracker,
            port,
            dashboard_dir: None,
            config_path,
        }
    }

    /// Create a new API server with custom port.
    pub fn with_port(
        config: Config,
        tracker: Arc<dyn FixAttemptTracker>,
        port: u16,
        config_path: PathBuf,
    ) -> Self {
        Self {
            config,
            tracker,
            port,
            dashboard_dir: None,
            config_path,
        }
    }

    /// Create a new API server with dashboard directory.
    pub fn with_dashboard(
        config: Config,
        tracker: Arc<dyn FixAttemptTracker>,
        port: u16,
        dashboard_dir: PathBuf,
        config_path: PathBuf,
    ) -> Self {
        Self {
            config,
            tracker,
            port,
            dashboard_dir: Some(dashboard_dir),
            config_path,
        }
    }

    /// Start the API server.
    pub async fn start(self) -> Result<()> {
        // Allow requests from common local dashboard origins.
        // In production behind a reverse proxy, the proxy should handle CORS.
        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(|origin, _| {
                if let Ok(origin_str) = origin.to_str() {
                    // Allow localhost and 127.0.0.1 on any port (common dev/local setup)
                    origin_str.starts_with("http://localhost")
                        || origin_str.starts_with("https://localhost")
                        || origin_str.starts_with("http://127.0.0.1")
                        || origin_str.starts_with("https://127.0.0.1")
                } else {
                    false
                }
            }))
            .allow_methods([
                http::Method::GET,
                http::Method::POST,
                http::Method::PUT,
                http::Method::DELETE,
                http::Method::OPTIONS,
            ])
            .allow_headers([
                http::header::CONTENT_TYPE,
                http::header::AUTHORIZATION,
                http::header::COOKIE,
            ])
            .allow_credentials(true);

        // Subscribe to indexing progress from the tracker's watch channel
        let indexing_rx = self.tracker.subscribe_indexing_progress();

        let app = create_api_router_with_dashboard(
            self.config.clone(),
            self.tracker.clone(),
            self.config_path,
            indexing_rx,
            self.dashboard_dir.clone(),
        )
        .layer(cors)
        .layer(CookieManagerLayer::new())
        // Sentry layers: NewSentryLayer must be outermost (added last in axum's layer chain)
        .layer(SentryHttpLayer::new().enable_transaction())
        .layer(NewSentryLayer::new_from_top());

        let tls_enabled = self.config.tls.enabled;
        let scheme = if tls_enabled { "https" } else { "http" };

        tracing::info!(
            "Dashboard API server starting ({}://{}:{})",
            scheme,
            self.config.bind_address,
            if tls_enabled {
                self.config.tls.https_port
            } else {
                self.port
            }
        );
        if self.dashboard_dir.is_some() {
            tracing::info!(
                "Serving dashboard from filesystem at {}://localhost:{}",
                scheme,
                if tls_enabled {
                    self.config.tls.https_port
                } else {
                    self.port
                }
            );
        } else if embedded::has_dashboard() {
            tracing::info!(
                "Serving embedded dashboard at {}://localhost:{}",
                scheme,
                if tls_enabled {
                    self.config.tls.https_port
                } else {
                    self.port
                }
            );
        } else {
            tracing::info!(
                "API only mode - no dashboard embedded. Use --dashboard-dir for development."
            );
        }

        // Spawn background task to periodically clean up expired sessions.
        // Runs every hour, cleaning up sessions past their expires_at timestamp.
        let tracker_for_cleanup = self.tracker.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                match tracker_for_cleanup.cleanup_expired_sessions() {
                    Ok(count) if count > 0 => {
                        tracing::info!(deleted = count, "Cleaned up expired sessions");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to clean up expired sessions");
                    }
                    _ => {}
                }
            }
        });

        if tls_enabled {
            crate::tls::serve_with_tls(&self.config.tls, &self.config.bind_address, app).await?;
        } else {
            crate::tls::serve_plain_http(&self.config.bind_address, self.port, app).await?;
        }

        Ok(())
    }
}
