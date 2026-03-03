//! GitHub App support for self-hosted deployments.
//!
//! This module provides the infrastructure for authenticating as a GitHub App,
//! which is the recommended authentication method for self-hosted deployments
//! where each user needs their own GitHub App.
//!
//! ## Overview
//!
//! GitHub Apps provide several advantages over Personal Access Tokens:
//! - Fine-grained permissions scoped to specific repositories
//! - Higher API rate limits
//! - No dependency on a specific user account
//! - Webhook secrets for secure event delivery
//!
//! ## Setup Flow
//!
//! For self-hosted deployments, users create their GitHub App via the manifest flow:
//!
//! 1. Visit `/github/setup?base_url=https://your-server:3100`
//! 2. Gets redirected to GitHub with a pre-filled App manifest
//! 3. Creates the App on GitHub
//! 4. Gets redirected back with credentials
//! 5. Credentials are saved to `.env` and PEM file
//!
//! ## Authentication
//!
//! GitHub App authentication is a two-step process:
//!
//! 1. **JWT Authentication**: Sign a JWT with the App's private key to authenticate
//!    as the App itself. Used for App-level operations like listing installations.
//!
//! 2. **Installation Token**: Exchange the JWT for an installation access token,
//!    which is scoped to a specific installation and used for repository operations.
//!
//! ## Modules
//!
//! - `auth`: JWT generation and installation token caching
//! - `manifest`: GitHub App manifest generation for the setup flow
//! - `routes`: HTTP handlers for the setup flow (not wired up by default)
//! - `client`: Client for GitHub App API operations

pub mod auth;
pub mod client;
pub mod manifest;
pub mod routes;

pub use auth::{CachedToken, GitHubAppAuth};
pub use client::GitHubAppClient;
pub use manifest::{AppManifest, AppPermissions, HookAttributes};
pub use routes::{github_callback_handler, github_setup_handler, SetupState};
