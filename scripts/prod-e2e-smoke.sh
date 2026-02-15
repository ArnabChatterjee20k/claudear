#!/usr/bin/env bash
set -Eeuo pipefail

log() {
  printf '[prod-e2e] %s\n' "$*"
}

warn() {
  printf '[prod-e2e][warn] %s\n' "$*" >&2
}

fail() {
  printf '[prod-e2e][error] %s\n' "$*" >&2
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
    log "Using existing Claude CLI login session (no auth env vars provided)"
    return 0
  fi

  fail "Claude auth missing. Set ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN, or run 'claude auth login'."
}

linear_graphql() {
  local query="$1"
  local variables_json="${2:-{}}"
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

extract_linear_errors() {
  jq -r '.errors[]?.message' <<<"$1" | tr '\n' '; '
}

resolve_linear_issue() {
  [[ -n "${LINEAR_ISSUE_ID:-}" ]] || return 0

  local query_states='query TeamStates($teamId: String!) {
    team(id: $teamId) {
      states {
        nodes {
          id
          type
          name
        }
      }
    }
  }'
  local vars_states
  vars_states="$(jq -cn --arg teamId "$CLAUDEAR_E2E_LINEAR_TEAM_ID" '{teamId: $teamId}')"

  local states_resp
  if ! states_resp="$(linear_graphql "$query_states" "$vars_states")"; then
    warn "Failed to query Linear team states while resolving issue ${LINEAR_ISSUE_ID}"
    return 0
  fi

  local completed_state_id
  completed_state_id="$(
    jq -r '.data.team.states.nodes[]? | select(.type=="completed") | .id' <<<"$states_resp" | head -n1
  )"
  if [[ -z "$completed_state_id" ]]; then
    warn "Could not find a completed Linear state for team ${CLAUDEAR_E2E_LINEAR_TEAM_ID}"
    return 0
  fi

  local mutation='mutation ResolveIssue($id: String!, $stateId: String!) {
    issueUpdate(id: $id, input: { stateId: $stateId }) {
      success
    }
  }'
  local vars_resolve
  vars_resolve="$(
    jq -cn \
      --arg id "$LINEAR_ISSUE_ID" \
      --arg stateId "$completed_state_id" \
      '{id: $id, stateId: $stateId}'
  )"

  local resolve_resp
  if ! resolve_resp="$(linear_graphql "$mutation" "$vars_resolve")"; then
    warn "Failed to resolve Linear issue ${LINEAR_ISSUE_ID}"
    return 0
  fi

  local success
  success="$(jq -r '.data.issueUpdate.success // false' <<<"$resolve_resp")"
  if [[ "$success" != "true" ]]; then
    warn "Linear issue update was not successful for ${LINEAR_ISSUE_ID}"
  else
    log "Resolved Linear issue ${LINEAR_ISSUE_ID}"
  fi
}

comment_linear_issue() {
  [[ -n "${LINEAR_ISSUE_ID:-}" ]] || return 0

  local body="$1"
  local mutation='mutation CreateComment($issueId: String!, $body: String!) {
    commentCreate(input: { issueId: $issueId, body: $body }) {
      success
    }
  }'
  local vars
  vars="$(jq -cn --arg issueId "$LINEAR_ISSUE_ID" --arg body "$body" '{issueId: $issueId, body: $body}')"

  local resp
  if ! resp="$(linear_graphql "$mutation" "$vars")"; then
    warn "Failed adding Linear cleanup comment to issue ${LINEAR_ISSUE_ID}"
    return 0
  fi

  local success
  success="$(jq -r '.data.commentCreate.success // false' <<<"$resp")"
  if [[ "$success" != "true" ]]; then
    warn "Linear comment mutation returned unsuccessful for issue ${LINEAR_ISSUE_ID}"
  fi
}

close_github_pr() {
  [[ -n "${GITHUB_PR_NUMBER:-}" ]] || return 0
  [[ "${CLAUDEAR_E2E_CLOSE_PR:-true}" == "true" ]] || return 0

  local api_url="https://api.github.com/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${GITHUB_PR_NUMBER}"
  local patch_resp
  patch_resp="$(
    curl -sSfL \
      -X PATCH \
      -H "Authorization: Bearer ${CLAUDEAR_E2E_GITHUB_TOKEN}" \
      -H "Accept: application/vnd.github+json" \
      "$api_url" \
      --data '{"state":"closed"}'
  )" || {
    warn "Failed to close GitHub PR #${GITHUB_PR_NUMBER}"
    return 0
  }

  local state
  state="$(jq -r '.state // empty' <<<"$patch_resp")"
  if [[ "$state" == "closed" ]]; then
    log "Closed GitHub PR #${GITHUB_PR_NUMBER}"
  else
    warn "GitHub PR #${GITHUB_PR_NUMBER} close response returned state='${state}'"
  fi
}

delete_github_branch() {
  [[ -n "${GITHUB_PR_BRANCH:-}" ]] || return 0
  [[ "${CLAUDEAR_E2E_DELETE_BRANCH:-false}" == "true" ]] || return 0

  local branch_escaped="${GITHUB_PR_BRANCH//\//%2F}"
  local api_url="https://api.github.com/repos/${CLAUDEAR_E2E_GITHUB_REPO}/git/refs/heads/${branch_escaped}"
  if curl -sSfL \
    -X DELETE \
    -H "Authorization: Bearer ${CLAUDEAR_E2E_GITHUB_TOKEN}" \
    -H "Accept: application/vnd.github+json" \
    "$api_url" >/dev/null; then
    log "Deleted branch ${GITHUB_PR_BRANCH}"
  else
    warn "Failed to delete branch ${GITHUB_PR_BRANCH}"
  fi
}

cleanup() {
  local exit_code=$?
  set +e

  if [[ "${FINAL_STATUS:-failed}" == "passed" ]]; then
    comment_linear_issue "✅ Production E2E smoke test passed. PR: ${GITHUB_PR_URL:-none}"
  else
    comment_linear_issue "❌ Production E2E smoke test failed. Check CI/script logs for details."
  fi

  close_github_pr
  delete_github_branch
  resolve_linear_issue

  if [[ -n "${TMP_DIR:-}" && -d "${TMP_DIR:-}" ]]; then
    rm -rf "${TMP_DIR}"
  fi

  exit "$exit_code"
}
trap cleanup EXIT

require_cmd git
require_cmd curl
require_cmd jq
require_cmd sqlite3
require_cmd cargo
require_cmd claude

require_env CLAUDEAR_E2E_LINEAR_API_KEY
require_env CLAUDEAR_E2E_LINEAR_TEAM_ID
require_env CLAUDEAR_E2E_GITHUB_REPO
require_env CLAUDEAR_E2E_GITHUB_TOKEN
ensure_claude_auth

CLAUDEAR_BIN="${CLAUDEAR_E2E_BINARY:-./target/release/claudear}"
if [[ ! -x "$CLAUDEAR_BIN" ]]; then
  log "Building release binary because ${CLAUDEAR_BIN} is missing"
  cargo build --release
fi

if [[ ! -x "$CLAUDEAR_BIN" ]]; then
  fail "Expected executable binary at ${CLAUDEAR_BIN}"
fi

if [[ ! "${CLAUDEAR_E2E_GITHUB_REPO}" =~ ^[^/]+/[^/]+$ ]]; then
  fail "CLAUDEAR_E2E_GITHUB_REPO must be in owner/repo format"
fi

REPO_OWNER="${CLAUDEAR_E2E_GITHUB_REPO%%/*}"
REPO_NAME="${CLAUDEAR_E2E_GITHUB_REPO##*/}"
SMOKE_ID="smoke-$(date -u +%Y%m%d%H%M%S)-$RANDOM"
SMOKE_FILE="e2e/${SMOKE_ID}.md"

TMP_DIR="$(mktemp -d)"
WORK_ROOT="${TMP_DIR}/work"
REPOS_DIR="${WORK_ROOT}/repos"
LOCAL_REPO_DIR="${REPOS_DIR}/${REPO_NAME}"
DB_PATH="${TMP_DIR}/claudear-e2e.db"
CONFIG_PATH="${TMP_DIR}/claudear.e2e.yaml"
TRIGGER_LOG="${TMP_DIR}/trigger.log"

mkdir -p "$REPOS_DIR"

log "Cloning sandbox repository ${CLAUDEAR_E2E_GITHUB_REPO}"
git clone --depth 1 \
  "https://x-access-token:${CLAUDEAR_E2E_GITHUB_TOKEN}@github.com/${CLAUDEAR_E2E_GITHUB_REPO}.git" \
  "$LOCAL_REPO_DIR" >/dev/null 2>&1 || fail "Failed to clone ${CLAUDEAR_E2E_GITHUB_REPO}"

ISSUE_TITLE="[claudear-e2e] ${SMOKE_ID}"
ISSUE_DESCRIPTION=$(
  cat <<EOF
Production E2E smoke task created by automation.

Repository: ${CLAUDEAR_E2E_GITHUB_REPO}
Required change:
1. Create file \`${SMOKE_FILE}\`
2. Put the exact text: \`${SMOKE_ID}\`
3. Commit and open a pull request

Note: This issue is auto-resolved after validation.
EOF
)

CREATE_ISSUE_MUTATION='mutation CreateIssue($input: IssueCreateInput!) {
  issueCreate(input: $input) {
    success
    issue {
      id
      identifier
      url
    }
  }
}'
CREATE_ISSUE_VARS="$(
  jq -cn \
    --arg teamId "$CLAUDEAR_E2E_LINEAR_TEAM_ID" \
    --arg title "$ISSUE_TITLE" \
    --arg description "$ISSUE_DESCRIPTION" \
    '{input: {teamId: $teamId, title: $title, description: $description}}'
)"

log "Creating Linear smoke issue"
create_issue_resp="$(linear_graphql "$CREATE_ISSUE_MUTATION" "$CREATE_ISSUE_VARS")"
create_issue_success="$(jq -r '.data.issueCreate.success // false' <<<"$create_issue_resp")"
if [[ "$create_issue_success" != "true" ]]; then
  fail "Linear issueCreate failed: $(extract_linear_errors "$create_issue_resp")"
fi

LINEAR_ISSUE_ID="$(jq -r '.data.issueCreate.issue.id // empty' <<<"$create_issue_resp")"
LINEAR_ISSUE_IDENTIFIER="$(jq -r '.data.issueCreate.issue.identifier // empty' <<<"$create_issue_resp")"
LINEAR_ISSUE_URL="$(jq -r '.data.issueCreate.issue.url // empty' <<<"$create_issue_resp")"
[[ -n "$LINEAR_ISSUE_ID" ]] || fail "Linear issue id missing from create response"

log "Created Linear issue ${LINEAR_ISSUE_IDENTIFIER} (${LINEAR_ISSUE_ID})"
log "Issue URL: ${LINEAR_ISSUE_URL}"

cat >"$CONFIG_PATH" <<EOF
work_dir: "${WORK_ROOT}/cloned"
known_orgs:
  - "${REPO_OWNER}"
auto_discover_paths:
  - "${REPOS_DIR}"
poll_interval_ms: 60000
webhook_port: 3100
db_path: "${DB_PATH}"
max_issues_per_cycle: 1
max_concurrent: 1
processing_delay_ms: 0
claude_timeout_secs: ${CLAUDEAR_E2E_CLAUDE_TIMEOUT_SECS:-3600}

claude:
  skip_permissions: true

ask:
  enabled: false

github:
  token: "${CLAUDEAR_E2E_GITHUB_TOKEN}"
  auto_resolve_on_merge: false

linear:
  enabled: true
  api_key: "${CLAUDEAR_E2E_LINEAR_API_KEY}"
  trigger_labels: []
  trigger_states: []
  team_id: "${CLAUDEAR_E2E_LINEAR_TEAM_ID}"

regression:
  enabled: false
EOF

log "Running claudear trigger flow against real issue"
if ! "$CLAUDEAR_BIN" --config "$CONFIG_PATH" trigger linear "$LINEAR_ISSUE_ID" >"$TRIGGER_LOG" 2>&1; then
  warn "Trigger command exited non-zero. Logs:"
  cat "$TRIGGER_LOG" >&2
  fail "claudear trigger failed"
fi

attempt_row="$(
  sqlite3 -readonly -noheader -separator $'\t' "$DB_PATH" \
    "SELECT status, COALESCE(pr_url, ''), COALESCE(error_message, '')
     FROM fix_attempts
     WHERE source = 'linear' AND issue_id = '${LINEAR_ISSUE_ID}'
     ORDER BY id DESC
     LIMIT 1;"
)"

[[ -n "$attempt_row" ]] || fail "No fix_attempts row found for Linear issue ${LINEAR_ISSUE_ID}"

IFS=$'\t' read -r ATTEMPT_STATUS GITHUB_PR_URL ATTEMPT_ERROR <<<"$attempt_row"

if [[ "$ATTEMPT_STATUS" != "success" ]]; then
  warn "Trigger logs:"
  cat "$TRIGGER_LOG" >&2
  fail "Expected status=success, got status=${ATTEMPT_STATUS}, error=${ATTEMPT_ERROR}"
fi

[[ -n "$GITHUB_PR_URL" ]] || fail "Attempt succeeded but pr_url was empty"

if [[ "$GITHUB_PR_URL" =~ ^https://github.com/([^/]+/[^/]+)/pull/([0-9]+)/?$ ]]; then
  PR_REPO="${BASH_REMATCH[1]}"
  GITHUB_PR_NUMBER="${BASH_REMATCH[2]}"
else
  fail "Could not parse GitHub PR URL: ${GITHUB_PR_URL}"
fi

if [[ "$PR_REPO" != "$CLAUDEAR_E2E_GITHUB_REPO" ]]; then
  fail "PR repo mismatch: expected ${CLAUDEAR_E2E_GITHUB_REPO}, got ${PR_REPO}"
fi

log "Validating GitHub PR ${GITHUB_PR_URL}"
pr_resp="$(
  curl -sSfL \
    -H "Authorization: Bearer ${CLAUDEAR_E2E_GITHUB_TOKEN}" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${CLAUDEAR_E2E_GITHUB_REPO}/pulls/${GITHUB_PR_NUMBER}"
)"
pr_state="$(jq -r '.state // empty' <<<"$pr_resp")"
GITHUB_PR_BRANCH="$(jq -r '.head.ref // empty' <<<"$pr_resp")"
[[ -n "$pr_state" ]] || fail "Failed to read PR state from GitHub API response"

log "PR state: ${pr_state}"
log "PR branch: ${GITHUB_PR_BRANCH:-unknown}"
log "Smoke issue: ${LINEAR_ISSUE_IDENTIFIER} (${LINEAR_ISSUE_URL})"
log "Smoke PR: ${GITHUB_PR_URL}"

FINAL_STATUS="passed"
log "Production E2E smoke test passed"
