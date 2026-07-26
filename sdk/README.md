# Cloudiy SDKs

Cloudiy splits in two parts:

- **Node** (`cargo install --git https://github.com/cloudiy-cloud/cloudiy cloudiy`, then `cloudiy share`) — the full app GPU **providers** run: announces the hardware, executes jobs on the GPU (wgpu/WGSL), signs results, gets paid in USDC.
- **Client / SDK** (this directory) — lightweight libraries **consumers** embed: find nodes, submit workloads, track progress, fetch results. Built so AI agents can buy GPU compute with one function call.

No SDK is strictly required: every node speaks plain HTTP + [x402](https://solana.com/x402/what-is-x402) (`402 Payment Required` → pay in USDC → retry). The SDKs are sugar over that flow.

**Driving the network from an AI agent instead?** `cloudiy mcp` exposes the whole
flow (discover → pay → run → release) as MCP tools with spend caps and a devnet
default — see the [MCP quickstart](../docs/MCP-QUICKSTART.md). Each SDK also ships
a runnable "an agent rents compute" quickstart under `examples/`.

| Language | Package | Transport | Highlights |
|---|---|---|---|
| Rust | [`crates/sdk`](../crates/sdk) (`cloudiy-sdk`) | P2P (iroh QUIC, dial-by-NodeID) | typed API, verifies result signatures |
| Python | [`sdk/python`](python) (`cloudiy-sdk`) | HTTP | zero deps, **verifies result signatures by default**, `PaymentRequired` exception, agent tool schema |
| JavaScript | [`sdk/js`](js) (`@cloudiy/sdk`) | HTTP (fetch) | zero deps, Node 18+/browser/edge, **verifies result signatures by default**, agent tool schema |
| Go | [`sdk/go`](go) (`github.com/cloudiy-cloud/cloudiy/sdk/go`) | HTTP | zero deps (stdlib `crypto/ed25519`), **verifies result signatures by default**, agent tool schema |

## Result verification (on by default)

Every provider signs `(job_id, sha256(input), sha256(output))` with its node
key (the ed25519 key behind its iroh identity) — domain `cloudiy/result/v2`,
so the signature binds the output to the **exact input submitted**. **All four
SDKs verify that signature before returning output** — a tampered or unsigned
result raises an error
(`SignatureError` in Python/JS/Go, `SubmitError::BadSignature` in Rust) instead
of handing an agent forged data. The check is self-contained (no extra crypto
dependency — Python/JS ship a small ed25519 verify, Go uses stdlib
`crypto/ed25519`, so all stay zero-dependency). Pass `verify=False` /
`verify: false` / `NoVerify` to accept unsigned results from a trusted-local/demo
node, and `expect_pubkey` / `expectPubkey` / `ExpectPubkey` to pin the provider's
identity.

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

## Releasing

The three thin clients share **one version line** and ship from **one tag**.
The Rust workspace (the `cloudiy` node and the `crates/*` libraries) is a
separate line with its own `v*` tags, because the node and the clients ship on
different cadences — tagging one never releases the other.

```bash
scripts/bump-version.sh                  # what version are we on?
scripts/bump-version.sh 0.4.0            # set pyproject, __init__, package.json, cloudiy.go
# ...update CHANGELOG.md for 0.4.0...
git commit -am "release: SDKs v0.4.0"
git tag sdk-v0.4.0 && git push origin sdk-v0.4.0
```

Pushing the tag runs
[`release-sdks.yml`](../.github/workflows/release-sdks.yml): it re-runs the
signature vectors and the live-node e2e, refuses to continue if the tag
disagrees with the version in the source, builds the sdist/wheel/npm tarball,
publishes, and cuts a GitHub Release with the artifacts and that version's
CHANGELOG section. Every push also runs the same packaging steps as a dry-run
in CI, so a broken manifest surfaces long before a release.

**Publishing is inert until the tokens exist.** Each publish step is gated on
its secret; without it the step is skipped and the run prints a warning naming
the missing secret, while the artifacts still land on the Release. To turn
publishing on, add these under *Settings → Secrets and variables → Actions*:

| Secret | For |
|---|---|
| `PYPI_API_TOKEN` | `cloudiy-sdk` on PyPI ([project token](https://pypi.org/manage/account/token/)) |
| `NPM_TOKEN` | `@cloudiy/sdk` on npm (`npm token create`) |

Rehearse without releasing anything: run the **Release SDKs** workflow via
`workflow_dispatch` — it does everything except publish.

### The Rust crates release separately

`cloudiy-sdk` (Rust) is versioned with the **workspace**, not with the thin
clients, so it ships on the `v*` tags next to the node binaries — see the
`crates-io` job in [`release.yml`](../.github/workflows/release.yml), gated on
`CARGO_REGISTRY_TOKEN`.

They publish in dependency order, and the order is not cosmetic:

```
cloudiy-protocol  ->  cloudiy-common  ->  cloudiy-sdk
```

crates.io resolves the `version` on each path dependency from the registry, so
a crate cannot be published — or even `--dry-run`ed — until its dependencies
are really up there. `cargo publish --dry-run -p cloudiy-sdk` fails today with
*"no matching package named `cloudiy-common` found"*; that is expected, not a
misconfiguration. Only the leaf crate is dry-runnable ahead of the first
publish, which is exactly what CI checks.
