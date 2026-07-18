# Cloudiy over MCP — quickstart

`cloudiy mcp` exposes the network as [MCP](https://modelcontextprotocol.io)
tools over stdio, so any MCP client (Claude Code, Claude Desktop, your own
agent) can discover providers, pay USDC escrow, run workloads, and release
payment against a verified result — **no API key, no dashboard**.

## 1. Install and point a client at it

```bash
cargo install --git https://github.com/w3-surfer/cloudiy cloudiy
```

**Claude Code** — `.mcp.json` in your project (or `~/.claude.json` for all projects):

```json
{
  "mcpServers": {
    "cloudiy": {
      "command": "cloudiy",
      "args": ["mcp", "--max-spend-usdc", "1.0", "--read-only"]
    }
  }
}
```

**Claude Desktop** — same block in `claude_desktop_config.json`
(macOS: `~/Library/Application Support/Claude/`), then restart the app.

Drop `--read-only` once you want the agent to actually spend. Start read-only.

## 2. The 8 tools

Five are read-only; three sign Solana transactions and are hidden by
`--read-only`.

| Tool | What it does |
|---|---|
| `cloudiy_list_providers` | List live providers from a directory (signed announcements, verified locally): node id, resources, capabilities, price, health |
| `cloudiy_quote` | A provider's x402 quote — price in USDC, mint, escrow program, payout pubkey |
| `cloudiy_run_job` | Run a kernel (`vector_add`, `matrix_mul`, `wgsl`) and get the **ed25519-signed** result |
| `cloudiy_launch` | Launch an OCI container workload; without `node_id` the client-side scheduler places it |
| `cloudiy_deploy` | Deploy a serverless worker — a template (`pytorch`, `ollama`, `comfyui`, `vllm`…) or an OCI image |
| 🔑 `cloudiy_pay_escrow` | Lock USDC on-chain for a provider; returns `escrow_account` + `job_id` |
| 🔑 `cloudiy_release_verified` | Release the escrow — the **contract re-verifies the provider's signature on-chain** before paying out |
| 🔑 `cloudiy_refund` | Refund an escrow after its deadline (consumer) or any time (provider cancelling) |

🔑 = requires transaction signing; omitted under `--read-only`.

The typical agent loop is: `list_providers` → `quote` → `pay_escrow` →
`run_job` → `release_verified`. The last step is the interesting one — the
escrow only pays out if the on-chain Ed25519 precompile agrees the provider
signed *this* output for *this* input, so releasing payment for work that
wasn't done is not a policy, it's a contract failure.

## 3. Safety flags

Money-moving defaults are deliberately conservative:

| Flag | Default | What it protects |
|---|---|---|
| `--read-only` | off | Exposes only the 5 discovery/run tools — no transaction-signing tools reach the model at all |
| `--max-spend-usdc` | `1.0` | Total USDC this **session** may lock into escrows; refuses once the running total would exceed it |
| `--max-per-job-usdc` | `0.25` | Ceiling for any **single** escrow |
| `--rpc-url` | `https://api.devnet.solana.com` | Devnet by default — play money |
| `--allow-mainnet` | off | Required to run against a mainnet RPC (see below) |
| `--keypair` | `~/.config/solana/id.json` | Payer keypair; override with `CLOUDIY_KEYPAIR` |

**The mainnet guard fails closed.** At startup the server checks the RPC
*host* — not a substring of the URL, since `mainnet-host/?x=devnet` would fool
that — and refuses to start unless the host is a recognized
devnet/testnet/localhost endpoint or you passed `--allow-mainnet`. An
unrecognized custom RPC counts as potentially-mainnet and needs the explicit
opt-in, so a typo can't silently move real USDC.

Spend caps are enforced server-side per session, not by prompting: exceeding
either cap returns an error to the model instead of signing. `cloudiy_pay_escrow`
reports `session_spent_usdc` and `session_cap_usdc` back on every call, so the
agent can see its own remaining budget.

## 4. Try it

With the server configured, ask your agent:

> List cloudiy providers and quote the cheapest one.

That path is entirely read-only. When you're ready to spend, drop
`--read-only`, keep devnet, and ask it to run a job and release payment — you
can watch the escrow settle against a verified signature.

## Related

- [`sdk/`](../sdk) — thin HTTP clients (Python, JS, Go) if you'd rather call the
  network directly than through an agent
- [RFC-0006](rfcs/RFC-0006-verifiable-settlement.md) — how the result signature
  binds `(job_id, sha256(input), sha256(output))` and what `release_verified`
  checks on-chain
