#!/usr/bin/env python3
"""Sync each worker manifest's `status` to reality, using worker_digests.json as
the single source of truth.

The pipeline (`.github/workflows/publish-workers.yml`) builds, signs and pins each
published image's digest into `crates/cloudiy/worker_digests.json`, keyed by the
exact image ref. That file is therefore an authoritative "these images exist and
are pinned" list. A manifest whose `image` is a key there should be `available`;
one that isn't should be `planned` (an `available` manifest whose image 404s is
exactly the "Deploy button that lies" this guards against — see manifest.rs).

This removes the hand step: nobody edits a digest into a manifest (install already
resolves the digest from worker_digests.json via `pinned_digest()`), and nobody
forgets to flip `planned`→`available` after a publish — just run this.

Usage:
  python3 scripts/sync-manifest-status.py            # apply: rewrite drifted manifests
  python3 scripts/sync-manifest-status.py --check     # CI: exit 1 on drift, write nothing

Exit: 0 = in sync (or applied) · 1 = drift found in --check mode · 2 = usage/IO error.
"""
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DIGESTS = ROOT / "crates/cloudiy/worker_digests.json"
MANIFEST_DIR = ROOT / "crates/cloudiy/manifests"


def published_refs():
    """The set of image refs the pipeline has published+pinned (worker_digests.json
    keys, minus the `_comment`)."""
    try:
        data = json.loads(DIGESTS.read_text())
    except (OSError, json.JSONDecodeError) as e:
        print(f"error: cannot read {DIGESTS}: {e}", file=sys.stderr)
        sys.exit(2)
    return {k for k in data if k != "_comment"}


def main():
    check = "--check" in sys.argv[1:]
    if any(a not in ("--check",) for a in sys.argv[1:]):
        print(__doc__)
        sys.exit(2)

    refs = published_refs()
    drifted = []
    for path in sorted(MANIFEST_DIR.glob("*.json")):
        try:
            doc = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as e:
            print(f"error: {path.name}: {e}", file=sys.stderr)
            sys.exit(2)
        worker = doc.get("worker", {})
        image = worker.get("image", "")
        # Third-party images are pinned by hand in worker_digests.json; built
        # workers are pinned by the pipeline. Either way, presence there == exists.
        want = "available" if image in refs else "planned"
        have = worker.get("status", "planned")  # schema default is planned
        if have != want:
            drifted.append((path, doc, worker, have, want))

    if not drifted:
        print(f"in sync: {len(list(MANIFEST_DIR.glob('*.json')))} manifests match worker_digests.json")
        return

    for path, _, _, have, want in drifted:
        print(f"{'DRIFT' if check else 'update'}: {path.name}: {have} -> {want}")

    if check:
        print(f"\n{len(drifted)} manifest(s) out of sync with worker_digests.json — "
              f"run `python3 scripts/sync-manifest-status.py` to fix.", file=sys.stderr)
        sys.exit(1)

    for path, doc, worker, _, want in drifted:
        worker["status"] = want
        path.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"\nupdated {len(drifted)} manifest(s).")


if __name__ == "__main__":
    main()
