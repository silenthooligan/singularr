# Singularr: one binary (singularrd, Axum), the rules and the web client. The build context is the
# repository root, because the server crate (app/) and the crates under crates/ share a workspace:
#   docker build -t singularr .
#
# Stages:
#   builder   the release build, then the tests in the release profile
#   check     the gate (tools/gate.sh): fmt, clippy and every test, the database-backed ones against
#             a throwaway PostgreSQL 18. Nothing depends on it, so a build without --target skips it:
#               docker buildx build --target check --output type=cacheonly .
#   ffmpeg-static   ffmpeg and ffprobe: FFmpeg-Builds' LGPL static build for the target
#             architecture, pinned to one release and checked against its SHA-256, with its
#             license and SOURCE.txt
#   runtime-base, media-static, media-distro   Debian slim, then Debian slim with the media tools at
#             /usr/local/bin: the static build, or Debian's packages. FFMPEG (default `static`)
#             picks the runtime's base
#   runtime   Debian slim with the binary, the rules and the web client: the image. It is the last
#             stage, so `docker build .` builds it.
#
# The toolchain is the nightly that rust-toolchain.toml asks for, pinned to one date here so a build
# does not change with the day it runs (RUSTUP_TOOLCHAIN wins over the file).
ARG RUST_IMAGE=rust:1-trixie@sha256:e84068000361bf3d4a5ba2b7647f62893528b1eb9f847d7a23b493f5ce8ea254
ARG RUNTIME_IMAGE=debian:trixie-slim@sha256:8aa5a7bf5a1955bddbe7d74fcdd0b91ab30e74362a8b73af672b0481b35b44a0
ARG RUST_NIGHTLY=nightly-2026-11-21
ARG FFMPEG=static

# ffmpeg and ffprobe for the tests of the builder and check stages and for the runtime: FFmpeg 9.0
# (n9.0.2-39-g7adfbe40dc) in FFmpeg-Builds' LGPL static build, from its release of 31 Oct 2026.
# FFmpeg-Builds keeps the last build of each month published. A daily build goes after about two
# weeks. The archive is checked against its SHA-256 before anything is unpacked. The binaries are
# static except for glibc. Both runtimes carry glibc. LGPL 3: the build holds no GPL part.
# Probing, subtitles, audio decoding and frame grabs need none.
FROM ${RUNTIME_IMAGE} AS ffmpeg-static
ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
    && rm -rf /var/lib/apt/lists/*
# The license of the build (LICENSE.txt) and SOURCE.txt: the version, the two commits, the
# configuration and the release that holds the corresponding source.
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) build=linux64; sum=6046d6731b26f58887e953561551d254a6eec9fbca4100dfeab56e19af8328d5 ;; \
      arm64) build=linuxarm64; sum=3d1f66827a73ce5a446312bc4924234dc1515726d3743e2df35c0177e1f82128 ;; \
      *) echo "no FFmpeg build for ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    name="ffmpeg-n9.0.2-39-g7adfbe40dc-${build}-lgpl-9.0"; \
    curl -fsSL --retry 3 -o /tmp/ffmpeg.tar.xz \
      "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/autobuild-2026-10-31-12-59/${name}.tar.xz"; \
    echo "${sum}  /tmp/ffmpeg.tar.xz" | sha256sum -c -; \
    tar -xJf /tmp/ffmpeg.tar.xz -C /tmp "${name}/bin/ffmpeg" "${name}/bin/ffprobe" "${name}/LICENSE.txt"; \
    mkdir -p /out/bin /out/licenses; \
    install -p -m 0755 "/tmp/${name}/bin/ffmpeg" "/tmp/${name}/bin/ffprobe" /out/bin/; \
    install -p -m 0644 "/tmp/${name}/LICENSE.txt" /out/licenses/LICENSE.txt; \
    { \
      printf '%s\n' \
        "FFmpeg n9.0.2-39-g7adfbe40dc in the LGPL static build of FFmpeg-Builds (${build})." \
        "" \
        "FFmpeg: https://git.ffmpeg.org/ffmpeg.git, commit 7adfbe40dcde598f93a6963d2f7b22cd75202ccc." \
        "FFmpeg-Builds: https://github.com/FFmpeg-Builds/FFmpeg-Builds, commit 5031c6f1082414e53688d39e521849905071070d." \
        "  Its scripts.d names the repository and the revision of every library in the build." \
        "Release: https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/tag/autobuild-2026-10-31-12-59," \
        "  ${name}.tar.xz, SHA-256 ${sum}." \
        "Corresponding source: https://github.com/silenthooligan/singularr/releases/tag/ffmpeg-source-n9.0.2-39-g7adfbe40dc" \
        "  FFmpeg at its commit, every library of the build at its revision, FFmpeg-Builds itself," \
        "  MANIFEST.txt, SHA256SUMS and ffmpeg-NOTICES.txt." \
        "" \
        "The build's configuration (ffmpeg -buildconf):"; \
      /out/bin/ffmpeg -hide_banner -buildconf; \
    } > /out/licenses/SOURCE.txt; \
    rm -rf /tmp/ffmpeg.tar.xz "/tmp/${name}"; \
    /out/bin/ffmpeg -hide_banner -version | head -n 1; \
    /out/bin/ffprobe -hide_banner -version | head -n 1

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
# The media tools for core::media's tests. SINGULARR_TEST_MEDIA=required: the tests cannot skip.
COPY --from=ffmpeg-static /out/bin/ /usr/local/bin/
ENV SINGULARR_TEST_MEDIA=required
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
# ffmpeg and ffprobe for core::media's tests. SINGULARR_TEST_MEDIA=required: a test without them
# fails.
COPY --from=ffmpeg-static /out/bin/ /usr/local/bin/
ENV SINGULARR_TEST_MEDIA=required
COPY . .
RUN --mount=type=cache,id=singularr-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=singularr-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=singularr-check-target,target=/build/target,sharing=locked \
    --mount=type=tmpfs,target=/run/singularr-pg \
    bash tools/gate.sh

FROM ${RUNTIME_IMAGE} AS runtime-base

# Debian slim with the media tools at /usr/local/bin (core::media). FFMPEG picks one as the
# runtime's base: `static`, the default, or `distro`, Debian's packages. Scratch files go to the
# data folder: /tmp is a 64 MB tmpfs in the compose file.
FROM runtime-base AS media-static
COPY --link --from=ffmpeg-static /out/bin/ /usr/local/bin/
COPY --link --from=ffmpeg-static /out/licenses/ /usr/local/share/licenses/ffmpeg/
# The GPL text the LGPL 3 builds on and the notices of the libraries the build links.
COPY --link third_party/ffmpeg/COPYING.GPLv3 third_party/ffmpeg/NOTICES.txt /usr/local/share/licenses/ffmpeg/

FROM runtime-base AS media-distro
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && ln -s /usr/bin/ffmpeg /usr/local/bin/ffmpeg \
    && ln -s /usr/bin/ffprobe /usr/local/bin/ffprobe

FROM media-${FFMPEG} AS runtime
# CA certificates for the outbound calls (TLS is rustls). The health check is the binary's own
# (`singularrd health`). The image holds no curl. Debian's security updates are applied at build
# time. The image then includes every fix released after its base image. The server runs as uid
# 10001, never as root. /data is its data folder (SINGULARR_DATA_DIR), owned by that uid so a volume
# mounted there starts out writable.
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 singularr \
    && useradd --system --uid 10001 --gid 10001 --home-dir /app --shell /usr/sbin/nologin singularr \
    && mkdir -p /data \
    && chown 10001:10001 /data
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
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD ["/app/singularrd", "health"]
CMD ["/app/singularrd"]
