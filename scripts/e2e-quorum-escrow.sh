#!/usr/bin/env bash
#
# RFC-0008 end-to-end: `run --replicas N --pay` on devnet with REAL money.
#
# Proves two things against the live escrow program:
#   1. happy path      — N replicas, one escrow each, quorum reached, every
#                        agreeing provider settled via `release_verified`;
#   2. adversarial     — a replica that returns a *signed but wrong* result is
#                        flagged, excluded from the quorum, earns NO release,
#                        and its escrow is reported back as refundable.
#
# The faulty replica is a real node built with the non-default `e2e-divergent`
# feature (it corrupts its output *before* signing), so case 2 exercises the
# genuine threat rather than faking divergence at the tally layer.
#
# PREREQUISITES (one-time, not automated — they spend real devnet SOL):
#   * solana + spl-token CLIs, and a funded devnet keypair.
#   * An SPL mint you control (`spl-token create-token --decimals 6`), and an
#     associated token account for: the consumer, EVERY provider wallet, and the
#     escrow FEE AUTHORITY (GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP).
#     `release_verified` takes `provider_token`/`fee_token` as existing
#     accounts — settlement fails if any of them is missing.
#   * Mint some of that token to the consumer.
# Export the results as MINT and CONSUMER_KEYPAIR before running.
#
# Each node gets its own HOME: both the iroh node key and the Solana keypair are
# derived from $HOME, so distinct HOMEs are what make the replicas independent.
set -uo pipefail
cd "$(dirname "$0")/.."

MINT="${MINT:?set MINT to an SPL mint you control (6 decimals)}"
CONSUMER_KEYPAIR="${CONSUMER_KEYPAIR:?set CONSUMER_KEYPAIR to a funded devnet keypair holding that mint}"
RPC="${RPC:-https://api.devnet.solana.com}"
PRICE="${PRICE:-0.05}"
WORK="${WORK:-$(mktemp -d)}"
BIN=./target/debug/cloudiy
DIV_BIN="$WORK/target-div/debug/cloudiy"

cleanup() { pkill -f "cloudiy.*--directory" 2>/dev/null; pkill -f "cloudiy directory" 2>/dev/null; }
trap cleanup EXIT

echo "==> work dir: $WORK"
cargo build -p cloudiy || exit 1
# Separate CARGO_TARGET_DIR, not a `cp` of the binary: copying a just-written
# Mach-O and executing it trips macOS code signing and the node hangs at start.
CARGO_TARGET_DIR="$WORK/target-div" cargo build -p cloudiy --features e2e-divergent || exit 1

for n in dir consumer provA provB provC; do mkdir -p "$WORK/$n/.config/solana"; done
cp "$CONSUMER_KEYPAIR" "$WORK/consumer/.config/solana/id.json"
for p in provA provB provC; do
    [ -f "$WORK/$p/.config/solana/id.json" ] ||
        solana-keygen new --no-bip39-passphrase -s -o "$WORK/$p/.config/solana/id.json" >/dev/null
    echo "    $p wallet: $(solana address -k "$WORK/$p/.config/solana/id.json")"
done
echo "!! each provider wallet above needs an ATA for $MINT — see PREREQUISITES"

echo "==> directory"
HOME="$WORK/dir" $BIN directory >"$WORK/dir.log" 2>&1 &
for _ in $(seq 1 40); do
    DIR=$(grep -oE "CLOUDIY_DIRECTORY=[a-f0-9]{64}" "$WORK/dir.log" | head -1 | cut -d= -f2)
    [ -n "${DIR:-}" ] && break
    sleep 1
done
[ -n "${DIR:-}" ] || { echo "!! directory never reported an id"; exit 1; }
echo "    $DIR"

share() { # share <home> <binary>
    HOME="$WORK/$1" "$2" share --no-http --directory "$DIR" --usdc-mint "$MINT" \
        --rpc-url "$RPC" --require-payment --price-usdc "$PRICE" >"$WORK/$1.log" 2>&1 &
}
echo "==> 2 honest providers + 1 divergent"
share provA "$BIN"
share provB "$BIN"
share provC "$DIV_BIN"
for _ in $(seq 1 60); do
    COUNT=$(HOME="$WORK/consumer" $BIN providers --via "$DIR" 2>/dev/null | grep -c "^•")
    [ "${COUNT:-0}" -ge 3 ] && break
    sleep 2
done
echo "    $COUNT provider(s) announced"

run_case() { # run_case <replicas>
    HOME="$WORK/consumer" $BIN run --via "$DIR" --replicas "$1" --pay --release \
        --kernel vector_add --data "1,2,3;4,5,6" --rpc-url "$RPC" 2>&1 |
        grep -vE "INFO|relay-actor|WARN"
}

echo
echo "==> CASE 1: quorum with every replica honest (expect all settled)"
# Only two providers are dialed, so this is a 2-of-2 among the honest pair.
OUT=$(run_case 2)
echo "$OUT"
echo "$OUT" | grep -q "Quorum reached" || { echo "!! no quorum"; exit 1; }
echo "$OUT" | grep -q "Result:" || { echo "!! no result"; exit 1; }
echo "    ok"

echo
echo "==> CASE 2: one divergent replica (expect it flagged, unpaid, refundable)"
OUT=$(run_case 3)
echo "$OUT"
echo "$OUT" | grep -q "divergent result from" || { echo "!! divergence not flagged"; exit 1; }
echo "$OUT" | grep -q "Quorum reached" || { echo "!! honest majority did not win"; exit 1; }
echo "$OUT" | grep -q "cloudiy refund --escrow" || { echo "!! divergent escrow not reported refundable"; exit 1; }
echo "    ok"

echo
echo "==> done. Check payouts:  spl-token balance --address <provider ATA> --url $RPC"
echo "    the divergent provider's balance must be UNCHANGED."
