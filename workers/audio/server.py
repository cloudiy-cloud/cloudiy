"""Cloudiy audio worker — MusicGen (open text-to-audio) over HTTP.

POST /generate {"text": "...", "seconds": 5} -> {"wav_b64": "<base64 wav>", ...}
GET  /health                                 -> {"ok": true}

Backs the `stable-audio` catalog endpoint. The gateway drives this container
(see gateway::run_audio_worker): bound to 127.0.0.1, capability-dropped, and
runnable egress-less because the model is baked into the image.
"""

import base64
import io
import os
import wave

import numpy as np
import torch
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
from transformers import AutoProcessor, MusicgenForConditionalGeneration

app = FastAPI()

MODEL_ID = os.environ.get("AUDIO_MODEL", "facebook/musicgen-small")
MAX_TEXT = 500          # bound the prompt so generation cost stays bounded
MAX_SECONDS = 12        # cap clip length (CPU inference scales with it)
TOKENS_PER_SEC = 50     # MusicGen's audio frame rate

_proc = None
_model = None


def model():
    """Lazy-load the baked model on first use (a few seconds from local disk)."""
    global _proc, _model
    if _model is None:
        _proc = AutoProcessor.from_pretrained(MODEL_ID)
        _model = MusicgenForConditionalGeneration.from_pretrained(MODEL_ID)
        _model.eval()
    return _proc, _model


class GenIn(BaseModel):
    text: str
    seconds: float | None = None


@app.get("/health")
def health():
    return {"ok": True}


@app.post("/generate")
def generate(inp: GenIn):
    text = (inp.text or "").strip()
    if not text:
        raise HTTPException(400, "text is required")
    if len(text) > MAX_TEXT:
        raise HTTPException(400, f"text too long (max {MAX_TEXT} chars)")
    seconds = min(float(inp.seconds or 5.0), MAX_SECONDS)
    max_new_tokens = max(64, int(seconds * TOKENS_PER_SEC))

    proc, mdl = model()
    inputs = proc(text=[text], padding=True, return_tensors="pt")
    with torch.no_grad():
        audio = mdl.generate(
            **inputs, max_new_tokens=max_new_tokens, do_sample=True, guidance_scale=3.0
        )

    sr = mdl.config.audio_encoder.sampling_rate
    arr = audio[0, 0].cpu().numpy()
    pcm = (np.clip(arr, -1.0, 1.0) * 32767.0).astype(np.int16)  # float[-1,1] -> 16-bit PCM
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(pcm.tobytes())
    return {
        "wav_b64": base64.b64encode(buf.getvalue()).decode(),
        "sample_rate": sr,
        "seconds": round(len(arr) / sr, 2),
    }
