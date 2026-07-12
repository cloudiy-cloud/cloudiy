#!/usr/bin/env bash
#
# build-workers.sh — build & push the Cloudiy GPU worker images to GHCR.
#
# Images produced:
#   ghcr.io/cloudiy/worker-sdxl:latest   (+ :<git-sha>)  GPU  — image endpoints
#   ghcr.io/cloudiy/worker-ltx:latest    (+ :<git-sha>)  GPU  — video endpoints
#   ghcr.io/cloudiy/worker-tts:latest    (+ :<git-sha>)  CPU  — chatterbox (TTS)
#   ghcr.io/cloudiy/worker-audio:latest  (+ :<git-sha>)  CPU  — stable-audio
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

DIGESTS=""
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
  # Capture the pushed digest so it can be pinned in crates/cloudiy/worker_digests.json.
  local dig
  dig=$(docker buildx imagetools inspect "${REGISTRY}/worker-${name}:latest" \
        --format '{{.Manifest.Digest}}' 2>/dev/null || true)
  [ -n "$dig" ] && DIGESTS="${DIGESTS}  \"${REGISTRY}/worker-${name}:latest\": \"${dig}\",
"
}

build_and_push sdxl
build_and_push ltx
build_and_push tts
build_and_push audio

echo ""
echo "Done. Pushed:"
echo "  ${REGISTRY}/worker-sdxl:latest   (+ :${SHA})"
echo "  ${REGISTRY}/worker-ltx:latest    (+ :${SHA})"
echo "  ${REGISTRY}/worker-tts:latest    (+ :${SHA})"
echo "  ${REGISTRY}/worker-audio:latest  (+ :${SHA})"
echo ""
echo "Pin these digests in crates/cloudiy/worker_digests.json (then rebuild the"
echo "node binary so installs pull by digest and verify):"
echo ""
printf '%s' "$DIGESTS"
