# Build stage
# Builder image must stay >= the workspace MSRV declared in [workspace.package]
# rust-version in Cargo.toml (and checked by CI). Bump both together.
# Pinned to digest for reproducibility and supply-chain security (#1218).
FROM rust:1.95-slim@sha256:e14e87345b4d5964ddcc3491d27ee046a0f23820f340c3c1e24da6880141f7c0 AS builder

WORKDIR /app

# Install dependencies

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY shared/config ./shared/config
COPY oracle ./oracle
# config/tokens.json is embedded via include_str! at compile time and must
# be present in the builder stage before cargo build runs (#502).
COPY config ./config

# Build the binary
RUN cargo build --release --locked --bin oracle

# Runtime stage
# Pinned to digest for reproducibility and supply-chain security (#1218).
FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary from builder
COPY --from=builder /app/target/release/oracle /app/oracle

# No configuration files are copied into the runtime stage: the binary embeds
# config/tokens.json at compile time via include_str! (see
# oracle/src/config.rs) and PRICE_FEED_CONFIG, when set, is an inline JSON
# value rather than a path. There is no file for the process to read (#769, #896).

# Create non-root user
RUN useradd -r -s /bin/false oracle
USER oracle

# Expose port
EXPOSE 8080

# Health check — /health is in-memory and instant, so a short timeout suffices.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Run the binary
CMD ["/app/oracle"]