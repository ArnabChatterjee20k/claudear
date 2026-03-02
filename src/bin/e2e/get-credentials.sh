#!/usr/bin/env bash
# Extract Claude Code OAuth token from macOS keychain and export it,
# along with the project .env file, for running E2E tests.
#
# Usage:
#   source src/bin/e2e/get-credentials.sh
#   cargo run --release --bin claudear-e2e -- --use-docker ...
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# ── 1. Source .env if present ───────────────────────────────────────────────
ENV_FILE="$PROJECT_ROOT/.env"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
  echo "[get-credentials] Loaded $(grep -c '^[A-Z]' "$ENV_FILE") vars from .env"
else
  echo "[get-credentials] WARNING: $ENV_FILE not found"
fi

# ── 2. Extract Claude Code OAuth token from macOS keychain ──────────────────
if [[ -z "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]]; then
  KEYCHAIN_CREDS="$(security find-generic-password -s "Claude Code-credentials" -w 2>/dev/null || true)"
  if [[ -n "$KEYCHAIN_CREDS" ]]; then
    TOKEN="$(echo "$KEYCHAIN_CREDS" | grep -oE '"accessToken"\s*:\s*"[^"]+"' | head -1 | sed 's/.*"accessToken"\s*:\s*"//' | sed 's/"$//')"
    if [[ -n "$TOKEN" && "$TOKEN" != "NOT_FOUND" ]]; then
      export CLAUDE_CODE_OAUTH_TOKEN="$TOKEN"
      echo "[get-credentials] Exported CLAUDE_CODE_OAUTH_TOKEN from macOS keychain (${#TOKEN} chars)"
    else
      echo "[get-credentials] WARNING: Could not extract accessToken from keychain"
    fi
  else
    echo "[get-credentials] WARNING: No 'Claude Code-credentials' keychain item found"
  fi
else
  echo "[get-credentials] CLAUDE_CODE_OAUTH_TOKEN already set"
fi

# ── 3. Ensure Docker mode ──────────────────────────────────────────────────
export CLAUDEAR_E2E_USE_DOCKER=true
echo "[get-credentials] CLAUDEAR_E2E_USE_DOCKER=true"

echo "[get-credentials] Done. Ready to run E2E tests."
