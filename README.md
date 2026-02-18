<p align="center">
  <h1 align="center">Claudear</h1>
  <p align="center">
    <strong>Autonomous issue-to-PR pipeline powered by Claude Code</strong>
  </p>
  <p align="center">
    <a href="https://github.com/abnegate/claudear/actions/workflows/ci.yml"><img src="https://github.com/abnegate/claudear/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://github.com/abnegate/claudear/releases"><img src="https://img.shields.io/github/v/release/abnegate/claudear" alt="Release"></a>
    <a href="https://github.com/abnegate/claudear/blob/main/LICENSE"><img src="https://img.shields.io/github/license/abnegate/claudear" alt="License"></a>
    <a href="https://github.com/abnegate/claudear"><img src="https://img.shields.io/badge/rust-1.93+-orange.svg" alt="Rust"></a>
  </p>
</p>

Claudear watches your issue trackers and error monitoring services, automatically spawning [Claude Code](https://docs.anthropic.com/en/docs/claude-code) agents to fix issues and open pull requests -- no human in the loop required (unless Claude has a question for you).

Point it at Linear, Sentry, Discord, or GitHub review comments. It figures out which repo the issue belongs to, clones it, runs Claude Code with your project's conventions, opens a PR, monitors the PR through merge, auto-resolves the source issue, and learns from the outcome to get smarter over time.

---

## Table of Contents

- [How It Works](#how-it-works)
- [Features](#features)
- [Architecture](#architecture)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Configuration](#configuration)
- [Usage](#usage)
  - [Daemon Mode](#daemon-mode)
  - [Polling Mode](#polling-mode-foreground)
  - [Webhook Mode](#webhook-mode)
  - [Manual Triggers](#manual-triggers)
  - [PR Management](#pr-management)
  - [Retry Management](#retry-management)
  - [Dashboard](#dashboard)
  - [Scheduled Reports](#scheduled-reports)
  - [Multi-Repository Cascading](#multi-repository-cascading)
  - [Human Q&A Loop](#human-qa-loop)
  - [Inference Analytics](#inference-analytics)
  - [Release Tracking](#release-tracking)
  - [Diagnostics](#diagnostics)
  - [Dry Run](#dry-run)
  - [User Registry](#user-registry)
- [Fix Attempt Lifecycle](#fix-attempt-lifecycle)
- [AI Feedback Loop](#ai-feedback-loop)
- [Custom Prompt Templates](#custom-prompt-templates)
- [Running as a Service](#running-as-a-service)
- [Docker](#docker)
- [CI/CD](#cicd)
- [Development](#development)
- [License](#license)

---

## How It Works

```
 Issue filed on Linear       Sentry escalation        Discord message       GitHub review comment
        |                          |                        |                        |
        +----------+---------------+------------+-----------+                        |
                   |                            |                                    |
                   v                            v                                    v
          ┌──────────────────────────────────────────────────────────────────────────────┐
          |                              Claudear Watcher                                |
          |                                                                              |
          |   1. Detect new issue (poll or webhook)                                      |
          |   2. Infer target repository from stack traces, file paths, context          |
          |   3. Clone repo, spawn Claude Code agent with project conventions            |
          |   4. Claude fixes the issue, creates a PR                                    |
          |   5. Notify you (Discord / Email / SMS / Push)                               |
          |   6. Monitor PR through merge                                                |
          |   7. Auto-resolve issue on source when PR merges                             |
          |   8. Watch for regressions for 24 hours                                      |
          |   9. Learn from outcome to improve future fixes                              |
          └──────────────────────────────────────────────────────────────────────────────┘
```

---

## Features

### Issue Sources
- **Linear** -- Trigger on labels (`auto-implement`, `claude`) or states (`backlog`, `todo`), with team/project filtering
- **Sentry** -- Process top escalating errors by event count, time period, and escalation threshold
- **Discord** -- Process messages and threads from Discord channels as issues
- **GitHub Review Comments** -- Respond to PR review comments tagged with `/claudear`
- Per-source rate limiting, concurrent processing controls, and configurable poll intervals

### Intelligent Repository Routing
- Automatically determines the target repository from stack traces, file paths, and issue content
- Confidence scoring ranks repository matches so fixes go to the right place
- Scans configured GitHub organizations and local paths for repo discovery
- Full-text file index across all repositories for fast context lookups

### Autonomous Fix Pipeline
- Spawns real Claude Code processes with full tool access (read, edit, bash, etc.)
- Configurable model selection: Sonnet, Opus, Haiku, or any model ID
- Project-specific `AGENT.md` files customize Claude's coding conventions per repo
- Global instructions and tool permissions via `claudear.toml`
- Configurable execution timeout (default 6 hours)

### Multi-Repository Cascading
- Tracks dependency graphs between repositories (NPM, Python, etc.)
- Automatically propagates fixes through dependent repos using BFS traversal
- Configurable cascade depth limits

### Human Q&A Loop
- When Claude is blocked on ambiguity, it asks a question via your notification channels
- Claudear fans out the question to all enabled notifiers (Discord, Email, etc.)
- First reply wins -- Claude resumes immediately with the answer
- Q&A pairs are stored and reused via embedding-based semantic matching so the same question is never asked twice
- Configurable timeouts with best-effort continuation

### AI Feedback Loop
- Tracks outcomes of every fix attempt (merged, closed, failed)
- Generates local vector embeddings for all issue content (no external APIs -- runs on ONNX Runtime)
- Finds similar past issues and extracts patterns from successes and failures
- Enhances future prompts with learnings from past outcomes
- Supported models: Nomic, MiniLM, BGE

### PR Lifecycle Management
- Monitors PRs through merge/close with configurable poll intervals
- Automatically resolves source issues (Linear/Sentry) when PRs merge
- Exponential backoff retries for failed or closed PRs (configurable max retries)
- Full lifecycle tracking: Pending -> Success -> Merged -> Resolved (with retry paths)

### Regression Monitoring
- Hourly checks for 24 hours after a fix deploys
- Detects re-opened issues and error rate spikes on both Linear and Sentry
- Configurable similarity thresholds and event count minimums

### Release Tracking
- Dependency-aware release detection across repository graphs
- Tracks when fixes land in production through dependency paths
- Semantic versioning support

### Notifications
- **Discord** -- Webhook messages + bot reply polling for Q&A
- **Email** -- SMTP sending + IMAP reply polling for Q&A
- **SMS** -- Twilio integration
- **Push** -- Pushover notifications
- **Console** -- Always-on logging
- Mix and match any combination of channels

### User Registry
- Map team members across Linear, GitHub, Sentry, Discord, Email, SMS, Push
- Route notifications to the person assigned to the issue
- Per-user notification channel preferences

### Analytics Dashboard
- Real-time web UI (React + TypeScript + Tailwind)
- Stats overview: total attempts, success rate, merge rate
- Status breakdown, source-by-source metrics, recent attempt history
- Retryable issues view with one-click retry
- Embedded in the release binary -- no separate deployment needed

### Scheduled Reports
- Daily, weekly, and monthly automated status reports
- Breakdown of attempts, success/failure rates, PR metrics, pending work
- Delivered via all configured notification channels

### Daemon Mode & IPC
- Runs as a background service with full IPC control
- `start` / `stop` / `pause` / `resume` / `status` / `activity` commands
- Unix socket communication with configurable timeout

### Webhooks
- Real-time event processing from Linear, Sentry, and GitHub
- HMAC-SHA256 signature verification
- One-command auto-configuration: `claudear webhook --setup-webhooks --base-url <url>`

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────────────┐
│                               Claudear                                   │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐                │
│  │  Linear  │  │  Sentry  │  │ Discord  │  │  GitHub  │  ← Sources     │
│  │  Source  │  │  Source  │  │  Source  │  │ Webhooks │                 │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘                │
│       │             │             │             │                       │
│       └──────┬──────┴──────┬──────┴──────┬──────┘                       │
│              │             │             │                              │
│              ▼             ▼             ▼                              │
│       ┌─────────────────────────────────────┐     ┌──────────────┐     │
│       │             Watcher                 │────>│  Repo Index  │     │
│       │  (polls, webhooks, matches,         │     │  (inference,  │     │
│       │   coordinates, rate-limits)         │     │  discovery)   │     │
│       └────────────────┬────────────────────┘     └──────────────┘     │
│                        │                                               │
│    ┌───────────────────┼───────────────────────┬───────────────┐       │
│    │                   │               │       │               │       │
│    ▼                   ▼               ▼       ▼               ▼       │
│ ┌─────────┐    ┌─────────┐    ┌─────────┐ ┌─────────┐ ┌───────────┐  │
│ │ Claude  │    │ SQLite  │    │Notifiers│ │   PR    │ │ Feedback  │  │
│ │ Runner  │    │ Tracker │    │(Discord,│ │ Monitor │ │ Analyzer  │  │
│ │         │    │         │    │Email,..)│ │         │ │(embeddings)│  │
│ └─────────┘    └─────────┘    └─────────┘ └─────────┘ └───────────┘  │
│                                                                        │
│ ┌─────────────┐ ┌──────────────┐ ┌──────────────┐ ┌────────────┐     │
│ │  Regression │ │   Release    │ │     IPC      │ │  Cascade   │     │
│ │  Monitor    │ │   Tracker    │ │ (daemon ctl) │ │  Engine    │     │
│ └─────────────┘ └──────────────┘ └──────────────┘ └────────────┘     │
│                                                                        │
│ ┌──────────────────────────────────────────────────────────────────┐   │
│ │                     Dashboard (React + Tailwind)                 │   │
│ │     Stats · Attempts · Retries · Sources · Reports · Config     │   │
│ └──────────────────────────────────────────────────────────────────┘   │
│                                                                        │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## Installation

### Homebrew (macOS/Linux)

```bash
brew tap abnegate/tap
brew install claudear
```

### APT (Debian/Ubuntu)

```bash
curl -fsSL https://abnegate.github.io/apt-repo/pubkey.gpg | sudo gpg --dearmor -o /usr/share/keyrings/claudear.gpg
echo "deb [signed-by=/usr/share/keyrings/claudear.gpg] https://abnegate.github.io/apt-repo stable main" | sudo tee /etc/apt/sources.list.d/claudear.list
sudo apt update && sudo apt install claudear
```

### Pre-built Binaries

Download from the [releases page](https://github.com/abnegate/claudear/releases) (Linux, macOS Intel/ARM).

### From Source

```bash
git clone https://github.com/abnegate/claudear.git
cd claudear
cargo build --release
# Binary at target/release/claudear
```

### Docker

```bash
docker pull ghcr.io/abnegate/claudear:latest
```

---

## Quick Start

```bash
# 1. Create your config file
cp claudear.example.toml claudear.toml
# Edit claudear.toml with your Linear API key, GitHub token, etc.

# 2. Seed existing issues (so Claudear doesn't process old issues)
claudear seed

# 3. Start watching (daemon mode with polling + webhooks + dashboard)
claudear start --poll --port 3100

# 4. Open the dashboard
open http://localhost:3100
```

That's it. Label a Linear issue with `auto-implement` or `claude`, and Claudear will pick it up, fix it, and open a PR.

---

## Configuration

Claudear uses a TOML configuration file. Copy the example and customize:

```bash
cp claudear.example.toml claudear.toml
```

By default, Claudear looks for `claudear.toml` in the current directory. Override with:

```bash
claudear --config /path/to/config.toml poll
```

### Environment Variable Overrides

All config values can be overridden with environment variables, useful for keeping secrets out of version control and for container deployments:

| Variable | Config Path |
|----------|-------------|
| `LINEAR_API_KEY` | `linear.api_key` |
| `SENTRY_AUTH_TOKEN` | `sentry.auth_token` |
| `GITHUB_TOKEN` | `github.token` |
| `LINEAR_WEBHOOK_SECRET` | `linear.webhook_secret` |
| `SENTRY_CLIENT_SECRET` | `sentry.client_secret` |
| `GITHUB_WEBHOOK_SECRET` | `github.webhook_secret` |
| `EMBEDDING_MODEL` | Embedding model (`nomic`, `minilm`, `bge`) |
| `EMBEDDING_CACHE_DIR` | Embedding model cache directory |

### Minimal Configuration

```toml
work_dir = "~/.claudear/repos"

known_orgs = ["your-github-org"]

auto_discover_paths = ["~/projects"]

[linear]
api_key = "lin_api_xxxx"
```

### Configuration Reference

| Section | Description |
|---------|-------------|
| `work_dir` | Directory where repositories are cloned **(required)** |
| `known_orgs` | GitHub organizations to track for auto-discovery |
| `auto_discover_paths` | Local paths to scan for repository clones |
| `poll_interval_ms` | Polling interval in milliseconds (default: 300000 = 5 min) |
| `db_path` | SQLite database path (default: `./claudear.db`) |
| `webhook_port` | Webhook server port (default: 3100) |
| `max_issues_per_cycle` | Max issues per poll cycle (default: 5) |
| `max_concurrent` | Max concurrent issue processing (default: 1) |
| `processing_delay_ms` | Delay between processing issues in ms (default: 5000) |
| `claude_timeout_secs` | Claude process timeout (default: 21600 = 6 hours) |
| `claude` | Claude model, instructions, permissions (see [Custom Prompt Templates](#custom-prompt-templates)) |
| `embeddings` | Embedding model config (nomic, minilm, bge) |
| `linear` | Linear API key, trigger labels/states, team/project filters, rate limits |
| `sentry` | Sentry auth token, org slug, project filters, escalation thresholds |
| `github` | GitHub token, PR poll interval, auto-resolve on merge, review trigger tag |
| `github_app` | GitHub App authentication (App ID, private key, installation ID) |
| `discord` | Discord webhook URL, bot token, channel ID for Q&A |
| `email` | SMTP sending + IMAP reply polling |
| `sms` | Twilio account SID, auth token, phone numbers |
| `push` | Pushover API token, user key, priority |
| `ask` | Human Q&A loop: timeout, poll interval, max rounds, semantic thresholds |
| `retry` | Max retries, base delay, max delay (exponential backoff) |
| `regression` | Check interval, monitoring duration, event thresholds |
| `cascade` | Enable/disable cascading, max depth |
| `users` | User registry mapping across services |

See [`claudear.example.toml`](claudear.example.toml) for a fully documented example with every option.

---

## Usage

### Daemon Mode

The recommended way to run Claudear in production. Starts a background daemon with IPC control.

```bash
# Start with polling, webhooks, and dashboard
claudear start --poll --port 3100

# Start with custom polling interval
claudear start --poll --poll-interval 60000

# Start without webhooks or dashboard
claudear start --poll --no-webhooks --no-dashboard

# Control the daemon
claudear status              # Check daemon health
claudear pause               # Stop picking up new issues
claudear resume              # Resume processing
claudear activity            # View recent activity
claudear activity 50         # Show last 50 entries
claudear stop                # Stop the daemon
```

### Polling Mode (Foreground)

```bash
# Poll all enabled sources (default 5 minute interval)
claudear poll

# Custom interval
claudear poll 60000

# With dashboard
claudear poll --port 8080
```

### Webhook Mode

Real-time event processing via webhooks from Linear, Sentry, and GitHub.

```bash
# Start webhook server
claudear webhook

# Auto-configure webhooks with Linear/Sentry APIs
claudear webhook --setup-webhooks --base-url https://my-server.example.com:3100
```

The `--setup-webhooks` flag:
1. Connects to Linear/Sentry APIs using your configured API keys
2. Creates webhooks pointing to your server
3. Retrieves signing secrets and writes them to your `.env` file
4. Starts the webhook server with verification enabled

### Manual Triggers

```bash
# Trigger a fix for a specific issue
claudear trigger linear abc123-def456
claudear trigger sentry 12345678

# Reset a failed attempt for retry
claudear reset sentry 12345678

# View statistics
claudear stats

# List configured sources
claudear sources
```

### PR Management

```bash
# List all tracked PRs
claudear prs list

# Check pending PRs once
claudear prs monitor

# Run continuously
claudear prs monitor --continuous
```

### Retry Management

```bash
# List issues eligible for retry
claudear retries list

# Process all ready retries now
claudear retries process
```

### Dashboard

The dashboard is a React + TypeScript web UI that's embedded directly in the release binary.

```bash
# Start dashboard (default port 3100)
claudear dashboard

# Custom port
claudear dashboard 8080

# Serve from external build directory
claudear dashboard --dashboard-dir ./dashboard/dist
```

**What the dashboard shows:**
- Real-time statistics: total attempts, success rate, merge rate
- Status breakdown: pending, success, merged, closed, failed, cannot fix
- Source-by-source metrics (Linear, Sentry, Discord, GitHub)
- Recent attempt history with direct PR links
- Retryable issues with one-click retry

**API endpoints:**

| Endpoint | Description |
|----------|-------------|
| `GET /api/health` | Health check |
| `GET /api/stats` | Statistics |
| `GET /api/stats/overview` | Full dashboard data |
| `GET /api/attempts` | List attempts (with filtering) |
| `GET /api/attempts/:id` | Single attempt detail |
| `GET /api/sources` | Source information |
| `GET /api/retries` | Retryable issues |

### Scheduled Reports

```bash
# Preview a report
claudear report preview daily
claudear report preview weekly

# Send a report immediately
claudear report send daily

# Start the scheduler
claudear report schedule --daily --hour 9
claudear report schedule --daily --weekly --hour 9
```

Reports include issues attempted/succeeded/failed, success rates, PRs created/merged/closed, source-by-source breakdown, and current pending/retryable issues. Delivered via all configured notification channels.

### Multi-Repository Cascading

Claudear tracks dependency graphs between repositories and can automatically propagate fixes through downstream repos.

```bash
# Discover repositories from configured paths
claudear repos discover
claudear repos discover --paths ~/projects ~/work --save

# List and manage indexed repos
claudear repos list
claudear repos index
claudear repos stats
claudear repos sync

# Search files across all repos
claudear repos search "auth middleware"

# Define and explore dependencies
claudear repos link my-lib my-app --dep-type npm
claudear repos graph
claudear repos graph --root my-lib

# Preview what a change would cascade to
claudear repos cascade my-lib
```

### Human Q&A Loop

When Claude encounters ambiguity that requires human input, it emits a structured question:

```json
{"question":"...","context":"...","options":["..."],"why":"..."}
```

**How it works:**
1. Question is fanned out to all enabled notification channels
2. Discord and Email support reply polling (first reply wins)
3. Claude resumes immediately with the answer
4. Q&A pairs are stored and reused via semantic matching (source+repo scoped, then global fallback)
5. If timeout is reached and `ask.best_effort_on_timeout=true`, Claude continues with an explicit uncertainty note

**Requirements for reply channels:**
- **Discord**: `discord.bot_token` + `discord.channel_id`
- **Email**: IMAP fields (`imap_host`, `imap_port`, `imap_username`, `imap_password`)

### Inference Analytics

Track how accurately Claudear routes issues to the correct repository.

```bash
# Success rates by confidence level
claudear inference stats

# Recent inference history
claudear inference history
claudear inference history --limit 50

# Provide feedback to improve future routing
claudear inference feedback 42 --correct
claudear inference feedback 43 --actual-repo my-other-repo
```

### Release Tracking

Dependency-aware release detection that tracks when fixes land in production.

```bash
# Show the release dependency graph
claudear diag release-graph

# Check if a PR's fix is in a target release
claudear diag release-check owner/repo 42 --target owner/target-repo

# Show the dependency path from source to target
claudear diag release-path owner/source-repo owner/target-repo
```

### Diagnostics

```bash
# Database stats and recent operations
claudear diag db
```

### Dry Run

```bash
# Preview what would be processed without actually running Claude
claudear dry-run
```

### User Registry

Map team members across services so notifications go to the right person.

```toml
[users.jake]
linear_name = "Jake Barnwell"
github_username = "jakebarnby"
sentry_username = "jake"
discord_id = "123456789012345678"
email = "jake@example.com"
push_user_key = "pushover_user_key"
sms_number = "+1234567890"
```

When an issue is assigned to a user, Claudear routes notifications to their configured channels.

---

## Fix Attempt Lifecycle

```
┌─────────┐     ┌─────────┐     ┌─────────┐     ┌─────────┐
│ Pending │────>│ Success │────>│ Merged  │────>│Resolved │
└─────────┘     └─────────┘     └─────────┘     └─────────┘
     │               │               │
     │               │               ▼
     │               │          ┌─────────┐
     │               └─────────>│ Closed  │──┐
     │                          └─────────┘  │
     │                                       │ retry
     ▼                                       │
┌─────────┐                                  │
│ Failed  │<─────────────────────────────────┘
└────┬────┘
     │ retry (max 2)
     ▼
┌───────────┐
│Cannot Fix │
└───────────┘
```

| Status | Description |
|--------|-------------|
| **Pending** | Fix attempt in progress |
| **Success** | PR created successfully |
| **Merged** | PR merged, issue auto-resolved on source |
| **Closed** | PR closed without merging (triggers retry) |
| **Failed** | Fix attempt failed (triggers retry) |
| **Cannot Fix** | Max retries exhausted |

---

## AI Feedback Loop

Claudear learns from every fix attempt to improve future performance.

1. **Track Outcomes** -- Every attempt result (merged, closed, failed) is recorded
2. **Generate Embeddings** -- Issue content is vectorized locally using fastembed (ONNX Runtime)
3. **Find Similar Issues** -- When a new issue arrives, similar past issues are retrieved
4. **Extract Patterns** -- Keywords and strategies from successful fixes are extracted
5. **Enhance Prompts** -- Future Claude prompts are augmented with learnings from similar past issues

All embedding computation runs locally. No external API calls. Supported models: `nomic`, `minilm`, `bge`.

---

## Custom Prompt Templates

### Per-Repository: AGENT.md

Create an `AGENT.md` file in any repository root. Claudear prepends this to all prompts for that repo.

```markdown
# Project Guidelines

## Code Style
- Use TypeScript strict mode
- Follow Airbnb style guide
- Keep functions under 50 lines

## Testing
- Write unit tests for all new functions
- Aim for 80% code coverage
- Use Jest for testing

## PR Guidelines
- Keep PRs focused on a single issue
- Include tests with all changes
```

### Global: claudear.toml

```toml
[claude]
# Model selection
model = "sonnet"   # sonnet, opus, haiku, or full model ID

# Custom instructions appended to the system prompt
instructions = "Always write tests. Follow existing code style."

# Or load from a file
instructions_file = "./claude-instructions.md"

# Tool permissions granted without prompting
permissions = ["Bash(git *)", "Read", "Edit"]

# Skip all permission prompts (default: true)
skip_permissions = true
```

---

## Running as a Service

### macOS (launchd)

Create `~/Library/LaunchAgents/com.claudear.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.claudear</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/claudear</string>
        <string>start</string>
        <string>--poll</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/claudear.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/claudear.log</string>
</dict>
</plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.claudear.plist
```

### Linux (systemd)

Create `/etc/systemd/system/claudear.service`:

```ini
[Unit]
Description=Claudear
After=network.target

[Service]
Type=simple
User=YOUR_USER
ExecStart=/usr/local/bin/claudear start --poll
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now claudear
```

---

## Docker

### Docker Compose (Recommended)

```bash
# Start all services
docker compose up -d

# View logs
docker compose logs -f

# Stop
docker compose down
```

### Standalone

```bash
# Build
docker build -t claudear .

# Run with config file
docker run -d \
  -p 3100:3100 \
  -v $(pwd)/claudear.toml:/app/claudear.toml \
  -v $(pwd):/app/project \
  -v claudear-data:/app/data \
  claudear

# Or with environment variable overrides
docker run -d \
  -p 3100:3100 \
  -v $(pwd)/claudear.toml:/app/claudear.toml \
  -v $(pwd):/app/project \
  -v claudear-data:/app/data \
  -e LINEAR_API_KEY=your-key \
  -e GITHUB_TOKEN=your-token \
  claudear
```

### Docker Details

The Docker image:
- Multi-stage build (Bun for dashboard, Rust for binary, Debian slim runtime)
- Includes Claude Code (installed via npm), git, and Node.js
- Embeds the dashboard UI in the binary
- Persists embedding model cache between restarts
- Supports both `ANTHROPIC_API_KEY` and OAuth login for Claude authentication
- Health check on `/api/health` every 30 seconds

---

## CI/CD

The project includes GitHub Actions workflows:

### CI (`ci.yml`)
Runs on every push/PR to `main`/`develop`:
- Tests on Ubuntu
- Linting (rustfmt, clippy)
- Multi-platform builds (Linux, macOS Intel/ARM)
- Code coverage via tarpaulin
- Dashboard tests

### Release (`release.yml`)
Runs on version tags (`v*`):
- Creates GitHub release with changelog
- Builds binaries for Linux, macOS (Intel & ARM)
- Publishes Docker image to GHCR
- Publishes to Homebrew tap
- Builds and publishes Debian packages to APT repository

### Production E2E Smoke (`e2e-prod-smoke.yml`)
Manual/nightly live-flow verification:
- Creates a real Linear issue
- Runs `claudear trigger` against a real GitHub repo
- Verifies PR creation via GitHub API
- Cleans up (closes PR, resolves issue)
- Enable with repository variable `CLAUDEAR_PROD_E2E_ENABLED=true`

```bash
# Create a release
git tag v1.0.0
git push origin v1.0.0
```

---

## Development

### Prerequisites

- Rust 1.93+
- Bun (for dashboard)
- Docker (optional)

### Building

```bash
make build              # Debug build
make build-release      # Release build with embedded dashboard (optimized, LTO, stripped)
make install            # Install to /usr/local/bin
```

### Testing

```bash
make test               # Run Rust tests
make test-all           # Rust + dashboard tests
make test-prod-e2e      # Real production E2E smoke test (requires credentials)
make check              # Format + lint + test
```

**Production E2E test** requires these environment variables:
- `CLAUDEAR_E2E_LINEAR_API_KEY`
- `CLAUDEAR_E2E_LINEAR_TEAM_ID`
- `CLAUDEAR_E2E_GITHUB_REPO` (format: `owner/repo`)
- `CLAUDEAR_E2E_GITHUB_TOKEN`
- `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`

### Dashboard

```bash
make dashboard          # Install dependencies
make dashboard-dev      # Dev server on :5173
make dashboard-build    # Production build
make dashboard-test     # Run tests
```

### All Makefile Targets

| Target | Description |
|--------|-------------|
| `make build` | Debug build |
| `make build-release` | Release build (optimized, LTO, stripped) |
| `make install` | Install to /usr/local/bin |
| `make test` | Run Rust tests |
| `make test-all` | Run Rust + dashboard tests |
| `make test-prod-e2e` | Run production E2E smoke test |
| `make lint` | Run clippy linter |
| `make fmt` | Format code |
| `make check` | Format + lint + test |
| `make dev` | Hot reload development |
| `make watch` | Watch mode for tests |
| `make doc` | Generate and open docs |
| `make audit` | Security audit on dependencies |
| `make dashboard-dev` | Dashboard dev server |
| `make dashboard-build` | Build dashboard for production |
| `make docker` | Build and start Docker services |
| `make docker-dev` | Development Docker environment |
| `make release-deb` | Build .deb package |
| `make db-reset` | Reset SQLite database |
| `make db-backup` | Backup SQLite database |

---

## License

MIT
