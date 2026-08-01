# syntax=docker/dockerfile:1
#
# picweight — single image containing the Rust backend, the built Vue SPA and
# the Android APK. Adapted from phos's Dockerfile (PRD §10) with the ML system
# dependencies removed: picweight has no ONNX and no ffmpeg, and `libsqlite3-sys`
# is pinned with the `bundled` feature, so SQLite is compiled *into* the binary.
#
# Runtime therefore needs exactly `libssl3` + `ca-certificates` — verified with
# `ldd` against the release binary: libssl, libcrypto, libgcc, libm, libc, libz.
# No libsqlite3-0. Build deps drop to `pkg-config libssl-dev`; no clang, no
# nasm, no libav*.
#
# Build:
#   DOCKER_BUILDKIT=1 docker build \
#     --build-arg PICWEIGHT_VERSION=v1.2.3 \
#     --build-arg PICWEIGHT_VERSION_CODE=$(git rev-list --count HEAD) \
#     --secret id=keystore_password,env=KEYSTORE_PASSWORD \
#     -t picweight .
#
# PICWEIGHT_VERSION_CODE is the Android versionCode and MUST come from outside:
# .dockerignore excludes .git, so no stage in here can count commits itself. CI
# passes `git rev-list --count HEAD`, which is monotonic on master rather than
# only on tags — the in-app updater compares that number against what the running
# APK reports, so a value that does not increase means "up to date" forever.

ARG PICWEIGHT_VERSION=dev
ARG PICWEIGHT_VERSION_CODE=1

# ---------------------------------------------------------------------------
# Stage 1 — frontend (Vue 3 + Vite + Tailwind 4)
# ---------------------------------------------------------------------------
FROM node:25-slim AS frontend-builder
ARG PICWEIGHT_VERSION
WORKDIR /app/frontend
# package*.json first so the npm layer survives source-only changes.
COPY frontend/package*.json ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci
COPY frontend/ ./
# The web client's SDK, types and enum constants are generated from the API's
# own OpenAPI document, and `npm run build` regenerates them via its `prebuild`
# hook. That hook reads ../android/openapi.json, so the spec has to be in the
# context here — without it the build fails with ENOENT on
# /app/android/openapi.json. Copying it also means the frontend baked into an
# image always matches the spec in that same image, rather than trusting the
# committed output. (npx resolves the generators from node_modules, so this
# needs no network.)
COPY android/openapi.json /app/android/openapi.json
RUN PICWEIGHT_VERSION=${PICWEIGHT_VERSION} npm run build

# ---------------------------------------------------------------------------
# Stage 2a — chef base (cargo-chef + system deps)
#
# rust:1.96 rather than the PRD's 1.94: the local toolchain is 1.96 and
# `backend/Cargo.lock` is written by it, so an older image could refuse a
# lockfile version it does not understand (docs/rig-spike.md).
# ---------------------------------------------------------------------------
FROM rust:1.96 AS chef
RUN apt-get update && apt-get install --no-install-recommends -y \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked
WORKDIR /app/backend

# ---------------------------------------------------------------------------
# Stage 2b — dependency recipe
# ---------------------------------------------------------------------------
FROM chef AS planner
COPY backend/Cargo.toml backend/Cargo.lock backend/build.rs ./
COPY backend/migrations ./migrations
COPY backend/src ./src
COPY backend/tests ./tests
RUN cargo chef prepare --recipe-path recipe.json

# ---------------------------------------------------------------------------
# Stage 2c — cook dependencies, then run the test suite
#
# A red test fails the image (PRD §10). The full suite runs, not just `--lib`:
# PRD §13 puts the integration tests — ingest → loop → confirm against a temp
# SQLite file, a mock IdP and a mock LLM — inside the Docker build, and those
# live in `tests/`. They bind loopback only and need no network.
#
# The OpenAPI drift guard in that suite looks for `../android/openapi.json`,
# which is deliberately not copied here; it skips rather than fails, because
# stage 2e regenerates the spec from this very binary anyway.
# ---------------------------------------------------------------------------
FROM chef AS backend-test
COPY --from=planner /app/backend/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

ARG PICWEIGHT_VERSION
ENV PICWEIGHT_VERSION=${PICWEIGHT_VERSION}
COPY backend/Cargo.toml backend/Cargo.lock backend/build.rs ./
COPY backend/migrations ./migrations
COPY backend/src ./src
COPY backend/tests ./tests
RUN cargo test --release

# ---------------------------------------------------------------------------
# Stage 2d — release binary + OpenAPI spec
#
# Reuses the compilation from the test stage. `picweight-backend openapi` needs
# no configuration at all — no database, no OIDC, no API key — precisely so it
# can run here.
# ---------------------------------------------------------------------------
FROM backend-test AS backend-builder
RUN cargo build --release && \
    cp target/release/picweight-backend /usr/local/bin/picweight-backend && \
    /usr/local/bin/picweight-backend openapi /app/openapi.json

# ---------------------------------------------------------------------------
# Stage 2e — Android APK
#
# The Retrofit client is generated by `org.openapi.generator` from
# `android/openapi.json`. That file is committed (so a local Gradle build works
# offline) but here it is *overwritten* with the spec stage 2d exported from the
# binary this image ships. An APK whose API client disagrees with the server in
# the same image is then structurally impossible — the contract-drift guard PRD
# §13 asks for. The cost is that this stage waits on the backend build; the
# Gradle cache mount absorbs most of it.
#
# `cargo test` enforces the committed copy locally; regenerate it with
#   cd backend && cargo run -- openapi ../android/openapi.json
# ---------------------------------------------------------------------------
FROM eclipse-temurin:17-jdk AS android-builder
RUN apt-get update && apt-get install --no-install-recommends -y wget unzip \
    && rm -rf /var/lib/apt/lists/*
ENV ANDROID_HOME=/opt/android-sdk
RUN mkdir -p ${ANDROID_HOME}/cmdline-tools && \
    wget -q https://dl.google.com/android/repository/commandlinetools-linux-13114758_latest.zip -O /tmp/cmdline-tools.zip && \
    unzip -q /tmp/cmdline-tools.zip -d ${ANDROID_HOME}/cmdline-tools && \
    mv ${ANDROID_HOME}/cmdline-tools/cmdline-tools ${ANDROID_HOME}/cmdline-tools/latest && \
    rm /tmp/cmdline-tools.zip && \
    yes | ${ANDROID_HOME}/cmdline-tools/latest/bin/sdkmanager --licenses > /dev/null && \
    ${ANDROID_HOME}/cmdline-tools/latest/bin/sdkmanager --install \
        "platform-tools" "platforms;android-36" "build-tools;36.0.0" > /dev/null
WORKDIR /app/android
COPY android/ ./
COPY --from=backend-builder /app/openapi.json ./openapi.json
ARG PICWEIGHT_VERSION
ARG PICWEIGHT_VERSION_CODE
# Signed release when the keystore_password secret is provided, unsigned
# otherwise. Keystore logic ported from phos.
#
# versionName AND versionCode are passed to Gradle on EVERY build, not only for
# semver tags. The old "derive them from PICWEIGHT_VERSION when it looks like
# semver" rule fell through on a master push (PICWEIGHT_VERSION is "master"), so
# every master image shipped versionCode 1 / versionName 1.0.0 and the in-app
# updater compared 1 against 1 forever.
#
# versionCode is the commit count handed in by CI rather than anything derived
# from the tag: mixing the two schemes would let a tag (v1.0.0 -> 10000) outrank
# every subsequent master build, which is the same "never updates" failure with
# extra steps.
#
# The sidecar picweight.apk.json is written from the SAME two values given to
# Gradle, and then checked against what aapt2 reads back out of the APK, so what
# the server advertises at /api/v1/client/version cannot disagree with what the
# client will actually install. Its version_name is JSON-escaped because a git
# ref may legally contain a quote or a backslash, and unparseable metadata would
# take the update endpoint down with it.
RUN --mount=type=cache,target=/root/.gradle \
    --mount=type=secret,id=keystore_password \
    set -eu; \
    if [ -s /run/secrets/keystore_password ]; then \
        KEYSTORE_PASSWORD="$(cat /run/secrets/keystore_password)"; \
        export KEYSTORE_PASSWORD; \
    fi; \
    VERSION_NAME="${PICWEIGHT_VERSION:-dev}"; \
    case "$VERSION_NAME" in \
      v[0-9]*.[0-9]*.[0-9]*) VERSION_NAME="${VERSION_NAME#v}" ;; \
    esac; \
    VERSION_CODE="${PICWEIGHT_VERSION_CODE:-1}"; \
    case "$VERSION_CODE" in \
      ''|0|*[!0-9]*) \
        echo "PICWEIGHT_VERSION_CODE must be a positive integer, got '${VERSION_CODE}'" >&2; \
        exit 1 ;; \
    esac; \
    chmod +x gradlew; \
    ./gradlew --no-daemon assembleRelease \
        "-PversionName=${VERSION_NAME}" "-PversionCode=${VERSION_CODE}"; \
    cp app/build/outputs/apk/release/app-release*.apk /picweight.apk; \
    BADGING="$(${ANDROID_HOME}/build-tools/36.0.0/aapt2 dump badging /picweight.apk | head -1)"; \
    case "$BADGING" in \
      *"versionCode='${VERSION_CODE}'"*"versionName='${VERSION_NAME}'"*) ;; \
      *) echo "APK does not carry the requested version: ${BADGING}" >&2; exit 1 ;; \
    esac; \
    SHA256="$(sha256sum /picweight.apk | cut -d' ' -f1)"; \
    SIZE_BYTES="$(stat -c %s /picweight.apk)"; \
    NAME_JSON="$(printf '%s' "$VERSION_NAME" | sed 's/\\/\\\\/g; s/"/\\"/g')"; \
    printf '{"version_name":"%s","version_code":%s,"sha256":"%s","size_bytes":%s}\n' \
        "$NAME_JSON" "$VERSION_CODE" "$SHA256" "$SIZE_BYTES" > /picweight.apk.json; \
    cat /picweight.apk.json

# ---------------------------------------------------------------------------
# Stage 3 — runtime
# ---------------------------------------------------------------------------
FROM debian:trixie-slim
RUN apt-get update && apt-get install --no-install-recommends -y \
    libssl3 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -g 1000 picweight && useradd -u 1000 -g picweight -m picweight

WORKDIR /app

# Backend binary
COPY --from=backend-builder /usr/local/bin/picweight-backend ./picweight-backend

# Built SPA — served by the static fallback under every non-/api path
COPY --from=frontend-builder /app/frontend/dist ./static

# APK, served at /picweight.apk and linked from the web UI's download card.
#
# The sidecar next to it is what GET /api/v1/client/version reads: the app compares
# its own BuildConfig.VERSION_CODE against version_code, and verifies the download
# against sha256 before installing it. It has to travel with the APK — metadata
# describing a *different* build is worse than none, because the client would
# reject every download as corrupt.
COPY --from=android-builder /picweight.apk ./static/picweight.apk
COPY --from=android-builder /picweight.apk.json ./static/picweight.apk.json

# SQLite database + thumbs/ live here; the Helm chart mounts the PVC over it.
RUN mkdir -p data/thumbs && chown -R picweight:picweight /app

EXPOSE 33100
ENV PICWEIGHT_STATIC_DIR=/app/static \
    PICWEIGHT_DATA_PATH=/app/data
USER 1000
CMD ["./picweight-backend"]
