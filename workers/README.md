# Cloudiy GPU Workers

Two container images provide the actual GPU compute the Cloudiy gateway drives:

| Image | Port | API | Purpose |
|-------|------|-----|---------|
| `ghcr.io/cloudiy/worker-sdxl:latest` | 7860 | `/sdapi/v1/txt2img`, `/sdapi/v1/img2img` | Stable Diffusion image generation (AUTOMATIC1111 webui, API-only) |
| `ghcr.io/cloudiy/worker-ltx:latest` | 7861 | `POST /generate` → writes `<id>.mp4` to `/out` | LTX-Video text-to-video |

Both are **GPU-only**: they require a Linux host with an NVIDIA GPU and the
NVIDIA Container Toolkit, and must be run with `--gpus all`. They will not run on
macOS or on a CPU-only host.

## Directory layout

```
workers/
├── sdxl/
│   ├── Dockerfile     # CUDA base + AUTOMATIC1111 webui (--api --listen --port 7860 --nowebui)
│   └── launch.sh      # optional checkpoint download, then boots the API server
└── ltx/
    ├── Dockerfile     # CUDA base + diffusers LTXPipeline + FastAPI wrapper
    └── server.py      # FastAPI server on :7861, writes /out/<id>.mp4
```

## Building & publishing (HUMAN steps)

Publishing images to GHCR and running them on GPU hardware are **human steps** — they
require GPU hardware, a registry you control, and moving large images around.

Two ways to build & push:

1. **Locally** with the helper script (requires `docker login ghcr.io` first):
   ```bash
   export GHCR_PAT=<github PAT with write:packages>
   echo "$GHCR_PAT" | docker login ghcr.io -u <github-username> --password-stdin
   ./scripts/build-workers.sh
   ```

2. **Via CI** — push a tag matching `worker-v*` and `.github/workflows/workers.yml`
   builds and pushes both images using the repo's `GITHUB_TOKEN`:
   ```bash
   git tag worker-v0.1.0
   git push origin worker-v0.1.0
   ```

## Testing locally (on a Linux + NVIDIA host)

```bash
# Image worker
docker run --gpus all -p 7860:7860 ghcr.io/cloudiy/worker-sdxl:latest
# then:  curl -X POST localhost:7860/sdapi/v1/txt2img -d '{"prompt":"a cat","steps":20}'

# Video worker (mount an /out volume so you can retrieve the mp4)
docker run --gpus all -p 7861:7861 -v "$PWD/out:/out" ghcr.io/cloudiy/worker-ltx:latest
# then:  curl -X POST localhost:7861/generate -d '{"prompt":"a rocket launch"}'
```

## Model weights

- **sdxl**: no checkpoint is baked in. Mount one at
  `/app/models/Stable-diffusion` or set `DOWNLOAD_MODEL_URL` to a `.safetensors` URL
  (fetched at boot). Recommended: SDXL base 1.0.
- **ltx**: weights (`Lightricks/LTX-Video`) are pulled from HuggingFace on first
  request. Mount a volume at `/root/.cache/huggingface` to persist the cache.
