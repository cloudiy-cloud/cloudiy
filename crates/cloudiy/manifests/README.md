# Worker manifests (RFC-0012 §18)

One JSON file per catalog entry. Each declares a worker's image, license,
category, hardware needs and — the field this note is about — its **status**.

## `status`: `planned` vs `available`

- **`planned`** — announced, image not published yet. The UI shows it but offers
  no Deploy button that would 404. **This is the default** (a missing `status`
  parses as `planned`): an entry is only `available` when its image provably
  exists.
- **`available`** — the image is published and pullable. `verify-worker-images.py`
  enforces existence for these in CI.

The rule the whole system defends: **never `available` disguising a `planned`**
(a Deploy button that 404s). See `crates/cloudiy/src/manifest.rs`.

## Digests are NOT hand-written here

Do **not** paste a digest into a manifest's `image`. Supply-chain pinning is
resolved from a single source — `crates/cloudiy/worker_digests.json` — keyed by
the image ref. The install path calls `pinned_digest(image)` to pull by that
digest and verify. The pipeline writes those keys; you never duplicate them.

(The optional `digest` field exists only for hand-reviewed **third-party** images;
built workers rely on `worker_digests.json`.)

## The `planned → available` procedure (when images go up)

The image owner is **`ghcr.io/cloudiy-cloud`** everywhere — the repo owner, what
`publish-workers.yml` publishes to (`ghcr.io/${repository_owner}/worker-*`), and
what `worker_digests.json` is keyed by. (A manifest pointing at a different owner
would fail both `verify-worker-images.py` and `pinned_digest()` even after a real
publish.)

1. A maintainer triggers the publish: `git tag workers-v0.1.0 && git push origin
   workers-v0.1.0` (or the `workflow_dispatch`). `publish-workers.yml` builds the
   four workers (`tts audio sdxl ltx`), cosign-signs them, and pins their digests
   into `worker_digests.json`.
2. Flip the matching manifests to `available` — **automatically**, from that same
   file:

   ```bash
   python3 scripts/sync-manifest-status.py          # apply
   python3 scripts/sync-manifest-status.py --check   # CI: exit 1 on drift
   ```

   It sets `status: available` for every manifest whose `image` is now a key in
   `worker_digests.json`, and `planned` for those that aren't — so the catalog can
   never drift out of step with what actually exists. Run it after a publish (or
   wire it into the pipeline right after the digest-pinning step and commit the
   result).

Only workers with a Dockerfile under `workers/` are built (`sdxl`, `ltx`, `tts`,
`audio`); the rest stay `planned` until one exists. `chatterbox` maps onto the
built `worker-tts` image; `kokoro` has no image yet and stays `planned`.
