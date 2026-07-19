#!/usr/bin/env bash
# End-to-end smoke test: two `cloudiy` processes talk over the real P2P
# network and run a job. Exercises P2P dial + one-shot RPC (`info`) and a
# container workload (`launch`, Docker — no GPU needed) through the payment
# gate and the signed-result path. Runs in CI (ubuntu has Docker) and locally.
set -euo pipefail

BIN=${CLOUDIY_BIN:-./target/debug/cloudiy}
LOG=$(mktemp)

cleanup() {
    [ -n "${PROV_PID:-}" ] && kill "$PROV_PID" 2>/dev/null || true
    docker ps -aq --filter "name=cloudiy-vm-" | xargs -r docker rm -f >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> starting provider (P2P, no HTTP)"
# CLOUDIY_E2E_SHARE_ARGS: flags extras do provider de teste. O CI passa
# "--runtime runsc" (gVisor) para exercitar o caminho sandboxed real — desde o
# P2 do audit de isolamento o provider recusa imagens de consumer sem runtime
# sandboxed. Rodando localmente sem gVisor (ex.: macOS), o operador decide a
# própria postura, ex.: CLOUDIY_E2E_SHARE_ARGS="--allow-runc-untrusted".
# shellcheck disable=SC2086
RUST_LOG=info "$BIN" share --no-http ${CLOUDIY_E2E_SHARE_ARGS:-} >"$LOG" 2>&1 &
PROV_PID=$!

NODE=""
for _ in $(seq 1 40); do
    NODE=$(grep -oE "Node ID:[[:space:]]+[a-f0-9]+" "$LOG" | awk '{print $NF}' | head -1 || true)
    [ -n "$NODE" ] && break
    sleep 1
done
TOK=$(grep -oE "Access code \(this session\): [A-Za-z0-9]+" "$LOG" | awk '{print $NF}' | head -1 || true)
if [ -z "$NODE" ]; then
    echo "!! provider did not report a Node ID"; cat "$LOG"; exit 1
fi
echo "    provider node: $NODE"

echo "==> info over P2P (dial by node id + one-shot RPC)"
"$BIN" info --to "$NODE" | tee /dev/stderr | grep -q "$NODE" \
    || { echo "!! info did not return the node id"; exit 1; }
echo "    ok"

echo "==> container workload over P2P (alpine echo)"
# `set -e` + command substitution: se o launch falhar, o script morreria AQUI,
# antes do echo — e o CI ficaria sem a mensagem de erro. Capture o rc primeiro.
LAUNCH_RC=0
OUT=$("$BIN" launch --to "$NODE" --token "$TOK" --image alpine:3.20 -- echo cloudiy-e2e-ok 2>&1) || LAUNCH_RC=$?
echo "$OUT"
[ "$LAUNCH_RC" -eq 0 ] || { echo "!! launch exited with $LAUNCH_RC"; exit 1; }
echo "$OUT" | grep -q "cloudiy-e2e-ok" \
    || { echo "!! workload output missing marker"; exit 1; }
echo "$OUT" | grep -q "Signature verified" \
    || { echo "!! result was not signature-verified"; exit 1; }
echo "    ok"

echo "E2E PASSED"
