#!/usr/bin/env bash
# Redeploy the cloudiy-escrow program to devnet.
#
# Needed because RFC-0006 changed the on-chain program (a HUMAN step — it needs
# the upgrade-authority key + devnet SOL, so it is deliberately not automated):
#   - release_verified now verifies the v2 result signature (binds input_hash),
#   - the challenge-window / clawback holdback machinery (disabled at
#     CHALLENGE_WINDOW_SECS = 0, so this redeploy is behaviour-neutral until you
#     raise it), and a `created_at` field on the Job account.
#
# Program id: 9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN (declare_id! in
# programs/cloudiy-escrow/src/lib.rs; keep it stable — the clients hardcode it).
#
# Existing devnet escrows predate the new account layout and are incompatible —
# fine pre-mainnet. Do NOT run this against mainnet (see docs/MAINNET.md).
set -euo pipefail

cd "$(dirname "$0")/../contracts"

echo "▸ Cluster / wallet"
solana config get | grep -E "RPC URL|Keypair Path" || true
CLUSTER_URL="$(solana config get | awk '/RPC URL/{print $NF}')"
case "$CLUSTER_URL" in
  *devnet*) : ;;
  *) echo "✗ Refusing: RPC URL is not devnet ($CLUSTER_URL). Set: solana config set --url devnet"; exit 1 ;;
esac

echo "▸ Upgrade authority must be the program's current authority:"
solana program show 9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN 2>/dev/null | grep -i "authority" || \
  echo "  (program not found on this cluster — first deploy will set your wallet as authority)"

echo "▸ Devnet SOL balance (need ~2-5 SOL for a program deploy):"
solana balance || true
echo "  Top up with:  solana airdrop 2"

read -r -p "Proceed with anchor build + deploy to devnet? [y/N] " ok
[ "$ok" = "y" ] || { echo "aborted"; exit 1; }

echo "▸ anchor build"
anchor build

echo "▸ anchor deploy (devnet)"
anchor deploy --provider.cluster devnet

echo "▸ Done. Verify the new program:"
solana program show 9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN
echo
echo "Next (optional, to ACTIVATE the holdback — a governance decision, RFC-0006 §6.2/§10):"
echo "  1. Decide the challenge window length (CHALLENGE_WINDOW_SECS in lib.rs)."
echo "  2. Decide WHO the challenge authority is (CHALLENGE_AUTHORITY) — ideally a"
echo "     decentralized attester set / reputation quorum, not a single key."
echo "  3. Re-run this script to redeploy with the window enabled."
