//! Dashboard API endpoints.
//!
//! Provides REST API for the analytics dashboard.

pub mod auth;
mod routes;

pub use routes::{create_api_router, create_api_router_with_dashboard};

use crate::config::Config;
use crate::error::Result;
use crate::storage::FixAttemptTracker;
use std::path::PathBuf;
use std::sync::Arc;
use tower_cookies::CookieManagerLayer;
use tower_http::cors::CorsLayer;

/// API server configuration.
pub struct ApiServer {
    config: Config,
    tracker: Arc<dyn FixAttemptTracker>,
    port: u16,
    dashboard_dir: Option<PathBuf>,
}

impl ApiServer {
    /// Create a new API server.
    pub fn new(config: Config, tracker: Arc<dyn FixAttemptTracker>) -> Self {
        let port = config.webhook_port; // Reuse webhook port for now
        Self {
            config,
            tracker,
            port,
            dashboard_dir: None,
        }
    }

    /// Create a new API server with custom port.
    pub fn with_port(config: Config, tracker: Arc<dyn FixAttemptTracker>, port: u16) -> Self {
        Self {
            config,
            tracker,
            port,
            dashboard_dir: None,
        }
    }

    /// Create a new API server with dashboard directory.
    pub fn with_dashboard(
        config: Config,
        tracker: Arc<dyn FixAttemptTracker>,
        port: u16,
        dashboard_dir: PathBuf,
    ) -> Self {
        Self {
            config,
            tracker,
            port,
            dashboard_dir: Some(dashboard_dir),
        }
    }

    /// Start the API server.
    pub async fn start(self) -> Result<()> {
        let cors = CorsLayer::permissive();

        let app = create_api_router_with_dashboard(
            self.config.clone(),
            self.tracker.clone(),
            self.dashboard_dir.clone(),
        )
        .layer(cors)
        .layer(CookieManagerLayer::new());

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;

        tracing::info!("Dashboard API server listening on port {}", self.port);
        if self.dashboard_dir.is_some() {
            tracing::info!("Dashboard available at http://localhost:{}", self.port);
        } else {
            tracing::info!("API only mode - serve dashboard separately or provide --dashboard-dir");
        }

        axum::serve(listener, app).await?;

        Ok(())
    }
}
