# Cloudiy GPU Workers

Two container images provide the actual GPU compute the Cloudiy gateway drives:

| Image | Port | API | Purpose |
|-------|------|-----|---------|
| `ghcr.io/cloudiy/worker-sdxl:latest` | 7860 | `/sdapi/v1/txt2img`, `/sdapi/v1/img2img` | Stable Diffusion image generation (AUTOMATIC1111 webui, API-only) |
| `ghcr.io/cloudiy/worker-ltx:latest` | 7861 | `POST /generate` → writes `<id>.mp4` to `/out` | LTX-Video text-to-video |
| `ghcr.io/cloudiy/worker-audio:latest` | — | (text-to-audio) | **Not built yet** — mapped in `gateway::audio_worker_for`; text-to-audio (music/SFX). |
| `ghcr.io/cloudiy/worker-tts:latest` | — | (text-to-speech) | **Not built yet** — TTS for `chatterbox`. A CPU option is Piper. |
| `ghcr.io/cloudiy/worker-whisper:latest` | — | (speech-to-text) | **Not built yet** — `whisper-ep` needs an audio-file input, so the prompt playground can't drive it; use the API with an audio input. |

Image and audio endpoints are GPU/worker-gated and report honestly (`"needs"` in
the JSON) until the image exists on the serving node; text (`llama-ep`, via a
CPU Ollama worker) runs today.

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

## Going live end-to-end (what's code vs. what's yours to run)

The software path is now wired; the remaining steps need your infra:

1. **Publish the worker images** (human): build & push `worker-sdxl`, `worker-ltx`
   (and, when built, the audio/tts/whisper images) to a registry the serving node
   can pull. See "Building & publishing" above.
2. **Run a serving node** (human): on a Linux + NVIDIA host,
   `cloudiy share --require-payment --rpc-url <solana-rpc>`. It serves model
   endpoints over iroh and **admits each run through the x402/escrow gate** — an
   endpoint run is now paid and signed exactly like a kernel job
   (`core::run_endpoint_guarded` → `authorize` → `serve_endpoint` → `signed_response`).
3. **Route runs to that node**: the gateway's `POST /api/endpoint` accepts an
   optional `to` (the provider's EndpointId) and `payment`; with `to` set it
   dispatches `proto::Request::RunEndpoint` over iroh to that provider (payment
   enforced there) instead of running on the gateway host. Without `to`, the
   model runs on the local gateway (free, for solo/dev use).
4. **Point the browser at a gateway**: the UI reads `?gw=<url>` (persisted) or
   defaults to the local gateway. **Do not** expose the gateway itself to the
   public internet — it holds the machine identity and is deliberately
   loopback-only (`guard_local_origin`, anti-CSRF/rebinding). Public reach is:
   browser → local `cloudiy os` (loopback) → iroh → remote provider (step 3),
   not a CORS-opened gateway.
