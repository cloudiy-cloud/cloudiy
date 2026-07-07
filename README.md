# ☁️ Cloudiy

**Decentralized cloud computing on Solana.** Cloudiy's first product is **GPU-as-a-Service**: rent or provide high-performance GPUs with zero intermediaries, paid in **USDC** on the Solana network.

- 🌐 Website / app: static pages in [`web/`](web) (landing, marketplace, dashboard, docs)
- 🦀 Node software: Rust workspace in [`crates/`](crates), installed by providers with `cargo`
- 🧩 Consumer SDKs (Rust · Python · JS) for apps and **AI agents** in [`sdk/`](sdk)
- ⚓ On-chain escrow: Anchor program deployed to devnet — [`contracts/`](contracts)

## Architecture: Node + SDK

Cloudiy is deliberately split in two parts:

1. **Node (provider)** — the full app GPU owners install: announces the hardware, receives jobs, executes them on the GPU, signs and returns results, receives USDC.
2. **Client / SDK (consumer)** — lightweight libraries ([`sdk/`](sdk), Rust/Python/JS today, Go planned): find available GPUs, reserve, submit workloads, track progress, download results.

Providers run the full app; developers and **AI agents** embed a small SDK — and since every node speaks HTTP + [x402](https://solana.com/x402/what-is-x402) (`402 Payment Required` → pay USDC → retry), an agent can integrate with zero dependencies:

```python
from cloudiy_sdk import CloudiyClient, PaymentRequired

client = CloudiyClient("node-host:8080")
try:
    result = client.submit(kernel="vector_add", data="1,2,3;4,5,6")
except PaymentRequired as quote:                 # x402 USDC quote
    result = client.submit(kernel="vector_add", data="1,2,3;4,5,6",
                           payment=quote.demo_payment())
print(result.output_text)                        # "5,7,9" — computed on a remote GPU
```

`as_tool_schema()` (Python/JS) emits a function-calling tool definition so any LLM agent can buy GPU compute directly — see [`sdk/python/examples/agent_tool.py`](sdk/python/examples/agent_tool.py).

## How it works

```
 Consumer (browser / CLI)                Provider (any machine with a GPU)
 ────────────────────────                ─────────────────────────────────
 1. Browse marketplace      ──────►      cloudiy-provider serve
 2. Rent GPU (USDC locked                (HTTP API: /health /info
    in Solana escrow)                     /submit /status/:job_id)
 3. Submit job              ──────►      4. Execute kernel on GPU
 6. Receive result          ◄──────      5. Sign + return output
        │                                        ▲
        └────── escrow releases USDC ────────────┘
```

| Layer | Stack |
|---|---|
| Application | Web dashboard, marketplace, docs (static HTML + Tailwind) |
| Network | HTTP/JSON today · gRPC/QUIC planned ([`proto/cloudiy.proto`](proto/cloudiy.proto)) |
| Blockchain | Solana + Anchor + USDC escrow (devnet beta) |
| Execution | Provider node in Rust (CUDA/OpenCL dispatch on the roadmap) |

## Quick start

### Provider — share your GPU, earn USDC

```bash
# 1. Install (requires Rust: https://rustup.rs)
cargo install cloudiy-provider

# 2. (optional) create a Solana wallet to receive USDC payouts
solana-keygen new

# 3. Start your node
cloudiy-provider serve --token my-secret --gpu-model "RTX 4090"
# 🚀 Cloudiy Provider Node running at http://0.0.0.0:8080
```

The auth token is set with `--token` or the `CLOUDIY_TOKEN` env var. It is compared
in constant time, and request bodies are capped at 16 MiB to protect the node.
Set a real token in production — the default `cloudiy-dev-token` is for local use only.

### Consumer — run a job

```bash
cargo install cloudiy-consumer

cloudiy-consumer submit \
  --server 127.0.0.1:8080 \
  --kernel vector_add \
  --data "1,2,3" \
  --token my-secret

cloudiy-consumer status --server 127.0.0.1:8080 --job-id <id>
cloudiy-consumer info   --server 127.0.0.1:8080
```

Or manage everything from the browser: boot [CloudiyOS](web/vm.html) — your identity-bound virtual machine with App Store, Hardware Store and terminal.

## Provider HTTP API

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Liveness + uptime |
| `GET` | `/info` | Node info: pubkey, GPU model, VRAM, jobs completed |
| `POST` | `/submit` | Submit a job (`JobRequest` JSON, requires `auth_token`) |
| `GET` | `/status/:job_id` | Job status + progress |

Invalid tokens get `401 Unauthorized`.

## Development

```bash
git clone https://github.com/w3-surfer/cloudiy.git
cd cloudiy

cargo check          # type-check the workspace
cargo build          # build provider + consumer + common
cargo test           # run the test suite
cargo clippy         # lint
cargo run -p cloudiy-provider -- serve --bind 127.0.0.1:8080

# Web: any static server
python3 -m http.server 3000 --directory web
```

Workspace layout:

```
crates/
  common/     # shared types, wire protocol, node keys, result signing, wallet helpers
  sdk/        # cloudiy-sdk — Rust consumer library (P2P iroh, typed, signature-verified)
  cloudiy/   # the `cloudiy` binary — share (provider) + run/status/info (consumer CLI)
sdk/
  python/     # cloudiy-sdk for Python — zero deps, PaymentRequired/x402, agent tool schema
  js/         # @cloudiy/sdk — fetch-based, Node 18+/browser/edge
proto/        # gRPC service definition (legacy/reference)
contracts/    # Anchor escrow program (deployed to devnet: 9zMBC7JD…c1TN)
web/          # landing page, marketplace, dashboard, docs
```

## Testing & CI

Unit tests cover the shared types (serde round-trips), the wallet helpers, and the
provider's token/kernel logic. Every push and pull request runs
[GitHub Actions](.github/workflows/ci.yml) with `cargo fmt`, `cargo clippy -D warnings`,
`cargo build`, and `cargo test` across the workspace.

```bash
cargo test --workspace
```


## Deploy (web)

The site is deployed on Vercel as a static site; [`vercel.json`](vercel.json) serves the `web/` directory with clean URLs.

## License

[MIT](LICENSE) © Cloudiy Team
