# ── Build stage ───────────────────────────────────────────────
FROM rust:1.82-bookworm AS builder

WORKDIR /app

# 1. Cache dependency build: copy only manifests + lock first
COPY Cargo.toml Cargo.lock ./
# Create skeleton src for each crate so `cargo build` can resolve deps
RUN mkdir -p crates/onto-core/src crates/onto-storage/src crates/onto-ontology/src \
            crates/onto-query/src crates/onto-raft/src crates/onto-sharding/src \
            crates/onto-server/src crates/onto-cli/src crates/onto-graph/src \
            crates/onto-enterprise/src && \
    touch crates/onto-core/src/lib.rs crates/onto-storage/src/lib.rs \
          crates/onto-ontology/src/lib.rs crates/onto-query/src/lib.rs \
          crates/onto-raft/src/lib.rs crates/onto-sharding/src/lib.rs \
          crates/onto-graph/src/lib.rs crates/onto-enterprise/src/lib.rs && \
    echo "fn main() {}" > crates/onto-server/src/main.rs && \
    echo "fn main() {}" > crates/onto-cli/src/main.rs && \
    cargo build --release --bin ontodb-server --bin ontodb-cli 2>/dev/null || true

# 2. Copy real source and rebuild (only changed layers recompile)
COPY crates/ crates/
RUN touch crates/onto-server/src/main.rs crates/onto-cli/src/main.rs && \
    cargo build --release --bin ontodb-server --bin ontodb-cli

# 3. Run tests during build (optional, controlled by build arg)
ARG RUN_TESTS=false
RUN if [ "$RUN_TESTS" = "true" ]; then cargo test --workspace; fi

# ── Runtime stage ────────────────────────────────────────────
FROM debian:bookworm-slim

LABEL maintainer="OntoDB Team" \
      description="OntoDB - Ontology-driven semantic multi-modal database" \
      version="0.1.0"

# Install minimal runtime dependencies + tini for proper signal handling
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      tini \
      curl && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd -r ontodb && useradd -r -g ontodb -d /home/ontodb -s /sbin/nologin ontodb

# Copy binaries
COPY --from=builder /app/target/release/ontodb-server /usr/local/bin/
COPY --from=builder /app/target/release/ontodb-cli /usr/local/bin/

# Prepare data directory
RUN mkdir -p /data/ontodb /data/backup /etc/ontodb && \
    chown -R ontodb:ontodb /data /etc/ontodb

VOLUME ["/data/ontodb", "/data/backup"]
EXPOSE 7912 7913 5432

# Health check: probe the readiness endpoint every 30s
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -f http://localhost:7912/api/health/live || exit 1

USER ontodb
WORKDIR /home/ontodb

# Use tini as PID 1 so signals (SIGTERM/SIGINT) propagate correctly
ENTRYPOINT ["tini", "--", "ontodb-server"]
CMD ["--data-dir", "/data/ontodb", "--http", "0.0.0.0:7912", "--listen", "0.0.0.0:7913"]
