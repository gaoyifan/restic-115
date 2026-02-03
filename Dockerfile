# syntax=docker/dockerfile:1.7
# Multi-stage build for optimized Rust binary
FROM rust:1.92-slim AS builder

# Install build dependencies (perl needed for vendored OpenSSL build)
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    ca-certificates \
    perl \
    make \
    gcc \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Create a dummy src directory to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies only (this layer will be cached unless Cargo.toml changes)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release && rm -rf src

# Copy source code
COPY src ./src

# Build the actual application
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    touch src/main.rs && cargo build --release \
    && cp /app/target/release/restic-115 /app/restic-115

# Runtime stage - Debian 13 (Trixie)
FROM debian:trixie-slim

# Install runtime dependencies (no OpenSSL needed due to vendored feature)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -m -u 1000 appuser

WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /app/restic-115 /app/restic-115

# Change ownership to non-root user
RUN chown -R appuser:appuser /app

# Switch to non-root user
USER appuser

# Expose the default port
EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f -I "http://localhost:${LISTEN_PORT:-8000}/config"

# Run the application
ENTRYPOINT ["/app/restic-115"]
