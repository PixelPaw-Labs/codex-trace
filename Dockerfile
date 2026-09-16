# syntax=docker/dockerfile:1.7

# =============================================================================
# Codex Trace — Docker image
#
# Runs the Rust/axum backend in headless mode behind a virtual X display.
# The React frontend is built to a static bundle and served from the same
# axum process, so the whole app is reachable on a single port.
#
# Build:
#   docker build -t codex-trace .
#
# Run (mount your Codex session data read-only):
#   docker run --rm -p 1422:1422 \
#     -v "$HOME/.codex/sessions:/home/app/.codex/sessions:ro" \
#     codex-trace
#
# Then open http://localhost:1422 in a browser.
#
# Configurable env vars:
#   CODEXTRACE_HTTP_HOST   bind host    (default: 0.0.0.0 in this image)
#   CODEXTRACE_HTTP_PORT   bind port    (default: 1422 in this image)
#   CODEXTRACE_STATIC_DIR  static dist  (default: /app/dist in this image)
# =============================================================================

ARG RUST_IMAGE=rust:latest
ARG NODE_VERSION=24
ARG DEBIAN_CODENAME=bookworm

# -----------------------------------------------------------------------------
# Stage 1 — build the React frontend
# -----------------------------------------------------------------------------
FROM node:${NODE_VERSION}-${DEBIAN_CODENAME}-slim AS frontend-builder

WORKDIR /build

COPY package.json package-lock.json ./
RUN npm ci --no-audit --no-fund

COPY tsconfig.json tsconfig.node.json vite.config.ts index.html ./
COPY bin ./bin
COPY src ./src
COPY shared ./shared

ENV VITE_API_BASE=""
RUN npm run build

# -----------------------------------------------------------------------------
# Stage 2 — build the Rust backend
# -----------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS backend-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libwebkit2gtk-4.1-dev \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        libxdo-dev \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY src-tauri ./src-tauri

WORKDIR /build/src-tauri
# Fat LTO (lto=true in Cargo.toml) loads all program bitcode at once and OOMs
# in memory-constrained Docker builds. Thin LTO delivers most of the same
# optimisation while keeping peak RSS under control.
RUN CARGO_PROFILE_RELEASE_LTO=thin cargo build --release --locked --jobs 2 --bin codex-trace

WORKDIR /build
COPY --from=frontend-builder /build/dist ./dist

# -----------------------------------------------------------------------------
# Stage 3 — runtime image
# -----------------------------------------------------------------------------
FROM debian:trixie-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-0 \
        libayatana-appindicator3-1 \
        librsvg2-2 \
        libxdo3 \
        xvfb \
        xauth \
        dumb-init \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --home-dir /home/app --shell /bin/bash --uid 1000 app

WORKDIR /app

COPY --from=backend-builder /build/src-tauri/target/release/codex-trace /usr/local/bin/codex-trace
COPY --from=frontend-builder /build/dist /app/dist
COPY script/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

ENV CODEXTRACE_HTTP_HOST=0.0.0.0 \
    CODEXTRACE_HTTP_PORT=1422 \
    CODEXTRACE_STATIC_DIR=/app/dist \
    XDG_CONFIG_HOME=/home/app/.config \
    XDG_DATA_HOME=/home/app/.local/share

USER app

VOLUME ["/home/app/.codex/sessions"]

EXPOSE 1422

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD /bin/sh -c 'exec 3<>/dev/tcp/127.0.0.1/${CODEXTRACE_HTTP_PORT:-1422}' || exit 1

ENTRYPOINT ["dumb-init", "--", "/usr/local/bin/docker-entrypoint.sh"]
CMD ["codex-trace", "--headless"]
