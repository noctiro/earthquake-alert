# syntax=docker/dockerfile:1

FROM rust:1.97-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY benches ./benches
COPY src ./src
COPY web ./web

RUN cargo build --locked --release --bin disaster-alert

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 disaster-alert \
    && useradd --system --uid 10001 --gid disaster-alert --home-dir /app disaster-alert \
    && install --directory --owner disaster-alert --group disaster-alert /app /data

COPY --from=builder /build/target/release/disaster-alert /usr/local/bin/disaster-alert

USER disaster-alert
WORKDIR /app

ENV SERVER_HOST=0.0.0.0 \
    SERVER_PORT=30010 \
    DB_PATH=/data/disaster-alert.fjall

EXPOSE 30010
VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error "http://127.0.0.1:${SERVER_PORT}/health"

ENTRYPOINT ["disaster-alert"]
