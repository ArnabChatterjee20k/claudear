//! # Claudear
//!
//! A unified watcher service that monitors issue trackers and error monitoring services,
//! automatically spawning Claude Code agents to fix issues and create pull requests.
//!
//! ## Features
//!
//! - **Multi-Source Support**: Monitor Linear issues and Sentry errors from a single service
//! - **Extensible Architecture**: Easy to add new sources (GitHub Issues, Jira, etc.)
//! - **Discord Notifications**: Get notified about fix attempts with PR links
//! - **SQLite Tracking**: Persistent tracking of fix attempts to avoid duplicates
//! - **Priority-Based Processing**: Urgent/escalating issues are processed first
//! - **Graceful Handling**: Proper error handling and retry support
//!
//! ## Usage
//!
//! ```bash
//! # First-time setup - mark existing issues as seen
//! claudear seed
//!
//! # Start polling for new issues
//! claudear poll
//!
//! # Start webhook server for real-time events
//! claudear webhook
//! ```

pub mod api;
pub mod config;
pub mod discord;
pub mod env_writer;
pub mod error;
pub mod feedback;
pub mod github;
pub mod inference;
pub mod ipc;
pub mod notifier;
pub mod release;
pub mod repo;
pub mod reports;
pub mod retry;
pub mod runner;
pub mod source;
pub mod storage;
pub mod templates;
pub mod types;
pub mod watcher;
pub mod webhook;

pub use config::{Config, RetryConfig};
pub use discord::{DiscordClient, ThreadManager, ThreadState};
pub use error::{Error, Result};
pub use feedback::{
    cosine_similarity, euclidean_distance, format_similar_issues_context, normalize,
    EmbeddingClient, EmbeddingConfig, EmbeddingResult, FeedbackAnalyzer, FixOutcome,
    IssueEmbeddingConfig, IssueEmbeddingService, MemoryVectorStore, Outcome, PromptSuggestion,
    SimilarIssue, SimilarIssueWithDetails,
};
pub use github::{
    GitHubClient, GitHubUser, PrMonitor, PrReview, PrReviewComment, PrReviewState, PrStatus,
    PrStatusUpdate, ReviewEvent, ReviewWatcher,
};
pub use inference::{
    resolve_repo_for_issue, Confidence, InferredRepo, IssueContext, RepoInferrer, RepoResolution,
};
pub use ipc::{
    default_socket_path, is_daemon_running, print_response, IpcClient, IpcCommand, IpcData,
    IpcResponse, IpcServer, WatcherState,
};
pub use repo::{
    DependencyDiscovery, DependencyGraph, DependencyType, DiscoveredDependency, IndexedRepo,
    RepoIndex, RepoRelationships, Repository,
};
pub use reports::{Report, ReportFrequency, ReportGenerator, ReportSchedule, ReportScheduler};
pub use retry::{RetryDecision, RetryManager};
pub use storage::{
    classify_error, compute_error_hash, AnalyticsService, FixAttemptTracker, SqliteTracker,
    StoredDependency, StoredRepository, TimePeriod, TrendAnalysis, TrendDirection,
};
pub use types::*;
