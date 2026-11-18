# Build stage
# Must stay >= the highest MSRV in Cargo.lock — stellar-rpc-client 27 requires
# Rust 1.93, jsonrpsee 0.26 requires 1.85, axum 0.8 requires 1.80.
# Pinned to digest for reproducibility and supply-chain security (#1218).
FROM rust:1.95-slim@sha256:e14e87345b4d5964ddcc3491d27ee046a0f23820f340c3c1e24da6880141f7c0 AS builder

WORKDIR /app

# ── Dependency caching layer (#1260) ─────────────────────────────────────────
# Copy only the manifest files and a stub source tree first.  Docker caches
# this layer and the `cargo build` below as long as Cargo.toml / Cargo.lock
# don't change, so subsequent builds skip the full dependency compile even when
# application source files are edited.
COPY Cargo.toml Cargo.lock ./
COPY shared/config/Cargo.toml ./shared/config/Cargo.toml
COPY oracle/Cargo.toml ./oracle/Cargo.toml
# config/tokens.json is embedded at compile time; provide a minimal stub so
# include_str! resolves during the dependency-cache build.
COPY config ./config
RUN mkdir -p shared/config/src oracle/src \
    && echo 'pub fn placeholder() {}' > shared/config/src/lib.rs \
    && echo 'fn main() {}' > oracle/src/main.rs \
    && cargo build --release --locked --bin oracle \
    && rm -rf oracle/src shared/config/src
# ─────────────────────────────────────────────────────────────────────────────

# Copy workspace files
COPY shared/config ./shared/config
COPY oracle ./oracle

# Build the binary (deps already compiled above; only changed crates recompile)
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

# Health check — timeout covers /ready's worst-case (RPC reachability +
# keeper-balance retries), capped at 15s by READY_CHECK_TIMEOUT_SECS (#1042,
# #1043). Use /health for pure liveness (always fast, in-memory only).
HEALTHCHECK --interval=30s --timeout=20s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Run the binary
CMD ["/app/oracle"]