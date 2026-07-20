FROM rust:1.97-bookworm AS builder

WORKDIR /build
ENV CARGO_BUILD_JOBS=1
COPY . .
RUN cargo build --locked --release --bin disaster-alert

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home disaster-alert \
    && install --directory --owner=disaster-alert --group=disaster-alert /data

COPY --from=builder /build/target/release/disaster-alert /usr/local/bin/disaster-alert

USER disaster-alert
WORKDIR /home/disaster-alert
ENV SERVER_HOST=0.0.0.0 \
    SERVER_PORT=30010 \
    DB_PATH=/data/disaster-alert.fjall
EXPOSE 30010
VOLUME ["/data"]

ENTRYPOINT ["/usr/local/bin/disaster-alert"]
