# Competitive positioning — cloudiy vs Nosana, io.net, Akash

*Last reviewed: July 2026. Competitor claims come from their public docs and market data; they change — keep this dated.*

## TL;DR

> **cloudiy is the only compute network where an AI agent buys GPU time with real dollars (x402/USDC) and payment releases only against a cryptographic proof of the result, verified on-chain.**

Every incumbent DePIN compute network is supply-heavy and demand-poor, monetizes through its own token, and settles on trust. cloudiy differentiates on the **demand side**: agent-native access, dollar settlement, and verifiable work.

## Where we are the same

All of cloudiy, Nosana and io.net (and Akash, one chain over) are decentralized compute marketplaces:

- Providers run a node binary and earn for compute time; consumers run containerized jobs.
- Solana-adjacent (Nosana and io.net are Solana projects; Akash is Cosmos).
- Template catalogs (hosted-hub style), inference endpoints, per-hour pricing under AWS/GCP.

## Where we are different (shipped, not roadmap)

| | **cloudiy** | Nosana | io.net | Akash |
|---|---|---|---|---|
| Payment asset | **USDC — real dollars** | NOS token | IO token | AKT token |
| Protocol fee | **4% on-chain** | token emissions | token emissions | up to 20% take rate (USDC) |
| Settlement | **result signature verified on-chain before payout** (`release_verified`) | trust-based | trust-based | trust-based |
| Hardware verification | result-level proof (the work itself is signed) | off-chain benchmark | off-chain PoW pings (added after the 2024 fake-GPU incident) | provider audits |
| Architecture | **true P2P** — iroh dial-by-identity, scheduler runs client-side, directories are untrusted relays of signed announcements | central backend + dashboard | central backend + dashboard | on-chain auction |
| Agent payments | **x402 pay-per-request + native MCP server** (`cloudiy mcp`) | none | none | none |
| Token risk | none — no token | NOS >90% off ATH | IO >90% off ATH; "IDE" emission engine added Jun 2026 | AKT volatility |

### 1. USDC-native, no token

Providers earn dollars, not exposure to an emissions schedule. io.net had to launch an "Incentive Dynamic Engine" (Jun 2026) to manage IO emissions; Nosana reworked NOS staking rewards toward usage. cloudiy has nothing to manage: 4% protocol fee, on-chain, done. (Akash's USDC path takes up to 20%.)

### 2. x402 — the 2026 tailwind

x402 (HTTP 402 + stablecoins) went from a Coinbase experiment to an industry standard: the x402 Foundation (Coinbase + Cloudflare), Stripe support (Feb 2026), AWS CloudFront/WAF first-party support, 150M+ transactions across Base and Solana. cloudiy quotes every job via x402 and settles USDC on Solana. Neither Nosana nor io.net supports x402.

### 3. Trustless settlement (`release_verified`) — unique

The provider signs `cloudiy/result/v1 ‖ job_id ‖ sha256(output)` with the ed25519 key that *is* its network identity. The escrow stores that key at `create_job`; `release_verified` makes the Solana Ed25519 precompile re-verify the signature in the same transaction before paying out. Permissionless settlement, payout fixed on-chain, adversarially tested on devnet (tampered signature and wrong-signer proofs rejected).

Competitors verify *hardware* off-chain and pay on trust. cloudiy verifies *the work* on-chain. After io.net's fake-GPU incident (Apr 2024) and with ~2% of its registered GPUs verified daily, "the chain checked the result" is a story no one else can tell.

### 4. Genuinely P2P

No backend: consumers dial providers by identity over iroh QUIC (NAT-punching, E2E-encrypted), the scheduler is a client-side filter+scorer pipeline (sovereign policy), and directories are stateless relays whose announcements are ed25519-signed by the originating node. Nosana and io.net route through their own dashboards and APIs.

### 5. Agent-native

- `cloudiy mcp` — the network as MCP tools (list/quote/pay/run/launch/release/refund) with spend caps and devnet-by-default. `claude mcp add cloudiy -- cloudiy mcp` gives any agent a budgeted compute wallet.
- Zero-dependency Python/JS SDKs with `as_tool_schema()` for function-calling LLMs and a typed `PaymentRequired` flow.

## Where they beat us (be honest)

- **Supply scale**: io.net claims 100k+ registered GPUs; Nosana has live markets with real pricing. cloudiy's network is nascent.
- **Clusters**: io.net does multi-GPU Ray clusters; cloudiy is single-node placements today.
- **Production maturity**: they are on mainnet with fiat rails; cloudiy escrow is devnet (mainnet blockers: contract audit, upgrade-authority multisig, mainnet USDC mint — see docs/MAINNET.md).
- **Reputation/staking**: both have staking and (weak) reputation; cloudiy's on-chain reputation is roadmap (RFC-0003 evolution).

Strategic read: DePIN compute is oversupplied and under-demanded. Racing them on GPU count is losing. Winning the **agent demand** (x402 momentum) with the only **verifiable settlement** story is a defensible wedge.

## Actions that follow

1. ✅ `cloudiy mcp` shipped — the agent on-ramp.
2. ✅ Landing "Why cloudiy" comparison + docs "Trustless settlement" and "AI agents & MCP" sections.
3. Next: clear mainnet blockers (audit, multisig, mainnet USDC), publish GPU worker images so public endpoints run on network nodes, on-chain reputation.

---

# Centralized GPU clouds, the incumbent to out-model

*Reviewed July 2026. The leading centralized GPU clouds are the reference for developer experience; the point of this section is what makes cloudiy structurally different, and what DX to borrow.*

## What the incumbent looks like

A **centralized GPU cloud**, "AWS for GPUs, cheaper and simpler." You sign up, get an API key, pay with a card or credits. The typical product line: per-second GPU pods, serverless autoscaling endpoints (sub-200 ms cold start, scale-to-zero, per-second billing), multi-GPU clusters over InfiniBand, a compliance tier (SOC 2 / HIPAA), a hub of GitHub-published deployable repos (manifests, presets, releases, instant rollback), and network volumes. Trust model: you trust their infrastructure.

## How cloudiy is disruptive, same UX, opposite model

| Axis | Centralized GPU cloud | cloudiy |
|---|---|---|
| Nature | Centralized company/cloud | **Open P2P protocol**, no central operator, dial-by-identity (iroh) |
| Supply | Datacenters/hosts vetted by the company | **Permissionless**, anyone runs `cloudiy share` and earns USDC |
| Payment | Fiat/credits, API keys, accounts | **USDC / x402, keyless, no signup**, pay-per-request |
| Agent demand | A coding-agent skills package that manages *their* resources | **Native `cloudiy mcp`**, the agent buys GPU with USDC, no API key |
| Trust | Trust the infra | **On-chain result-signature settlement** (`release_verified`) |
| Take rate | Company margin | **4% on-chain**, no token, no hyperscaler tax |
| Lock-in | Container-portable, single vendor | **A protocol others build on** (SDKs, MCP) |
| State | Lives in their infra | **Identity-bound VM**, state lives outside providers |

**Thesis:** the incumbent cannot exist without its company. cloudiy runs without any company. The incumbent is a product; cloudiy is the "TCP/IP of compute": permissionless supply, agent-native keyless demand, trustless settlement. Note we are *ahead* on the agent surface: the incumbents shipped coding-agent skills packages in 2026; cloudiy already has native MCP plus keyless plus USDC. Amplify it, don't copy it.

## What we learned and shipped

The incumbents are mature in **deploy and operations DX**; cloudiy had the disruptive model but was young in production tooling. This pass borrowed the DX, kept the model:

1. **Configurable deploy** (repo/template detail pages): version picker, GPU/disk choice, and environment variables (e.g. `JUPYTER_PASSWORD`, `HF_TOKEN`), matching the incumbent's deploy form.
2. **`cloudiy_deploy` MCP tool**: the agent deploys a template/image with env vars and pays keyless via escrow. Our answer to their skills package, one level deeper (no API key, on-chain settlement).
3. **"Zero idle, by design"**: reframes the cold-start trade-off (pay for idle *or* eat cold-start). x402 pay-per-request means there is no idle to bill. Messaged in the endpoint API tab and docs.
4. **Endpoint analytics + usage receipts**: a live per-endpoint strip (runs, USDC spent, avg latency, warm/cold) and a **My Wallet, Activity** tab with per-request compute spend, from a persisted receipt log.

## Still to borrow (roadmap)

- **Warm worker pool / autoscaling**: scheduler `ReservationFilter` plus a pool manager (RFC-0005); today placement is stateless.
- **Bring-your-own repo/image deploy**: a "Deploy your own" path (GitHub URL / OCI image) beyond the catalog.
- **Portable network volumes** (RFC-0004). Note: do **not** claim "no egress fees", RFC-0004 currently *charges* storage egress; reframe honestly.
- **Instant clusters**: multi-node gang scheduling (RFC-0005 roadmap).
- **Reliability/SLA + failover narrative**: quorum/replicas plus provider uptime exist; the story doesn't yet.

## Sources

- Centralized GPU cloud DX: the public product and documentation pages of the leading centralized GPU clouds (pods, serverless, hub publishing, per-second pricing, worker templates), 2026.
- Nosana: nosana.com, learn.nosana.com (GPU markets, deployments, NOS staking/tokenomics posts)
- io.net: io.net docs (Proof of Work, IO coin), Messari "Understanding io.net", ownyourmind.ai io.net review (registered vs daily-verified GPUs, IDE), Cointelegraph on the Apr 2024 GPU metadata attack
- x402: docs.cdp.coinbase.com/x402, solana.com/x402, The Defiant (AWS CloudFront x402), BlockEden (x402 Foundation), Stripe x402 preview (Feb 2026)
- Akash take rate: public Akash network economics docs
