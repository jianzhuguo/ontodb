FROM rust:1.77-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/ontodb-server /usr/local/bin/
COPY --from=builder /app/target/release/ontodb-cli /usr/local/bin/

RUN useradd -m ontodb && mkdir -p /data && chown ontodb:ontodb /data
USER ontodb
WORKDIR /home/ontodb

EXPOSE 7912 7913 7914

VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD curl -f http://localhost:7912/api/health || exit 1

ENTRYPOINT ["ontodb-server"]
CMD ["--data-dir", "/data", "--http", "0.0.0.0:7912"]
