# Build stage
FROM rust:1.93 AS builder

WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Create dummy src to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs

# Build dependencies (this layer is cached)
RUN cargo build --release && rm -rf src

# Copy source code
COPY src ./src

# Build application
RUN touch src/main.rs src/lib.rs && cargo build --release

# Runtime stage
FROM debian:trixie-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /app/target/release/claudear /usr/local/bin/claudear

# Create non-root user
RUN useradd -m -u 1000 appuser

# Create directories for data and cache
RUN mkdir -p /app/data /root/.cache/fastembed && \
    chown -R appuser:appuser /app /root/.cache

USER appuser

# Set environment variables
ENV PROJECT_DIR=/app/project
ENV DATA_DIR=/app/data

# Expose ports
EXPOSE 3100

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3100/api/health || exit 1

# Default command
CMD ["claudear", "dashboard"]
