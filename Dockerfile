# Build stage
# Must stay >= the highest MSRV in Cargo.lock — stellar-rpc-client 27 requires
# Rust 1.93, jsonrpsee 0.26 requires 1.85, axum 0.8 requires 1.80.
# Pinned to immutable multi-arch digest; tag: rust:1.95-slim
FROM rust:1.95-slim@sha256:e14e87345b4d5964ddcc3491d27ee046a0f23820f340c3c1e24da6880141f7c0 AS builder

WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

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
# Pinned to immutable multi-arch digest; tag: debian:bookworm-slim
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary from builder
COPY --from=builder /app/target/release/oracle /app/oracle

# Copy configuration files
COPY config/tokens.json /app/config/tokens.json

# Create non-root user
RUN useradd -r -s /bin/false oracle
USER oracle

# Expose port
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/ready || exit 1

# Run the binary
CMD ["/app/oracle"]