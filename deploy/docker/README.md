# Container image — `ghcr.io/w3-surfer/cloudiy`

The multi-arch (amd64 + arm64) image that runs the unified `cloudiy` node in a
container. It's what [`integrations/ods/compose.yaml`](../../integrations/ods/compose.yaml)
and [`deploy/directory/docker-compose.yml`](../directory/docker-compose.yml)
pull.

## How it's built

The image is **published by the release pipeline**, not by hand:
[`.github/workflows/release.yml`](../../.github/workflows/release.yml) → the
`image` job. On a `v*` tag it builds both arches and pushes
`ghcr.io/<owner>/cloudiy:<tag>` and `:latest`. It **reuses the release
binaries** the same workflow already built (the glibc `*-unknown-linux-gnu`
ones) — no second Rust compile — so the image ships the exact bytes as the
published tarballs.

Publishing only happens **on a tag** (an external, irreversible action, like the
binaries and crates). A manual `workflow_dispatch` builds amd64 to validate the
Dockerfile but pushes nothing.

## What's inside

- The `cloudiy` release binary at `/usr/local/bin/cloudiy` (ENTRYPOINT).
- The **static `docker` CLI** (client only, no daemon). A provider node launches
  each tenant workload as a *sibling* container on the host's Docker daemon — the
  compose files mount `/var/run/docker.sock` — and the node shells out to
  `docker` to do it. Without the client, a containerized node can accept jobs but
  can't run them.
- `ca-certificates` for TLS to relays / the Solana RPC.

The node key + config live in a volume at `/root/.config/cloudiy`; persist it or
the Node ID (the address consumers dial, and the result-signing key) changes on
every restart.

## Running it

The image picks the mode from the command; the compose files set it:

```bash
# provider (share CPU/GPU), the ODS model — needs the docker socket to run jobs:
docker run --rm -v /var/run/docker.sock:/var/run/docker.sock \
  -v cloudiy-config:/root/.config/cloudiy \
  ghcr.io/w3-surfer/cloudiy:latest share --no-http

# directory / discovery node (pure P2P, no ports):
docker run --rm -v cloudiy-config:/root/.config/cloudiy \
  ghcr.io/w3-surfer/cloudiy:latest directory

# print the node identity (also the compose healthcheck):
docker run --rm -v cloudiy-config:/root/.config/cloudiy \
  ghcr.io/w3-surfer/cloudiy:latest id
```

## Local build (optional)

You normally don't build this by hand, but you can. Stage the two release
binaries into a context named by docker's arch, then buildx:

```bash
mkdir ctx
cp path/to/cloudiy-linux-amd64 ctx/cloudiy-amd64
cp path/to/cloudiy-linux-arm64 ctx/cloudiy-arm64
cp deploy/docker/cloudiy.Dockerfile ctx/Dockerfile
docker buildx build ctx --platform linux/amd64,linux/arm64 -t cloudiy:dev
```

## Worker images are separate

The model-worker images (`worker-sdxl`, `worker-ltx`, `worker-tts`,
`worker-audio`) are built by
[`.github/workflows/publish-workers.yml`](../../.github/workflows/publish-workers.yml)
from [`workers/`](../../workers), signed with cosign, and their digests pinned
into `crates/cloudiy/worker_digests.json`. This image is the node itself, not a
worker.
