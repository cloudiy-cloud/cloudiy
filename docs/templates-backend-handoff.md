# Handoff: make App Store templates deploy for real

Context for the backend/gateway session. The CloudiyOS App Store (`web/os.html`)
now deploys **single-image templates for real** via `POST /api/vm/up` when a
gateway is up and a node is connected (same call the Terminal + custom deploy
use). Everything else is honestly labeled **preview** in the UI. To close the
gap so every template runs for real, the gateway/scheduler needs the following.
Ordered by impact.

## A. Auto-placement (biggest UX gap)

Today the browser must pass a specific `to: <NodeID>` — the user types it in the
Terminal. A "Deploy" button can't require that.

**Ask:** `POST /api/vm/up` with **no `to`** should let the client-side scheduler
pick a provider through the directory (by capability/price/health) and return the
chosen node. Response should include `node` so the UI can show where it landed.

```
POST /api/vm/up  { image, cpu, memory_mb, ... }        // no `to`
-> { state, vm_id, node, image, cpu_millis, memory_mib, volume }
```

## B. Bundle → one-VM model (why multi-image templates are still preview)

A VM is **one container per identity** (+ named volume). A template like
`Data Science` = `pytorch/pytorch` **+** `jupyter/scipy-notebook` has no mapping
onto a single container yet, so the UI records it as a preview.

**Ask:** decide how a bundle composes one identity-bound VM. Options:
1. **Compose** — several containers sharing one volume + network under one VM id
   (docker-compose-like spec).
2. **Baked image** — publish a single combined image per bundle (simplest for the
   runtime; heavier to maintain).
3. **Sidecars** — a primary container + attached services on the same volume.

Then expose it, e.g. `POST /api/vm/up { images: [...] }` or `{ compose: {...} }`
or `{ template: "<key>" }`. The frontend `TEMPLATES[].apps` array already lists
the image set per template.

## C. Deploy-config passthrough

The template detail page collects **version, GPU/CPU, disk, and env vars**, but
`/api/vm/up` today only accepts `cpu`, `memory_mb`, `ports`.

**Ask:** accept `env` (string map), `gpu` (capability, e.g. `cuda`), and `disk`
(GB) on `/api/vm/up`, and honor them.

## D. VM state / placement query

The browser has no durable idea of **where the user's VM lives** — it relies on a
manually re-entered Node ID each session.

**Ask:** `GET /api/vm/status` returns the current `{ node, image, state, volume }`
for the caller's identity, so the UI reflects reality across reloads and the
App Store can show "running here" instead of asking again.

## E. (optional) Catalog as source of truth

`TEMPLATES`, `APPS`, `REPOS` are hardcoded in `web/os.html`, and nothing
guarantees they match what providers can actually run — see F, which is exactly
this drift.

**Ask (later):** serve the catalog from the backend (`GET /api/templates`,
`/api/apps`) so it stays in sync with actually-available worker images and can't
drift into fiction.

## F. Publish the `cloudiy/worker-*` images (blocks the Serverless Repos)

Verified with `docker manifest inspect`: of the 11 Serverless Repos, **only 2
images actually exist** — `ollama/ollama` and
`onerahmet/openai-whisper-asr-webservice`. The other 9 point at images that are
NOT in any registry:

- **8× `cloudiy/worker-*`** — `sdxl`, `wan22`, `ltx-video`, `musicgen`, `esrgan`,
  `axolotl`, plus the two now repointed (see below). These are Cloudiy's own
  worker images and have never been built/pushed (roadmap: "Published GPU worker
  images" = NEXT).
- **1× `ghcr.io/comfyanonymous/comfyui`** — that path isn't a published image.

**Frontend interim (this session):** repointed the two that HAVE a real official
upstream — `vllm` → `vllm/vllm-openai`, `infinity` → `michaelf34/infinity`. The
remaining 7 (SDXL, Wan, LTX, MusicGen, Real-ESRGAN, ComfyUI, Axolotl) are flagged
`soon: true` and render as **"Coming soon"** with Deploy disabled, so the UI never
claims a nonexistent image runs.

**Ask:** build and publish the worker images (a `cloudiy/worker-*` per model, with
the x402/protocol serving layer) via the release pipeline, then drop the `soon`
flag on each repo as its image ships. For the ones with a usable upstream (ComfyUI,
Axolotl), decide whether to wrap a community image or ship a Cloudiy worker.

## G. Close the reputation ramp on the paid path (from beta report v5)

The report scores 888/1000 and flags the canary→reputation→holdback wiring as the
one engineering gap left ("not-yet-wired", marked in the code).

- **Value-cap half of the ramp** (`crates/cloudiy/src/reputation.rs:176`):
  `max_job_micro_usdc_for_score()` is pure and tested but never called at
  placement, because `WorkloadSpec` carries no job value. **Ask:** add a job-value
  field to `WorkloadSpec` (from the quote/escrow) and gate placement with it — the
  routing floor is enforced today, the hard value cap is not.
- **End-to-end canary → reputation → holdback on the paid path**: live canary
  injection + on-chain clawback enforcement of the holdback window — part is
  on-chain (v2), part still integrating. Wire the paid path through it.

## H. Serve the catalog + a supply/deploy metric (unblocks honest UI)

The frontend now shows real *local* deploy counts and a live Network Explorer
(`web/explorer.html`, reads `/api/machines`). Both would be stronger as network
truth. **Ask:** `GET /api/templates`/`/api/apps` (catalog source of truth, see E)
and a network deploy counter so the cards can show real network deploys instead
of a per-device count.

---

**Frontend side already done** (this session): single-image templates hit
`/api/vm/up` with 402/escrow handling; bundles + no-gateway fall back to a labeled
preview; the detail page states which path applies before you click; the
unpublished repos are gated behind "Coming soon"; deploy counts are real (local);
a public Network Explorer reads live `/api/machines`; the docs surface the
reputation/canary ramp; CI badges prove the Rust tests run.
