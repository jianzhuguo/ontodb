# Build stage
FROM rust:1.82-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

RUN cargo build --release --bin ontodb-server --bin ontodb-cli

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --shell /bin/bash ontodb

COPY --from=builder /app/target/release/ontodb-server /usr/local/bin/
COPY --from=builder /app/target/release/ontodb-cli /usr/local/bin/

RUN mkdir -p /data && chown ontodb:ontodb /data

VOLUME /data
EXPOSE 8080 6500

USER ontodb
WORKDIR /home/ontodb

ENTRYPOINT ["ontodb-server"]
CMD ["--data-dir", "/data", "--http", "0.0.0.0:8080", "--listen", "0.0.0.0:6500"]
