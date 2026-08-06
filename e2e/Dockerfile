# End-to-end image: the headless engine + the InitConnection server, built for
# Linux so the whole mesh can run under Docker Desktop on any dev machine.
# Context is the REPO ROOT (docker build -f e2e/Dockerfile .).
FROM rust:1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config clang cmake libopus-dev libasound2-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p companion-core -p init-connection

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2 libopus0 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# The engine refuses to start without an audio device (rate handshake in
# setup_audio). Containers have none — give ALSA a null default: capture reads
# silence, playback discards. The mesh, crypto and signaling paths are real.
RUN printf 'pcm.!default { type null }\nctl.!default { type null }\n' > /etc/asound.conf
COPY --from=build /src/target/release/companion-core /usr/local/bin/subraum-headless
COPY --from=build /src/target/release/init-connection /usr/local/bin/init-connection
CMD ["subraum-headless"]

# ── Linux DESKTOP client (the real Tauri app) ────────────────────────────────
# Frontend: tsc + vite, exactly what `pnpm build` produces for the webview.
FROM node:20-bookworm-slim AS webbuild
# corepack would fetch pnpm 11, which needs newer Node builtins than 20 has.
RUN npm install -g pnpm@10
WORKDIR /web
COPY apps/companion/package.json apps/companion/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY apps/companion/index.html apps/companion/vite.config.ts apps/companion/tsconfig.json ./
COPY apps/companion/src ./src
RUN pnpm build

# Tauri shell against webkit2gtk — the same stack the .deb/.rpm/AppImage use.
FROM rust:1-bookworm AS desktopbuild
RUN apt-get update && apt-get install -y --no-install-recommends \
    libwebkit2gtk-4.1-dev libgtk-3-0 libgtk-3-dev librsvg2-dev libxdo-dev \
    libssl-dev libasound2-dev libopus-dev clang cmake pkg-config file \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
COPY --from=webbuild /web/dist apps/companion/dist
# `custom-protocol` is what `tauri build` would enable: serve the EMBEDDED
# dist instead of the dev-server URL. A plain cargo build without it produces a
# binary that tries to reach localhost:1420 and shows a blank error page.
RUN cd apps/companion/src-tauri && cargo build --release --features tauri/custom-protocol

# Runtime: the deb's declared deps + Xvfb so the GUI can boot with no display,
# dbus for webkit, python3-websockets to probe the local control API.
FROM debian:bookworm-slim AS desktop
RUN apt-get update && apt-get install -y --no-install-recommends \
    libwebkit2gtk-4.1-0 libgtk-3-0 libasound2 libopus0 ca-certificates \
    xvfb xauth dbus dbus-x11 python3 python3-websockets \
    && rm -rf /var/lib/apt/lists/*
RUN printf 'pcm.!default { type null }\nctl.!default { type null }\n' > /etc/asound.conf
COPY --from=desktopbuild /src/apps/companion/src-tauri/target/release/subraum /usr/local/bin/subraum
COPY e2e/probe-control.py /usr/local/bin/probe-control.py
# WEBKIT_DISABLE_* are set by the app itself (blank-window workaround).
CMD ["dbus-run-session", "xvfb-run", "-a", "/usr/local/bin/subraum"]
