#!/usr/bin/env bash
# End-to-end for the HTTP SDKs (Python + JS): start one `cloudiy share` node
# with the HTTP API on and drive it through both thin clients — info(), the
# x402 402->quote flow, and (on a GPU node) a signed-result submit verified
# end-to-end. Complements scripts/e2e.sh, which exercises the Rust/P2P path.
#
# CPU-only hosts (like CI) have no GPU, so the kernel path returns an honest
# "no GPU" error and the drivers assert that graceful outcome; the x402 wire
# integration is still fully exercised. Needs `python3` and `node` (>=18).
set -euo pipefail

# Default to an uncommon port: 8080 is frequently taken (a busy port makes the
# node degrade to P2P-only, and a readiness probe could latch onto the foreign
# server sitting there). Override with CLOUDIY_HTTP_BIND.
BIN=${CLOUDIY_BIN:-./target/debug/cloudiy}
BIND=${CLOUDIY_HTTP_BIND:-127.0.0.1:8477}
LOG=$(mktemp)

cleanup() {
    [ -n "${PROV_PID:-}" ] && kill "$PROV_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> starting provider (HTTP on $BIND)"
RUST_LOG=info "$BIN" share --bind "$BIND" >"$LOG" 2>&1 &
PROV_PID=$!

# Wait until THIS node's HTTP API answers: probe /info and require it to report
# our own Node ID, so we never latch onto a foreign server on the same port.
NODE=""
for _ in $(seq 1 40); do
    NODE=$(grep -oE "Node ID:[[:space:]]+[a-f0-9]+" "$LOG" | awk '{print $NF}' | head -1 || true)
    if [ -n "$NODE" ] && curl -fsS "http://$BIND/info" 2>/dev/null | grep -q "$NODE"; then
        break
    fi
    sleep 1
done
if [ -z "$NODE" ] || ! curl -fsS "http://$BIND/info" 2>/dev/null | grep -q "$NODE"; then
    echo "!! provider HTTP API did not come up on $BIND for node ${NODE:-<none>}"
    echo "   (is the port taken? the node degrades to P2P-only when it is)"
    cat "$LOG"; exit 1
fi
echo "    provider node: $NODE  (HTTP up on $BIND)"

echo "==> Python SDK over HTTP"
python3 sdk/python/tests/e2e_http.py "$BIND"

echo "==> JS SDK over HTTP"
node sdk/js/e2e_http.mjs "$BIND"

if command -v go >/dev/null 2>&1; then
    echo "==> Go SDK over HTTP"
    ( cd sdk/go && go run ./e2e_http.go "$BIND" )
else
    echo "==> Go SDK over HTTP (skip — go not installed)"
fi

echo "SDK E2E PASSED"
