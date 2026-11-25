# Singularr: one binary (singularrd, Axum), the rules and the web client. The build context is the
# repository root, because the server crate (app/) and the pure crate (crates/singularr-core) share a
# workspace:
#   docker build -t singularr .
#
# Stages:
#   builder   the release build, then the tests in the release profile
#   check     the gate (tools/gate.sh): fmt, clippy and every test, the database-backed ones against
#             a throwaway PostgreSQL 18. Nothing depends on it, so a build without --target skips it:
#               docker buildx build --target check --output type=cacheonly .
#   runtime   Debian slim with the binary, the rules and the web client: the image. It is the last
#             stage, so `docker build .` builds it.
#
# The toolchain is the nightly that rust-toolchain.toml asks for, pinned to one date here so a build
# does not change with the day it runs (RUSTUP_TOOLCHAIN wins over the file).
ARG RUST_IMAGE=rust:1-trixie@sha256:e84068000361bf3d4a5ba2b7647f62893528b1eb9f847d7a23b493f5ce8ea254
ARG RUNTIME_IMAGE=debian:trixie-slim@sha256:8aa5a7bf5a1955bddbe7d74fcdd0b91ab30e74362a8b73af672b0481b35b44a0
ARG RUST_NIGHTLY=nightly-2026-11-21

FROM ${RUST_IMAGE} AS toolchain
ARG RUST_NIGHTLY
ENV RUSTUP_TOOLCHAIN=${RUST_NIGHTLY}
RUN rustup toolchain install "${RUST_NIGHTLY}" --profile minimal
WORKDIR /build

FROM toolchain AS builder
COPY . .
# rules/ holds the default policy; the server falls back to its built-in policy (with a warning)
# when rules/policy.toml is missing.
RUN mkdir -p rules
# --locked: build exactly the committed Cargo.lock and fail when it is out of date.
RUN --mount=type=cache,id=singularr-release-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=singularr-release-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo build --release --locked -p singularr --no-default-features --features ssr \
    && mkdir -p /build/out \
    && cp target/release/singularrd /build/out/singularrd
# The tests, in the release profile so they reuse its compiled dependencies. The database-backed
# tests skip themselves here (no database is reachable from this step; the check stage runs them).
RUN --mount=type=cache,id=singularr-release-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=singularr-release-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo test --release --locked --workspace --no-default-features --features ssr

FROM toolchain AS check
RUN rustup component add --toolchain "${RUSTUP_TOOLCHAIN}" rustfmt clippy
# PostgreSQL 18 from the PostgreSQL project's Debian repository, for the database-backed tests. No
# cluster is created here: tools/gate.sh makes a throwaway one.
RUN apt-get update \
    && apt-get install -y --no-install-recommends postgresql-common \
    && echo 'create_main_cluster = false' >> /etc/postgresql-common/createcluster.conf \
    && /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh -y \
    && apt-get install -y --no-install-recommends postgresql-18 \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN --mount=type=cache,id=singularr-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=singularr-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=singularr-check-target,target=/build/target,sharing=locked \
    --mount=type=tmpfs,target=/run/singularr-pg \
    bash tools/gate.sh

FROM ${RUNTIME_IMAGE} AS runtime
# CA certificates for the outbound calls (TLS is rustls) and curl for the health check. Debian's
# security updates are applied at build time. The image then includes every fix released after its
# base image. The server runs as uid 10001, never as root.
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 singularr \
    && useradd --system --uid 10001 --gid 10001 --home-dir /app --shell /usr/sbin/nologin singularr
WORKDIR /app
COPY --from=builder --chown=10001:10001 /build/out/singularrd /app/singularrd
COPY --from=builder --chown=10001:10001 /build/rules /app/rules
# The web client: static files that read /api/v1.
COPY --from=builder --chown=10001:10001 /build/web /app/web

ENV SINGULARR_LISTEN="0.0.0.0:3000" \
    SINGULARR_POLICY="/app/rules/policy.toml" \
    SINGULARR_WEB_DIR="/app/web" \
    RUST_LOG="info"
EXPOSE 3000
USER 10001:10001
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD curl -fsS http://127.0.0.1:3000/healthz || exit 1
CMD ["/app/singularrd"]
