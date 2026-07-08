# worker-ltx: text-to-video worker for Cloudiy
#
# Wraps Lightricks/LTX-Video in a small FastAPI service on 0.0.0.0:7861.
# The Cloudiy gateway calls POST /generate and reads the resulting .mp4 from /out.
#
# GPU-ONLY: run on a Linux + NVIDIA host with `--gpus all` and mount an /out volume.
#
# MODEL WEIGHTS
# -------------
# LTX-Video weights are downloaded from HuggingFace (Lightricks/LTX-Video) on first
# request and cached under HF_HOME. Mount a volume at /root/.cache/huggingface to
# persist the cache across container restarts.

FROM nvidia/cuda:12.1.1-runtime-ubuntu22.04

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    HF_HOME=/root/.cache/huggingface \
    OUT_DIR=/out

RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip git ffmpeg libgl1 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

RUN pip3 install --upgrade pip \
    && pip3 install torch==2.4.0 torchvision==0.19.0 \
       --index-url https://download.pytorch.org/whl/cu121

# LTX-Video is supported by diffusers via LTXPipeline; imageio provides mp4 export.
RUN pip3 install \
      "diffusers>=0.31.0" \
      transformers==4.44.2 \
      accelerate==0.34.2 \
      sentencepiece==0.2.0 \
      "imageio[ffmpeg]==2.35.1" \
      fastapi==0.115.0 \
      "uvicorn[standard]==0.30.6" \
      pydantic==2.9.2

COPY server.py /app/server.py

RUN mkdir -p /out

EXPOSE 7861

CMD ["python3", "server.py"]
