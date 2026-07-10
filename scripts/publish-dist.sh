#!/bin/sh
# Republish a private release's prebuilt binaries to the PUBLIC distribution
# repo, so `install.sh` (which points at the public repo) can fetch them
# anonymously. Uses your local `gh` auth — no CI secret / PAT required.
#
#   scripts/publish-dist.sh v0.1.0
#
# Prereq: the Release workflow already ran for the tag on the private repo and
# attached the cloudiy-<target>.{tar.gz,zip}(.sha256) assets.
set -eu

TAG="${1:-}"
SRC="w3-surfer/cloudiy"        # private source repo (holds the built assets)
DST="w3-surfer/cloudiy-dist"   # public distribution repo (installers point here)

[ -n "$TAG" ] || { echo "usage: $0 <tag>   (e.g. v0.1.0)" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "→ downloading $TAG assets from $SRC"
gh release download "$TAG" --repo "$SRC" --dir "$tmp"

echo "→ publishing $TAG to $DST"
if gh release view "$TAG" --repo "$DST" >/dev/null 2>&1; then
  # Release already exists — just (re)upload assets, clobbering same-named ones.
  gh release upload "$TAG" --repo "$DST" --clobber "$tmp"/*
else
  gh release create "$TAG" --repo "$DST" \
    --title "Cloudiy $TAG" \
    --notes "Prebuilt provider-node binaries. Install: curl -fsSL https://cloudiy-cloud.vercel.app/install.sh | sh" \
    "$tmp"/*
fi

echo "✓ done — https://github.com/$DST/releases/tag/$TAG"
