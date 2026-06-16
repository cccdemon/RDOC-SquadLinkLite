# InitConnection signaling server. Build context = repo root.
# Pin fixed versions so a deploy is reproducible and not silently moved by an
# upstream `:1` / `:bookworm-slim` tag. For full integrity, append the manifest
# digest too, e.g. `FROM rust:1.83.0-bookworm@sha256:<digest> AS build`.
FROM rust:1.83.0-bookworm AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p init-connection

FROM debian:12-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/init-connection /usr/local/bin/init-connection
EXPOSE 8080
ENTRYPOINT ["init-connection"]
