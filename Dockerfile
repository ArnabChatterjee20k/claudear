ARG ONNXRUNTIME_VERSION=1.23.2
ARG VECTORLITE_VERSION=16a01af79add
ARG RUST_VERSION=1.93
ARG DEBIAN_VERSION=trixie
ARG GIT_USER_NAME="Claudear"
ARG GIT_USER_EMAIL="claudear@noreply.local"

# Dashboard build stage
FROM oven/bun:1 AS dashboard

WORKDIR /app/dashboard
COPY dashboard/package.json dashboard/bun.lock* ./
RUN bun install --frozen-lockfile
COPY dashboard/ ./
RUN bun run build

# Vectorlite build stage (no prebuilt binaries for linux arm64)
FROM debian:${DEBIAN_VERSION}-slim AS vectorlite
ARG VECTORLITE_VERSION

RUN apt-get update && apt-get install -y \
    build-essential \
    cmake \
    curl \
    git \
    ninja-build \
    pkg-config \
    python3 \
    zip \
    unzip \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

RUN git clone --recurse-submodules https://github.com/1yefuwang1/vectorlite.git . \
    && git checkout "${VECTORLITE_VERSION}"

ENV CMAKE_POLICY_VERSION_MINIMUM=3.5

RUN python3 bootstrap_vcpkg.py

RUN cmake --preset release && cmake --build build/release -j$(nproc)

FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y \
    build-essential \
    libssl-dev \
    pkg-config \
    perl \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
RUN mkdir -p dashboard/dist
RUN cargo build --release && rm -rf src

COPY src ./src

# Copy dashboard for embedding
COPY --from=dashboard /app/dashboard/dist ./dashboard/dist
RUN touch src/main.rs src/lib.rs && cargo build --release

# Install Claude Code native binary in a throwaway stage
FROM debian:${DEBIAN_VERSION}-slim AS claude-code
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -m -u 1000 appuser
USER appuser
RUN curl -fsSL https://claude.ai/install.sh | bash

FROM debian:${DEBIAN_VERSION}-slim
ARG GIT_USER_NAME
ARG GIT_USER_EMAIL

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    jq \
    openssh-client \
    && rm -rf /var/lib/apt/lists/* /usr/share/doc/* /usr/share/man/* /usr/share/locale/*

COPY --from=vectorlite /build/build/release/vectorlite/vectorlite.so /usr/local/lib/vectorlite.so
COPY --from=builder /app/target/release/claudear /usr/local/bin/claudear
COPY --chmod=755 docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

RUN adduser --disabled-password --uid 1000 --gecos "" appuser \
    && mkdir -p /app/data /app/repos /home/appuser/.cache/fastembed /home/appuser/.claude \
    && chown -R appuser:appuser /app /home/appuser/.cache /home/appuser/.claude

# Copy only the Claude Code binary from the install stage
COPY --from=claude-code --chown=appuser:appuser /home/appuser/.local /home/appuser/.local

USER appuser

RUN git config --global user.name "${GIT_USER_NAME}" \
    && git config --global user.email "${GIT_USER_EMAIL}" \
    && git config --global init.defaultBranch main

ENV PATH="/home/appuser/.local/bin:${PATH}"

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
