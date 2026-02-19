# Claudear Makefile
# ==================

BINARY_NAME := claudear
INSTALL_PATH := /usr/local/bin
CARGO := cargo
BUN := bun

# Detect OS for install command
UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
	INSTALL_CMD := install -m 755
else
	INSTALL_CMD := install -Dm755
endif

.PHONY: all build build-release install uninstall clean test test-all test-prod-e2e lint fmt check \
        dashboard dashboard-build dashboard-dev dashboard-test \
        docker docker-build docker-up docker-down docker-logs docker-clean \
        dev run help

# Default target
all: build

# =============================================================================
# CLI Targets
# =============================================================================

## build: Build the CLI in debug mode
build:
	$(CARGO) build

## build-release: Build the CLI in release mode with embedded dashboard
build-release: dashboard-build
	$(CARGO) build --release

## install: Install the CLI to /usr/local/bin (requires sudo)
install: build-release
	sudo $(INSTALL_CMD) target/release/$(BINARY_NAME) $(INSTALL_PATH)/$(BINARY_NAME)
	@echo "Installed $(BINARY_NAME) to $(INSTALL_PATH)"

## uninstall: Remove the CLI from /usr/local/bin
uninstall:
	sudo rm -f $(INSTALL_PATH)/$(BINARY_NAME)
	@echo "Uninstalled $(BINARY_NAME) from $(INSTALL_PATH)"

## clean: Remove build artifacts
clean:
	$(CARGO) clean
	rm -rf dashboard/dist dashboard/node_modules

## test: Run Rust tests
test:
	$(CARGO) test

## test-all: Run all tests (Rust + Dashboard)
test-all: test dashboard-test

## test-prod-e2e: Run real production smoke test (requires live service credentials)
## Required env vars:
##   CLAUDEAR_E2E_LINEAR_API_KEY      Linear API key
##   CLAUDEAR_E2E_LINEAR_TEAM_ID      Linear team UUID
##   CLAUDEAR_E2E_GITHUB_REPO         GitHub repo (owner/name)
##   CLAUDEAR_E2E_GITHUB_TOKEN        GitHub PAT
##   CLAUDEAR_E2E_DISCORD_BOT_TOKEN   Discord bot token (for Scenario 2)
##   CLAUDEAR_E2E_DISCORD_CHANNEL_ID  Discord channel ID (for Scenario 2)
##   Claude auth via ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, or CLI session
test-prod-e2e:
	./scripts/prod-e2e-smoke.sh

## test-prod-e2e-docker: Run production smoke test using Docker (avoids nested claude issues)
test-prod-e2e-docker:
	CLAUDEAR_E2E_USE_DOCKER=true ./scripts/prod-e2e-smoke.sh

## lint: Run clippy linter
lint:
	$(CARGO) clippy -- -D warnings

## fmt: Format Rust code
fmt:
	$(CARGO) fmt

## fmt-check: Check Rust code formatting
fmt-check:
	$(CARGO) fmt -- --check

## check: Run all checks (format, lint, test)
check: fmt-check lint test

# =============================================================================
# Dashboard Targets
# =============================================================================

## dashboard: Install dashboard dependencies
dashboard:
	cd dashboard && $(BUN) install

## dashboard-build: Build the dashboard for production
dashboard-build: dashboard
	cd dashboard && $(BUN) run build

## dashboard-dev: Start dashboard development server
dashboard-dev: dashboard
	cd dashboard && $(BUN) run dev

## dashboard-test: Run dashboard tests
dashboard-test: dashboard
	cd dashboard && $(BUN) test

## dashboard-typecheck: Run TypeScript type checking
dashboard-typecheck: dashboard
	cd dashboard && $(BUN) run typecheck

# =============================================================================
# Docker Targets
# =============================================================================

## docker: Build and start all Docker services
docker: docker-build docker-up

## docker-build: Build Docker images
docker-build:
	docker compose build

## docker-up: Start Docker services
docker-up:
	docker compose up -d

## docker-down: Stop Docker services
docker-down:
	docker compose down

## docker-logs: Show Docker logs (follow mode)
docker-logs:
	docker compose logs -f

## docker-clean: Remove Docker containers, images, and volumes
docker-clean:
	docker compose down -v --rmi local

## docker-dev: Start development Docker environment
docker-dev:
	docker compose -f docker-compose.dev.yml up --build

# =============================================================================
# Development Targets
# =============================================================================

## dev: Run the CLI in development mode with hot reload
dev:
	$(CARGO) watch -x run

## run: Run the CLI (debug build)
run: build
	./target/debug/$(BINARY_NAME)

## run-release: Run the CLI (release build)
run-release: build-release
	./target/release/$(BINARY_NAME)

## watch: Watch for changes and run tests
watch:
	$(CARGO) watch -x test

## doc: Generate and open documentation
doc:
	$(CARGO) doc --open

## audit: Run security audit on dependencies
audit:
	$(CARGO) audit

## outdated: Check for outdated dependencies
outdated:
	$(CARGO) outdated

## update: Update dependencies
update:
	$(CARGO) update

# =============================================================================
# Release Targets
# =============================================================================

## release-deb: Build a .deb package (requires cargo-deb)
release-deb: build-release
	$(CARGO) deb

## release-all: Build release binaries for all platforms
release-all:
	@echo "Building for current platform..."
	$(CARGO) build --release
	@echo "Note: Cross-compilation requires 'cross' or platform-specific toolchains"

# =============================================================================
# Database Targets
# =============================================================================

## db-reset: Reset the SQLite database
db-reset:
	rm -f data/claudear.db
	@echo "Database reset. Will be recreated on next run."

## db-backup: Backup the SQLite database
db-backup:
	@mkdir -p backups
	cp data/claudear.db backups/claudear-$(shell date +%Y%m%d-%H%M%S).db
	@echo "Database backed up to backups/"

# =============================================================================
# Help
# =============================================================================

## help: Show this help message
help:
	@echo "Claudear - Available targets:"
	@echo ""
	@echo "CLI:"
	@grep -E '^## [a-z]' $(MAKEFILE_LIST) | grep -E '(build|install|uninstall|clean|test|lint|fmt|check|run|dev|watch|doc|audit|outdated|update):' | \
		sed 's/## /  /' | sed 's/: /\t/' | column -t -s '	'
	@echo ""
	@echo "Dashboard:"
	@grep -E '^## dashboard' $(MAKEFILE_LIST) | sed 's/## /  /' | sed 's/: /\t/' | column -t -s '	'
	@echo ""
	@echo "Docker:"
	@grep -E '^## docker' $(MAKEFILE_LIST) | sed 's/## /  /' | sed 's/: /\t/' | column -t -s '	'
	@echo ""
	@echo "Database:"
	@grep -E '^## db-' $(MAKEFILE_LIST) | sed 's/## /  /' | sed 's/: /\t/' | column -t -s '	'
	@echo ""
	@echo "Release:"
	@grep -E '^## release' $(MAKEFILE_LIST) | sed 's/## /  /' | sed 's/: /\t/' | column -t -s '	'
