#!/usr/bin/env bash
# Run the black-box protocol conformance suite against a freshly started
# reference node. Symmetric with scripts/e2e-sdk.sh: boots `cloudiy share` with
# the HTTP API on, points conformance/cloudiy_conformance.py at it, tears down.
#
# CPU-only hosts (like CI) can't produce a signed GPU result, so the signature
# checks SKIP there — that is conformant, not a failure. The x402 flow, /info
# contract, error behavior and (with --slow) the frame limit are still checked.
#
#   scripts/conformance.sh              # against a dev node on an uncommon port
#   scripts/conformance.sh --slow       # also exercise the >16 MiB frame limit
set -euo pipefail

BIN=${CLOUDIY_BIN:-./target/debug/cloudiy}
BIND=${CLOUDIY_HTTP_BIND:-127.0.0.1:8479}
TOKEN=${CLOUDIY_CONF_TOKEN:-conformance-probe}
LOG=$(mktemp)
EXTRA=""
[ "${1:-}" = "--slow" ] && EXTRA="--slow"

cleanup() { [ -n "${PROV_PID:-}" ] && kill "$PROV_PID" 2>/dev/null || true; }
trap cleanup EXIT

echo "==> starting reference node (HTTP on $BIND, dev token)"
# CLOUDIY_CONF_SHARE_ARGS lets a caller add flags (e.g. --no-gpu to rehearse the
# CPU-only CI path, or --runtime runsc). Unquoted on purpose: word-split into args.
"$BIN" share --bind "$BIND" --token "$TOKEN" ${CLOUDIY_CONF_SHARE_ARGS:-} >"$LOG" 2>&1 &
PROV_PID=$!

# Wait until the node's own HTTP API answers /info with an endpoint_id. Read it
# straight from /info rather than scraping the log — no dependency on RUST_LOG
# level, and it proves the HTTP surface (not just the process) is up.
node_id_from_info() {
    local body
    body="$(curl -fsS "http://$BIND/info" 2>/dev/null || true)"
    printf '%s' "$body" | sed -n 's/.*"endpoint_id":"\([0-9a-f]*\)".*/\1/p'
}
NODE=""
for _ in $(seq 1 40); do
    NODE="$(node_id_from_info)"
    [ -n "$NODE" ] && break
    sleep 1
done
if [ -z "$NODE" ]; then
    echo "!! reference node HTTP API did not come up on $BIND"; cat "$LOG"; exit 1
fi
echo "    node $NODE up"
echo

# --token lets the dev node open the payment gate so the signed path is
# attempted (it still SKIPs on a CPU-only host, where there's no GPU result).
python3 conformance/cloudiy_conformance.py "$BIND" --token "$TOKEN" $EXTRA
