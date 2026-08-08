# Build stage
# Must stay >= the highest MSRV in Cargo.lock — stellar-rpc-client 27 requires
# Rust 1.93, jsonrpsee 0.26 requires 1.85, axum 0.8 requires 1.80.
FROM rust:1.95-slim AS builder

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
RUN cargo build --release --bin oracle

# Runtime stage
FROM debian:bookworm-slim

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
    CMD curl -f http://localhost:8080/health || exit 1

# Run the binary
CMD ["/app/oracle"]