#!/bin/sh
# Cloudiy provider node installer (macOS / Linux). No Rust, no compiler.
#   curl -fsSL https://cloudiy-cloud.vercel.app/install.sh | sh
#
# Downloads the prebuilt `cloudiy` binary for your OS/arch from the latest
# GitHub Release and drops it in ~/.local/bin (override with CLOUDIY_INSTALL_DIR).
set -eu

REPO="w3-surfer/cloudiy"
BIN="cloudiy"
DEST="${CLOUDIY_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64|amd64)   target="x86_64-unknown-linux-gnu" ;;
      aarch64|arm64)  target="aarch64-unknown-linux-gnu" ;;
      *) echo "cloudiy: unsupported architecture '$arch' on Linux" >&2; exit 1 ;;
    esac ;;
  Darwin)
    case "$arch" in
      arm64)   target="aarch64-apple-darwin" ;;
      x86_64)  target="x86_64-apple-darwin" ;;
      *) echo "cloudiy: unsupported architecture '$arch' on macOS" >&2; exit 1 ;;
    esac ;;
  *)
    echo "cloudiy: unsupported OS '$os'. On Windows use the PowerShell installer:" >&2
    echo "  irm https://cloudiy-cloud.vercel.app/install.ps1 | iex" >&2
    exit 1 ;;
esac

url="https://github.com/${REPO}/releases/latest/download/${BIN}-${target}.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "cloudiy: downloading ${target}…"
if ! curl -fsSL "$url" -o "$tmp/pkg.tar.gz"; then
  echo "cloudiy: download failed ($url)." >&2
  echo "cloudiy: no release yet for this platform? See github.com/${REPO}/releases" >&2
  exit 1
fi

tar -xzf "$tmp/pkg.tar.gz" -C "$tmp"
binpath="$(find "$tmp" -type f -name "$BIN" | head -n1)"
[ -n "$binpath" ] || { echo "cloudiy: binary not found in archive" >&2; exit 1; }

mkdir -p "$DEST"
install -m 0755 "$binpath" "$DEST/$BIN" 2>/dev/null || { cp "$binpath" "$DEST/$BIN"; chmod 0755 "$DEST/$BIN"; }

echo "cloudiy: installed to $DEST/$BIN"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "cloudiy: add it to your PATH:  export PATH=\"$DEST:\$PATH\"" ;;
esac
echo "cloudiy: get started with  $BIN --help"
