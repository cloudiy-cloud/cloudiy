#!/usr/bin/env bash
#
# Bring up a complete local Cloudiy network on THIS machine and open CloudiyOS:
#   1) a directory node        (discovery registry)
#   2) this machine as provider (cloudiy share)
#   3) the CloudiyOS gateway    (cloudiy os) pointed at that directory
#
# All three share CLOUDIY_DIRECTORY so they find each other, which is the whole
# trick: run any of them without it and they talk to different (or no) directory
# and nothing shows up. Ctrl+C stops everything.
#
# Override what you share:  SHARE_CPU=8 SHARE_MEM_MB=8192 PRICE=0.16 ./scripts/run-local-network.sh
# macOS shares CPU/RAM only (Docker can't pass through the GPU): GPU_ARGS stays --no-gpu.
# NOTE: no `set -e` here on purpose — the poll loop below expects `grep` to
# "fail" (find nothing) until the directory prints its ID; -e would abort it.
set -uo pipefail
cd "$(dirname "$0")/.."

# --- locate the binary ---
if command -v cloudiy >/dev/null 2>&1; then CLOUDIY=cloudiy
elif [ -x "./target/release/cloudiy" ]; then CLOUDIY="./target/release/cloudiy"
else
  echo "cloudiy not found. Build it first:  cargo build --release -p cloudiy"
  exit 1
fi

SHARE_CPU="${SHARE_CPU:-4}"
SHARE_MEM_MB="${SHARE_MEM_MB:-4096}"
PRICE="${PRICE:-0.10}"
GPU_ARGS="${GPU_ARGS:---no-gpu}"

LOG=$(mktemp -d)
DIR_PID=""; SHARE_PID=""
cleanup() { echo; echo "Stopping local network..."; kill "$DIR_PID" "$SHARE_PID" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

# --- free port 4600 if a stale gateway (started without a directory) holds it ---
if command -v lsof >/dev/null 2>&1 && lsof -ti :4600 >/dev/null 2>&1; then
  echo "Port 4600 is in use — stopping the old gateway so a fresh one can take over."
  lsof -ti :4600 | xargs kill 2>/dev/null || true
  sleep 1
fi

# 1) directory ------------------------------------------------------------
echo "→ Starting directory node..."
"$CLOUDIY" directory > "$LOG/dir.log" 2>&1 &
DIR_PID=$!
DIRID=""
for _ in $(seq 1 30); do
  DIRID=$(grep -oE "Directory ID: [0-9a-f]+" "$LOG/dir.log" 2>/dev/null | head -1 | awk '{print $3}')
  if [ -n "$DIRID" ]; then break; fi
  sleep 0.5
done
if [ -z "$DIRID" ]; then echo "Directory failed to start:"; cat "$LOG/dir.log"; exit 1; fi
export CLOUDIY_DIRECTORY="$DIRID"
echo "  directory ID: $DIRID"

# 2) share this machine ---------------------------------------------------
echo "→ Sharing this machine: ${SHARE_CPU} vCPU, ${SHARE_MEM_MB} MB, \$${PRICE}/h ${GPU_ARGS}"
"$CLOUDIY" share --share-cpu "$SHARE_CPU" --share-memory-mb "$SHARE_MEM_MB" $GPU_ARGS \
  --price-usdc-per-hour "$PRICE" > "$LOG/share.log" 2>&1 &
SHARE_PID=$!

# 3) gateway (foreground: its output shows here, Ctrl+C stops the whole set)
echo "→ Starting CloudiyOS gateway on http://127.0.0.1:4600 ..."
echo
echo "  When it is up, open:  http://127.0.0.1:4600/os.html"
echo "  Then: Hardware Store → Rent parts → this machine appears as a CPU node."
echo "  Logs: $LOG   (Ctrl+C stops everything)"
echo
# Foreground (not exec) so Ctrl+C also triggers the cleanup trap above, which
# stops the directory and share instead of leaving them orphaned.
"$CLOUDIY" os --web-dir web
