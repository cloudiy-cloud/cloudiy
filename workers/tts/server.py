"""Cloudiy TTS worker — Piper over HTTP.

POST /tts {"text": "..."} -> {"wav_b64": "<base64 wav>"}
GET  /health               -> {"ok": true}

The gateway drives this container (see gateway::run_tts_worker); it binds to
127.0.0.1 on the host, is capability-dropped, and may run egress-less.
"""

import base64
import glob
import io
import os
import wave

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
from piper import PiperVoice

app = FastAPI()

VOICE_DIR = os.environ.get("VOICE_DIR", "/voices")
MAX_TEXT = 4000  # bound synthesis cost per request

_voice = None


def voice():
    global _voice
    if _voice is None:
        models = sorted(glob.glob(os.path.join(VOICE_DIR, "**", "*.onnx"), recursive=True))
        if not models:
            raise HTTPException(503, f"no piper voice found under {VOICE_DIR}")
        _voice = PiperVoice.load(models[0])
    return _voice


class TtsIn(BaseModel):
    text: str


@app.get("/health")
def health():
    return {"ok": True}


@app.post("/tts")
def tts(inp: TtsIn):
    text = (inp.text or "").strip()
    if not text:
        raise HTTPException(400, "text is required")
    if len(text) > MAX_TEXT:
        raise HTTPException(400, f"text too long (max {MAX_TEXT} chars)")
    v = voice()
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wav_file:
        v.synthesize(text, wav_file)
    return {"wav_b64": base64.b64encode(buf.getvalue()).decode()}
