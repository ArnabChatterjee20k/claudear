# Claudear

A high-performance unified watcher service written in Rust that monitors issue trackers and error monitoring services, automatically spawning Claude Code agents to fix issues and create pull requests.

## Features

- **Multi-Source Support**: Monitor Linear issues and Sentry errors from a single service
- **PR Merge Tracking**: Automatically track when PRs are merged and resolve issues
- **Automatic Retries**: Exponential backoff retry logic for failed attempts
- **Custom Prompt Templates**: Use AGENT.md for project-specific Claude instructions
- **Analytics Dashboard**: Real-time web UI with statistics, attempt history, and source metrics
- **Scheduled Reports**: Automated daily/weekly status reports via notifications
- **Multi-Repository Support**: Dependency tracking and cascading change detection
- **AI Feedback Loop**: Learn from past outcomes to improve future prompts
- **Multiple Notification Channels**: Discord, Email (SMTP), SMS (Twilio), Push (Pushover)
- **Webhook Support**: Real-time event processing via webhooks
- **SQLite Tracking**: Persistent tracking of fix attempts with full history
- **Priority-Based Processing**: Urgent/escalating issues are processed first
- **Rate Limiting**: Configurable concurrent processing and delays
- **Extensible Architecture**: Easy to add new sources and notifiers

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                            Claudear                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                       │
│  │  Linear  │  │  Sentry  │  │  GitHub  │  ← Sources            │
│  │  Source  │  │  Source  │  │ (Monitor)│    (IssueSource)      │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘                       │
│       │             │             │                              │
│       └──────┬──────┴──────┬──────┘                              │
│              │             │                                     │
│              ▼             ▼                                     │
│       ┌────────────────────────┐                                │
│       │        Watcher         │  ← Core orchestrator           │
│       │  (polls, matches,      │                                │
│       │   coordinates)         │                                │
│       └───────────┬────────────┘                                │
│                   │                                              │
│    ┌──────────────┼──────────────────────┐                      │
│    │              │              │       │                       │
│    ▼              ▼              ▼       ▼                       │
│ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐                 │
│ │ Claude  │ │ SQLite  │ │Notifiers│ │   PR    │                 │
│ │ Runner  │ │ Tracker │ │(Discord,│ │ Monitor │                 │
│ │         │ │         │ │Email,..)│ │         │                 │
│ └─────────┘ └─────────┘ └─────────┘ └─────────┘                 │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
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
- `PROJECT_DIR` for `project_dir`
- `LINEAR_API_KEY` for `linear.api_key`
- `SENTRY_AUTH_TOKEN` for `sentry.auth_token`

### Minimal Configuration

```yaml
project_dir: /path/to/your/project

linear:
  api_key: lin_api_xxxx
```

### Full Configuration Example

See `claudear.example.yaml` for a complete configuration file with all options documented.

### Key Configuration Sections

| Section | Description |
|---------|-------------|
| `project_dir` | Path to the project for Claude to work on (REQUIRED) |
| `linear` | Linear integration settings (API key, trigger labels/states) |
| `sentry` | Sentry integration settings (auth token, org slug) |
| `github` | GitHub PR monitoring (token, auto-resolve on merge) |
| `discord` | Discord notification webhook |
| `email` | SMTP email notifications |
| `sms` | Twilio SMS notifications |
| `push` | Pushover push notifications |
| `retry` | Retry configuration (max retries, backoff delays) |

## Usage

### First-Time Setup

```bash
# Mark all existing issues as seen (run this first!)
claudear seed
```

### Polling Mode

```bash
# Poll all enabled sources (default 5 minute interval)
claudear poll

# Poll with custom interval (in milliseconds)
claudear poll 60000
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

**Example workflow:**

```bash
# 1. Set up your .env with API keys
echo "LINEAR_API_KEY=lin_api_xxxxx" >> .env

# 2. Run with auto-configuration
claudear webhook --setup-webhooks --base-url https://my-server.example.com:3100

# Output:
# === Webhook Configuration Complete ===
#
# Linear:
#   Status: Configured
#   Webhook ID: abc123
#   Secret: lin_...xyz (saved to .env)
```

**Notes:**
- Linear webhooks are created at the organization level (or team-scoped if `LINEAR_TEAM_ID` is set)
- Sentry webhooks are created for each project in `SENTRY_PROJECT_SLUGS`
- If a webhook with the same URL already exists, configuration will fail (delete it manually first)
- Secrets are automatically quoted if they contain special characters

### PR Monitoring

```bash
# Check pending PRs once
claudear monitor-prs

# Run continuously
claudear monitor-prs --continuous

# List all tracked PRs
claudear list-prs
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
claudear list-retries

# Process all ready retries now
claudear process-retries
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
claudear report-preview daily
claudear report-preview weekly

# Generate and send a report immediately
claudear report-send daily

# Start the report scheduler (background daemon)
claudear report-scheduler --daily --hour 9

# Start with both daily and weekly reports
claudear report-scheduler --daily --weekly --hour 9
```

Reports include:
- Issues attempted, succeeded, failed, and "cannot fix"
- Success and failure rates
- PRs created, merged, and closed
- Source-by-source breakdown
- Current pending and retryable issues

Reports are sent via all configured notification channels (Discord, Email, SMS, Push).

### Multi-Repository Support

Track dependencies between repositories and understand cascading changes.

```bash
# List tracked repositories
claudear repos-list

# Add a repository
claudear repos-add my-app --path /path/to/my-app --github-url myorg/my-app

# Link repositories (upstream -> downstream)
claudear repos-link my-lib my-app --dep-type npm

# View the dependency graph
claudear repos-graph

# View from a specific root
claudear repos-graph --root my-lib

# See what cascades from a change
claudear repos-cascade my-lib
```

**Default Appwrite relationships are pre-configured:**
- `utopia-*` -> `appwrite` -> `cloud`

When changes are made to an upstream repository, the tool can identify which downstream repositories may need updates.

### AI Feedback Loop

The feedback system learns from past fix attempts to improve future prompts.

**Features:**
- Tracks outcomes of all fix attempts (merged, closed, failed)
- Extracts keywords and patterns from issues
- Finds similar past issues using text similarity
- Generates suggestions based on successful and failed attempts
- Enhances prompts with learnings from similar issues

**Usage in code:**
```rust
use claudear::feedback::{FeedbackAnalyzer, Outcome};

let mut analyzer = FeedbackAnalyzer::new();

// Record an outcome
analyzer.record_outcome(&attempt, &issue, "prompt used", Outcome::Merged)?;

// Add learnings
analyzer.add_learnings(outcome_id, "Check null values in API responses")?;

// Find similar issues and get suggestions for a new issue
let similar = analyzer.find_similar(&new_issue);
let suggestions = analyzer.suggest_improvements(&new_issue);

// Generate enhanced prompt
let enhanced_prompt = analyzer.enhance_prompt("Fix this bug", &new_issue);
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

When the watcher detects an AGENT.md file, it will include these instructions in the prompt:

```
[Contents of AGENT.md]

---

Fix the following Linear issue:
...
```

This allows you to customize Claude's behavior for your specific project without modifying the watcher code.

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
        <string>poll</string>
    </array>
    <key>WorkingDirectory</key>
    <string>/Users/YOUR_USER/Local/your-project</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PROJECT_DIR</key>
        <string>/Users/YOUR_USER/Local/your-project</string>
    </dict>
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
WorkingDirectory=/home/YOUR_USER/Local/your-project
ExecStart=/usr/local/bin/claudear poll
Environment=PROJECT_DIR=/home/YOUR_USER/Local/your-project
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
```

### Linting

```bash
# Check formatting
cargo fmt --check

# Run clippy
cargo clippy
```

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

- **Security** (`security.yml`): Runs weekly and on main
  - Cargo audit for vulnerabilities
  - Cargo deny for license/dependency checks
  - Dependency review for PRs

### Creating a Release

```bash
# Tag and push
git tag v1.0.0
git push origin v1.0.0
```

This triggers the release workflow to build and publish artifacts.

## License

MIT
