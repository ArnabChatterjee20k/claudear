//! Multi-tenant storage routing.
//!
//! Self-hosted mode uses [`SingleTenantRouter`] which always returns the same backend.
//! SaaS mode (in a private repo) implements [`TenantRouter`] with per-tenant backends.

use super::FixAttemptTracker;
use std::sync::Arc;

/// Opaque tenant identifier.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TenantId(String);

impl TenantId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn default_tenant() -> Self {
        Self("default".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TenantId {
    fn default() -> Self {
        Self::default_tenant()
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Routes requests to the appropriate storage backend for a tenant.
pub trait TenantRouter: Send + Sync {
    /// Get the storage backend for a given tenant.
    fn storage_for(&self, tenant: &TenantId) -> Arc<dyn FixAttemptTracker>;
}

/// Self-hosted mode: always returns the same backend regardless of tenant.
pub struct SingleTenantRouter {
    storage: Arc<dyn FixAttemptTracker>,
}

impl SingleTenantRouter {
    pub fn new(storage: Arc<dyn FixAttemptTracker>) -> Self {
        Self { storage }
    }
}

impl TenantRouter for SingleTenantRouter {
    fn storage_for(&self, _tenant: &TenantId) -> Arc<dyn FixAttemptTracker> {
        self.storage.clone()
    }
}
