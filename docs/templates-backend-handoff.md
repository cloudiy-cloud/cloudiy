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

`TEMPLATES`, `APPS`, `REPOS` are hardcoded in `web/os.html`. Images are all real
and pullable, but nothing guarantees they match what providers can actually run.

**Ask (later):** serve the catalog from the backend (`GET /api/templates`,
`/api/apps`) so it stays in sync with actually-available worker images and can't
drift into fiction.

---

**Frontend side already done** (this session): single-image templates hit
`/api/vm/up` with 402/escrow handling; bundles + no-gateway fall back to a labeled
preview; the detail page states which path applies before you click.
