//! Engine layer for claudear.
//!
//! This crate provides the watcher, processing pipeline, API server,
//! IPC server/client, housekeeping, and retry management.

pub mod api;
pub mod api_events;
pub mod housekeeping;
pub mod ipc;
pub mod llm_analyzer;
pub mod llm_classifier;
pub mod processing;
pub mod repo_index;
pub mod retry;
pub mod watcher;
