# syntax=docker/dockerfile:1
#
# Runtime image for the Cloudiy node — ghcr.io/w3-surfer/cloudiy.
#
# It packages the RELEASE binary already built by the release matrix (a glibc
# / `*-unknown-linux-gnu` build), NOT a from-source compile: the image ships
# the exact bytes as the published tarballs, and the build stays cheap (no Rust
# toolchain, no wgpu/iroh compile in Docker). The release job stages the
# per-arch binaries into the build context as `cloudiy-amd64` / `cloudiy-arm64`;
# `TARGETARCH` (amd64|arm64) selects the right one for each platform of the
# multi-arch manifest.
#
# Why the docker CLI is here: a provider node launches each tenant workload as a
# *sibling* container on the host's Docker daemon (the compose files mount
# /var/run/docker.sock), and the node shells out to the `docker` client to do
# it (`crates/runtime::DockerRuntime`, default binary "docker"). Without the
# client in the image, a containerized node can accept jobs but can't run them.
# Only the client is installed — no daemon.

FROM debian:stable-slim

ARG TARGETARCH
# Pin the static docker CLI. Bump deliberately; it is a supply-chain input.
ARG DOCKER_CLI_VERSION=27.3.1

# ca-certificates: rustls verifies TLS to relays/RPC against the system roots.
# curl: only to fetch the static docker client, then removed.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    case "$TARGETARCH" in \
      amd64) dockerarch=x86_64 ;; \
      arm64) dockerarch=aarch64 ;; \
      *) echo "unsupported TARGETARCH: '$TARGETARCH'" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://download.docker.com/linux/static/stable/${dockerarch}/docker-${DOCKER_CLI_VERSION}.tgz" \
      | tar -xz -C /usr/local/bin --strip-components=1 docker/docker; \
    docker --version; \
    apt-get purge -y curl; apt-get autoremove -y; rm -rf /var/lib/apt/lists/*

# The release binary for this platform (staged by the release job).
COPY cloudiy-${TARGETARCH} /usr/local/bin/cloudiy
# chmod + a load-time smoke: proves the glibc binary runs in this base, per arch.
RUN chmod +x /usr/local/bin/cloudiy && cloudiy --version

# The node identity + config lives here; compose files persist it as a volume so
# the Node ID (the address consumers dial, and the result-signing key) survives
# restarts. Regenerating it loses reputation and any in-flight escrow.
VOLUME ["/root/.config/cloudiy"]
ENV RUST_LOG=info

# Callers pick the mode: `share` (provider), `directory`, `os` (gateway), `id`
# (the compose healthcheck). A bare run prints usage rather than doing anything
# network-facing by default.
ENTRYPOINT ["cloudiy"]
CMD ["--help"]
