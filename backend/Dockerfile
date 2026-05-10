# syntax=docker/dockerfile:1.7
#
# Multi-stage build for both jitacart-api and jitacart-worker. Single
# image; the docker-compose service picks the binary via `command:`.
#
# Migrations are embedded by `sqlx::migrate!()` at compile time, so the
# runtime image doesn't need the migrations directory.
#
# We rely on BuildKit's cache mounts (`--mount=type=cache,...`) for the
# cargo registry + target/ dir so incremental rebuilds reuse compiled
# dependencies. We deliberately don't do the "stub manifest" trick to
# pre-cache the dep graph — cargo's fingerprinting is finicky about
# stubs vs. real sources, and the cache mount alone gives us most of
# the win on second builds.

FROM rust:1.95-slim-bookworm@sha256:b8ecdb97c5b9c1ae058249f72710dbe33d4da19f7b8d911bd3c72e5f048af251 AS builder
WORKDIR /build
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin jitacart-api --bin jitacart-worker \
    && cp target/release/jitacart-api /usr/local/bin/jitacart-api \
    && cp target/release/jitacart-worker /usr/local/bin/jitacart-worker

# ── Runtime image ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim@sha256:67b30a61dc87758f0caf819646104f29ecbda97d920aaf5edc834128ac8493d3 AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /app jitacart \
    && mkdir -p /app && chown jitacart /app

COPY --from=builder /usr/local/bin/jitacart-api /usr/local/bin/
COPY --from=builder /usr/local/bin/jitacart-worker /usr/local/bin/

USER jitacart
WORKDIR /app

# Default to api; the worker service overrides via `command:`.
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["jitacart-api"]
