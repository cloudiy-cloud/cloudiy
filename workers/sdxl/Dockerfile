# worker-sdxl: Stable Diffusion image worker for Cloudiy
#
# Exposes an AUTOMATIC1111 stable-diffusion-webui API-only server on 0.0.0.0:7860.
# The Cloudiy gateway calls:
#   POST /sdapi/v1/txt2img
#   POST /sdapi/v1/img2img
#
# GPU-ONLY: must be run on a Linux + NVIDIA host with `--gpus all`.
#
# MODEL WEIGHTS
# -------------
# No checkpoint is baked into this image (SDXL base is ~6.9GB and license/redistribution
# terms vary). At container start, download a checkpoint into /app/models/Stable-diffusion.
# Options:
#   - Mount a host directory:  -v /path/to/models:/app/models/Stable-diffusion
#   - Or set DOWNLOAD_MODEL_URL to a direct .safetensors URL (fetched at boot by launch.sh).
# Recommended: SDXL base 1.0 (stabilityai/stable-diffusion-xl-base-1.0) from HuggingFace.

FROM nvidia/cuda:12.1.1-runtime-ubuntu22.04

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    COMMANDLINE_ARGS="--api --listen --port 7860 --nowebui --skip-torch-cuda-test --no-half-vae"

RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-venv python3-pip git wget curl libgl1 libglib2.0-0 \
      google-perftools ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Clone AUTOMATIC1111 stable-diffusion-webui (pinned to a release tag for reproducibility).
RUN git clone --depth 1 --branch v1.10.1 \
      https://github.com/AUTOMATIC1111/stable-diffusion-webui.git /app

# Pre-install torch with CUDA wheels so the first boot is fast.
RUN pip3 install --upgrade pip \
    && pip3 install torch==2.1.2 torchvision==0.16.2 \
       --index-url https://download.pytorch.org/whl/cu121

COPY launch.sh /app/launch.sh
RUN chmod +x /app/launch.sh

EXPOSE 7860

# webui's own launch.py resolves the remaining Python deps into a venv on first run.
ENTRYPOINT ["/app/launch.sh"]
