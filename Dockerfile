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
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/claudear /usr/local/bin/claudear

RUN useradd -m -u 1000 appuser

RUN mkdir -p /app/data /home/appuser/.cache/fastembed && \
    chown -R appuser:appuser /app /home/appuser/.cache

USER appuser

ENV PROJECT_DIR=/app/project
ENV DATA_DIR=/app/data
ENV EMBEDDING_CACHE_DIR=/home/appuser/.cache/fastembed

EXPOSE 3100

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3100/api/health || exit 1

CMD ["claudear"]
