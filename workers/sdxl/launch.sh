#!/usr/bin/env bash
# Boot script for worker-sdxl.
# Optionally fetches a checkpoint, then launches the API-only webui server.
set -euo pipefail

MODEL_DIR="/app/models/Stable-diffusion"
mkdir -p "$MODEL_DIR"

# If DOWNLOAD_MODEL_URL is set and no checkpoint is present, fetch one at boot.
if [ -n "${DOWNLOAD_MODEL_URL:-}" ] && [ -z "$(ls -A "$MODEL_DIR" 2>/dev/null)" ]; then
  echo "[worker-sdxl] downloading checkpoint from DOWNLOAD_MODEL_URL ..."
  wget -q --show-progress -O "$MODEL_DIR/model.safetensors" "$DOWNLOAD_MODEL_URL"
fi

if [ -z "$(ls -A "$MODEL_DIR" 2>/dev/null)" ]; then
  echo "[worker-sdxl] WARNING: no checkpoint in $MODEL_DIR."
  echo "[worker-sdxl] Mount one with -v or set DOWNLOAD_MODEL_URL. Starting anyway."
fi

# COMMANDLINE_ARGS is honoured by launch.py: --api --listen --port 7860 --nowebui
exec python3 launch.py
