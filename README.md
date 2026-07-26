# ☁️ Cloudiy

[![CI](https://github.com/w3-surfer/cloudiy/actions/workflows/ci.yml/badge.svg)](https://github.com/w3-surfer/cloudiy/actions/workflows/ci.yml)
[![Release](https://github.com/w3-surfer/cloudiy/actions/workflows/release.yml/badge.svg)](https://github.com/w3-surfer/cloudiy/actions/workflows/release.yml)

> CI runs `cargo fmt`, `clippy -D warnings`, `build` and the full
> `cargo test --workspace` (reputation, canary, pricing and escrow tests) on
> every push — the green badge above means they pass.

**Decentralized cloud computing on Solana — with its own browser OS.**

Cloudiy is a peer-to-peer compute network: anyone shares a machine's spare
capacity (GPU, CPU, containers, model inference) and anyone rents it **per
request**, paid in **USDC** via [x402](https://solana.com/x402/what-is-x402) and
an on-chain escrow. No accounts, no middlemen — the provider signs the result,
the chain settles the payment.

- 🖥️ **CloudiyOS** — a browser operating system at [`web/os.html`](web/os.html):
  boot an identity-bound VM with an App Store, Hardware Store, Models and a
  terminal. Landing + docs are the other static pages in [`web/`](web).
- 🦀 **Node software** — Rust workspace in [`crates/`](crates): the `cloudiy`
  binary providers run (`cloudiy share`), plus the discovery directory,
  scheduler, runtime and shared protocol.
- 🧩 **Consumer SDKs** (Rust · Python · JS) for apps and **AI agents** in
  [`sdk/`](sdk) — signature-verified by default.
- ⚓ **On-chain escrow** — Anchor program on Solana **devnet** in
  [`contracts/`](contracts) (`9zMBC7JD…c1TN`).
- 📐 **Design** — the protocol RFCs live in [`docs/rfcs/`](docs/rfcs).

> **Status: devnet beta.** GPU image/video workers need a Linux + NVIDIA host;
> text (Llama via Ollama) and speech-to-text (Whisper) run on CPU and work today.
> Verifiable settlement (RFC-0006) is implemented at the crypto layer and partly
> beyond — see [Verifiable settlement](#verifiable-settlement) for what's real
> vs. economic vs. still-a-decision.

## How it works

The network has three roles — **providers** share compute, **consumers** rent
it, and lightweight **directories** relay signed provider announcements so
consumers can discover and schedule. Everything speaks P2P **QUIC over
[iroh](https://iroh.computer)** (NAT-traversing, no port-forwarding); the
provider also exposes a small local HTTP API for the browser/marketplace path.

```
 Consumer                              Provider (shares a machine)
 ─────────                             ───────────────────────────
 browser → local gateway              cloudiy share
   (CloudiyOS, loopback /api/*)          • announces resources + served models
        │                                • runs the job (wgpu kernel, Docker
        │   iroh QUIC (or CLI/SDK)          workload, or a model worker)
        └──────────────────────────►     • signs the result (job_id · input · output)
                                          • settles via the Solana escrow (USDC)
        ▲                                        │
        └────────── signed result ◄──────────────┘

 Discovery: providers announce to a directory; consumers fetch + verify every
 signature and schedule client-side (or dial a provider directly with --to).
```

What a provider can serve:

- **GPU/CPU kernels** — deterministic `wgpu`/WGSL primitives (`vector_add`,
  `matrix_mul`), verifiable by re-execution / quorum (`--replicas N`).
- **Container workloads** — an OCI image or a template (`pytorch`, `ollama`, …)
  run in an isolated Docker runtime (Open Compute Protocol).
- **Model endpoints** — chat (Llama 3.2 via a CPU Ollama worker) and
  speech-to-text (Whisper, CPU) run today; image/video (SDXL, LTX) are
  GPU-gated. Consumers call them per request; the node reports honestly when a
  model needs hardware it doesn't have.

## Quick start

### Provider — share your machine, earn USDC

```bash
# 1. Install — one line, no Rust toolchain (downloads a prebuilt binary).
curl -fsSL https://cloudiy-cloud.vercel.app/install.sh | sh
#    Windows:      irm https://cloudiy-cloud.vercel.app/install.ps1 | iex
#    From source:  cargo install --git https://github.com/w3-surfer/cloudiy cloudiy  (devs, needs Rust)

# 2. (optional) a Solana wallet to receive USDC payouts
solana-keygen new

# 3. Share (P2P is always on — no port-forwarding). Announce to a directory to
#    be discoverable, and require payment to only run against locked USDC.
cloudiy share --token my-secret --gpu-model "RTX 4090" \
  --directory <DirectoryNodeID> --require-payment --rpc-url https://api.devnet.solana.com
# 🚀 Node online · ID 9846…b1ec
```

To serve **container images** (not just kernels/models), install
[gVisor](https://gvisor.dev) and add `--runtime runsc`: consumer images are
refused under plain runc unless you explicitly accept the shared-kernel risk
with `--allow-runc-untrusted`. On multi-GPU machines, restrict what workloads
see with `--gpu-device 0`.

The token is set with `--token` / `CLOUDIY_TOKEN`; omit it and the node prints a
random per-session access code (compared in constant time). Request bodies are
capped and each worker runs hardened (cap-drop, no-new-privileges, memory/pid
limits, optional sealed egress) — see [`workers/README.md`](workers/README.md).

### Consumer — run a job

```bash
curl -fsSL https://cloudiy-cloud.vercel.app/install.sh | sh   # one binary, both roles

# Dial a provider directly (--to), or let the scheduler pick one (--via a directory)
cloudiy run --to <NodeID> --kernel vector_add --data "1,2,3;4,5,6" --token my-secret
cloudiy run --via <DirectoryNodeID> --kernel matrix_mul --data "2,2,2;1,2,3,4;5,6,7,8" --replicas 3

cloudiy status --to <NodeID> --job-id <id>
cloudiy info   --to <NodeID>
cloudiy canary --model llama-ep     # self-check that a served model answers honestly
```

Or from an app / AI agent, via the SDK — zero dependencies, and the result's
provider signature is **verified by default**:

```python
from cloudiy_sdk import CloudiyClient, PaymentRequired

client = CloudiyClient("node-host:8080")
try:
    r = client.submit(kernel="vector_add", data="1,2,3;4,5,6")
except PaymentRequired as quote:                 # x402 USDC quote
    r = client.submit(kernel="vector_add", data="1,2,3;4,5,6", payment=quote.demo_payment())
print(r.output_text, r.signature_verified)       # "5,7,9" True  — computed on a remote node
```

`as_tool_schema()` (Python/JS) emits an OpenAI/Anthropic function-tool
definition so any LLM agent can buy compute directly — see
[`sdk/python/examples/agent_tool.py`](sdk/python/examples/agent_tool.py). Agents
can also drive the whole flow (quote → pay → run → release) over **MCP**:
`cloudiy mcp`.

### CloudiyOS — the browser

Boot **[CloudiyOS](web/os.html)** (hosted at `/os`) and connect a Solana wallet:
your identity-bound VM with an App Store (templates + serverless repos), a
Hardware Store, Models (call chat/whisper per request), My Wallet and a
terminal. Point it at a local node with `cloudiy os --web-dir web` (then open
`http://127.0.0.1:4600/os.html`) to run models for real; served statically it
runs in a demo/preview mode.

## Verifiable settlement

The escrow releases USDC on a provider-signed result. The result signature binds
`(job_id, sha256(input), sha256(output))`, so it proves *which node produced
which output for which input* — checked off-chain by every SDK and on-chain by
`release_verified` ([RFC-0006 §4](docs/rfcs/RFC-0006-verifiable-settlement.md)).

That proves **provenance**, not **honest work**. On consumer hardware (no TEE,
no stake) that last mile is **economic**, not cryptographic:

- **Canary probes** — known-answer jobs, indistinguishable from real ones, catch
  a wrong/cheaper model or an altered prompt.
- **Reputation ramp** — a clean record earns bigger jobs; one caught cheat
  craters it. The directory serves signed, authoritative scores; consumers drop
  providers below a routing floor.
- **Redundancy** — high-value or deterministic jobs run on N providers and must
  agree (`--replicas N`).
- **Holdback** (on-chain, dormant) — an optional challenge window before payout.

One honest limit to know: **your prompt and inputs are visible to the provider
node that runs the job.** Signatures prove who produced what, not secrecy;
confidentiality from the provider needs attested (TEE) execution, which is on
the roadmap. Don't send secrets to a public endpoint.

In one line: **what was asked and what was returned are locked by cryptography;
whether it was honest work on the right model is verified statistically and
enforced economically.** Per-job mathematical certainty (zkML) isn't economical
for large models today — the design swaps it in later without touching the rest.
Full detail + the open governance decisions are in
[RFC-0006 §11](docs/rfcs/RFC-0006-verifiable-settlement.md).

## Development

```bash
git clone https://github.com/w3-surfer/cloudiy.git && cd cloudiy

cargo build          # provider + consumer CLI + libs
cargo test           # workspace test suite
cargo clippy         # lint

# Run a full local network in one command (directory + provider + gateway):
./scripts/run-local-network.sh
# …or piece by piece:
cargo run -p cloudiy -- directory
cargo run -p cloudiy -- share --bind 127.0.0.1:8080 --directory <id>
cargo run -p cloudiy -- os --web-dir web           # CloudiyOS at 127.0.0.1:4600/os.html
```

Workspace layout:

```
crates/
  protocol/   # protocol types — Identity, Resource, Capability, Workload, ProviderAnnouncement
  common/     # shared wire protocol, node keys, result/announcement/run-auth signing, wallet
  scheduler/  # placement engine — filters + weighted scorers (incl. reputation)
  runtime/    # execution backends — wgpu/WGSL kernels + Docker/OCI behind one Runtime trait
  sdk/        # cloudiy-sdk — Rust consumer library (P2P iroh, typed, signature-verified)
  cloudiy/    # the `cloudiy` binary: share · run · directory · os (gateway) · mcp · pay/release
sdk/
  python/     # cloudiy-sdk — zero deps, verifies result signatures, x402, agent tool schema
  js/         # @cloudiy/sdk — fetch-based, Node 18+/browser/edge, signature-verified
contracts/    # Anchor escrow program (devnet: 9zMBC7JD…c1TN) + TS tests
workers/      # containerized model workers (SDXL, LTX, TTS) — human-published to a registry
docs/         # RFCs (docs/rfcs), SECURITY-AUDIT.md, MAINNET-RUNBOOK.md
web/          # CloudiyOS (os.html), landing (index.html), docs (docs.html)
```

## Testing & CI

Unit tests cover the shared types, result/run-auth/announcement signatures,
reputation + canary logic, the scheduler, and the payment path. Every push runs
[GitHub Actions](.github/workflows/ci.yml) — `cargo fmt`, `cargo clippy -D
warnings`, `cargo build`, `cargo test`. The Anchor program has its own pipeline
([contracts.yml](.github/workflows/contracts.yml)) running `anchor build` +
`anchor test` on changes under `contracts/`. The Python/JS SDK verifiers are
checked against test vectors generated from the Rust signer.

```bash
cargo test --workspace          # Rust workspace
cd contracts && anchor test     # on-chain escrow program
python3 sdk/python/tests/test_verify.py && node sdk/js/test.mjs   # SDK crypto vectors
```

## Deploy

- **Web** — Vercel serves [`web/`](web) as a static site with clean URLs
  ([`vercel.json`](vercel.json)); `/vm` redirects to `/os`.
- **Contract** — the escrow redeploy is a human step:
  [`scripts/redeploy-escrow-devnet.sh`](scripts/redeploy-escrow-devnet.sh).
  Mainnet has its own runbook: [`docs/MAINNET-RUNBOOK.md`](docs/MAINNET-RUNBOOK.md).

## License

[Apache-2.0](LICENSE) © 2026 Cloudiy — see [NOTICE](NOTICE) for trademark and scope
