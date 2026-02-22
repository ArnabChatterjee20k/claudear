-- Claudear Demo Database Seed
-- Populates a fresh SQLite DB with realistic demo data.
-- Usage: sqlite3 claudear.db < scripts/seed-demo.sql

-- ── Repositories ────────────────────────────────────────────────────────

INSERT INTO repositories (id, name, path, scm_url, default_branch, file_count, last_indexed_at, created_at) VALUES
  (1, 'acme/api-gateway',      '/home/claudear/repos/api-gateway',      'https://github.com/acme/api-gateway',      'main',    1247, datetime('now', '-1 hour'),  datetime('now', '-60 days')),
  (2, 'acme/web-frontend',     '/home/claudear/repos/web-frontend',     'https://github.com/acme/web-frontend',     'main',    2310, datetime('now', '-1 hour'),  datetime('now', '-60 days')),
  (3, 'acme/billing-service',  '/home/claudear/repos/billing-service',  'https://github.com/acme/billing-service',  'main',     891, datetime('now', '-1 hour'),  datetime('now', '-45 days')),
  (4, 'acme/mobile-app',       '/home/claudear/repos/mobile-app',       'https://github.com/acme/mobile-app',       'develop', 1834, datetime('now', '-2 hours'), datetime('now', '-45 days')),
  (5, 'acme/shared-utils',     '/home/claudear/repos/shared-utils',     'https://github.com/acme/shared-utils',     'main',     342, datetime('now', '-1 hour'),  datetime('now', '-90 days'));

-- ── Repository Dependencies ─────────────────────────────────────────────

INSERT INTO repository_dependencies (upstream_id, downstream_id, dependency_type, created_at) VALUES
  (5, 1, 'npm', datetime('now', '-60 days')),
  (5, 2, 'npm', datetime('now', '-60 days')),
  (5, 3, 'npm', datetime('now', '-45 days')),
  (1, 2, 'api', datetime('now', '-30 days'));

-- ── Issues ──────────────────────────────────────────────────────────────

INSERT INTO issues (source, issue_id, short_id, title, description, url, priority, status, labels, created_at, updated_at) VALUES
  ('linear',  'lin-4892',       'LIN-4892',       'Fix pagination offset in search results',              'Off-by-one error in search pagination',                           'https://linear.app/acme/issue/LIN-4892',        'urgent', 'resolved',    '["bug","claudear"]',       datetime('now', '-1 day'),   datetime('now', '-12 hours')),
  ('sentry',  'sentry-90142',   'SENTRY-90142',   'Null pointer in payment webhook handler',              'NullPointerException at PaymentWebhook.java:42',                 NULL,                                            'high',   'resolved',    '["error","production"]',   datetime('now', '-2 days'),  datetime('now', '-1 day')),
  ('github',  'gh-312',         'GH-312',         'Update deprecated crypto.createCipher',                'crypto.createCipher is deprecated, use createCipheriv instead',   'https://github.com/acme/web-frontend/issues/312','medium', 'resolved',    '["claudear"]',             datetime('now', '-3 days'),  datetime('now', '-2 days')),
  ('discord', 'disc-78',        'DISC-78',        'Mobile nav menu not closing on route change',          'User reported nav stays open after clicking a link on mobile',    NULL,                                            'low',    'resolved',    '["bug","mobile"]',         datetime('now', '-4 days'),  datetime('now', '-3 days')),
  ('linear',  'lin-4887',       'LIN-4887',       'Add retry logic to external API calls',                'External API calls fail silently without retries',                'https://linear.app/acme/issue/LIN-4887',        'medium', 'in_progress', '["enhancement","claudear"]',datetime('now', '-5 days'), datetime('now', '-4 days')),
  ('sentry',  'sentry-89234',   'SENTRY-89234',   'Race condition in WebSocket reconnection',             'WebSocket reconnect fires multiple times causing duplicate msgs', NULL,                                            'urgent', 'in_progress', '["bug","production"]',     datetime('now', '-6 days'),  datetime('now', '-5 days')),
  ('github',  'gh-308',         'GH-308',         'CORS headers missing for preflight requests',          'OPTIONS requests return 403 for cross-origin calls',              'https://github.com/acme/api-gateway/issues/308','high',   'resolved',    '["claudear"]',             datetime('now', '-7 days'),  datetime('now', '-6 days')),
  ('linear',  'lin-4521',       'LIN-4521',       'Auth token refresh race condition',                    'Multiple concurrent requests cause token refresh to race',        'https://linear.app/acme/issue/LIN-4521',        'urgent', 'resolved',    '["bug","claudear"]',       datetime('now', '-8 days'),  datetime('now', '-7 days')),
  ('sentry',  'sentry-88901',   'SENTRY-88901',   'Memory leak in event stream subscription',             'EventSource listeners not cleaned up on component unmount',       NULL,                                            'high',   'failed',      '["error","production"]',   datetime('now', '-9 days'),  datetime('now', '-8 days')),
  ('discord', 'disc-74',        'DISC-74',        'Button click handler firing twice on iOS',             'iOS Safari fires click event on both touchend and click',         NULL,                                            'medium', 'resolved',    '["bug","mobile"]',         datetime('now', '-10 days'), datetime('now', '-9 days')),
  ('linear',  'lin-4510',       'LIN-4510',       'Date range validation edge case',                      'End date before start date passes validation',                    'https://linear.app/acme/issue/LIN-4510',        'low',    'failed',      '["bug","claudear"]',       datetime('now', '-11 days'), datetime('now', '-10 days')),
  ('github',  'gh-299',         'GH-299',         'CSS grid layout breaking on Safari 16',                'Grid template areas not rendering correctly on Safari',           'https://github.com/acme/web-frontend/issues/299','medium', 'resolved',   '["claudear"]',             datetime('now', '-12 days'), datetime('now', '-11 days')),
  ('sentry',  'sentry-88456',   'SENTRY-88456',   'Connection pool exhaustion under load',                'DB connections not returned to pool in error paths',              NULL,                                            'urgent', 'resolved',    '["error","production"]',   datetime('now', '-13 days'), datetime('now', '-12 days')),
  ('linear',  'lin-4498',       'LIN-4498',       'Incorrect error codes in REST API',                    'Several endpoints return 500 instead of 4xx for client errors',   'https://linear.app/acme/issue/LIN-4498',        'high',   'resolved',    '["bug","claudear"]',       datetime('now', '-14 days'), datetime('now', '-13 days')),
  ('discord', 'disc-71',        'DISC-71',        'Dark mode toggle not persisting preference',           'Theme resets to light mode on page reload',                       NULL,                                            'low',    'resolved',    '["bug","claudear"]',       datetime('now', '-15 days'), datetime('now', '-14 days'));

-- ── Fix Attempts ────────────────────────────────────────────────────────

INSERT INTO fix_attempts (id, source, issue_id, short_id, attempted_at, pr_url, scm_repo, scm_pr_number, status, retry_count, issue_labels) VALUES
  (247, 'linear',  'lin-4892',     'LIN-4892',     datetime('now', '-30 minutes'), 'https://github.com/acme/api-gateway/pull/891',      'acme/api-gateway',     891, 'success', 0, '["bug","claudear"]'),
  (246, 'sentry',  'sentry-90142', 'SENTRY-90142', datetime('now', '-2 hours'),    'https://github.com/acme/billing-service/pull/334',   'acme/billing-service', 334, 'merged',  0, '["error","production"]'),
  (245, 'github',  'gh-312',       'GH-312',       datetime('now', '-4 hours'),    'https://github.com/acme/web-frontend/pull/1247',     'acme/web-frontend',   1247, 'merged',  0, '["claudear"]'),
  (244, 'linear',  'lin-4887',     'LIN-4887',     datetime('now', '-5 hours'),    'https://github.com/acme/api-gateway/pull/889',       'acme/api-gateway',     889, 'success', 0, '["enhancement","claudear"]'),
  (243, 'discord', 'disc-78',      'DISC-78',      datetime('now', '-7 hours'),    'https://github.com/acme/web-frontend/pull/1245',     'acme/web-frontend',   1245, 'merged',  0, '["bug","mobile"]'),
  (242, 'sentry',  'sentry-89234', 'SENTRY-89234', datetime('now', '-12 minutes'), NULL,                                                  NULL,                  NULL, 'pending', 0, '["bug","production"]'),
  (241, 'linear',  'lin-4521',     'LIN-4521',     datetime('now', '-12 hours'),   'https://github.com/acme/api-gateway/pull/847',       'acme/api-gateway',     847, 'merged',  0, '["bug","claudear"]'),
  (240, 'sentry',  'sentry-88901', 'SENTRY-88901', datetime('now', '-14 hours'),   NULL,                                                  NULL,                  NULL, 'failed',  1, '["error","production"]');

-- ── Pull Requests ───────────────────────────────────────────────────────

INSERT INTO prs (pr_url, scm_repo, pr_number, attempt_id, issue_id, issue_source, title, author, head_branch, base_branch, status, created_at, merged_at, approvals_count, time_to_first_review_mins, time_to_merge_mins, review_cycles, files_changed, lines_added, lines_removed) VALUES
  ('https://github.com/acme/api-gateway/pull/891',    'acme/api-gateway',     891, 247, 'lin-4892',     'linear',  'fix: resolve pagination offset in search results',       'claudear[bot]', 'claudear/fix-891', 'main', 'open',   datetime('now', '-30 minutes'), NULL,                        0,  NULL, NULL, 0, 3,  24, 8),
  ('https://github.com/acme/billing-service/pull/334', 'acme/billing-service', 334, 246, 'sentry-90142', 'sentry',  'fix: handle null pointer in payment webhook',            'claudear[bot]', 'claudear/fix-334', 'main', 'merged', datetime('now', '-4 hours'),    datetime('now', '-2 hours'), 1,  45, 240, 1, 4,  38, 12),
  ('https://github.com/acme/web-frontend/pull/1247',   'acme/web-frontend',   1247, 245, 'gh-312',       'github',  'fix: update deprecated crypto.createCipher to Cipheriv', 'claudear[bot]', 'claudear/fix-1247','main', 'merged', datetime('now', '-8 hours'),    datetime('now', '-4 hours'), 1,  62, 360, 1, 2,  18, 14),
  ('https://github.com/acme/api-gateway/pull/889',     'acme/api-gateway',     889, 244, 'lin-4887',     'linear',  'feat: add retry logic to external API calls',            'claudear[bot]', 'claudear/fix-889', 'main', 'open',   datetime('now', '-5 hours'),    NULL,                        0, NULL, NULL, 0, 6,  92, 4),
  ('https://github.com/acme/web-frontend/pull/1245',   'acme/web-frontend',   1245, 243, 'disc-78',      'discord', 'fix: mobile nav menu not closing on route change',       'claudear[bot]', 'claudear/fix-1245','main', 'merged', datetime('now', '-12 hours'),   datetime('now', '-7 hours'), 1,  38, 420, 1, 3,  28, 6),
  ('https://github.com/acme/api-gateway/pull/847',     'acme/api-gateway',      847, 241, 'lin-4521',     'linear',  'fix: resolve auth token refresh race condition',         'claudear[bot]', 'claudear/fix-847', 'main', 'merged', datetime('now', '-18 hours'),   datetime('now', '-12 hours'),1,  52, 480, 1, 5,  64, 22);

-- ── Activity Log ────────────────────────────────────────────────────────

INSERT INTO activity_log (timestamp, activity_type, source, issue_id, short_id, message, metadata) VALUES
  (datetime('now', '-18 minutes'), 'issue_detected',   'linear',  'lin-4892',     'LIN-4892',     'New issue detected: Fix pagination offset in search results', '{"confidence":0.96,"repo":"acme/api-gateway"}'),
  (datetime('now', '-30 minutes'), 'pr_created',       'linear',  'lin-4892',     'LIN-4892',     'PR #891 created for LIN-4892',                               '{"pr_url":"https://github.com/acme/api-gateway/pull/891"}'),
  (datetime('now', '-2 hours'),    'pr_merged',        'sentry',  'sentry-90142', 'SENTRY-90142', 'PR #334 merged for SENTRY-90142',                             NULL),
  (datetime('now', '-150 minutes'),'issue_resolved',   'sentry',  'sentry-90142', 'SENTRY-90142', 'Issue auto-resolved after PR merge',                          NULL),
  (datetime('now', '-4 hours'),    'pr_merged',        'github',  'gh-312',       'GH-312',       'PR #1247 merged for GH-312',                                  NULL),
  (datetime('now', '-5 hours'),    'agent_spawn',      'linear',  'lin-4887',     'LIN-4887',     'Claude Code agent spawned for LIN-4887',                      '{"model":"sonnet","repo":"acme/api-gateway"}'),
  (datetime('now', '-7 hours'),    'question_asked',   'discord', 'disc-78',      'DISC-78',      'Human input needed: Which CSS framework handles the nav?',    '{"channel":"discord"}'),
  (datetime('now', '-450 minutes'),'question_answered', 'discord', 'disc-78',      'DISC-78',      'Answer received: Tailwind CSS with custom components',        '{"responder":"jake"}'),
  (datetime('now', '-12 hours'),   'regression_clear', 'linear',  'lin-4521',     'LIN-4521',     'Regression watch cleared for LIN-4521 (24h passed)',          NULL),
  (datetime('now', '-14 hours'),   'attempt_failed',   'sentry',  'sentry-88901', 'SENTRY-88901', 'Fix attempt failed: test suite assertion failure',            '{"error":"Test suite assertion failure"}');

-- ── Processing Metrics ──────────────────────────────────────────────────

INSERT INTO processing_metrics (timestamp, metric_name, metric_value, source) VALUES
  (datetime('now', '-1 hour'),  'processing_time_secs', 142, 'linear'),
  (datetime('now', '-2 hours'), 'processing_time_secs', 186, 'sentry'),
  (datetime('now', '-3 hours'), 'processing_time_secs', 210, 'github'),
  (datetime('now', '-4 hours'), 'processing_time_secs', 158, 'discord'),
  (datetime('now', '-5 hours'), 'processing_time_secs', 174, 'linear'),
  (datetime('now', '-6 hours'), 'processing_time_secs', 198, 'sentry');

-- ── Error Patterns ──────────────────────────────────────────────────────

INSERT INTO error_patterns (pattern_hash, error_type, error_message, first_seen, last_seen, occurrence_count, sources, example_issue_ids, resolution_hints) VALUES
  ('a1b2c3', 'test_failure',  'Test suite assertion failure',       datetime('now', '-30 days'), datetime('now', '-14 hours'), 8, '["linear","sentry"]',  '["LIN-4510","SENTRY-88901"]', 'Check test fixtures and mock data'),
  ('d4e5f6', 'timeout',       'Claude process timed out after 6h',  datetime('now', '-21 days'), datetime('now', '-5 days'),   4, '["linear"]',           '["LIN-4312"]',                'Issue may be too complex for single attempt'),
  ('g7h8i9', 'build_failure', 'TypeScript compilation error',       datetime('now', '-14 days'), datetime('now', '-3 days'),   3, '["github","linear"]',  '["GH-290"]',                  'Verify type definitions match');

-- ── Feedback Outcomes ───────────────────────────────────────────────────

INSERT INTO feedback_outcomes (attempt_id, source, issue_id, issue_text, prompt_used, outcome, error_type, learnings, keywords) VALUES
  (246, 'sentry',  'sentry-90142', 'Null pointer in payment webhook handler',       'Fix the null pointer exception...',      'merged', NULL,           'Added null check before accessing nested payment.metadata.customer_id', '["null-pointer","webhook","payment"]'),
  (245, 'github',  'gh-312',       'Update deprecated crypto API',                  'Update deprecated crypto.createCipher...','merged', NULL,           'Replaced createCipher with createCipheriv using random IV',             '["crypto","deprecation","security"]'),
  (240, 'sentry',  'sentry-88901', 'Memory leak in event stream',                   'Fix the memory leak...',                 'failed', 'test_failure', 'EventSource cleanup requires removing all listeners before closing',    '["memory-leak","event-stream","cleanup"]'),
  (243, 'discord', 'disc-78',      'Mobile nav not closing',                        'Fix the mobile navigation...',           'merged', NULL,           'Added useEffect cleanup with route change listener',                    '["mobile","navigation","react"]'),
  (241, 'linear',  'lin-4521',     'Auth token refresh race condition',             'Fix the auth token refresh...',          'merged', NULL,           'Used mutex lock pattern to prevent concurrent token refresh',           '["auth","race-condition","token"]');

-- ── Regression Watches ──────────────────────────────────────────────────

INSERT INTO regression_watches (issue_type, issue_id, fix_attempt_id, status, pr_merged_at, monitoring_started_at, resolved_at, regressed_at, created_at) VALUES
  ('linear',  'lin-4892',     247, 'monitoring', NULL,                        datetime('now', '-30 minutes'), NULL,                       NULL, datetime('now', '-30 minutes')),
  ('sentry',  'sentry-90142', 246, 'monitoring', datetime('now', '-2 hours'), datetime('now', '-2 hours'),    NULL,                       NULL, datetime('now', '-2 hours')),
  ('github',  'gh-312',       245, 'resolved',   datetime('now', '-28 hours'),datetime('now', '-28 hours'),   datetime('now', '-4 hours'),NULL, datetime('now', '-28 hours')),
  ('linear',  'lin-4521',     241, 'resolved',   datetime('now', '-36 hours'),datetime('now', '-36 hours'),   datetime('now', '-12 hours'),NULL,datetime('now', '-36 hours'));

-- ── Prompt Experiments ──────────────────────────────────────────────────

INSERT INTO prompt_experiments (experiment_name, variant, prompt_template, prompt_hash, active, success_count, failure_count, avg_time_to_merge, avg_review_score) VALUES
  ('prompt_v2', 'structured_cot', 'Think step by step...', 'abc123', 1, 42, 5, 320, 4.2),
  ('prompt_v2', 'baseline',       'Fix the following issue...', 'def456', 0, 38, 8, 380, 3.8);

-- ── Inference Attempts ──────────────────────────────────────────────────

INSERT INTO inference_attempts (issue_id, issue_source, extracted_keywords, inferred_repo_id, confidence, inference_reason, was_correct, inference_duration_ms, created_at) VALUES
  ('lin-4892',     'linear',  'pagination, search, offset',    1, 'high',   'Stack trace and file path analysis', 1, 142, datetime('now', '-1 day')),
  ('sentry-90142', 'sentry',  'payment, webhook, null',        3, 'high',   'Stack trace and file path analysis', 1, 156, datetime('now', '-2 days')),
  ('gh-312',       'github',  'crypto, cipher, deprecated',    2, 'high',   'File path analysis',                 1, 128, datetime('now', '-3 days')),
  ('disc-78',      'discord', 'mobile, nav, route',            2, 'medium', 'Content analysis',                   1, 168, datetime('now', '-4 days')),
  ('lin-4887',     'linear',  'retry, api, external',          1, 'high',   'Stack trace and file path analysis', 1, 134, datetime('now', '-5 days'));

-- ── Review Patterns ─────────────────────────────────────────────────────

INSERT INTO review_patterns (scm_repo, category, pattern_text, example_comments, occurrence_count, promoted_to_instruction) VALUES
  ('acme/api-gateway', 'error_handling', 'Wrap async handlers in try/catch',  '["Please add error handling here","This should catch the async error"]', 14, 1),
  ('acme/api-gateway', 'testing',        'Add test for edge case',            '["Can you add a test for empty input?","Missing test for null case"]',   11, 0);

-- ── Promoted Instructions ───────────────────────────────────────────────

INSERT INTO promoted_instructions (repo, source_type, instruction_text, occurrence_count, confidence, is_active) VALUES
  ('acme/api-gateway', 'review_pattern', 'Always add integration tests for new API endpoints', 18, 0.94, 1),
  ('acme/api-gateway', 'review_pattern', 'Use zod for request validation schemas',             12, 0.91, 1);

-- ── Strategy Fingerprints ───────────────────────────────────────────────

INSERT INTO strategy_fingerprints (attempt_id, files_explored, tests_run, tools_used, fix_approach, strategy_summary, fix_quality_score) VALUES
  (247, '["src/routes/search.ts","src/utils/pagination.ts","test/routes/search.test.ts"]', 3, '{"Read":8,"Edit":3,"Bash":5}', 'Modified pagination offset calculation', 'Identified off-by-one error in pagination utility, fixed calculation and updated tests', 0.92);

-- ── Cross-Repo Correlations ─────────────────────────────────────────────

INSERT INTO cross_repo_correlations (repo_a, repo_b, correlation_count, last_seen_at, window_hours) VALUES
  ('acme/api-gateway',  'acme/web-frontend',  8, datetime('now', '-1 day'),  48),
  ('acme/api-gateway',  'acme/shared-utils',  5, datetime('now', '-3 days'), 48);
