#!/usr/bin/env bash
#
# build-workers.sh — build & push the Cloudiy GPU worker images to GHCR.
#
# Images produced:
#   ghcr.io/cloudiy/worker-sdxl:latest  (+ :<git-sha>)
#   ghcr.io/cloudiy/worker-ltx:latest   (+ :<git-sha>)
#
# PREREQUISITES (human steps):
#   1. Authenticate to GHCR before running:
#         export GHCR_PAT=<a github PAT with write:packages>
#         echo "$GHCR_PAT" | docker login ghcr.io -u <your-github-username> --password-stdin
#   2. Have docker buildx available (docker >= 20.10 with the buildx plugin).
#   3. Run from the repo root so the workers/ paths resolve.
#
# NOTE: these are large GPU images built for linux/amd64. Building on Apple Silicon
# will emulate amd64 and be slow — building on a Linux/amd64 host is recommended.

set -euo pipefail

cd "$(dirname "$0")/.."

REGISTRY="ghcr.io/cloudiy"
SHA="$(git rev-parse --short HEAD)"

if ! docker buildx version >/dev/null 2>&1; then
  echo "ERROR: docker buildx is not available. Install the buildx plugin first." >&2
  exit 1
fi

# Warn if the caller has not logged in. We can't fully verify auth, but GHCR_PAT is a hint.
if [ -z "${GHCR_PAT:-}" ]; then
  echo "WARNING: GHCR_PAT is not set. Make sure you have run 'docker login ghcr.io' already."
  echo "         (see the header of this script for the exact command)"
fi

build_and_push() {
  local name="$1"
  local ctx="workers/${name}"
  echo ""
  echo "==> building ${REGISTRY}/worker-${name}  (latest + ${SHA})"
  docker buildx build \
    --platform linux/amd64 \
    -t "${REGISTRY}/worker-${name}:latest" \
    -t "${REGISTRY}/worker-${name}:${SHA}" \
    --push \
    "${ctx}"
}

build_and_push sdxl
build_and_push ltx

echo ""
echo "Done. Pushed:"
echo "  ${REGISTRY}/worker-sdxl:latest  (+ :${SHA})"
echo "  ${REGISTRY}/worker-ltx:latest   (+ :${SHA})"
