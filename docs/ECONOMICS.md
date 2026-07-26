# Cloudiy — Economic Model (working draft v0.1)

> **Status: working draft for an owner decision.** This document exists because
> "how does the network sustainably reward every participant?" had no written
> answer — flagged by outside feedback (Jul 2026) as the project's weakest point.
> Sections 1–2 describe what the code **enforces today** (checked against the
> implementation); sections 4–6 are proposals with DECISION POINTS. Nothing here
> is a public promise until it is decided.

---

## 1. Value flows the code enforces today

| Flow | Mechanism | Where in the code |
|---|---|---|
| Consumer → escrow | USDC locked per job (`create_job`), or per VM lease (budget/rate) | `contracts/`, `crates/cloudiy/src/solana.rs`, `vm.rs` (lease + reaper) |
| Escrow → provider | `release`/`release_verified`: payout = price − fee | contract, 400 bps |
| Escrow → protocol | **4% fee** (400 bps) to `FEE_AUTHORITY` (a compile-time constant; today a hot key) | `PROTOCOL_FEE_BPS`, `FEE_AUTHORITY` in the contract |
| Replicas (RFC-0008) | Trustlessness costs N× the price: one escrow per replica; a divergent one is refunded after the deadline | `client.rs`, `sdk/quorum.rs` |
| VM metering | Prepaid: rate (node's USDC/h) × budget (escrow); a reaper stops an exhausted VM | `vm.rs` |

**What is NOT paid for today** (the holes):

1. **Directory nodes** — they store announcements, serve discovery and run canary
   probing (real CPU, bandwidth and reputation cost) and receive nothing.
2. **Idle supply** — a provider online without jobs earns nothing. There is no
   emission and no subsidy.
3. **Volume storage** (RFC-0009) — syncing/snapshotting state has no price.
4. **The fee's destination** — 4% accrues to a hot key with no declared purpose.

## 2. The design constraint (and why it is the wedge)

**No native token, by choice.** The competitive research (docs/COMPETITIVE.md)
shows the sector's pattern: Nosana (NOS), io.net (IO) and Render (RNDR) use a
token to subsidise supply; Akash charges a ~20% take rate in USDC. The DePIN
literature in the Colosseum corpus ("Why DePIN matters, and how to make it work")
describes the classic flywheel — a token pays for supply before demand exists —
and also its graveyard: mercenary supply that evaporates when emissions fall,
structural sell pressure, and demand that never arrives.

Cloudiy bets the other way: **a demand-pulled network with real USDC revenue from
the first job.** That bootstraps supply more slowly and is more honest as a
business. This document does not propose creating a token; it proposes making the
4% fee do work.

## 3. The organising principle: every network service is a paid Resource

The architecture manifest already says *everything is a Resource* and that
payment is x402. The economic consequence follows naturally: **discovery, storage
and reputation are not altruistic infrastructure — they are Resources sold over
the same rails** that sell compute. No new mechanism, no token: the protocol
already knows how to charge for a service.

## 4. Candidate models

### Model A — Protocol fee split (routing fee)
The 4% stops accruing and is split at settlement:
`provider payout 96% · the directory that brokered discovery 1% · treasury 3%`
(illustrative numbers). This needs attribution: the quote/announce carries the id
of the directory through which the consumer found the provider, and the split
happens on release (the contract already supports multi-payee via
`create_job_split` / RFC-0004).
- ✅ A directory becomes a business proportional to the volume it originates.
- ✅ Direct precedent in the corpus: stablecoin fee splits (validator rewards).
- ⚠️ Attribution is gameable (a directory can self-attribute); it needs the
  announce path signed — a protocol extension (candidate for RFC-0011).

### Model B — Discovery as an x402 service (preferred direction)
The directory charges a micro-fee via x402 for the discovery query (a consumer
pays ~0.0001 USDC per `Providers` call) and/or for the announce (a provider pays
for listing).
- ✅ Fully coherent with the "everything is a Resource" axiom; zero contract
  change — x402 already exists in the transport.
- ✅ Anyone can stand up a directory and compete on price/quality —
  decentralisation by market, not by altruism.
- ⚠️ Micro-payment friction on the first query (mitigable: first N queries free,
  charge only volume users — i.e. agents).

### Model C — Availability bonds (staking-lite in USDC)
A provider posts a small USDC bond; failed canary probes (reputation already
exists, RFC-0006 §6) can slash it, and the pool funds uptime rebates.
- ✅ Skin in the game without a token; improves supply quality.
- ⚠️ Slashing is the most delicate mechanism to get right (a false-positive
  canary means an unjust confiscation); high complexity for the current stage.
  **Defer.**

### Idle supply: the honest position
Do not subsidise it. The network is demand-led: a provider earns when it works,
and one-command onboarding makes it cheap to *join when there is demand*. That is
weaker than io.net in the short run and saner in the long run — and it should be
said that way, including in the pitch.

## 5. Sustainability sketch (orders of magnitude)

A directory costs roughly US$10–20/month (small VPS + bandwidth). Under Model B
at 0.0001 USDC/query, breakeven is ≈100–200k queries/month — agent volume, not
human volume, which is exactly the target audience. Under Model A with a 1%
routing fee, breakeven is ≈US$1,000–2,000/month of settled volume originated.
Both close at small scale; Model B closes sooner.

## 6. DECISION POINTS (owner)

- **E1 — Fee destination**: keep 4%? Split A (routing) or pure treasury? The
  destination must move off the hot key to a multisig **before** mainnet
  (already a blocker in MAINNET-RUNBOOK; the fee authority is compile-time).
- **E2 — Paid discovery (Model B)**: approve as the direction and specify it in
  an RFC? Recommendation: **yes** — it is the structural answer to "who pays for
  the directory" and it does not touch the contract.
- **E3 — Volume storage (RFC-0009)**: price the snapshot (USDC/GB·month, paid to
  the store operator) in volume v2, or let the operator absorb the cost while it
  is beta?
- **E4 — Bonds (Model C)**: drop for now or keep on the roadmap? Recommendation:
  roadmap, revisit post-mainnet.
- **E5 — Replicas in pricing**: the N× cost of quorum is the price of
  trustlessness; surface it as an explicit choice in CloudiyOS/the SDK
  (1× trusted vs N× proven)?

## 7. What this answers

The "economic model 6.5/10" was fair: the flows existed, the *network* did not.
With E1 and E2 decided, every participant has revenue: providers (jobs),
directories (paid discovery or a routing fee), store operators (E3), and the
protocol (a treasury with a purpose: audits, relays, bounties). No token, no
emission, and no promise the code does not enforce.
