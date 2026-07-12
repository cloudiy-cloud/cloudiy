# Cloudiy audio worker — open text-to-audio (MusicGen) over HTTP. CPU-capable,
# so any provider node can serve the `stable-audio` endpoint. The catalog markets
# it as "stable audio", but the open worker runs MusicGen-small — mirroring how
# the SDXL / LTX workers back the branded image / video endpoints.
#
# Build & publish (see workers/README.md):
#   docker build -t ghcr.io/w3-surfer/worker-audio:latest workers/audio
#   docker push ghcr.io/w3-surfer/worker-audio:latest
FROM python:3.11-slim

# Torch CPU wheel (no CUDA) keeps the image runnable on any node.
RUN pip install --no-cache-dir torch --index-url https://download.pytorch.org/whl/cpu \
 && pip install --no-cache-dir transformers scipy numpy fastapi uvicorn

# Bake the model so the worker runs egress-less (sealed mode) with no runtime
# downloads. ~2 GB — the image is correspondingly large.
ENV HF_HOME=/models
RUN python -c "from transformers import AutoProcessor, MusicgenForConditionalGeneration; \
AutoProcessor.from_pretrained('facebook/musicgen-small'); \
MusicgenForConditionalGeneration.from_pretrained('facebook/musicgen-small')"

COPY server.py /app/server.py
EXPOSE 8000
CMD ["uvicorn", "--app-dir", "/app", "server:app", "--host", "0.0.0.0", "--port", "8000"]
