# syntax=docker/dockerfile:1.7

ARG BUN_VERSION=1.3.14
ARG RUST_VERSION=1.97.1

FROM oven/bun:${BUN_VERSION}-slim AS frontend
WORKDIR /build

COPY package.json pnpm-lock.yaml ./
RUN bun install --frozen-lockfile --ignore-scripts

COPY . .
RUN bun x tsc --noEmit -p src-web/tsconfig.json \
    && bun x vite build --config src-web/vite.config.ts

FROM rust:${RUST_VERSION}-bookworm AS backend
WORKDIR /build

RUN apt-get update \
    && apt-get install --yes --no-install-recommends cmake libdbus-1-dev libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --locked --release -p pebble-web

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl libdbus-1-3 libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 pebble \
    && useradd --system --uid 10001 --gid pebble --home-dir /app pebble \
    && install --directory --owner pebble --group pebble /app /data

COPY --from=backend /build/target/release/pebble-web /usr/local/bin/pebble-web
COPY --from=frontend --chown=pebble:pebble /build/dist /app/dist

ENV PEBBLE_PORT=8080 \
    PEBBLE_DATA_DIR=/data \
    PEBBLE_STATIC_DIR=/app/dist

USER pebble
WORKDIR /app

EXPOSE 8080
VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:8080/api/v1/health >/dev/null || exit 1

ENTRYPOINT ["/usr/local/bin/pebble-web"]
