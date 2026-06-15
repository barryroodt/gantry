# gantry-bench container: run the harness benchmark (live or keyless smoke)
# with no local dev environment. Build from the repo root:
#
#   docker build -f bench/docker/Dockerfile -t gantry-bench \
#       --build-arg GANTRY_SHA="$(git rev-parse HEAD)" .
#
# All version pins are build args; bumping one is a reviewed change (harness
# versions land in every results file via the fairness footer).

# Keep in sync with mise.toml (single source of truth for the toolchain).
ARG RUST_VERSION=1.96.0

# ---------------------------------------------------------------------------
# Stage 1 — build gantry, gantry-bench, and the mock upstream
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-slim-bookworm AS builder

WORKDIR /build
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked -p gantry -p gantry-bench --bins --example mock_upstream \
    && mkdir -p /out \
    && cp target/release/gantry target/release/gantry-bench \
          target/release/examples/mock_upstream /out/

# ---------------------------------------------------------------------------
# Stage 2 — slim runtime with the competitor harness CLIs
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# Competitor harness pins (fairness: reproducible images benchmark the same
# competitors; results record versions either way).
ARG CLAUDE_CODE_VERSION=2.1.175
ARG OMP_VERSION=15.11.7
# omp requires bun >= 1.3.14 (its package `engines`); installed via the npm
# `bun` package to avoid curl|bash.
ARG BUN_VERSION=1.3.14
# Baked into results as gantry_sha; the runtime stage has no git checkout.
ARG GANTRY_SHA=unknown

# git + CA certs for workspace clones; node >= 18 (claude-code engine floor —
# bookworm's distro node is 18.x); curl for the entrypoint readiness poll.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates curl nodejs npm \
    && rm -rf /var/lib/apt/lists/*

# bun hosts omp (`#!/usr/bin/env bun`); BUN_INSTALL=/usr/local puts the global
# bin symlinks on PATH.
ENV BUN_INSTALL=/usr/local
RUN npm install -g "bun@${BUN_VERSION}" \
    && npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && bun install -g "@oh-my-pi/pi-coding-agent@${OMP_VERSION}" \
    && claude --version && omp --version

COPY --from=builder /out/gantry /out/gantry-bench /out/mock_upstream /usr/local/bin/
COPY bench/tasks /opt/gantry-bench/tasks
COPY bench/docker/entrypoint.sh /usr/local/bin/bench-entrypoint

ENV GANTRY_BENCH_GANTRY_BIN=/usr/local/bin/gantry \
    GANTRY_BENCH_TASKS_DIR=/opt/gantry-bench/tasks \
    GANTRY_BENCH_CACHE_DIR=/var/cache/gantry-bench \
    GANTRY_BENCH_SHA=${GANTRY_SHA}

RUN mkdir -p /var/cache/gantry-bench /results && chmod +x /usr/local/bin/bench-entrypoint

VOLUME /results
ENTRYPOINT ["bench-entrypoint"]
CMD ["smoke"]
