#!/usr/bin/env bash
set -Eeuo pipefail

###############################################################################
# Production E2E Smoke Test
#
# Tests the full claudear lifecycle across two scenarios:
#   Scenario 1: Linear source -> daemon poll -> PR -> review comments ->
#               request_changes review -> merge -> regression watch -> resolve
#   Scenario 2: Discord source -> ask/question flow -> PR -> merge ->
#               simulated regression -> retry -> re-merge -> resolve
#
# Required env vars:
#   CLAUDEAR_E2E_LINEAR_API_KEY
#   CLAUDEAR_E2E_LINEAR_TEAM_ID
#   CLAUDEAR_E2E_GITHUB_REPO        (owner/repo format)
#   CLAUDEAR_E2E_GITHUB_TOKEN
#   CLAUDEAR_E2E_DISCORD_BOT_TOKEN  (Scenario 2 only, skipped if absent)
#   CLAUDEAR_E2E_DISCORD_CHANNEL_ID (Scenario 2 only, skipped if absent)
#   Claude auth via ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, or CLI login
###############################################################################

# =============================================================================
# 1. Preamble & Constants
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Timeouts (seconds)
WAIT_TIMEOUT="${CLAUDEAR_E2E_WAIT_TIMEOUT:-600}"
POLL_INTERVAL=5
CLAUDE_TIMEOUT="${CLAUDEAR_E2E_CLAUDE_TIMEOUT_SECS:-600}"

# Ports for daemon instances
S1_PORT=3150
S2_PORT=3151

# Docker mode: run claudear inside Docker container to avoid nested claude issues
USE_DOCKER="${CLAUDEAR_E2E_USE_DOCKER:-false}"
DOCKER_IMAGE="${CLAUDEAR_E2E_DOCKER_IMAGE:-claudear-app:latest}"

# Track all daemon PIDs/containers for cleanup
DAEMON_PIDS=()
DOCKER_CONTAINERS=()

# Track all PR numbers/branches for cleanup
declare -a CLEANUP_PR_NUMBERS=()
declare -a CLEANUP_PR_BRANCHES=()
declare -a CLEANUP_LINEAR_IDS=()

# Scenario status
S1_STATUS="not_started"
S2_STATUS="not_started"
FINAL_STATUS="failed"

# Warning counter
WARN_COUNT=0

# =============================================================================
# 2. Utility Functions
# =============================================================================

log() {
  printf '[prod-e2e] %s\n' "$*"
}

log_checkpoint() {
  printf '\n[prod-e2e] ========== %s ==========\n\n' "$*"
}

warn() {
  printf '[prod-e2e][WARN] %s\n' "$*" >&2
  ((WARN_COUNT++)) || true
}

fail() {
  printf '[prod-e2e][FAIL] %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    fail "Missing required environment variable: ${name}"
  fi
}

has_claude_session() {
  local status_json
  status_json="$(claude auth status 2>/dev/null || true)"
  jq -e '.loggedIn == true' >/dev/null 2>&1 <<<"$status_json"
}

ensure_claude_auth() {
  if [[ -n "${ANTHROPIC_API_KEY:-}" || -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]]; then
    return 0
  fi
  if has_claude_session; then
    log "Using existing Claude CLI login session"
    return 0
  fi
  fail "Claude auth missing. Set ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN, or run 'claude auth login'."
}

# wait_for DESCRIPTION TIMEOUT_SECS POLL_SECS CONDITION_CMD...
# Waits for a condition to become true, polling at POLL_SECS interval.
wait_for() {
  local desc="$1" timeout="$2" poll="$3"
  shift 3
  local elapsed=0
  local progress_interval=30
  local next_progress=$progress_interval
  log "Waiting for: ${desc} (timeout: ${timeout}s)"
  while ! "$@" 2>/dev/null; do
    sleep "$poll"
    elapsed=$((elapsed + poll))
    if [[ $elapsed -ge $timeout ]]; then
      fail "Timeout after ${timeout}s waiting for: ${desc}"
    fi
    if [[ $elapsed -ge $next_progress ]]; then
      log "  ...still waiting (${elapsed}s elapsed): ${desc}"
      next_progress=$((next_progress + progress_interval))
    fi
  done
  log "Condition met: ${desc} (${elapsed}s)"
}

# wait_for_pr_verbose DB SOURCE ISSUE_ID TIMEOUT POLL DAEMON_LOG
# Like wait_for but with detailed Claude execution progress reporting.
wait_for_pr_verbose() {
  local db="$1" source="$2" issue_id="$3" timeout="$4" poll="$5" daemon_log="$6"
  local elapsed=0
  local last_exec_count=0
  local progress_interval=15
  local next_progress=$progress_interval

  log "Waiting for: PR creation (timeout: ${timeout}s)"

  while true; do
    # Check if PR exists
    local pr_url
    pr_url="$(db_query "$db" "SELECT pr_url FROM fix_attempts WHERE source='${source}' AND issue_id='${issue_id}' AND pr_url IS NOT NULL AND pr_url != '' ORDER BY id DESC LIMIT 1")"
    if [[ -n "$pr_url" ]]; then
      log "Condition met: PR created (${elapsed}s) -> ${pr_url}"
      return 0
    fi

    sleep "$poll"
    elapsed=$((elapsed + poll))

    if [[ $elapsed -ge $timeout ]]; then
      warn "Timeout waiting for PR. Dumping diagnostics:"
      warn "fix_attempts:"
      db_query "$db" "SELECT id, status, COALESCE(error_message,'(none)') FROM fix_attempts WHERE source='${source}' AND issue_id='${issue_id}' ORDER BY id DESC LIMIT 5" >&2
      warn "claude_executions (last 5):"
      db_query "$db" "SELECT id, exit_code, timed_out, duration_secs FROM claude_executions ORDER BY id DESC LIMIT 5" >&2
      warn "Daemon log tail:"
      tail -30 "$daemon_log" >&2
      fail "Timeout after ${timeout}s waiting for PR creation"
    fi

    # Progress reporting
    if [[ $elapsed -ge $next_progress ]]; then
      local attempt_status exec_count
      attempt_status="$(db_query "$db" "SELECT status FROM fix_attempts WHERE source='${source}' AND issue_id='${issue_id}' ORDER BY id DESC LIMIT 1")"
      exec_count="$(db_count "$db" "SELECT 1 FROM claude_executions")"

      if [[ "$exec_count" -gt "$last_exec_count" ]]; then
        local latest_exit latest_dur
        latest_exit="$(db_query "$db" "SELECT exit_code FROM claude_executions ORDER BY id DESC LIMIT 1")"
        latest_dur="$(db_query "$db" "SELECT duration_secs FROM claude_executions ORDER BY id DESC LIMIT 1")"
        log "  ...${elapsed}s | attempt=${attempt_status} | executions=${exec_count} | latest: exit=${latest_exit} dur=${latest_dur}s"
        last_exec_count="$exec_count"
      else
        log "  ...${elapsed}s | attempt=${attempt_status} | executions=${exec_count} (Claude running...)"
      fi
      next_progress=$((next_progress + progress_interval))
    fi
  done
}

# DB helpers
db_query() {
  local db="$1" sql="$2"
  sqlite3 -readonly -noheader -separator $'\t' "$db" "$sql" 2>/dev/null || true
}

db_count() {
  local db="$1" sql="$2"
  local result
  result="$(sqlite3 -readonly -noheader "$db" "SELECT COUNT(*) FROM ($sql);" 2>/dev/null || echo "0")"
  echo "${result:-0}"
}

db_exec() {
  local db="$1" sql="$2"
  sqlite3 "$db" "$sql"
}

# assert_db DB TABLE WHERE_CLAUSE MIN_COUNT MESSAGE [FATAL=true]
# Asserts that a SQL query returns at least MIN_COUNT rows.
assert_db() {
  local db="$1" table="$2" where="$3" min="$4" msg="$5" fatal="${6:-true}"
  local sql="SELECT * FROM ${table} WHERE ${where}"
  local count
  count="$(db_count "$db" "$sql")"
  if [[ "$count" -lt "$min" ]]; then
    if [[ "$fatal" == "true" ]]; then
      fail "DB assert failed: ${msg} (expected >= ${min} rows, got ${count}) | SQL: ${sql}"
    else
      warn "DB assert (non-fatal): ${msg} (expected >= ${min} rows, got ${count})"
    fi
  else
    log "  DB OK: ${msg} (${count} rows)"
  fi
}

# assert_db_field DB SQL FIELD_INDEX EXPECTED MESSAGE [FATAL=true]
# Asserts that a specific field in the first row matches expected value.
assert_db_field() {
  local db="$1" sql="$2" field_idx="$3" expected="$4" msg="$5" fatal="${6:-true}"
  local row
  row="$(db_query "$db" "$sql" | head -n1)"
  if [[ -z "$row" ]]; then
    if [[ "$fatal" == "true" ]]; then
      fail "DB field assert failed (no rows): ${msg} | SQL: ${sql}"
    else
      warn "DB field assert (non-fatal, no rows): ${msg}"
      return
    fi
  fi
  local actual
  actual="$(echo "$row" | cut -f"$field_idx")"
  if [[ "$actual" != "$expected" ]]; then
    if [[ "$fatal" == "true" ]]; then
      fail "DB field assert failed: ${msg} (expected '${expected}', got '${actual}')"
    else
      warn "DB field assert (non-fatal): ${msg} (expected '${expected}', got '${actual}')"
    fi
  else
    log "  DB OK: ${msg} = '${actual}'"
  fi
}

# GitHub API helpers
gh_api() {
  local method="$1" path="$2"
  shift 2
  curl -sSfL \
    -X "$method" \
    -H "Authorization: Bearer ${CLAUDEAR_E2E_GITHUB_TOKEN}" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com${path}" \
    "$@"
}

gh_api_nofail() {
  local method="$1" path="$2"
  shift 2
  curl -sSL \
    -X "$method" \
    -H "Authorization: Bearer ${CLAUDEAR_E2E_GITHUB_TOKEN}" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com${path}" \
    "$@" 2>/dev/null || true
}

# GitHub API helper using reviewer token (for posting reviews on PRs owned by the main token)
gh_api_reviewer() {
  local method="$1" path="$2"
  shift 2
  curl -sSfL \
    -X "$method" \
    -H "Authorization: Bearer ${CLAUDEAR_E2E_GITHUB_REVIEWER_TOKEN}" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com${path}" \
    "$@"
}

# Linear GraphQL helper
linear_graphql() {
  local query="$1"
  local _empty_json='{}'
  local variables_json="${2:-$_empty_json}"
  local payload
  payload="$(
    jq -cn \
      --arg query "$query" \
      --argjson variables "$variables_json" \
      '{query: $query, variables: $variables}'
  )"
  curl -sSfL "https://api.linear.app/graphql" \
    -H "Authorization: ${CLAUDEAR_E2E_LINEAR_API_KEY}" \
    -H "Content-Type: application/json" \
    --data "$payload"
}

# Discord API helper
discord_api() {
  local method="$1" path="$2"
  shift 2
  curl -sSfL \
    -X "$method" \
    -H "Authorization: Bot ${CLAUDEAR_E2E_DISCORD_BOT_TOKEN}" \
    -H "Content-Type: application/json" \
    "https://discord.com/api/v10${path}" \
    "$@"
}

discord_api_nofail() {
  local method="$1" path="$2"
  shift 2
  curl -sSL \
    -X "$method" \
    -H "Authorization: Bot ${CLAUDEAR_E2E_DISCORD_BOT_TOKEN}" \
    -H "Content-Type: application/json" \
    "https://discord.com/api/v10${path}" \
    "$@" 2>/dev/null || true
}

# Parse PR number from URL
parse_pr_number() {
  local url="$1"
  if [[ "$url" =~ /pull/([0-9]+)/?$ ]]; then
    echo "${BASH_REMATCH[1]}"
  fi
}

# Parse head branch from PR via API
get_pr_branch() {
  local pr_number="$1"
  local resp
  resp="$(gh_api GET "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${pr_number}")"
  jq -r '.head.ref // empty' <<<"$resp"
}

# Kill daemon by PID if alive (non-docker mode)
kill_daemon_pid() {
  local pid="$1"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    log "Killed daemon PID ${pid}"
  fi
}

# Stop Docker container
stop_container() {
  local name="$1"
  [[ -n "$name" ]] || return 0
  docker stop "$name" 2>/dev/null || true
  docker rm -f "$name" 2>/dev/null || true
  log "Stopped container ${name}"
}

# Unified daemon start: returns a handle (PID or container name)
# Usage: daemon_handle="$(start_daemon CONFIG PORT LOG_FILE)"
start_daemon() {
  local config_path="$1" port="$2" daemon_log="$3"
  if [[ "$USE_DOCKER" == "true" ]]; then
    local mount_dir
    mount_dir="$(dirname "$config_path")"
    local name="claudear-e2e-${port}-${RANDOM}"

    # Resolve Claude auth token for the container
    local claude_oauth="${CLAUDE_CODE_OAUTH_TOKEN:-}"
    local api_key="${ANTHROPIC_API_KEY:-}"
    if [[ -z "$api_key" && -z "$claude_oauth" ]]; then
      # Try macOS keychain (Claude Code stores OAuth as JSON in keychain)
      local keychain_json
      keychain_json="$(security find-generic-password -s "Claude Code-credentials" -w 2>/dev/null || true)"
      if [[ -n "$keychain_json" ]]; then
        claude_oauth="$(echo "$keychain_json" | jq -r '.claudeAiOauth.accessToken // empty' 2>/dev/null || true)"
      fi
    fi
    [[ -n "$api_key" || -n "$claude_oauth" ]] || fail "No Claude auth available (ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, or macOS keychain)"

    # Write env vars to a temp file to avoid shell escaping issues with tokens
    local env_file="${mount_dir}/.docker-env-${name}"
    {
      printf 'ANTHROPIC_API_KEY=%s\n' "$api_key"
      printf 'CLAUDE_CODE_OAUTH_TOKEN=%s\n' "$claude_oauth"
    } > "$env_file"

    docker run -d \
      --name "$name" \
      -p "${port}:${port}" \
      -v "${mount_dir}:${mount_dir}" \
      --env-file "$env_file" \
      "${DOCKER_IMAGE}" \
      claudear --config "$config_path" start --poll --poll-interval 5000 --port "$port" --no-webhooks \
      >/dev/null

    rm -f "$env_file"
    # Stream container logs to the daemon log file
    docker logs -f "$name" >>"$daemon_log" 2>&1 &
    DOCKER_CONTAINERS+=("$name")
    echo "$name"
  else
    "$CLAUDEAR_BIN" --config "$config_path" start --poll --poll-interval 5000 --port "$port" --no-webhooks >>"$daemon_log" 2>&1 &
    local pid=$!
    DAEMON_PIDS+=("$pid")
    echo "$pid"
  fi
}

# Unified daemon kill
kill_daemon() {
  local handle="$1"
  [[ -n "$handle" ]] || return 0
  if [[ "$USE_DOCKER" == "true" ]]; then
    stop_container "$handle"
  else
    kill_daemon_pid "$handle"
  fi
}

# Unified daemon alive check
check_daemon_alive() {
  local handle="$1"
  if [[ "$USE_DOCKER" == "true" ]]; then
    [[ "$(docker inspect -f '{{.State.Running}}' "$handle" 2>/dev/null)" == "true" ]]
  else
    kill -0 "$handle" 2>/dev/null
  fi
}

# =============================================================================
# 3. Cleanup (trap EXIT)
# =============================================================================

resolve_linear_issue() {
  local issue_id="$1"
  [[ -n "$issue_id" ]] || return 0

  local query_states='query TeamStates($teamId: String!) {
    team(id: $teamId) {
      states { nodes { id type name } }
    }
  }'
  local vars_states
  vars_states="$(jq -cn --arg teamId "$CLAUDEAR_E2E_LINEAR_TEAM_ID" '{teamId: $teamId}')"

  local states_resp
  if ! states_resp="$(linear_graphql "$query_states" "$vars_states" 2>/dev/null)"; then
    warn "Failed to query Linear team states for issue ${issue_id}"
    return 0
  fi

  local completed_state_id
  completed_state_id="$(
    jq -r '.data.team.states.nodes[]? | select(.type=="completed") | .id' <<<"$states_resp" | head -n1
  )"
  [[ -n "$completed_state_id" ]] || return 0

  local mutation='mutation ResolveIssue($id: String!, $stateId: String!) {
    issueUpdate(id: $id, input: { stateId: $stateId }) { success }
  }'
  local vars_resolve
  vars_resolve="$(jq -cn --arg id "$issue_id" --arg stateId "$completed_state_id" '{id: $id, stateId: $stateId}')"
  linear_graphql "$mutation" "$vars_resolve" >/dev/null 2>&1 || true
  log "Resolved Linear issue ${issue_id}"
}

close_github_pr() {
  local pr_number="$1"
  [[ -n "$pr_number" ]] || return 0
  gh_api_nofail PATCH "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${pr_number}" \
    --data '{"state":"closed"}' >/dev/null
  log "Closed GitHub PR #${pr_number}"
}

delete_github_branch() {
  local branch="$1"
  [[ -n "$branch" ]] || return 0
  local branch_escaped="${branch//\//%2F}"
  gh_api_nofail DELETE "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/git/refs/heads/${branch_escaped}" >/dev/null
  log "Deleted branch ${branch}"
}

cleanup() {
  local exit_code=$?
  set +e

  log "Cleaning up..."

  # Kill all daemon processes
  for pid in ${DAEMON_PIDS[@]+"${DAEMON_PIDS[@]}"}; do
    kill_daemon_pid "$pid"
  done

  # Stop all Docker containers
  for name in ${DOCKER_CONTAINERS[@]+"${DOCKER_CONTAINERS[@]}"}; do
    stop_container "$name"
  done

  # Close PRs (only if not merged)
  for pr in ${CLEANUP_PR_NUMBERS[@]+"${CLEANUP_PR_NUMBERS[@]}"}; do
    close_github_pr "$pr"
  done

  # Delete branches
  if [[ "${CLAUDEAR_E2E_DELETE_BRANCH:-false}" == "true" ]]; then
    for branch in ${CLEANUP_PR_BRANCHES[@]+"${CLEANUP_PR_BRANCHES[@]}"}; do
      delete_github_branch "$branch"
    done
  fi

  # Resolve Linear issues
  for issue_id in ${CLEANUP_LINEAR_IDS[@]+"${CLEANUP_LINEAR_IDS[@]}"}; do
    resolve_linear_issue "$issue_id"
  done

  # Clean temp directories
  for dir in "${S1_TMP_DIR:-}" "${S2_TMP_DIR:-}"; do
    if [[ -n "$dir" && -d "$dir" ]]; then
      rm -rf "$dir"
    fi
  done

  if [[ "$FINAL_STATUS" == "passed" ]]; then
    log "Production E2E smoke test PASSED (warnings: ${WARN_COUNT})"
  else
    log "Production E2E smoke test FAILED"
  fi

  exit "$exit_code"
}
trap cleanup EXIT

# =============================================================================
# 4. Global Setup - Prerequisites & Binary Build
# =============================================================================

require_cmd git
require_cmd curl
require_cmd jq
require_cmd sqlite3

if [[ "$USE_DOCKER" == "true" ]]; then
  require_cmd docker
  log "Docker mode enabled (image: ${DOCKER_IMAGE})"
  if ! docker image inspect "${DOCKER_IMAGE}" >/dev/null 2>&1; then
    log "Pulling Docker image ${DOCKER_IMAGE}..."
    docker pull "${DOCKER_IMAGE}" || {
      log "Pull failed, building locally..."
      (cd "$PROJECT_ROOT" && docker compose build)
      DOCKER_IMAGE="claudear-app:latest"
    }
  fi
else
  if [[ -z "${CLAUDEAR_E2E_BINARY:-}" ]] || [[ ! -x "${CLAUDEAR_E2E_BINARY:-}" ]]; then
    require_cmd cargo
  fi
  require_cmd claude
  ensure_claude_auth
fi

require_env CLAUDEAR_E2E_LINEAR_API_KEY
require_env CLAUDEAR_E2E_LINEAR_TEAM_ID
require_env CLAUDEAR_E2E_GITHUB_REPO
require_env CLAUDEAR_E2E_GITHUB_TOKEN
require_env CLAUDEAR_E2E_GITHUB_REVIEWER_TOKEN

if [[ ! "${CLAUDEAR_E2E_GITHUB_REPO}" =~ ^[^/]+/[^/]+$ ]]; then
  fail "CLAUDEAR_E2E_GITHUB_REPO must be in owner/repo format"
fi

REPO_OWNER="${CLAUDEAR_E2E_GITHUB_REPO%%/*}"
REPO_NAME="${CLAUDEAR_E2E_GITHUB_REPO##*/}"

CLAUDEAR_BIN="${CLAUDEAR_E2E_BINARY:-${PROJECT_ROOT}/target/release/claudear}"
if [[ "$USE_DOCKER" != "true" ]]; then
  if [[ ! -x "$CLAUDEAR_BIN" ]]; then
    log "Building release binary..."
    (cd "$PROJECT_ROOT" && cargo build --release)
  fi
  [[ -x "$CLAUDEAR_BIN" ]] || fail "Expected executable at ${CLAUDEAR_BIN}"
fi

# Check if Discord scenario is available
HAS_DISCORD=false
if [[ -n "${CLAUDEAR_E2E_DISCORD_BOT_TOKEN:-}" && -n "${CLAUDEAR_E2E_DISCORD_CHANNEL_ID:-}" ]]; then
  HAS_DISCORD=true
  log "Discord credentials found - Scenario 2 enabled"
else
  log "Discord credentials not found - Scenario 2 will be skipped"
fi

# =============================================================================
# 5. Helper: Find/Create "bug" Label on Linear Team
# =============================================================================

# find_or_create_label LABEL_NAME [COLOR]
# Finds or creates a label on the Linear team. Outputs the label ID.
find_or_create_label() {
  local label_name="$1"
  local color="${2:-#e11d48}"

  local query='query Labels($teamId: String!) {
    team(id: $teamId) {
      labels { nodes { id name } }
    }
  }'
  local vars
  vars="$(jq -cn --arg teamId "$CLAUDEAR_E2E_LINEAR_TEAM_ID" '{teamId: $teamId}')"
  local resp
  resp="$(linear_graphql "$query" "$vars")"

  local label_id
  label_id="$(jq -r --arg name "$label_name" '.data.team.labels.nodes[]? | select(.name==$name) | .id' <<<"$resp" | head -n1)"

  if [[ -n "$label_id" ]]; then
    echo "$label_id"
    return
  fi

  # Also check workspace labels
  local ws_query='query { issueLabels { nodes { id name } } }'
  local ws_resp
  ws_resp="$(linear_graphql "$ws_query")" 2>/dev/null || true
  label_id="$(jq -r --arg name "$label_name" '.data.issueLabels.nodes[]? | select(.name==$name) | .id' <<<"$ws_resp" | head -n1)" 2>/dev/null || true

  if [[ -n "$label_id" ]]; then
    echo "$label_id"
    return
  fi

  # Create the label
  local mutation='mutation CreateLabel($teamId: String!, $name: String!, $color: String!) {
    issueLabelCreate(input: { teamId: $teamId, name: $name, color: $color }) {
      success
      issueLabel { id }
    }
  }'
  local create_vars
  create_vars="$(jq -cn --arg teamId "$CLAUDEAR_E2E_LINEAR_TEAM_ID" --arg name "$label_name" --arg color "$color" '{teamId: $teamId, name: $name, color: $color}')"
  local create_resp
  create_resp="$(linear_graphql "$mutation" "$create_vars")"
  jq -r '.data.issueLabelCreate.issueLabel.id // empty' <<<"$create_resp"
}

# =============================================================================
# 6. Helper: Clone Mock Repo
# =============================================================================

clone_mock_repo() {
  local dest="$1"
  git clone \
    "https://x-access-token:${CLAUDEAR_E2E_GITHUB_TOKEN}@github.com/${CLAUDEAR_E2E_GITHUB_REPO}.git" \
    "$dest" >/dev/null 2>&1 || fail "Failed to clone ${CLAUDEAR_E2E_GITHUB_REPO}"

  # Ensure the repo has at least one commit on main (empty repos break worktree creation)
  if ! git -C "$dest" rev-parse HEAD >/dev/null 2>&1; then
    log "Seeding empty repo with initial commit..."
    git -C "$dest" checkout -b main 2>/dev/null || true
    echo "# E2E Test Repository" > "$dest/README.md"
    git -C "$dest" add README.md
    git -C "$dest" -c user.name="Claudear E2E" -c user.email="e2e@claudear.local" commit -m "chore: seed repo for e2e testing" >/dev/null
    git -C "$dest" push -u origin main >/dev/null 2>&1 || fail "Failed to push initial commit"
    log "Seeded repo with initial commit on main"
  fi
}

# =============================================================================
# 7. Helper: Generate Config TOML
# =============================================================================

generate_config() {
  local config_path="$1"
  local work_dir="$2"
  local repos_dir="$3"
  local db_path="$4"
  local port="$5"
  local discord_source="${6:-false}"
  local ask_enabled="${7:-false}"
  local claude_instructions="${8:-}"

  cat >"$config_path" <<TOML
work_dir = "${work_dir}"
known_orgs = ["${REPO_OWNER}"]
auto_discover_paths = ["${repos_dir}"]
poll_interval_ms = 5000
webhook_port = ${port}
db_path = "${db_path}"
max_issues_per_cycle = 1
max_concurrent = 1
processing_delay_ms = 0
claude_timeout_secs = ${CLAUDE_TIMEOUT}

[claude]
skip_permissions = true
instructions = "${claude_instructions}"

[ask]
enabled = ${ask_enabled}
wait_timeout_secs = 120
poll_interval_secs = 5
best_effort_on_timeout = true

[github]
token = "${CLAUDEAR_E2E_GITHUB_TOKEN}"
auto_resolve_on_merge = false
review_trigger = "/claudear"

[discord]
bot_token = "${CLAUDEAR_E2E_DISCORD_BOT_TOKEN:-}"
channel_id = "${CLAUDEAR_E2E_DISCORD_CHANNEL_ID:-}"
source_enabled = ${discord_source}
listen_channel_id = "${CLAUDEAR_E2E_DISCORD_CHANNEL_ID:-}"

[linear]
enabled = true
api_key = "${CLAUDEAR_E2E_LINEAR_API_KEY}"
trigger_labels = ["claudear"]
trigger_states = []
team_id = "${CLAUDEAR_E2E_LINEAR_TEAM_ID}"

[regression]
enabled = true
check_interval_secs = 10
monitoring_duration_secs = 10

[learning]
auto_extract_learnings = true
diff_analysis = true
qa_promotion = true
repo_knowledge = true
review_classification = true
strategy_fingerprinting = true
quality_scoring = true
cluster_detection = true
TOML
}

# =============================================================================
# 8. Helper: Condition checkers for wait_for
# =============================================================================

check_fix_attempt_exists() {
  local db="$1" source="$2" issue_id="$3"
  local count
  count="$(db_count "$db" "SELECT 1 FROM fix_attempts WHERE source='${source}' AND issue_id='${issue_id}'")"
  [[ "$count" -ge 1 ]]
}

check_fix_attempt_has_pr() {
  local db="$1" source="$2" issue_id="$3"
  local pr_url
  pr_url="$(db_query "$db" "SELECT pr_url FROM fix_attempts WHERE source='${source}' AND issue_id='${issue_id}' AND pr_url IS NOT NULL AND pr_url != '' ORDER BY id DESC LIMIT 1")"
  [[ -n "$pr_url" ]]
}

check_fix_attempt_merged() {
  local db="$1" source="$2" issue_id="$3"
  local status
  status="$(db_query "$db" "SELECT status FROM fix_attempts WHERE source='${source}' AND issue_id='${issue_id}' ORDER BY id DESC LIMIT 1")"
  [[ "$status" == "merged" ]]
}

check_regression_watch_exists() {
  local db="$1" issue_id="$2"
  local count
  count="$(db_count "$db" "SELECT 1 FROM regression_watches WHERE issue_id='${issue_id}'")"
  [[ "$count" -ge 1 ]]
}

check_regression_watch_resolved() {
  local db="$1" issue_id="$2"
  local status
  status="$(db_query "$db" "SELECT status FROM regression_watches WHERE issue_id='${issue_id}' ORDER BY id DESC LIMIT 1")"
  [[ "$status" == "resolved" ]]
}

check_claude_executions_count() {
  local db="$1" min="$2"
  local count
  count="$(db_count "$db" "SELECT 1 FROM claude_executions")"
  [[ "$count" -ge "$min" ]]
}

###############################################################################
#
#  SCENARIO 1: Linear Full Lifecycle
#
###############################################################################

run_scenario_1() {
  log_checkpoint "SCENARIO 1: Linear Full Lifecycle"
  S1_STATUS="running"

  # --- Step 1: Setup ---
  local smoke_id="s1-smoke-$(date -u +%Y%m%d%H%M%S)-$RANDOM"
  local smoke_file="e2e/${smoke_id}.md"

  S1_TMP_DIR="$(mktemp -d)"
  local work_root="${S1_TMP_DIR}/work"
  local repos_dir="${work_root}/repos"
  local local_repo="${repos_dir}/${REPO_NAME}"
  local db_path="${S1_TMP_DIR}/claudear-s1.db"
  local config_path="${S1_TMP_DIR}/claudear.s1.toml"
  local daemon_log="${S1_TMP_DIR}/daemon.log"

  mkdir -p "$repos_dir"

  log "Cloning mock repo for Scenario 1..."
  clone_mock_repo "$local_repo"

  generate_config "$config_path" "${work_root}/cloned" "$repos_dir" "$db_path" "$S1_PORT" \
    "false" "false" ""

  # --- Step 2: Create Linear Issue with "bug" + "claudear" labels ---
  log "Finding/creating labels..."
  local bug_label_id
  bug_label_id="$(find_or_create_label "bug" "#e11d48")"
  [[ -n "$bug_label_id" ]] || fail "Could not find or create 'bug' label"
  log "Bug label ID: ${bug_label_id}"

  local claudear_label_id
  claudear_label_id="$(find_or_create_label "claudear" "#6366f1")"
  [[ -n "$claudear_label_id" ]] || fail "Could not find or create 'claudear' label"
  log "Claudear label ID: ${claudear_label_id}"

  local issue_title="[claudear-e2e] ${smoke_id}"
  local issue_desc
  issue_desc="$(cat <<EOF
Production E2E smoke task (Scenario 1: Linear).

Repository: ${CLAUDEAR_E2E_GITHUB_REPO}
Required change:
1. Create file \`${smoke_file}\`
2. Put the exact text: \`${smoke_id}\`
3. Commit and open a pull request

Note: This issue is auto-resolved after validation.
EOF
)"

  local create_mutation='mutation CreateIssue($input: IssueCreateInput!) {
    issueCreate(input: $input) {
      success
      issue { id identifier url }
    }
  }'
  local create_vars
  create_vars="$(
    jq -cn \
      --arg teamId "$CLAUDEAR_E2E_LINEAR_TEAM_ID" \
      --arg title "$issue_title" \
      --arg description "$issue_desc" \
      --arg bugId "$bug_label_id" \
      --arg claudearId "$claudear_label_id" \
      '{input: {teamId: $teamId, title: $title, description: $description, labelIds: [$bugId, $claudearId]}}'
  )"

  log "Creating Linear issue..."
  local create_resp
  create_resp="$(linear_graphql "$create_mutation" "$create_vars")"
  local create_ok
  create_ok="$(jq -r '.data.issueCreate.success // false' <<<"$create_resp")"
  [[ "$create_ok" == "true" ]] || fail "Linear issueCreate failed: $(jq -r '.errors[]?.message' <<<"$create_resp" | tr '\n' '; ')"

  local S1_ISSUE_ID S1_IDENTIFIER S1_ISSUE_URL
  S1_ISSUE_ID="$(jq -r '.data.issueCreate.issue.id // empty' <<<"$create_resp")"
  S1_IDENTIFIER="$(jq -r '.data.issueCreate.issue.identifier // empty' <<<"$create_resp")"
  S1_ISSUE_URL="$(jq -r '.data.issueCreate.issue.url // empty' <<<"$create_resp")"
  [[ -n "$S1_ISSUE_ID" ]] || fail "Linear issue ID missing"
  CLEANUP_LINEAR_IDS+=("$S1_ISSUE_ID")

  log "Created Linear issue ${S1_IDENTIFIER} (${S1_ISSUE_ID})"
  log "Issue URL: ${S1_ISSUE_URL}"

  # --- Step 3: Start Daemon ---
  log "Starting daemon (Scenario 1, port ${S1_PORT})..."
  local daemon_pid
  daemon_pid="$(start_daemon "$config_path" "$S1_PORT" "$daemon_log")"
  log "Daemon handle: ${daemon_pid}"
  sleep 3

  if ! check_daemon_alive "$daemon_pid"; then
    warn "Daemon exited early. Logs:"
    cat "$daemon_log" >&2
    fail "Scenario 1 daemon failed to start"
  fi

  # --- Step 4: Checkpoint A - Issue Accepted ---
  log_checkpoint "S1 Checkpoint A: Issue Accepted"

  wait_for "fix_attempts row for Linear issue" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_fix_attempt_exists "$db_path" "linear" "$S1_ISSUE_ID"

  assert_db "$db_path" "fix_attempts" \
    "source='linear' AND issue_id='${S1_ISSUE_ID}' AND short_id IS NOT NULL" \
    1 "fix_attempts: issue accepted with short_id"

  assert_db "$db_path" "repositories" "1=1" 1 "repositories: mock repo indexed"
  assert_db "$db_path" "repo_files" "1=1" 1 "repo_files: files indexed" "false"

  assert_db "$db_path" "inference_attempts" \
    "issue_id='${S1_ISSUE_ID}' AND issue_source='linear' AND inferred_repo_id IS NOT NULL AND confidence > 0" \
    1 "inference_attempts: repo inferred" "false"

  assert_db "$db_path" "activity_log" "issue_id='${S1_ISSUE_ID}'" 1 "activity_log: entry exists"

  assert_db "$db_path" "processing_metrics" \
    "metric_name='issues_fetched'" 1 "processing_metrics: issues_fetched recorded" "false"

  assert_db "$db_path" "issues" \
    "source='linear' AND issue_id='${S1_ISSUE_ID}'" 0 "issues: issue record created" "false"

  # --- Step 5: Checkpoint B - PR Created ---
  log_checkpoint "S1 Checkpoint B: PR Created"

  wait_for_pr_verbose "$db_path" "linear" "$S1_ISSUE_ID" "$WAIT_TIMEOUT" "$POLL_INTERVAL" "$daemon_log"

  local S1_PR_URL S1_PR_NUMBER S1_PR_BRANCH
  S1_PR_URL="$(db_query "$db_path" "SELECT pr_url FROM fix_attempts WHERE source='linear' AND issue_id='${S1_ISSUE_ID}' AND pr_url IS NOT NULL ORDER BY id DESC LIMIT 1")"
  S1_PR_NUMBER="$(parse_pr_number "$S1_PR_URL")"
  [[ -n "$S1_PR_NUMBER" ]] || fail "Could not parse PR number from: ${S1_PR_URL}"
  S1_PR_BRANCH="$(get_pr_branch "$S1_PR_NUMBER")"
  CLEANUP_PR_NUMBERS+=("$S1_PR_NUMBER")
  CLEANUP_PR_BRANCHES+=("${S1_PR_BRANCH:-}")

  log "PR created: ${S1_PR_URL} (#${S1_PR_NUMBER}, branch: ${S1_PR_BRANCH:-unknown})"

  assert_db "$db_path" "fix_attempts" \
    "source='linear' AND issue_id='${S1_ISSUE_ID}' AND status='success' AND pr_url LIKE 'https://github.com/%' AND github_pr_number > 0 AND github_repo='${CLAUDEAR_E2E_GITHUB_REPO}' AND issue_labels LIKE '%bug%'" \
    1 "fix_attempts: success with PR and bug label"

  assert_db "$db_path" "prs" \
    "pr_url='${S1_PR_URL}' AND status='open' AND pr_number > 0 AND attempt_id IS NOT NULL AND issue_id='${S1_ISSUE_ID}' AND issue_source='linear' AND head_branch IS NOT NULL AND base_branch IS NOT NULL" \
    1 "prs: row with correct fields"

  assert_db "$db_path" "claude_executions" \
    "attempt_id IS NOT NULL AND duration_secs > 0 AND exit_code = 0 AND timed_out = 0" \
    1 "claude_executions: successful execution"

  assert_db "$db_path" "activity_log" "issue_id='${S1_ISSUE_ID}'" 2 "activity_log: >= 2 entries"

  # --- Step 6: Post COMMENT Review (using reviewer token) ---
  # Use a PR review (not an issue comment) so the review watcher picks it up
  # via GET /repos/{owner}/{repo}/pulls/{pr}/reviews
  log "Posting COMMENT review on PR #${S1_PR_NUMBER}..."

  local s1_pre_exec_count
  s1_pre_exec_count="$(db_count "$db_path" "SELECT 1 FROM claude_executions")"

  gh_api_reviewer POST "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${S1_PR_NUMBER}/reviews" \
    --data '{"body":"Please also add a line saying '\''Updated by review comment'\''","event":"COMMENT"}' >/dev/null

  # --- Step 7: Checkpoint C - Comment Addressed ---
  log_checkpoint "S1 Checkpoint C: Comment Addressed"

  local s1_target_exec=$((s1_pre_exec_count + 1))
  wait_for "claude_executions count >= ${s1_target_exec}" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_claude_executions_count "$db_path" "$s1_target_exec"

  assert_db "$db_path" "claude_executions" "1=1" 2 "claude_executions: >= 2 (original + review)"

  # Allow an extra polling cycle for the review watcher to persist state
  sleep 10

  assert_db "$db_path" "pr_review_states" \
    "pr_url='${S1_PR_URL}' AND is_active = 1 AND last_review_id IS NOT NULL" \
    1 "pr_review_states: active review state"

  assert_db "$db_path" "pr_reviews" \
    "pr_url='${S1_PR_URL}' AND review_state='COMMENTED'" \
    1 "pr_reviews: comment review recorded"

  # pr_review_comments tracks inline diff comments only (no inline comments in this test)
  assert_db "$db_path" "pr_review_comments" \
    "pr_url='${S1_PR_URL}'" \
    0 "pr_review_comments: no inline comments expected" "false"

  # --- Step 8: Verify PR unchanged (worktrees prevent new PR creation) ---
  log "Verifying PR URL unchanged after COMMENT review..."
  local s1_current_pr_url
  s1_current_pr_url="$(db_query "$db_path" \
    "SELECT pr_url FROM fix_attempts WHERE source='linear' AND issue_id='${S1_ISSUE_ID}' ORDER BY id DESC LIMIT 1")"
  if [[ -n "$s1_current_pr_url" && "$s1_current_pr_url" != "$S1_PR_URL" ]]; then
    warn "PR URL changed after COMMENT review: ${S1_PR_URL} -> ${s1_current_pr_url} (worktree should have prevented this)"
  else
    log "  PR URL unchanged: ${S1_PR_URL}"
  fi

  # --- Step 9: Post REQUEST_CHANGES Review on current PR (using reviewer token) ---
  log "Posting REQUEST_CHANGES review on PR #${S1_PR_NUMBER}..."

  local s1_pre_review_exec_count
  s1_pre_review_exec_count="$(db_count "$db_path" "SELECT 1 FROM claude_executions")"

  gh_api_reviewer POST "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${S1_PR_NUMBER}/reviews" \
    --data '{"body":"Please make the content uppercase","event":"REQUEST_CHANGES"}' >/dev/null

  # --- Step 10: Checkpoint D - Review Addressed ---
  log_checkpoint "S1 Checkpoint D: Review Addressed"

  local s1_target_review_exec=$((s1_pre_review_exec_count + 1))
  wait_for "claude_executions count >= ${s1_target_review_exec}" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_claude_executions_count "$db_path" "$s1_target_review_exec"

  assert_db "$db_path" "claude_executions" "1=1" 3 "claude_executions: >= 3"

  assert_db "$db_path" "pr_reviews" \
    "review_state='CHANGES_REQUESTED'" \
    1 "pr_reviews: changes_requested review recorded" "false"

  assert_db "$db_path" "review_patterns" "1=1" 0 "review_patterns: may populate" "false"

  # --- Step 11: Re-read latest PR (may have changed during reviews) ---
  local s1_latest_pr_url
  s1_latest_pr_url="$(db_query "$db_path" \
    "SELECT pr_url FROM fix_attempts WHERE source='linear' AND issue_id='${S1_ISSUE_ID}' AND pr_url IS NOT NULL AND pr_url != '' ORDER BY id DESC LIMIT 1")"
  if [[ -n "$s1_latest_pr_url" && "$s1_latest_pr_url" != "$S1_PR_URL" ]]; then
    warn "PR URL changed during reviews: ${S1_PR_URL} -> ${s1_latest_pr_url}"
    S1_PR_URL="$s1_latest_pr_url"
    S1_PR_NUMBER="$(parse_pr_number "$S1_PR_URL")"
    S1_PR_BRANCH="$(get_pr_branch "$S1_PR_NUMBER")"
    CLEANUP_PR_NUMBERS+=("$S1_PR_NUMBER")
    CLEANUP_PR_BRANCHES+=("${S1_PR_BRANCH:-}")
    log "  Now tracking PR #${S1_PR_NUMBER}"
  else
    log "  PR URL unchanged: ${S1_PR_URL}"
  fi

  # --- Step 12: Merge PR ---
  log "Merging PR #${S1_PR_NUMBER}..."
  gh_api PUT "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${S1_PR_NUMBER}/merge" \
    --data '{"merge_method":"squash"}' >/dev/null

  # --- Step 11: Checkpoint E - Merged ---
  log_checkpoint "S1 Checkpoint E: Merged"

  wait_for "fix_attempts status=merged" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_fix_attempt_merged "$db_path" "linear" "$S1_ISSUE_ID"

  assert_db "$db_path" "fix_attempts" \
    "source='linear' AND issue_id='${S1_ISSUE_ID}' AND status='merged' AND merged_at IS NOT NULL" \
    1 "fix_attempts: merged with timestamp"

  assert_db "$db_path" "prs" \
    "pr_url='${S1_PR_URL}' AND status='merged' AND merged_at IS NOT NULL AND review_cycles >= 1 AND files_changed > 0" \
    1 "prs: merged with review_cycles and files_changed" "false"

  assert_db "$db_path" "diff_analyses" \
    "attempt_id IS NOT NULL AND files_changed > 0 AND change_categories IS NOT NULL" \
    1 "diff_analyses: analysis recorded" "false"

  assert_db "$db_path" "strategy_fingerprints" \
    "attempt_id IS NOT NULL AND fix_approach IS NOT NULL AND tools_used IS NOT NULL" \
    1 "strategy_fingerprints: fingerprint recorded" "false"

  assert_db "$db_path" "feedback_outcomes" \
    "attempt_id IS NOT NULL AND outcome='merged'" \
    1 "feedback_outcomes: success outcome" "false"

  assert_db "$db_path" "repo_knowledge" "1=1" 0 "repo_knowledge: may populate" "false"

  # --- Step 12: Checkpoint F - Regression Watch Created ---
  log_checkpoint "S1 Checkpoint F: Regression Watch Created"

  wait_for "regression_watches row" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_regression_watch_exists "$db_path" "$S1_ISSUE_ID"

  assert_db "$db_path" "regression_watches" \
    "issue_id='${S1_ISSUE_ID}' AND fix_attempt_id IS NOT NULL AND status IN ('monitoring','awaiting_release') AND pr_merged_at IS NOT NULL" \
    1 "regression_watches: watch created"

  # If awaiting_release (no release tracker configured), nudge to monitoring
  local rw_status
  rw_status="$(db_query "$db_path" "SELECT status FROM regression_watches WHERE issue_id='${S1_ISSUE_ID}' ORDER BY id DESC LIMIT 1")"
  if [[ "$rw_status" == "awaiting_release" ]]; then
    log "Watch is awaiting_release; nudging to monitoring for e2e test..."
    kill_daemon "$daemon_pid"
    db_exec "$db_path" "UPDATE regression_watches SET status='monitoring', monitoring_started_at=datetime('now') WHERE issue_id='${S1_ISSUE_ID}'"
    log "Restarting daemon..."
    daemon_pid="$(start_daemon "$config_path" "$S1_PORT" "$daemon_log")"
    sleep 3
  fi

  # --- Step 13: Checkpoint G - Regression Resolved ---
  log_checkpoint "S1 Checkpoint G: Regression Resolved"

  wait_for "regression_watches status=resolved" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_regression_watch_resolved "$db_path" "$S1_ISSUE_ID"

  assert_db "$db_path" "regression_watches" \
    "issue_id='${S1_ISSUE_ID}' AND status='resolved' AND resolved_at IS NOT NULL AND regressed_at IS NULL" \
    1 "regression_watches: resolved without regression"

  local s1_watch_id
  s1_watch_id="$(db_query "$db_path" "SELECT id FROM regression_watches WHERE issue_id='${S1_ISSUE_ID}' ORDER BY id DESC LIMIT 1")"
  if [[ -n "$s1_watch_id" ]]; then
    assert_db "$db_path" "regression_checks" \
      "regression_watch_id=${s1_watch_id} AND issue_still_exists = 0 AND check_details IS NOT NULL" \
      1 "regression_checks: clean check recorded"
  fi

  # --- Step 14: Checkpoint H - Learnings Complete ---
  log_checkpoint "S1 Checkpoint H: Learnings Complete"

  assert_db "$db_path" "diff_analyses" "diff_summary IS NOT NULL" 1 "learnings: diff_analyses" "false"
  assert_db "$db_path" "strategy_fingerprints" "fix_approach IS NOT NULL" 1 "learnings: strategy_fingerprints" "false"
  assert_db "$db_path" "feedback_outcomes" "outcome IS NOT NULL" 1 "learnings: feedback_outcomes" "false"
  assert_db "$db_path" "repo_knowledge" "1=1" 0 "learnings: repo_knowledge" "false"
  assert_db "$db_path" "review_patterns" "1=1" 0 "learnings: review_patterns" "false"
  assert_db "$db_path" "processing_metrics" "1=1" 3 "learnings: processing_metrics >= 3" "false"
  assert_db "$db_path" "activity_log" "issue_id='${S1_ISSUE_ID}'" 5 "learnings: activity_log >= 5"

  # --- Step 15: Kill Daemon ---
  kill_daemon "$daemon_pid"

  S1_STATUS="passed"
  log_checkpoint "SCENARIO 1 PASSED"
}

###############################################################################
#
#  SCENARIO 2: Discord Source + Ask Question + Regression Cycling
#
###############################################################################

run_scenario_2() {
  if [[ "$HAS_DISCORD" != "true" ]]; then
    log_checkpoint "SCENARIO 2: SKIPPED (no Discord credentials)"
    S2_STATUS="skipped"
    return 0
  fi

  log_checkpoint "SCENARIO 2: Discord Source + Ask + Regression Cycling"
  S2_STATUS="running"

  # --- Step 1: Setup ---
  local smoke_id="s2-smoke-$(date -u +%Y%m%d%H%M%S)-$RANDOM"
  local smoke_file="e2e/${smoke_id}.md"

  S2_TMP_DIR="$(mktemp -d)"
  local work_root="${S2_TMP_DIR}/work"
  local repos_dir="${work_root}/repos"
  local local_repo="${repos_dir}/${REPO_NAME}"
  local db_path="${S2_TMP_DIR}/claudear-s2.db"
  local config_path="${S2_TMP_DIR}/claudear.s2.toml"
  local daemon_log="${S2_TMP_DIR}/daemon.log"

  mkdir -p "$repos_dir"

  log "Cloning mock repo for Scenario 2..."
  clone_mock_repo "$local_repo"

  local ask_instructions="IMPORTANT: Before making any code changes, you MUST ask the user a blocking question: Which testing framework should I use for this change? Provide these options: pytest, unittest, none. Wait for their answer before proceeding with the fix."

  generate_config "$config_path" "${work_root}/cloned" "$repos_dir" "$db_path" "$S2_PORT" \
    "true" "true" "$ask_instructions"

  # --- Step 2: Start Daemon + Seed ---
  log "Starting daemon (Scenario 2, port ${S2_PORT})..."
  local daemon_pid
  daemon_pid="$(start_daemon "$config_path" "$S2_PORT" "$daemon_log")"
  log "Daemon handle: ${daemon_pid}"

  # Wait for Discord source to seed cursor (first poll returns empty)
  log "Waiting 8s for Discord source to seed cursor..."
  sleep 8

  if ! check_daemon_alive "$daemon_pid"; then
    warn "Scenario 2 daemon exited early. Logs:"
    cat "$daemon_log" >&2
    fail "Scenario 2 daemon failed to start"
  fi

  # --- Step 3: Post Discord Message ---
  local discord_msg_body
  discord_msg_body="$(cat <<EOF
[claudear-e2e] ${smoke_id}

Bug report: The file \`${smoke_file}\` is missing from the repository ${CLAUDEAR_E2E_GITHUB_REPO}.

Steps to reproduce:
1. Check the e2e directory
2. File \`${smoke_file}\` does not exist

Expected: File exists with content \`${smoke_id}\`

Please create the file with the exact content specified.
EOF
)"

  log "Posting issue message to Discord channel..."
  local discord_resp
  discord_resp="$(discord_api POST "/channels/${CLAUDEAR_E2E_DISCORD_CHANNEL_ID}/messages" \
    --data "$(jq -cn --arg content "$discord_msg_body" '{content: $content}')")"

  local S2_DISCORD_MSG_ID
  S2_DISCORD_MSG_ID="$(jq -r '.id // empty' <<<"$discord_resp")"
  [[ -n "$S2_DISCORD_MSG_ID" ]] || fail "Failed to get Discord message ID"
  log "Discord message ID: ${S2_DISCORD_MSG_ID}"

  # --- Step 4: Checkpoint A - Discord Issue Accepted ---
  log_checkpoint "S2 Checkpoint A: Discord Issue Accepted"

  wait_for "fix_attempts row for Discord issue" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_fix_attempt_exists "$db_path" "discord" "$S2_DISCORD_MSG_ID"

  assert_db "$db_path" "fix_attempts" \
    "source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND short_id IS NOT NULL" \
    1 "fix_attempts: discord issue accepted"

  assert_db "$db_path" "repositories" "1=1" 1 "repositories: repo indexed"
  assert_db "$db_path" "repo_files" "1=1" 1 "repo_files: files indexed" "false"

  assert_db "$db_path" "inference_attempts" \
    "issue_id='${S2_DISCORD_MSG_ID}' AND issue_source='discord' AND inferred_repo_id IS NOT NULL" \
    1 "inference_attempts: repo inferred" "false"

  assert_db "$db_path" "activity_log" "1=1" 1 "activity_log: entry exists"

  # --- Step 5: Checkpoint B - Ask Flow Initiated (non-fatal) ---
  log_checkpoint "S2 Checkpoint B: Ask Flow (non-fatal)"

  local ask_detected=false
  local ask_elapsed=0
  local ask_timeout=120
  log "Polling Discord channel for blocking question (timeout: ${ask_timeout}s, non-fatal)..."

  while [[ $ask_elapsed -lt $ask_timeout ]]; do
    local recent_messages
    recent_messages="$(discord_api_nofail GET "/channels/${CLAUDEAR_E2E_DISCORD_CHANNEL_ID}/messages?limit=10")"
    if echo "$recent_messages" | jq -e '.[]? | select(.author.bot == true) | .content' 2>/dev/null | grep -qi "testing framework\|which.*framework\|pytest\|unittest"; then
      ask_detected=true
      log "Blocking question detected in Discord!"
      break
    fi
    sleep "$POLL_INTERVAL"
    ask_elapsed=$((ask_elapsed + POLL_INTERVAL))
  done

  if [[ "$ask_detected" == "true" ]]; then
    assert_db "$db_path" "activity_log" "message LIKE '%question%' OR message LIKE '%ask%' OR message LIKE '%blocking%'" \
      0 "activity_log: ask-related entry" "false"

    # --- Step 6: Reply to Question ---
    log "Replying 'none' to blocking question on Discord..."
    discord_api POST "/channels/${CLAUDEAR_E2E_DISCORD_CHANNEL_ID}/messages" \
      --data '{"content":"none"}' >/dev/null

    # --- Step 7: Checkpoint C - Ask Reply Processed (non-fatal) ---
    log_checkpoint "S2 Checkpoint C: Ask Reply Processed (non-fatal)"
    sleep 15

    assert_db "$db_path" "qa_knowledge" \
      "question_text IS NOT NULL AND answer_text IS NOT NULL AND source='discord'" \
      1 "qa_knowledge: Q&A recorded" "false"

    assert_db "$db_path" "qa_usage" "1=1" 0 "qa_usage: may populate" "false"
  else
    warn "Blocking question not detected in Discord within ${ask_timeout}s (non-fatal)"
  fi

  # --- Step 8: Checkpoint D - PR Created ---
  log_checkpoint "S2 Checkpoint D: PR Created"

  wait_for_pr_verbose "$db_path" "discord" "$S2_DISCORD_MSG_ID" "$WAIT_TIMEOUT" "$POLL_INTERVAL" "$daemon_log"

  local S2_PR_URL S2_PR_NUMBER S2_PR_BRANCH
  S2_PR_URL="$(db_query "$db_path" "SELECT pr_url FROM fix_attempts WHERE source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND pr_url IS NOT NULL ORDER BY id DESC LIMIT 1")"
  S2_PR_NUMBER="$(parse_pr_number "$S2_PR_URL")"
  [[ -n "$S2_PR_NUMBER" ]] || fail "Could not parse PR number from: ${S2_PR_URL}"
  S2_PR_BRANCH="$(get_pr_branch "$S2_PR_NUMBER")"
  CLEANUP_PR_NUMBERS+=("$S2_PR_NUMBER")
  CLEANUP_PR_BRANCHES+=("${S2_PR_BRANCH:-}")

  log "PR created: ${S2_PR_URL} (#${S2_PR_NUMBER}, branch: ${S2_PR_BRANCH:-unknown})"

  assert_db "$db_path" "fix_attempts" \
    "source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND status='success' AND pr_url LIKE 'https://github.com/%' AND github_pr_number > 0" \
    1 "fix_attempts: success with PR"

  assert_db "$db_path" "prs" \
    "pr_url='${S2_PR_URL}' AND status='open' AND attempt_id IS NOT NULL AND issue_source='discord'" \
    1 "prs: open PR for Discord issue"

  assert_db "$db_path" "claude_executions" "1=1" 1 "claude_executions: at least 1"

  assert_db "$db_path" "discord_threads" "1=1" 0 "discord_threads: may create thread" "false"

  # --- Step 9: Add "bug" label to fix_attempts ---
  log "Adding 'bug' label to fix_attempts for regression watch creation..."
  db_exec "$db_path" "UPDATE fix_attempts SET issue_labels='[\"bug\"]' WHERE source='discord' AND issue_id='${S2_DISCORD_MSG_ID}'"

  local labels_check
  labels_check="$(db_query "$db_path" "SELECT issue_labels FROM fix_attempts WHERE source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' ORDER BY id DESC LIMIT 1")"
  log "  Labels set to: ${labels_check}"

  # --- Step 10: Checkpoint E - First Merge ---
  log_checkpoint "S2 Checkpoint E: First Merge"

  log "Merging PR #${S2_PR_NUMBER}..."
  gh_api PUT "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${S2_PR_NUMBER}/merge" \
    --data '{"merge_method":"squash"}' >/dev/null

  wait_for "fix_attempts status=merged (discord)" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_fix_attempt_merged "$db_path" "discord" "$S2_DISCORD_MSG_ID"

  assert_db "$db_path" "fix_attempts" \
    "source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND status='merged' AND merged_at IS NOT NULL" \
    1 "fix_attempts: merged"

  assert_db "$db_path" "prs" \
    "pr_url='${S2_PR_URL}' AND status='merged'" \
    1 "prs: merged"

  assert_db "$db_path" "diff_analyses" "1=1" 1 "diff_analyses: analysis exists" "false"
  assert_db "$db_path" "strategy_fingerprints" "1=1" 1 "strategy_fingerprints: fingerprint exists" "false"

  # --- Step 11: Checkpoint F - Regression Watch ---
  log_checkpoint "S2 Checkpoint F: Regression Watch"

  wait_for "regression_watches row (discord)" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_regression_watch_exists "$db_path" "$S2_DISCORD_MSG_ID"

  assert_db "$db_path" "regression_watches" \
    "issue_id='${S2_DISCORD_MSG_ID}' AND status IN ('monitoring','awaiting_release') AND fix_attempt_id IS NOT NULL" \
    1 "regression_watches: watch created for Discord issue"

  # --- Step 12: Simulate Regression Detection ---
  log_checkpoint "S2 Step 12: Simulate Regression"

  log "Killing daemon for regression simulation..."
  kill_daemon "$daemon_pid"

  # Set watch to monitoring if not already
  db_exec "$db_path" "UPDATE regression_watches SET status='monitoring', monitoring_started_at=datetime('now', '-20 seconds') WHERE issue_id='${S2_DISCORD_MSG_ID}'"

  # Get the watch ID
  local s2_watch_id
  s2_watch_id="$(db_query "$db_path" "SELECT id FROM regression_watches WHERE issue_id='${S2_DISCORD_MSG_ID}' ORDER BY id DESC LIMIT 1")"

  # Insert regression check showing regression
  db_exec "$db_path" "INSERT INTO regression_checks (regression_watch_id, issue_still_exists, check_details, checked_at) VALUES (${s2_watch_id}, 1, 'Simulated regression detected by e2e test', datetime('now'))"

  # Set watch to regressed
  db_exec "$db_path" "UPDATE regression_watches SET status='regressed', regressed_at=datetime('now') WHERE id=${s2_watch_id}"

  # Reset fix_attempts for retry
  db_exec "$db_path" "UPDATE fix_attempts SET status='pending', retry_count=retry_count+1, pr_url=NULL, github_pr_number=NULL, error_message=NULL WHERE source='discord' AND issue_id='${S2_DISCORD_MSG_ID}'"

  # Delete old regression watch (UNIQUE constraint on issue_type+issue_id prevents new one)
  db_exec "$db_path" "DELETE FROM regression_watches WHERE id=${s2_watch_id}"

  assert_db "$db_path" "fix_attempts" \
    "source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND status='pending' AND retry_count >= 1 AND pr_url IS NULL" \
    1 "fix_attempts: reset for retry"

  assert_db "$db_path" "regression_checks" \
    "issue_still_exists=1" 1 "regression_checks: simulated regression recorded"

  # --- Step 13: Restart Daemon for Retry ---
  log "Restarting daemon for retry..."
  daemon_pid="$(start_daemon "$config_path" "$S2_PORT" "$daemon_log")"
  log "Daemon handle: ${daemon_pid}"
  sleep 3

  # --- Step 14: Checkpoint G - Retry PR Created ---
  log_checkpoint "S2 Checkpoint G: Retry PR Created"

  wait_for_pr_verbose "$db_path" "discord" "$S2_DISCORD_MSG_ID" "$WAIT_TIMEOUT" "$POLL_INTERVAL" "$daemon_log"

  local S2_RETRY_PR_URL S2_RETRY_PR_NUMBER S2_RETRY_PR_BRANCH
  S2_RETRY_PR_URL="$(db_query "$db_path" "SELECT pr_url FROM fix_attempts WHERE source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND pr_url IS NOT NULL ORDER BY id DESC LIMIT 1")"
  S2_RETRY_PR_NUMBER="$(parse_pr_number "$S2_RETRY_PR_URL")"
  [[ -n "$S2_RETRY_PR_NUMBER" ]] || fail "Could not parse retry PR number from: ${S2_RETRY_PR_URL}"
  S2_RETRY_PR_BRANCH="$(get_pr_branch "$S2_RETRY_PR_NUMBER")"
  CLEANUP_PR_NUMBERS+=("$S2_RETRY_PR_NUMBER")
  CLEANUP_PR_BRANCHES+=("${S2_RETRY_PR_BRANCH:-}")

  log "Retry PR created: ${S2_RETRY_PR_URL} (#${S2_RETRY_PR_NUMBER})"

  assert_db "$db_path" "fix_attempts" \
    "source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND status='success' AND pr_url IS NOT NULL AND retry_count >= 1 AND github_pr_number > 0" \
    1 "fix_attempts: retry succeeded with new PR"

  assert_db "$db_path" "prs" \
    "pr_url='${S2_RETRY_PR_URL}' AND status='open' AND attempt_id IS NOT NULL" \
    1 "prs: retry PR open"

  # --- Step 15: Checkpoint H - Re-add bug label + Merge Retry PR ---
  log_checkpoint "S2 Checkpoint H: Retry Merged"

  # Re-add bug label (it was cleared during reset)
  db_exec "$db_path" "UPDATE fix_attempts SET issue_labels='[\"bug\"]' WHERE source='discord' AND issue_id='${S2_DISCORD_MSG_ID}'"

  log "Merging retry PR #${S2_RETRY_PR_NUMBER}..."
  gh_api PUT "/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${S2_RETRY_PR_NUMBER}/merge" \
    --data '{"merge_method":"squash"}' >/dev/null

  wait_for "fix_attempts status=merged (retry)" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_fix_attempt_merged "$db_path" "discord" "$S2_DISCORD_MSG_ID"

  assert_db "$db_path" "fix_attempts" \
    "source='discord' AND issue_id='${S2_DISCORD_MSG_ID}' AND status='merged' AND retry_count >= 1" \
    1 "fix_attempts: retry merged"

  assert_db "$db_path" "prs" \
    "pr_url='${S2_RETRY_PR_URL}' AND status='merged'" \
    1 "prs: retry PR merged"

  # --- Step 16: Checkpoint I - Final Regression Resolution ---
  log_checkpoint "S2 Checkpoint I: Final Resolution"

  wait_for "new regression_watches row (retry)" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_regression_watch_exists "$db_path" "$S2_DISCORD_MSG_ID"

  # Nudge to monitoring if awaiting_release
  local rw2_status
  rw2_status="$(db_query "$db_path" "SELECT status FROM regression_watches WHERE issue_id='${S2_DISCORD_MSG_ID}' ORDER BY id DESC LIMIT 1")"
  if [[ "$rw2_status" == "awaiting_release" ]]; then
    log "Retry watch is awaiting_release; nudging to monitoring..."
    kill_daemon "$daemon_pid"
    db_exec "$db_path" "UPDATE regression_watches SET status='monitoring', monitoring_started_at=datetime('now') WHERE issue_id='${S2_DISCORD_MSG_ID}'"
    daemon_pid="$(start_daemon "$config_path" "$S2_PORT" "$daemon_log")"
    sleep 3
  fi

  wait_for "regression_watches resolved (retry)" "$WAIT_TIMEOUT" "$POLL_INTERVAL" \
    check_regression_watch_resolved "$db_path" "$S2_DISCORD_MSG_ID"

  assert_db "$db_path" "regression_watches" \
    "issue_id='${S2_DISCORD_MSG_ID}' AND status='resolved' AND resolved_at IS NOT NULL AND regressed_at IS NULL" \
    1 "regression_watches: final resolution"

  local s2_final_watch_id
  s2_final_watch_id="$(db_query "$db_path" "SELECT id FROM regression_watches WHERE issue_id='${S2_DISCORD_MSG_ID}' ORDER BY id DESC LIMIT 1")"
  if [[ -n "$s2_final_watch_id" ]]; then
    assert_db "$db_path" "regression_checks" \
      "regression_watch_id=${s2_final_watch_id} AND issue_still_exists=0" \
      1 "regression_checks: clean final check"
  fi

  # --- Step 17: Checkpoint J - Learnings Complete ---
  log_checkpoint "S2 Checkpoint J: Learnings Complete"

  assert_db "$db_path" "diff_analyses" "1=1" 2 "learnings: diff_analyses >= 2 (original + retry)" "false"
  assert_db "$db_path" "strategy_fingerprints" "1=1" 2 "learnings: strategy_fingerprints >= 2" "false"
  assert_db "$db_path" "feedback_outcomes" "outcome IS NOT NULL" 1 "learnings: feedback_outcomes >= 1" "false"
  assert_db "$db_path" "repo_knowledge" "1=1" 0 "learnings: repo_knowledge" "false"
  assert_db "$db_path" "qa_knowledge" "1=1" 1 "learnings: qa_knowledge >= 1" "false"
  assert_db "$db_path" "processing_metrics" "1=1" 3 "learnings: processing_metrics >= 3" "false"
  assert_db "$db_path" "activity_log" "1=1" 8 "learnings: activity_log >= 8"

  # --- Step 18: Kill Daemon ---
  kill_daemon "$daemon_pid"

  S2_STATUS="passed"
  log_checkpoint "SCENARIO 2 PASSED"
}

###############################################################################
#
#  MAIN
#
###############################################################################

log_checkpoint "Production E2E Smoke Test Starting"
log "GitHub repo: ${CLAUDEAR_E2E_GITHUB_REPO}"
log "Linear team: ${CLAUDEAR_E2E_LINEAR_TEAM_ID}"
log "Discord available: ${HAS_DISCORD}"
log "Binary: ${CLAUDEAR_BIN}"
log ""

run_scenario_1

run_scenario_2

# Final status
if [[ "$S1_STATUS" == "passed" && ("$S2_STATUS" == "passed" || "$S2_STATUS" == "skipped") ]]; then
  FINAL_STATUS="passed"
else
  fail "One or more scenarios failed (S1: ${S1_STATUS}, S2: ${S2_STATUS})"
fi

log_checkpoint "ALL SCENARIOS PASSED (warnings: ${WARN_COUNT})"
