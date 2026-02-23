-- V1: Initial Claudear schema
-- All tables consolidated (no ALTER TABLE migrations needed).

-- ============================================================
-- Core Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS fix_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    short_id TEXT NOT NULL,
    attempted_at TEXT NOT NULL DEFAULT (datetime('now')),
    pr_url TEXT,
    scm_repo TEXT,
    scm_pr_number INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    error_message TEXT,
    merged_at TEXT,
    resolved_at TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_retry_at TEXT,
    issue_labels TEXT,
    parent_attempt_id INTEGER REFERENCES fix_attempts(id),
    cascade_repo TEXT,
    reset_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_fix_attempts_status ON fix_attempts(status);
CREATE INDEX IF NOT EXISTS idx_fix_attempts_source_issue ON fix_attempts(source, issue_id);
CREATE INDEX IF NOT EXISTS idx_fix_attempts_pr_url ON fix_attempts(pr_url);
CREATE INDEX IF NOT EXISTS idx_fix_attempts_retryable ON fix_attempts(status, retry_count, attempted_at);
CREATE INDEX IF NOT EXISTS idx_fix_attempts_status_attempted ON fix_attempts(status, attempted_at DESC);
CREATE INDEX IF NOT EXISTS idx_fix_attempts_source_status_attempted ON fix_attempts(source, status, attempted_at DESC);
CREATE INDEX IF NOT EXISTS idx_fix_attempts_parent ON fix_attempts(parent_attempt_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_fix_attempts_unique_original ON fix_attempts(source, issue_id) WHERE cascade_repo IS NULL;

CREATE TABLE IF NOT EXISTS feedback_outcomes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER REFERENCES fix_attempts(id),
    source TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    issue_text TEXT NOT NULL,
    prompt_used TEXT NOT NULL,
    outcome TEXT NOT NULL,
    error_type TEXT,
    learnings TEXT,
    keywords TEXT,
    strategy_fingerprint_id INTEGER,
    embedding BLOB,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_feedback_outcomes_source ON feedback_outcomes(source);
CREATE INDEX IF NOT EXISTS idx_feedback_outcomes_outcome ON feedback_outcomes(outcome);
CREATE INDEX IF NOT EXISTS idx_feedback_outcomes_attempt ON feedback_outcomes(attempt_id);
CREATE INDEX IF NOT EXISTS idx_feedback_source_issue ON feedback_outcomes(source, issue_id);

CREATE TABLE IF NOT EXISTS discord_threads (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id TEXT NOT NULL UNIQUE,
    thread_name TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    pr_url TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    source TEXT NOT NULL,
    is_active INTEGER DEFAULT 1,
    last_message_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_discord_threads_pr ON discord_threads(pr_url);
CREATE INDEX IF NOT EXISTS idx_discord_threads_active ON discord_threads(is_active);
CREATE INDEX IF NOT EXISTS idx_discord_threads_channel ON discord_threads(channel_id);

CREATE TABLE IF NOT EXISTS pr_review_states (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pr_url TEXT NOT NULL UNIQUE,
    repo TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    issue_id TEXT NOT NULL,
    source TEXT NOT NULL,
    last_review_id INTEGER,
    last_review_time TEXT,
    last_comment_id INTEGER,
    last_comment_time TEXT,
    is_active INTEGER DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_pr_review_states_active ON pr_review_states(is_active);

CREATE TABLE IF NOT EXISTS repositories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    path TEXT NOT NULL DEFAULT '',
    scm_url TEXT,
    default_branch TEXT DEFAULT 'main',
    file_count INTEGER DEFAULT 0,
    last_indexed_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS repository_dependencies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    upstream_id INTEGER REFERENCES repositories(id),
    downstream_id INTEGER REFERENCES repositories(id),
    dependency_type TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(upstream_id, downstream_id)
);
CREATE INDEX IF NOT EXISTS idx_repository_dependencies_downstream ON repository_dependencies(downstream_id);

-- ============================================================
-- Analytics Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS activity_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    activity_type TEXT NOT NULL,
    source TEXT,
    issue_id TEXT,
    short_id TEXT,
    message TEXT NOT NULL,
    metadata TEXT
);
CREATE INDEX IF NOT EXISTS idx_activity_timestamp ON activity_log(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_activity_issue ON activity_log(issue_id);
CREATE INDEX IF NOT EXISTS idx_activity_source_issue ON activity_log(source, issue_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_activity_source_timestamp ON activity_log(source, timestamp DESC);

-- claude_executions: all columns consolidated (including cost/token/provider columns)
CREATE TABLE IF NOT EXISTS claude_executions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER REFERENCES fix_attempts(id),
    started_at TEXT NOT NULL,
    completed_at TEXT,
    duration_secs REAL,
    exit_code INTEGER,
    timed_out INTEGER DEFAULT 0,
    stdout_preview TEXT,
    stderr_preview TEXT,
    stdout_log_path TEXT,
    stderr_log_path TEXT,
    event_log_path TEXT,
    prompt_used TEXT,
    prompt_hash TEXT,
    model_version TEXT,
    working_directory TEXT,
    git_branch TEXT,
    git_commit_before TEXT,
    git_commit_after TEXT,
    files_changed INTEGER,
    lines_added INTEGER,
    lines_removed INTEGER,
    total_cost_usd REAL,
    num_turns INTEGER,
    session_id TEXT,
    duration_api_ms INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_input_tokens INTEGER,
    cache_creation_input_tokens INTEGER,
    provider TEXT DEFAULT 'claude',
    experiment_name TEXT,
    experiment_variant TEXT
);
CREATE INDEX IF NOT EXISTS idx_executions_attempt ON claude_executions(attempt_id);
CREATE INDEX IF NOT EXISTS idx_executions_prompt_hash ON claude_executions(prompt_hash);
CREATE INDEX IF NOT EXISTS idx_executions_provider ON claude_executions(provider);
CREATE INDEX IF NOT EXISTS idx_executions_experiment ON claude_executions(experiment_name);

CREATE TABLE IF NOT EXISTS pr_reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER REFERENCES fix_attempts(id),
    pr_url TEXT NOT NULL,
    reviewer TEXT,
    review_state TEXT,
    submitted_at TEXT,
    body TEXT,
    sentiment TEXT,
    actionable_feedback TEXT
);
CREATE INDEX IF NOT EXISTS idx_pr_reviews_attempt ON pr_reviews(attempt_id);

CREATE TABLE IF NOT EXISTS pr_review_comments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scm_comment_id INTEGER NOT NULL UNIQUE,
    pr_url TEXT NOT NULL,
    review_id INTEGER REFERENCES pr_reviews(id),
    path TEXT NOT NULL,
    position INTEGER,
    line INTEGER,
    body TEXT NOT NULL,
    author TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    html_url TEXT
);
CREATE INDEX IF NOT EXISTS idx_pr_review_comments_pr ON pr_review_comments(pr_url);

CREATE TABLE IF NOT EXISTS issues (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    short_id TEXT,
    title TEXT,
    description TEXT,
    url TEXT,
    priority TEXT DEFAULT 'none',
    status TEXT DEFAULT 'open',
    labels TEXT,
    embedding BLOB,
    embedding_model TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT,
    UNIQUE(source, issue_id)
);
CREATE INDEX IF NOT EXISTS idx_issues_source ON issues(source);

CREATE TABLE IF NOT EXISTS error_patterns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pattern_hash TEXT UNIQUE,
    error_type TEXT,
    error_message TEXT,
    first_seen TEXT,
    last_seen TEXT,
    occurrence_count INTEGER DEFAULT 1,
    sources TEXT,
    example_issue_ids TEXT,
    resolution_hints TEXT
);
CREATE INDEX IF NOT EXISTS idx_error_patterns_type ON error_patterns(error_type);
CREATE INDEX IF NOT EXISTS idx_error_patterns_count ON error_patterns(occurrence_count DESC);

CREATE TABLE IF NOT EXISTS processing_metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    metric_name TEXT NOT NULL,
    metric_value REAL NOT NULL,
    source TEXT,
    tags TEXT
);
CREATE INDEX IF NOT EXISTS idx_metrics_name_time ON processing_metrics(metric_name, timestamp DESC);

CREATE TABLE IF NOT EXISTS prompt_experiments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    experiment_name TEXT NOT NULL,
    variant TEXT NOT NULL,
    prompt_template TEXT NOT NULL,
    prompt_hash TEXT NOT NULL,
    created_at TEXT DEFAULT (datetime('now')),
    active INTEGER DEFAULT 1,
    success_count INTEGER DEFAULT 0,
    failure_count INTEGER DEFAULT 0,
    avg_time_to_merge REAL,
    avg_review_score REAL
);
CREATE INDEX IF NOT EXISTS idx_experiments_active ON prompt_experiments(active, experiment_name);

CREATE TABLE IF NOT EXISTS similar_issues (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_issue_id TEXT NOT NULL,
    similar_issue_id TEXT NOT NULL,
    similarity_score REAL NOT NULL,
    computed_at TEXT DEFAULT (datetime('now')),
    UNIQUE(source_issue_id, similar_issue_id)
);
CREATE INDEX IF NOT EXISTS idx_similar_source ON similar_issues(source_issue_id);
CREATE INDEX IF NOT EXISTS idx_similar_score ON similar_issues(similarity_score DESC);

-- ============================================================
-- Q&A Knowledge Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS qa_knowledge (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    repo TEXT,
    issue_id TEXT NOT NULL,
    short_id TEXT NOT NULL,
    question_text TEXT NOT NULL,
    question_norm TEXT NOT NULL,
    question_embedding BLOB,
    answer_text TEXT NOT NULL,
    answer_norm TEXT NOT NULL,
    answer_embedding BLOB,
    channel TEXT NOT NULL,
    responder TEXT,
    correlation_id TEXT NOT NULL,
    asked_at TEXT NOT NULL,
    answered_at TEXT NOT NULL,
    success_count INTEGER NOT NULL DEFAULT 0,
    failure_count INTEGER NOT NULL DEFAULT 0,
    last_used_at TEXT,
    metadata TEXT
);
CREATE INDEX IF NOT EXISTS idx_qa_scoped_time ON qa_knowledge(source, repo, answered_at DESC);
CREATE INDEX IF NOT EXISTS idx_qa_question_norm ON qa_knowledge(question_norm);

CREATE TABLE IF NOT EXISTS qa_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER NOT NULL REFERENCES fix_attempts(id),
    qa_id INTEGER NOT NULL REFERENCES qa_knowledge(id),
    usage_type TEXT NOT NULL,
    similarity_score REAL NOT NULL DEFAULT 0.0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(attempt_id, qa_id)
);
CREATE INDEX IF NOT EXISTS idx_qa_usage_attempt ON qa_usage(attempt_id);

CREATE TABLE IF NOT EXISTS question_channel_cursor (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel TEXT NOT NULL,
    cursor_key TEXT NOT NULL,
    cursor_value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(channel, cursor_key)
);

-- ============================================================
-- Repository File Index
-- ============================================================

CREATE TABLE IF NOT EXISTS repo_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    file_path TEXT NOT NULL,
    file_type TEXT,
    last_modified TEXT,
    UNIQUE(repo_id, file_path)
);
CREATE INDEX IF NOT EXISTS idx_repo_files_path ON repo_files(file_path);
CREATE INDEX IF NOT EXISTS idx_repo_files_type ON repo_files(file_type);

-- ============================================================
-- Inference Tracking Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS inference_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id TEXT NOT NULL,
    issue_source TEXT NOT NULL,
    extracted_filenames TEXT,
    extracted_functions TEXT,
    extracted_keywords TEXT,
    raw_context TEXT,
    inferred_repo_id INTEGER REFERENCES repositories(id),
    confidence TEXT,
    inference_reason TEXT,
    match_details TEXT,
    was_correct INTEGER,
    actual_repo_id INTEGER REFERENCES repositories(id),
    feedback_source TEXT,
    inference_duration_ms INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    feedback_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_inference_issue ON inference_attempts(issue_id);
CREATE INDEX IF NOT EXISTS idx_inference_confidence ON inference_attempts(confidence);
CREATE INDEX IF NOT EXISTS idx_inference_correct ON inference_attempts(was_correct);
CREATE INDEX IF NOT EXISTS idx_inference_created ON inference_attempts(created_at DESC);

-- ============================================================
-- PR Lifecycle Tracking Table
-- ============================================================

CREATE TABLE IF NOT EXISTS prs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pr_url TEXT NOT NULL UNIQUE,
    scm_repo TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    attempt_id INTEGER REFERENCES fix_attempts(id),
    issue_id TEXT,
    issue_source TEXT,
    title TEXT,
    description TEXT,
    author TEXT,
    head_branch TEXT,
    base_branch TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT,
    merged_at TEXT,
    closed_at TEXT,
    approvals_count INTEGER DEFAULT 0,
    changes_requested_count INTEGER DEFAULT 0,
    comments_count INTEGER DEFAULT 0,
    last_review_at TEXT,
    time_to_first_review_mins INTEGER,
    time_to_merge_mins INTEGER,
    review_cycles INTEGER DEFAULT 0,
    files_changed INTEGER,
    lines_added INTEGER,
    lines_removed INTEGER,
    fix_quality_score REAL
);
CREATE INDEX IF NOT EXISTS idx_prs_status ON prs(status);
CREATE INDEX IF NOT EXISTS idx_prs_repo ON prs(scm_repo);
CREATE INDEX IF NOT EXISTS idx_prs_attempt ON prs(attempt_id);
CREATE INDEX IF NOT EXISTS idx_prs_issue ON prs(issue_source, issue_id);
CREATE INDEX IF NOT EXISTS idx_prs_created ON prs(created_at DESC);

-- ============================================================
-- Regression Tracking Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS regression_watches (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_type TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    fix_attempt_id INTEGER NOT NULL REFERENCES fix_attempts(id),
    status TEXT NOT NULL DEFAULT 'awaiting_release',
    pr_merged_at TEXT,
    monitoring_started_at TEXT,
    resolved_at TEXT,
    regressed_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(issue_type, issue_id)
);
CREATE INDEX IF NOT EXISTS idx_regression_watches_status ON regression_watches(status);
CREATE INDEX IF NOT EXISTS idx_regression_watches_fix_attempt ON regression_watches(fix_attempt_id);

CREATE TABLE IF NOT EXISTS release_tracking (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    regression_watch_id INTEGER NOT NULL REFERENCES regression_watches(id),
    release_version TEXT NOT NULL,
    release_commit TEXT NOT NULL,
    released_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_release_tracking_watch ON release_tracking(regression_watch_id);

CREATE TABLE IF NOT EXISTS regression_checks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    regression_watch_id INTEGER NOT NULL REFERENCES regression_watches(id),
    issue_still_exists INTEGER NOT NULL DEFAULT 0,
    checked_at TEXT,
    check_details TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_regression_checks_watch ON regression_checks(regression_watch_id);

-- ============================================================
-- Authentication Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    name TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'viewer',
    avatar_url TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    delivery_id TEXT NOT NULL,
    source TEXT NOT NULL,
    received_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(delivery_id, source)
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_cleanup
    ON webhook_deliveries(received_at);

-- ============================================================
-- Continuous Learning Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS diff_analyses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER REFERENCES fix_attempts(id),
    pr_url TEXT NOT NULL,
    scm_repo TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    files_changed TEXT NOT NULL DEFAULT '[]',
    file_types TEXT NOT NULL DEFAULT '{}',
    change_categories TEXT NOT NULL DEFAULT '[]',
    diff_summary TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_diff_analyses_repo ON diff_analyses(scm_repo);
CREATE INDEX IF NOT EXISTS idx_diff_analyses_attempt ON diff_analyses(attempt_id);

CREATE TABLE IF NOT EXISTS promoted_instructions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo TEXT NOT NULL,
    source_type TEXT NOT NULL,
    instruction_text TEXT NOT NULL,
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    confidence REAL NOT NULL DEFAULT 0.5,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_promoted_instructions_repo ON promoted_instructions(repo, is_active);

CREATE TABLE IF NOT EXISTS repo_knowledge (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo TEXT NOT NULL,
    knowledge_key TEXT NOT NULL,
    knowledge_value TEXT NOT NULL,
    source_type TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 0.5,
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(repo, knowledge_key, knowledge_value)
);
CREATE INDEX IF NOT EXISTS idx_repo_knowledge_key ON repo_knowledge(repo, knowledge_key);

CREATE TABLE IF NOT EXISTS review_patterns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scm_repo TEXT NOT NULL,
    category TEXT NOT NULL,
    pattern_text TEXT NOT NULL,
    example_comments TEXT NOT NULL DEFAULT '[]',
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    promoted_to_instruction INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_review_patterns_category ON review_patterns(scm_repo, category);

CREATE TABLE IF NOT EXISTS strategy_fingerprints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER NOT NULL REFERENCES fix_attempts(id),
    files_explored TEXT NOT NULL DEFAULT '[]',
    tests_run INTEGER NOT NULL DEFAULT 0,
    tools_used TEXT NOT NULL DEFAULT '{}',
    fix_approach TEXT NOT NULL DEFAULT '',
    strategy_summary TEXT NOT NULL DEFAULT '',
    fix_quality_score REAL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_strategy_fingerprints_attempt ON strategy_fingerprints(attempt_id);

CREATE TABLE IF NOT EXISTS issue_clusters (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cluster_key TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL,
    issue_ids TEXT NOT NULL DEFAULT '[]',
    window_start TEXT NOT NULL,
    window_end TEXT NOT NULL,
    resolved_by_issue_id TEXT,
    resolved_by_attempt_id INTEGER,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_issue_clusters_source ON issue_clusters(source, status);

CREATE TABLE IF NOT EXISTS issue_cluster_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cluster_id INTEGER NOT NULL REFERENCES issue_clusters(id),
    issue_id TEXT NOT NULL,
    arrived_at TEXT NOT NULL,
    UNIQUE(cluster_id, issue_id)
);

-- ============================================================
-- Prioritisation Engine Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS content_clusters (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cluster_key TEXT NOT NULL,
    source TEXT NOT NULL,
    representative_issue_id TEXT NOT NULL,
    issue_ids TEXT NOT NULL DEFAULT '[]',
    error_type TEXT,
    culprit TEXT,
    avg_similarity REAL NOT NULL DEFAULT 0.0,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(cluster_key, source)
);
CREATE INDEX IF NOT EXISTS idx_content_clusters_source ON content_clusters(source, status);

CREATE TABLE IF NOT EXISTS severity_scores (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    score REAL NOT NULL,
    severity_component REAL NOT NULL DEFAULT 0.0,
    frequency_component REAL NOT NULL DEFAULT 0.0,
    regression_component REAL NOT NULL DEFAULT 0.0,
    blast_radius_component REAL NOT NULL DEFAULT 0.0,
    cluster_boost REAL NOT NULL DEFAULT 0.0,
    blast_radius TEXT NOT NULL DEFAULT 'core',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(source, issue_id)
);
CREATE INDEX IF NOT EXISTS idx_severity_scores_source_score ON severity_scores(source, score DESC);

CREATE TABLE IF NOT EXISTS suppression_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    rule_name TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(source, issue_id, rule_name)
);
CREATE INDEX IF NOT EXISTS idx_suppression_log_source ON suppression_log(source);

-- ============================================================
-- Code Indexing Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS code_symbols (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    file_path TEXT NOT NULL,
    symbol_name TEXT NOT NULL,
    symbol_kind TEXT NOT NULL,
    parent_symbol TEXT,
    language TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    signature TEXT
);
CREATE INDEX IF NOT EXISTS idx_code_symbols_name ON code_symbols(symbol_name);
CREATE INDEX IF NOT EXISTS idx_code_symbols_kind ON code_symbols(symbol_kind);
CREATE INDEX IF NOT EXISTS idx_code_symbols_file ON code_symbols(repo_id, file_path);

CREATE TABLE IF NOT EXISTS code_chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    file_path TEXT NOT NULL,
    chunk_type TEXT NOT NULL,
    symbol_name TEXT,
    language TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    chunk_text TEXT NOT NULL,
    context_text TEXT NOT NULL,
    file_hash TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_code_chunks_file ON code_chunks(repo_id, file_path);
CREATE INDEX IF NOT EXISTS idx_code_chunks_symbol ON code_chunks(symbol_name);
CREATE INDEX IF NOT EXISTS idx_code_chunks_hash ON code_chunks(repo_id, file_path, file_hash);

CREATE TABLE IF NOT EXISTS code_chunk_embeddings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chunk_id INTEGER NOT NULL UNIQUE REFERENCES code_chunks(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL,
    embedding_model TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS indexing_progress (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    status TEXT NOT NULL DEFAULT 'idle',
    total_repos INTEGER NOT NULL DEFAULT 0,
    indexed_repos INTEGER NOT NULL DEFAULT 0,
    current_repo TEXT,
    current_repo_files INTEGER NOT NULL DEFAULT 0,
    total_files_indexed INTEGER NOT NULL DEFAULT 0,
    started_at TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT OR IGNORE INTO indexing_progress (id) VALUES (1);

-- ============================================================
-- Cross-Repo & Evaluation Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS cross_repo_correlations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_a TEXT NOT NULL,
    repo_b TEXT NOT NULL,
    correlation_count INTEGER NOT NULL DEFAULT 1,
    last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
    window_hours INTEGER NOT NULL DEFAULT 24,
    UNIQUE(repo_a, repo_b)
);

CREATE TABLE IF NOT EXISTS code_complexity (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    avg_cyclomatic REAL,
    max_cyclomatic REAL,
    avg_func_length REAL,
    max_func_length REAL,
    avg_nesting REAL,
    max_nesting REAL,
    total_lines INTEGER,
    function_count INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(repo_id, file_path)
);

CREATE TABLE IF NOT EXISTS eval_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER,
    phase TEXT NOT NULL,
    category TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    exit_code INTEGER NOT NULL DEFAULT -1,
    passed INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0,
    skipped INTEGER NOT NULL DEFAULT 0,
    warnings INTEGER NOT NULL DEFAULT 0,
    errors INTEGER NOT NULL DEFAULT 0,
    diagnostics_json TEXT NOT NULL DEFAULT '[]',
    raw_output TEXT NOT NULL DEFAULT '',
    duration_secs REAL NOT NULL DEFAULT 0.0,
    line_coverage_pct REAL,
    branch_coverage_pct REAL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_eval_snapshots_attempt ON eval_snapshots(attempt_id, phase);

CREATE TABLE IF NOT EXISTS eval_deltas (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id INTEGER,
    repo TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    category TEXT NOT NULL,
    new_passes INTEGER NOT NULL DEFAULT 0,
    new_failures INTEGER NOT NULL DEFAULT 0,
    regressions_json TEXT NOT NULL DEFAULT '[]',
    fixed_json TEXT NOT NULL DEFAULT '[]',
    coverage_delta_pct REAL,
    overall_improved INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_eval_deltas_attempt ON eval_deltas(attempt_id);
