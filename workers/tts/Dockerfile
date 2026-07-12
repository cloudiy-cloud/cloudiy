# Cloudiy TTS worker — Piper text-to-speech over HTTP. CPU-only (no GPU
# needed), so any provider node can serve the `chatterbox` endpoint.
#
# Build & publish (see workers/README.md):
#   docker build -t ghcr.io/w3-surfer/worker-tts:latest workers/tts
#   docker push ghcr.io/w3-surfer/worker-tts:latest
FROM python:3.11-slim

RUN pip install --no-cache-dir piper-tts fastapi uvicorn

# Bake the voice into the image so the worker can run egress-less (sealed
# mode) with no runtime downloads.
RUN python -m piper.download_voices en_US-lessac-medium --data-dir /voices \
    || python -c "print('voice download helper unavailable; mount /voices at run time')"

COPY server.py /app/server.py
EXPOSE 8000
CMD ["uvicorn", "--app-dir", "/app", "server:app", "--host", "0.0.0.0", "--port", "8000"]
