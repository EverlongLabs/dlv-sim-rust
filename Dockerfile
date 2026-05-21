# ── Build stage ──
FROM rust:1.87-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml ./
COPY v3-pool/Cargo.toml v3-pool/

# Build deps only (dummy sources), then nuke our crate artifacts + fingerprints
# so the real build recompiles them. Third-party deps stay cached.
RUN mkdir -p src v3-pool && \
    echo "fn main() {}" > src/main.rs && \
    echo "pub fn dummy() {}" > v3-pool/lib.rs && \
    cargo build --release 2>/dev/null || true && \
    rm -rf src v3-pool/lib.rs \
           target/release/dlv-sim \
           target/release/deps/*dlv_sim* \
           target/release/deps/*v3_pool* \
           target/release/.fingerprint/dlv-sim-* \
           target/release/.fingerprint/v3-pool-*

# Copy actual source
COPY v3-pool/ v3-pool/
COPY src/ src/

# Build for real (only our crates recompile; deps are cached)
RUN cargo build --release

# ── Runtime stage ──
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/dlv-sim /app/dlv-sim

ENTRYPOINT ["/app/dlv-sim"]
