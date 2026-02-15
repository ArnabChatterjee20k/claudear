# Claudear

A high-performance unified watcher service written in Rust that monitors issue trackers and error monitoring services, automatically spawning Claude Code agents to fix issues and create pull requests.

## Features

- **Multi-Source Support**: Monitor Linear issues and Sentry errors from a single service
- **Repository Auto-Discovery**: Automatically discovers and indexes repositories from known GitHub organizations
- **Intelligent Issue Routing**: Infers the target repository from stack traces, file paths, and issue content
- **Daemon Mode**: Full background daemon with IPC control (start/stop/pause/resume/status)
- **PR Merge Tracking**: Automatically track when PRs are merged and resolve issues
- **Automatic Retries**: Exponential backoff retry logic for failed attempts
- **Custom Prompt Templates**: Use AGENT.md for project-specific Claude instructions
- **Configurable Claude Models**: Choose between Sonnet, Opus, Haiku, or any model ID
- **Analytics Dashboard**: Real-time Svelte web UI with statistics, attempt history, and source metrics
- **Scheduled Reports**: Automated daily/weekly/monthly status reports via notifications
- **Multi-Repository Cascading**: Dependency tracking with automatic cascading fix propagation
- **AI Feedback Loop**: Embedding-based similarity matching to learn from past outcomes
- **Human Q&A Loop**: Claude can ask blocking questions over notifications and resume on first reply
- **Regression Monitoring**: Post-fix verification to detect regressions on Linear and Sentry
- **Release Tracking**: Dependency-aware release detection and path analysis
- **Multiple Notification Channels**: Discord, Email (SMTP), SMS (Twilio), Push (Pushover)
- **Webhook Support**: Real-time event processing via webhooks with auto-configuration
- **SQLite Tracking**: Persistent tracking of fix attempts with full history
- **Per-Source Rate Limiting**: Configurable concurrent processing and delays per source
- **Extensible Architecture**: Trait-based design makes it easy to add new sources, notifiers, and storage backends

## Architecture

```
┌──────────────────────────────────────────────────────────────────────────┐
│                               Claudear                                   │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                               │
│  │  Linear  │  │  Sentry  │  │  GitHub  │  ← Sources (IssueSource)      │
│  │  Source  │  │  Source  │  │ Webhooks │                                │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘                               │
│       │             │             │                                      │
│       └──────┬──────┴──────┬──────┘                                      │
│              │             │                                             │
│              ▼             ▼                                             │
│       ┌────────────────────────┐     ┌──────────────┐                   │
│       │        Watcher         │────>│  Repo Index  │                   │
│       │  (polls, matches,      │     │  (inference,  │                   │
│       │   coordinates)         │     │  discovery)   │                   │
│       └───────────┬────────────┘     └──────────────┘                   │
│                   │                                                      │
│    ┌──────────────┼──────────────────────┬───────────────┐              │
│    │              │              │       │               │               │
│    ▼              ▼              ▼       ▼               ▼               │
│ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌───────────┐         │
│ │ Claude  │ │ SQLite  │ │Notifiers│ │   PR    │ │ Feedback  │         │
│ │ Runner  │ │ Tracker │ │(Discord,│ │ Monitor │ │ Analyzer  │         │
│ │         │ │         │ │Email,..)│ │         │ │(embeddings)│         │
│ └─────────┘ └─────────┘ └─────────┘ └─────────┘ └───────────┘         │
│                                                                          │
│ ┌─────────────┐ ┌──────────────┐ ┌──────────────┐ ┌────────────┐       │
│ │  Regression │ │   Release    │ │     IPC      │ │  Cascade   │       │
│ │  Monitor    │ │   Tracker    │ │ (daemon ctl) │ │  Engine    │       │
│ └─────────────┘ └──────────────┘ └──────────────┘ └────────────┘       │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
```

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/abnegate/claudear.git
cd claudear

# Build the release binary
cargo build --release

# The binary is at target/release/claudear
```

### Pre-built Binaries

Download from the [releases page](https://github.com/abnegate/claudear/releases).

### Homebrew (macOS/Linux)

```bash
# Add the tap
brew tap abnegate/tap

# Install
brew install claudear
```

### APT (Debian/Ubuntu)

```bash
# Add GPG key
curl -fsSL https://abnegate.github.io/apt-repo/pubkey.gpg | sudo gpg --dearmor -o /usr/share/keyrings/claudear.gpg

# Add repository
echo "deb [signed-by=/usr/share/keyrings/claudear.gpg] https://abnegate.github.io/apt-repo stable main" | sudo tee /etc/apt/sources.list.d/claudear.list

# Install
sudo apt update
sudo apt install claudear
```

### Docker

```bash
docker pull ghcr.io/abnegate/claudear:latest
```

## Configuration

Claudear uses a YAML configuration file. Create your config file:

```bash
cp claudear.example.yaml claudear.yaml
```

Edit `claudear.yaml` with your settings.

### Config File Location

By default, the tool looks for `claudear.yaml` in the current directory. You can specify a different path:

```bash
claudear --config /path/to/config.yaml poll
```

### Environment Variable Overrides

All configuration values can be overridden by environment variables. This is useful for:
- Keeping secrets out of version control
- Container deployments
- CI/CD environments

Environment variable names follow this pattern:
- `LINEAR_API_KEY` for `linear.api_key`
- `SENTRY_AUTH_TOKEN` for `sentry.auth_token`
- `GITHUB_TOKEN` for `github.token`

### Minimal Configuration

```yaml
work_dir: ~/.claudear/repos

known_orgs:
  - your-github-org

auto_discover_paths:
  - ~/projects

linear:
  api_key: lin_api_xxxx
```

### Full Configuration Example

See `claudear.example.yaml` for a complete configuration file with all options documented.

### Key Configuration Sections

| Section | Description |
|---------|-------------|
| `work_dir` | Working directory where repositories are cloned (REQUIRED) |
| `known_orgs` | GitHub organizations to track for auto-discovery |
| `auto_discover_paths` | Local paths to scan for repository clones |
| `claude` | Claude CLI settings (model, instructions, permissions) |
| `embeddings` | Local embedding model for similarity search (nomic, minilm, bge) |
| `linear` | Linear integration (API key, trigger labels/states, per-source rate limits) |
| `sentry` | Sentry integration (auth token, org slug, escalation thresholds) |
| `github` | GitHub PR monitoring (token, auto-resolve on merge) |
| `retry` | Retry configuration (max retries, backoff delays) |
| `discord` | Discord notification webhook |
| `email` | SMTP + optional IMAP reply polling |
| `ask` | Human Q&A loop settings (timeouts, semantic thresholds, rounds) |
| `sms` | Twilio SMS notifications |
| `push` | Pushover push notifications |

## Usage

### First-Time Setup

```bash
# Mark all existing issues as seen (run this first!)
claudear seed
```

### Daemon Mode

The recommended way to run Claudear in production. Starts a background daemon with IPC control.

```bash
# Start the daemon with polling, webhooks, and dashboard
claudear start --port 3100 --poll

# Start with custom polling interval (ms)
claudear start --poll --poll-interval 60000

# Start without webhooks or dashboard
claudear start --poll --no-webhooks --no-dashboard

# Check daemon status
claudear status

# Pause processing (stops picking up new issues)
claudear pause

# Resume processing
claudear resume

# View recent activity
claudear activity
claudear activity 50    # Show last 50 entries

# Stop the daemon
claudear stop
```

### Polling Mode (Foreground)

```bash
# Poll all enabled sources (default 5 minute interval)
claudear poll

# Poll with custom interval (in milliseconds)
claudear poll 60000

# Poll with dashboard on custom port
claudear poll --port 8080
```

### Webhook Mode

```bash
# Start webhook server on default port (3100)
claudear webhook

# Start on custom port
claudear webhook 8080
```

#### Webhook Auto-Configuration

The watcher can automatically register webhooks with Linear and Sentry using their APIs, eliminating manual webhook setup.

**Requirements:**
- API keys for the sources you want to configure (Linear API key, Sentry auth token)

**Usage:**

```bash
# Auto-configure webhooks and start the server
claudear webhook --setup-webhooks --base-url https://my-server.example.com:3100

# Use a custom .env file for storing secrets
claudear webhook --setup-webhooks --base-url https://... --env-file /path/to/.env
```

**What it does:**
1. Connects to Linear/Sentry APIs using your existing API keys
2. Creates webhooks pointing to your server (`<base-url>/webhook/linear`, `<base-url>/webhook/sentry`)
3. Retrieves the webhook signing secrets returned by the APIs
4. Writes secrets to your `.env` file (`LINEAR_WEBHOOK_SECRET`, `SENTRY_CLIENT_SECRET`)
5. Starts the webhook server with the configured secrets

**Notes:**
- Linear webhooks are created at the organization level (or team-scoped if `team_id` is set)
- Sentry webhooks are created for each project in `sentry.project_slugs`
- If a webhook with the same URL already exists, configuration will fail (delete it manually first)
- Secrets are automatically quoted if they contain special characters

### PR Management

```bash
# List all tracked PRs
claudear prs list

# Check pending PRs once
claudear prs monitor

# Run continuously
claudear prs monitor --continuous
```

### Manual Operations

```bash
# Trigger a Linear issue
claudear trigger linear abc123-def456

# Trigger a Sentry issue
claudear trigger sentry 12345678

# Reset a failed attempt
claudear reset sentry 12345678

# View statistics
claudear stats

# List configured sources
claudear sources
```

### Retry Management

```bash
# List issues eligible for retry
claudear retries list

# Process all ready retries now
claudear retries process
```

### Dashboard

```bash
# Start dashboard API server on default port (3100)
claudear dashboard

# Start on custom port
claudear dashboard 8080

# Serve built dashboard files (full UI)
claudear dashboard --dashboard-dir ./dashboard/dist
```

The dashboard provides:
- Real-time statistics overview (total attempts, success rate, merge rate)
- Status breakdown (pending, success, merged, closed, failed, cannot fix)
- Source-by-source metrics
- Recent attempts list with PR links
- Retryable issues view

API endpoints available:
- `GET /api/health` - Health check
- `GET /api/stats` - Statistics
- `GET /api/stats/overview` - Full dashboard data
- `GET /api/attempts` - List attempts (with filtering)
- `GET /api/attempts/:id` - Single attempt detail
- `GET /api/sources` - Source information
- `GET /api/retries` - Retryable issues

### Scheduled Reports

```bash
# Preview a report (daily, weekly, or monthly)
claudear report preview daily
claudear report preview weekly

# Generate and send a report immediately
claudear report send daily

# Start the report scheduler (background)
claudear report schedule --daily --hour 9

# Start with both daily and weekly reports
claudear report schedule --daily --weekly --hour 9
```

Reports include:
- Issues attempted, succeeded, failed, and "cannot fix"
- Success and failure rates
- PRs created, merged, and closed
- Source-by-source breakdown
- Current pending and retryable issues

Reports are sent via all configured notification channels (Discord, Email, SMS, Push).

### Human Q&A Notifications

When Claude is blocked on missing human context, it emits a machine-readable line and Claudear runs a question loop:

```text
CLAUDEAR_QUESTION: {"question":"...","context":"...","options":["..."],"why":"..."}
```

Behavior:
- Ask delivery fan-outs to all enabled notifiers.
- v1 reply-capable channels are Discord and Email.
- Claudear waits for the first valid reply and ignores later replies for that round.
- If timeout is reached and `ask.best_effort_on_timeout=true`, Claudear continues with explicit uncertainty instead of hard-failing.
- Q&A pairs are stored and reused with embedding-based semantic matching (scoped by source+repo first, then global fallback).

Reply ingestion requirements:
- Discord replies require `discord.bot_token` and `discord.channel_id` (webhook-only still sends asks).
- Email replies require IMAP fields under `email` (`imap_host`, `imap_port`, `imap_username`, `imap_password`, optional folder/TLS settings).

### Multi-Repository Support

Claudear auto-discovers repositories from configured GitHub organizations and tracks dependencies between them for cascading fix propagation.

```bash
# Auto-discover repositories from configured paths
claudear repos discover
claudear repos discover --paths ~/projects ~/work --save

# List all indexed repositories
claudear repos list

# Build/refresh the file index
claudear repos index
claudear repos index --force

# Search files across all repos
claudear repos search "auth middleware"

# Show index statistics
claudear repos stats

# Sync repository data to database
claudear repos sync

# Link repositories (upstream -> downstream)
claudear repos link my-lib my-app --dep-type npm

# View the dependency graph
claudear repos graph

# View from a specific root
claudear repos graph --root my-lib

# See what cascades from a change
claudear repos cascade my-lib
```

When changes are made to an upstream repository, the cascade engine automatically identifies and propagates fixes to downstream repositories using BFS dependency traversal.

### Inference Analytics

Track how accurately Claudear routes issues to the correct repository.

```bash
# Show inference success rates by confidence level
claudear inference stats

# View recent inference attempts
claudear inference history
claudear inference history --limit 50

# Provide feedback on an inference (for learning)
claudear inference feedback 42 --correct
claudear inference feedback 43 --actual-repo my-other-repo
```

### AI Feedback Loop

The feedback system uses local embedding models (fastembed) to learn from past fix attempts and improve future prompts.

**How it works:**
- Tracks outcomes of all fix attempts (merged, closed, failed)
- Generates vector embeddings for issue content using local models (nomic, minilm, bge)
- Finds similar past issues using embedding similarity
- Extracts keywords and patterns from successful/failed attempts
- Enhances future prompts with learnings from similar issues

**No external services required** - all embedding computation runs locally via ONNX Runtime.

### Regression Monitoring

Post-fix verification that monitors for regressions after fixes are deployed.

- Runs hourly checks on Linear and Sentry for 24 hours after a fix
- Detects if fixed issues re-open or error rates spike
- Configurable checkers per source type
- Composable checker architecture

### Release Tracking

Dependency-aware release detection to track when fixes land in production.

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
# Show database table counts and recent operations
claudear diag db

# Release tracking diagnostics (see above)
claudear diag release-graph
claudear diag release-check <repo> <pr> [--target <repo>]
claudear diag release-path <source> <target>
```

### Dry Run Mode

```bash
# See what would be processed without actually running Claude
claudear dry-run
```

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

- **Pending**: Fix attempt in progress
- **Success**: PR created successfully
- **Merged**: PR was merged, issue auto-resolved on source
- **Closed**: PR was closed without merging (triggers retry)
- **Failed**: Fix attempt failed (triggers retry)
- **Cannot Fix**: Max retries reached, issue cannot be automatically fixed

## Custom Prompt Templates

You can customize Claude's behavior by creating an `AGENT.md` file in your project root. This file will be prepended to all prompts sent to Claude.

### Example AGENT.md

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
- Update documentation as needed
```

### Claude Configuration

You can also configure Claude's behavior globally via `claudear.yaml`:

```yaml
claude:
  # Model to use (sonnet, opus, haiku, or full model ID)
  model: sonnet

  # Custom instructions appended to the system prompt
  instructions: "Always write tests. Follow existing code style."

  # Tool permissions granted without prompting
  permissions:
    - "Bash(git *)"
    - "Read"
    - "Edit"

  # Skip all permission prompts (default: true)
  skip_permissions: true
```

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

Load it:

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

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable claudear
sudo systemctl start claudear
```

## Development

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release
```

### Testing

The project has 1100+ tests covering all modules.

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run tests for a specific module
cargo test config::tests

# Run a specific test
cargo test test_exponential_backoff_delay

# Run all checks (format, lint, test)
make check
```

### Production E2E Smoke Test

For a real production-path verification (live Linear + GitHub + Claude), use:

```bash
make test-prod-e2e
```

Required environment variables:

- `CLAUDEAR_E2E_LINEAR_API_KEY`
- `CLAUDEAR_E2E_LINEAR_TEAM_ID`
- `CLAUDEAR_E2E_GITHUB_REPO` (format: `owner/repo`)
- `CLAUDEAR_E2E_GITHUB_TOKEN`
- `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN` (optional if `claude auth status` shows `"loggedIn": true` on the runner/machine)

What this smoke test does:

1. Creates a real Linear issue.
2. Clones the real target GitHub repository into a temporary workspace.
3. Runs the real `claudear trigger` flow against that issue.
4. Verifies success + PR URL in the SQLite tracker and verifies the PR via GitHub API.
5. Cleans up by closing the PR and resolving the Linear issue.

Script location: `scripts/prod-e2e-smoke.sh`

### Linting

```bash
# Check formatting
cargo fmt --check

# Run clippy
cargo clippy -- -D warnings
```

### Dashboard Development

The dashboard is a Svelte web application.

```bash
# Install dependencies
make dashboard

# Start dev server
make dashboard-dev

# Build for production
make dashboard-build

# Run dashboard tests
make dashboard-test
```

### Makefile Targets

Run `make help` for a full list. Key targets:

| Target | Description |
|--------|-------------|
| `make build` | Debug build |
| `make build-release` | Release build (optimized, LTO, stripped) |
| `make install` | Install to /usr/local/bin |
| `make test` | Run Rust tests |
| `make test-all` | Run Rust + dashboard tests |
| `make test-prod-e2e` | Run real production smoke test |
| `make lint` | Run clippy linter |
| `make fmt` | Format code |
| `make check` | Format + lint + test |
| `make dashboard-dev` | Start dashboard dev server |
| `make dashboard-build` | Build dashboard for production |
| `make docker` | Build and start Docker services |
| `make docker-dev` | Start development Docker environment |
| `make release-deb` | Build .deb package |
| `make db-reset` | Reset SQLite database |
| `make db-backup` | Backup SQLite database |

## Docker

### Build and Run

```bash
# Build the Docker image
docker build -t claudear .

# Run with docker-compose (recommended)
docker-compose up -d

# Run standalone with config file
docker run -d \
  -p 3100:3100 \
  -v $(pwd)/claudear.yaml:/app/claudear.yaml \
  -v $(pwd):/app/project \
  -v claudear-data:/app/data \
  claudear

# Or use environment variables to override config
docker run -d \
  -p 3100:3100 \
  -v $(pwd)/claudear.yaml:/app/claudear.yaml \
  -v $(pwd):/app/project \
  -v claudear-data:/app/data \
  -e LINEAR_API_KEY=your-key \
  claudear
```

### Docker Compose

The included `docker-compose.yml` provides:
- Claudear service
- PostgreSQL with pgvector for AI similarity search
- Volume persistence for data

```bash
# Start all services
docker-compose up -d

# View logs
docker-compose logs -f

# Stop services
docker-compose down
```

## CI/CD

The project includes GitHub Actions workflows for:

- **CI** (`ci.yml`): Runs on push/PR to main/develop
  - Tests on Ubuntu
  - Linting (rustfmt, clippy)
  - Build for Linux and macOS
  - Code coverage via tarpaulin

- **Release** (`release.yml`): Runs on version tags
  - Creates GitHub release
  - Builds binaries for Linux, macOS (Intel & ARM)
  - Publishes Docker image to GHCR
  - Publishes to Homebrew tap
  - Builds and publishes Debian packages to APT repository

- **Production E2E Smoke** (`e2e-prod-smoke.yml`): Manual/nightly live-flow verification
  - Disabled by default; enable with repository variable `CLAUDEAR_PROD_E2E_ENABLED=true`
  - Requires repository secrets listed in the Production E2E section above
  - Executes `scripts/prod-e2e-smoke.sh` against real services and performs cleanup

### Creating a Release

```bash
# Tag and push
git tag v1.0.0
git push origin v1.0.0
```

This triggers the release workflow to build and publish artifacts.

## License

MIT
