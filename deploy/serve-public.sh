#!/usr/bin/env bash
#
# Put a Cloudiy network ONLINE behind a public HTTPS URL, so the deployed web
# app (e.g. https://cloudiy-cloud.vercel.app) can talk to it via ?gw=<url>.
#
# It starts, all wired to share one CLOUDIY_DIRECTORY:
#   1) a directory node      (discovery registry, stable ID across restarts)
#   2) the CloudiyOS gateway (cloudiy os, serves /api/* on 127.0.0.1:4600)
#   3) a cloudflared tunnel  (127.0.0.1:4600 -> a public https URL)
# and prints the exact link to open the deployed app against this network.
#
# Providers (your Mac, a Linux+NVIDIA box) then announce to the SAME directory:
#   CLOUDIY_DIRECTORY=<printed id> cloudiy share --share-cpu 4 --share-memory-mb 4096 --no-gpu --price-usdc-per-hour 0.10
#
# Run SHARE=1 to also share THIS machine.  Ctrl+C stops everything.
#
# SECURITY: the gateway exposes /api/vm/* and a /api/shell WebSocket. A quick
# tunnel URL is random but PUBLIC — do not leave it up unattended without the
# origin/auth hardening noted in crates/cloudiy/src/http.rs. For production use a
# NAMED cloudflared tunnel + a firewall (see deploy/cloudflared/).
set -uo pipefail
cd "$(dirname "$0")/.."

if command -v cloudiy >/dev/null 2>&1; then CLOUDIY=cloudiy
elif [ -x "./target/release/cloudiy" ]; then CLOUDIY="./target/release/cloudiy"
else echo "cloudiy not found. Build it:  cargo build --release -p cloudiy"; exit 1; fi
command -v cloudflared >/dev/null 2>&1 || { echo "cloudflared not found. Install it: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/"; exit 1; }

WEB_APP="${WEB_APP:-https://cloudiy-cloud.vercel.app}"
SHARE_CPU="${SHARE_CPU:-4}"; SHARE_MEM_MB="${SHARE_MEM_MB:-4096}"; PRICE="${PRICE:-0.10}"; GPU_ARGS="${GPU_ARGS:---no-gpu}"

LOG=$(mktemp -d)
DIR_PID=""; OS_PID=""; CF_PID=""; SHARE_PID=""
cleanup() { echo; echo "Stopping..."; kill "$DIR_PID" "$OS_PID" "$CF_PID" "$SHARE_PID" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

if command -v lsof >/dev/null 2>&1 && lsof -ti :4600 >/dev/null 2>&1; then
  echo "Port 4600 in use — stopping the old gateway."; lsof -ti :4600 | xargs kill 2>/dev/null || true; sleep 1
fi

# 1) directory
echo "-> directory node..."
"$CLOUDIY" directory > "$LOG/dir.log" 2>&1 & DIR_PID=$!
DIRID=""
for _ in $(seq 1 30); do
  DIRID=$(grep -oE "Directory ID: [0-9a-f]+" "$LOG/dir.log" 2>/dev/null | head -1 | awk '{print $3}')
  if [ -n "$DIRID" ]; then break; fi; sleep 0.5
done
if [ -z "$DIRID" ]; then echo "directory failed:"; cat "$LOG/dir.log"; exit 1; fi
export CLOUDIY_DIRECTORY="$DIRID"

# 2) gateway
echo "-> gateway (cloudiy os)..."
"$CLOUDIY" os --web-dir web > "$LOG/os.log" 2>&1 & OS_PID=$!
for _ in $(seq 1 20); do grep -q "gateway on" "$LOG/os.log" 2>/dev/null && break; sleep 0.3; done

# 2b) optionally share this machine too
if [ "${SHARE:-0}" = "1" ]; then
  echo "-> sharing this machine ($SHARE_CPU vCPU, $SHARE_MEM_MB MB, \$$PRICE/h)..."
  "$CLOUDIY" share --share-cpu "$SHARE_CPU" --share-memory-mb "$SHARE_MEM_MB" $GPU_ARGS --price-usdc-per-hour "$PRICE" > "$LOG/share.log" 2>&1 & SHARE_PID=$!
fi

# 3) public HTTPS tunnel
echo "-> cloudflared tunnel..."
cloudflared tunnel --url http://127.0.0.1:4600 --no-autoupdate > "$LOG/cf.log" 2>&1 & CF_PID=$!
PUB=""
for _ in $(seq 1 40); do
  PUB=$(grep -oE "https://[a-z0-9-]+\.trycloudflare\.com" "$LOG/cf.log" 2>/dev/null | head -1)
  if [ -n "$PUB" ]; then break; fi; sleep 0.5
done
if [ -z "$PUB" ]; then echo "cloudflared failed to get a URL:"; tail "$LOG/cf.log"; exit 1; fi

echo
echo "============================================================"
echo " Cloudiy is ONLINE."
echo "   Public gateway : $PUB"
echo "   Directory ID   : $DIRID"
echo
echo "   Open the deployed app against it:"
echo "     ${WEB_APP}/vm.html?gw=${PUB}"
echo
echo "   Point a provider (your Mac / a GPU box) at this directory:"
echo "     CLOUDIY_DIRECTORY=$DIRID cloudiy share --share-cpu 4 --share-memory-mb 4096 --no-gpu --price-usdc-per-hour 0.10"
echo
echo "   Logs: $LOG   ·   Ctrl+C to stop everything (or ./deploy/stop-public.sh)."
echo "============================================================"
wait "$CF_PID"
