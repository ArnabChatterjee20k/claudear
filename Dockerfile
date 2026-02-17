# Dashboard build stage
FROM oven/bun:1 AS dashboard-builder

WORKDIR /app/dashboard
COPY dashboard/package.json dashboard/bun.lock* ./
RUN bun install --frozen-lockfile
COPY dashboard/ ./
RUN bun run build

# Rust build stage
FROM rust:1.93 AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
# Create empty dashboard/dist so the dependency cache step compiles
RUN mkdir -p dashboard/dist
RUN cargo build --release && rm -rf src

COPY src ./src
# Copy the built dashboard assets for embedding
COPY --from=dashboard-builder /app/dashboard/dist ./dashboard/dist
RUN touch src/main.rs src/lib.rs && cargo build --release

FROM debian:trixie-slim

WORKDIR /app

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    git \
    git-lfs \
    openssh-client \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code

COPY --from=builder /app/target/release/claudear /usr/local/bin/claudear
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

RUN useradd -m -u 1000 appuser

RUN mkdir -p /app/data /app/repos /home/appuser/.cache/fastembed /home/appuser/.claude && \
    chown -R appuser:appuser /app /home/appuser/.cache /home/appuser/.claude

USER appuser

RUN git config --global user.name "Claudear Bot" && \
    git config --global user.email "claudear@noreply.local" && \
    git config --global init.defaultBranch main

ENV PROJECT_DIR=/app/project
ENV DATA_DIR=/app/data
ENV REPOS_DIR=/app/repos
ENV EMBEDDING_CACHE_DIR=/home/appuser/.cache/fastembed

# Claude Code authentication (provide at runtime):
#   Option 1: Set ANTHROPIC_API_KEY env var (API key)
#   Option 2: Omit ANTHROPIC_API_KEY and the entrypoint will run 'claude auth login'
#             (prints a URL to open in your browser for OAuth)

EXPOSE 3100

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3100/api/health || exit 1

ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["claudear"]
