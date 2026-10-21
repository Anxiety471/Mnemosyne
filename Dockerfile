# Multi-stage Rust + React build, with a small runtime.
FROM node:22-alpine AS ui
WORKDIR /src/ui
COPY ui/package*.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release --locked

FROM debian:bookworm-slim
# bash is here for the HEALTHCHECK: /dev/tcp gives a dependency-free TCP probe.
RUN apt-get update && apt-get install -y --no-install-recommends tar bash \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 -m drawer \
    && mkdir -p /data /usr/share/drawer/ui /backups \
    && chown -R drawer:drawer /data /backups
COPY --from=builder /src/target/release/drawer /usr/local/bin/drawer
COPY --from=ui /src/ui/dist/ /usr/share/drawer/ui/
# 0.0.0.0 is required here: published ports reach the container from outside its
# network namespace, so a loopback bind would be unreachable. Compose keeps the port
# unpublished on the host by default (see compose.yaml); a bare `docker run` exposes it.
ENV DRAWER_ADDR=0.0.0.0:8787
ENV DRAWER_UI_DIR=/usr/share/drawer/ui
ENV DRAWER_DATA_DIR=/data
USER drawer
EXPOSE 8787
VOLUME /data
# The store lives under /data and /backups only, and /tmp is a tmpfs in compose, so the
# root filesystem can stay read-only. The probe is a plain TCP connect to the port the
# server binds: no curl, no extra HTTP client, and it still fails when the process is
# deadlocked rather than just slow to answer a request.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8787 || exit 1"]
LABEL org.opencontainers.image.title="Drawer" \
      org.opencontainers.image.description="Versioned file storage for agents, modelled on Google Drive" \
      org.opencontainers.image.source="https://github.com/Anxiety471/drawer" \
      org.opencontainers.image.licenses="MIT"
CMD ["drawer", "serve"]
