# Cloudiy SDKs

Cloudiy splits in two parts:

- **Node** (`cargo install --git https://github.com/w3-surfer/cloudiy cloudiy`, then `cloudiy share`) — the full app GPU **providers** run: announces the hardware, executes jobs on the GPU (wgpu/WGSL), signs results, gets paid in USDC.
- **Client / SDK** (this directory) — lightweight libraries **consumers** embed: find nodes, submit workloads, track progress, fetch results. Built so AI agents can buy GPU compute with one function call.

No SDK is strictly required: every node speaks plain HTTP + [x402](https://solana.com/x402/what-is-x402) (`402 Payment Required` → pay in USDC → retry). The SDKs are sugar over that flow.

| Language | Package | Transport | Highlights |
|---|---|---|---|
| Rust | [`crates/sdk`](../crates/sdk) (`cloudiy-sdk`) | P2P (iroh QUIC, dial-by-NodeID) | typed API, verifies result signatures |
| Python | [`sdk/python`](python) (`cloudiy-sdk`) | HTTP | zero deps, **verifies result signatures by default**, `PaymentRequired` exception, agent tool schema |
| JavaScript | [`sdk/js`](js) (`@cloudiy/sdk`) | HTTP (fetch) | zero deps, Node 18+/browser/edge, **verifies result signatures by default**, agent tool schema |
| Go | planned | HTTP | — |

## Result verification (on by default)

Every provider signs `(job_id, sha256(output))` with its node key (the ed25519
key behind its iroh identity). **All three SDKs verify that signature before
returning output** — a tampered or unsigned result raises an error
(`SignatureError` in Python/JS, `SubmitError::BadSignature` in Rust) instead of
handing an agent forged data. The check is self-contained (no extra crypto
dependency, so Python/JS stay zero-dependency). Pass `verify=False` /
`verify: false` to accept unsigned results from a trusted-local/demo node, and
`expect_pubkey` / `expectPubkey` to pin the provider's identity.

## 60-second agent integration (Python)

```python
from cloudiy_sdk import CloudiyClient, PaymentRequired

client = CloudiyClient("127.0.0.1:8080")

def cloudiy_gpu_run(kernel: str, data: str) -> str:
    try:
        return client.submit(kernel=kernel, data=data).output_text
    except PaymentRequired as quote:          # x402: node quoted its USDC price
        pay = settle(quote)                   # escrow create_job / x402 payload
        return client.submit(kernel=kernel, data=data, payment=pay).output_text
```

`as_tool_schema()` returns an OpenAI/Anthropic-style function-tool definition, so any function-calling LLM can invoke the network directly — see [`python/examples/agent_tool.py`](python/examples/agent_tool.py).

## Rust (P2P, signature-verified)

```rust
use cloudiy_sdk::{Client, SubmitOptions};

let client = Client::connect("<node-id>").await?;
let result = client
    .submit(SubmitOptions::kernel("matrix_mul", "2,2,2;1,2,3,4;5,6,7,8").token("code"))
    .await?;                                  // Err(PaymentRequired(quote)) if unpaid
assert!(result.signature_verified);           // ed25519 proof of which node computed it
```
