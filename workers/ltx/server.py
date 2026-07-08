"""
worker-ltx HTTP server for Cloudiy.

A tiny FastAPI wrapper around Lightricks/LTX-Video. Listens on 0.0.0.0:7861 and
generates a short text-to-video clip per request, writing the result into /out.

Endpoints
---------
GET  /health                 -> {"status": "ok"}
POST /generate               -> {"media": "<id>.mp4"}
    body: {"prompt": "...", "num_frames": 121, "fps": 24, "seed": 42}

The gateway reads the generated file from the shared /out volume.

GPU-ONLY: requires an NVIDIA CUDA device. Model weights (Lightricks/LTX-Video) are
pulled from HuggingFace on first request and cached under HF_HOME (mount a volume to
persist the cache across restarts).
"""
import os
import uuid
import logging

from fastapi import FastAPI
from pydantic import BaseModel

logging.basicConfig(level=logging.INFO)
log = logging.getLogger("worker-ltx")

OUT_DIR = os.environ.get("OUT_DIR", "/out")
MODEL_ID = os.environ.get("LTX_MODEL_ID", "Lightricks/LTX-Video")
os.makedirs(OUT_DIR, exist_ok=True)

app = FastAPI(title="cloudiy-worker-ltx")

# Lazily loaded so the container can start (and answer /health) before weights download.
_pipeline = None


def get_pipeline():
    global _pipeline
    if _pipeline is None:
        import torch
        from diffusers import LTXPipeline

        log.info("loading LTX-Video pipeline: %s", MODEL_ID)
        _pipeline = LTXPipeline.from_pretrained(MODEL_ID, torch_dtype=torch.bfloat16)
        _pipeline.to("cuda")
    return _pipeline


class GenerateRequest(BaseModel):
    prompt: str
    negative_prompt: str = "worst quality, blurry, distorted"
    num_frames: int = 121
    fps: int = 24
    width: int = 704
    height: int = 480
    seed: int | None = None


@app.get("/health")
def health():
    return {"status": "ok"}


@app.post("/generate")
def generate(req: GenerateRequest):
    import torch
    from diffusers.utils import export_to_video

    pipe = get_pipeline()
    generator = None
    if req.seed is not None:
        generator = torch.Generator(device="cuda").manual_seed(req.seed)

    log.info("generating clip for prompt: %s", req.prompt[:80])
    result = pipe(
        prompt=req.prompt,
        negative_prompt=req.negative_prompt,
        width=req.width,
        height=req.height,
        num_frames=req.num_frames,
        generator=generator,
    )
    frames = result.frames[0]

    media_id = uuid.uuid4().hex
    filename = f"{media_id}.mp4"
    out_path = os.path.join(OUT_DIR, filename)
    export_to_video(frames, out_path, fps=req.fps)
    log.info("wrote %s", out_path)

    return {"media": filename}


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host="0.0.0.0", port=7861)
