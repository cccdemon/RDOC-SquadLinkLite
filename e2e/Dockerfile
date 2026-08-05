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
