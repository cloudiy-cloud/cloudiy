# ☁️ Cloudify

**Decentralized cloud computing on Solana.** Cloudify's first product is **GPU-as-a-Service**: rent or provide high-performance GPUs with zero intermediaries, paid in **USDC** on the Solana network.

- 🌐 Website / app: static pages in [`web/`](web) (landing, marketplace, dashboard, docs)
- 🦀 Node software: Rust workspace in [`crates/`](crates), installed by providers with `cargo`
- ⚓ On-chain escrow: Anchor program (in development) in [`contracts/`](contracts)

## How it works

```
 Consumer (browser / CLI)                Provider (any machine with a GPU)
 ────────────────────────                ─────────────────────────────────
 1. Browse marketplace      ──────►      cloudify-provider serve
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
| Network | HTTP/JSON today · gRPC/QUIC planned ([`proto/cloudify.proto`](proto/cloudify.proto)) |
| Blockchain | Solana + Anchor + USDC escrow (devnet beta) |
| Execution | Provider node in Rust (CUDA/OpenCL dispatch on the roadmap) |

## Quick start

### Provider — share your GPU, earn USDC

```bash
# 1. Install (requires Rust: https://rustup.rs)
cargo install cloudify-provider

# 2. (optional) create a Solana wallet to receive USDC payouts
solana-keygen new

# 3. Start your node
cloudify-provider serve --token my-secret --gpu-model "RTX 4090"
# 🚀 Cloudify Provider Node running at http://0.0.0.0:8080
```

The auth token is set with `--token` or the `CLOUDIFY_TOKEN` env var. It is compared
in constant time, and request bodies are capped at 16 MiB to protect the node.
Set a real token in production — the default `cloudify-dev-token` is for local use only.

### Consumer — run a job

```bash
cargo install cloudify-consumer

cloudify-consumer submit \
  --server 127.0.0.1:8080 \
  --kernel vector_add \
  --data "1,2,3" \
  --token my-secret

cloudify-consumer status --server 127.0.0.1:8080 --job-id <id>
cloudify-consumer info   --server 127.0.0.1:8080
```

Or manage everything from the browser: connect a Solana wallet (Phantom) on the [marketplace](web/marketplace.html) and rent capacity directly.

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
git clone https://github.com/w3-surfer/cloudify.git
cd cloudify

cargo check          # type-check the workspace
cargo build          # build provider + consumer + common
cargo test           # run the test suite
cargo clippy         # lint
cargo run -p cloudify-provider -- serve --bind 127.0.0.1:8080

# Web: any static server
python3 -m http.server 3000 --directory web
```

Workspace layout:

```
crates/
  common/     # shared types (JobRequest/JobResponse/ProviderInfo) + Solana wallet helpers
  provider/   # cloudify-provider — node that shares a GPU
  consumer/   # cloudify-consumer — CLI to submit jobs
proto/        # gRPC service definition (future transport)
contracts/    # Anchor escrow program (in development)
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

[MIT](LICENSE) © Cloudify Team
