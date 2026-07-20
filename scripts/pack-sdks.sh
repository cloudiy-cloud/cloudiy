#!/usr/bin/env bash
# Build & verify the publishable SDK artifacts (Python wheel/sdist + npm tarball)
# without releasing them. The actual publish — `twine upload` / `npm publish` —
# is a HUMAN step: it needs PyPI/npm credentials (or OIDC trusted publishing)
# and is intentionally not automated here.
#
#   scripts/pack-sdks.sh            # build + dry-run, no upload
#
# Prereqs: python3 with `build` (`pip install build`) for the wheel; node/npm.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/dist-sdks"
mkdir -p "$OUT"

echo "==> Python (sdk/python)"
if python3 -c "import build" 2>/dev/null; then
    ( cd "$ROOT/sdk/python" && python3 -m build --outdir "$OUT" )
    echo "    built: $(ls "$OUT"/cloudiy_sdk-*.whl 2>/dev/null | xargs -n1 basename | tr '\n' ' ')"
    # Confirm the PEP 561 marker made it into the wheel.
    #
    # The listing is captured BEFORE grepping on purpose. Piping into `grep -q`
    # under `set -o pipefail` is a trap: grep exits at the first match, closing
    # the pipe, so the producer dies with SIGPIPE (141) and pipefail reports the
    # whole pipeline as failed — a false "missing" on a wheel that is fine.
    whl="$(ls -t "$OUT"/cloudiy_sdk-*.whl | head -1)"
    listing="$(unzip -l "$whl")"
    if printf '%s\n' "$listing" | grep -q "cloudiy_sdk/py.typed"; then
        echo "    ok  py.typed present in wheel"
    else
        echo "    !! py.typed missing from wheel" >&2; exit 1
    fi
else
    echo "    (skip) python 'build' not installed — run: pip install build"
fi

echo "==> JavaScript (sdk/js)"
( cd "$ROOT/sdk/js" && npm pack --pack-destination "$OUT" >/dev/null )
echo "    packed: $(ls "$OUT"/cloudiy-sdk-*.tgz 2>/dev/null | xargs -n1 basename | tr '\n' ' ')"

echo
echo "Artifacts in $OUT/. To publish (HUMAN, needs credentials):"
echo "  python:  twine upload $OUT/cloudiy_sdk-*"
echo "  js:      (cd sdk/js && npm publish --access public)"
