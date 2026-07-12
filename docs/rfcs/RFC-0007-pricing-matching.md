# RFC-0007 — Pricing & Matching: the Protocol Posts the Price

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.1 |
| **Requires** | RFC-0001 (Vision), RFC-0005 (Scheduling), RFC-0006 (Verifiable Settlement) |
| **Reference implementation (today)** | `crates/scheduler`, `crates/protocol::{workload,provider,settlement}`, `crates/cloudiy::payments`, `web/os.html` (`ENDPOINTS` catalog + x402 quote path) |

> **Positioning note.** RFC-0005 says the scheduler must never become a price
> oracle: scorers read announced prices, they do not set them. This RFC decides
> the other half of that sentence: if the scheduler does not set the price, who
> does. The answer is the protocol, through a dedicated pricing layer, not the
> provider and not the scheduler. This is a design spec, not yet implemented.

---

## Abstract

Who sets the per-request price for model inference on the network? The reference
implementation lets each **provider** announce a rate (`price_micro_usdc_per_hour`)
and the scheduler can prefer the cheapest. That promotes price competition, but
for the network we are building it optimizes the wrong thing:

1. Inference on a given model is a **commodity** (same model, same output), so
   provider-set prices collapse toward marginal cost (Bertrand competition).
2. Under RFC-0006 security is **economic, not cryptographic**. The cheapest bid
   is disproportionately the **cheater** (a smaller/quantized model, an altered
   prompt, canned output all have lower real cost), so "pick the cheapest" and
   "pick the most likely to cheat" become nearly the same filter.
3. In a **thin market** (few providers, or few users during bootstrap) there is
   no competition to discover a price. Endogenous supply/demand pricing drives
   the price to the floor and it stops being **worth providing compute** before
   the network reaches critical mass.

This RFC replaces provider-set pricing with a **protocol-posted, per-request,
compute-metered price** built as `posted = floor × surge`, where the **floor is
anchored to an external cost base** (so it never collapses in a thin market) and
the **surge only ever rises above the floor** when supply is scarce. Providers
do not price each call: they decide whether to **participate** at the posted
price, and they compete on **reputation, latency and uptime**, not price.

Explicit non-goal: this RFC does not change *what* is verified (RFC-0006) or the
placement engine mechanics (RFC-0005). It defines the price the settlement quote
carries and the default matching policy.

---

## 1. Problem & constraints

Today, three things about price live in the code:

- **`provider.price_micro_usdc_per_hour`** (`crates/protocol/src/provider.rs`):
  the provider's announced hourly rate.
- **`CheapestPrice` scorer** and the **`price_ceiling` filter**
  (`crates/scheduler/src/lib.rs`): the scheduler can rank by low price and drop
  nodes above the consumer's `max_price_micro_usdc`.
- **`ENDPOINTS[].price`** (`web/os.html`): a static, per-model list price shown
  on the Models cards, reused verbatim as the x402 quote amount in the app path.

So there are two disconnected prices (a provider hourly rate the backend can
match on, and a flat per-model sticker price the frontend charges), and the unit
is wrong for serverless inference (per hour, not per request).

The constraints that shape the solution (decided in design):

- **Per request, not per hour.** Serverless inference bills the actual work, not
  wall-clock rental. This is also the product's stated model ("zero idle").
- **The protocol posts the price.** Providers are price takers, not price
  makers.
- **Viable in a thin market.** Few users during bootstrap must not push the
  price below the cost of providing compute, or supply leaves.
- **Do not weaken RFC-0006.** Pricing must not reopen the underprice-to-cheat
  vector.
- **Do not turn the scheduler into a price oracle** (RFC-0005 invariant). The
  pricing authority is a separate layer; the scheduler still only reads a price.
- **Stay decentralizable.** Any external anchor must be replaceable by a
  decentralized source, not permanently a single key.

## 2. Decision: the protocol posts the price

Provider-set pricing is removed from the matching path. The network operates
like every real inference marketplace (Replicate, Together, Bedrock) and like
ride-hailing supply matching: **a price is posted per model, nobody bids per
request, and supply competes on quality of service.**

The provider's price signal does not vanish, it **moves from per-request to
enter/exit**: a provider decides whether to serve a given model at the posted
price. That participation (or its absence) is the supply signal that feeds price
discovery (§3.3), and it is a cleaner signal than a per-call bid.

## 3. The pricing model

```
posted_price(model, t) = floor(model) × surge(model, t)         with surge ≥ 1
```

`floor` never depends on internal demand, so in a thin market `surge → 1` and the
price rests at the floor, which is cost + margin: always worth providing.

### 3.1 Unit of metering

Price is per request, metered by **compute actually consumed**, not per hour:

| Model class | Compute unit (CU) | Posted as |
|---|---|---|
| Chat / language | 1k tokens (in + out) | price per 1k tokens |
| Image (diffusion) | one generation at a declared resolution/step budget | flat price per request |
| Video | one clip at a declared length/resolution budget | flat price per request |
| Audio (TTS / STT) | second of audio in or out | price per second, or flat per request for bounded jobs |

For fixed-shape jobs (one image, one clip) the CU count is 1, so the posted price
is a flat per-request number, which is exactly what the Models cards already
show. Chat is the one class that is genuinely metered by usage (per 1k tokens),
which the catalog already labels.

### 3.2 The floor (external anchor), phased

The floor is **cost-plus**, anchored outside the network so it cannot collapse:

```
floor(model) = cost_per_CU(model) × margin      (margin > 1)
```

`cost_per_CU` is grounded in the real cost of producing that compute. Three
sources, from most robust to simplest:

| Source | How | Trade-off |
|---|---|---|
| **GPU-cost oracle** | an oracle publishes a reference cost/hour for the GPU class (median of public-cloud spot prices), converted to per-CU via the model's measured compute | anchored in real production cost, manipulation-resistant if it is a median of several feeds; needs an oracle |
| **Incumbent peg** | posted = price of the same model on a public API × a discount factor | easy, market-validated, transparent; depends on third parties who change prices, and is less sovereign |
| **Governance table** | a per-model reference maintained and adjusted by governance | trivial at devnet, no oracle; manual and centralized at first |

**Recommendation: start on the governance table, designed to be swapped for the
GPU-cost oracle.** The `floor × surge` structure does not change; only the source
of `floor` changes. This ships now without an oracle and without becoming a
hostage to a third-party API peg.

### 3.3 The surge (demand premium), above the floor only

`surge(model, t)` is a multiplier `≥ 1` driven by supply scarcity for that model:

- Supply abundant relative to demand (the bootstrap case): `surge = 1`, price at
  the floor. Providers still profit because the floor is cost + margin.
- Supply scarce at peak: `surge > 1`, up to a clamp `M_max`. This is the only
  place demand touches price, and it can only push **up**, never below the floor.

Discovery is via **entry/exit plus fill rate**, not per-request bidding: if the
floor is set too low for a model, providers stop serving it, fill rate drops, and
governance/oracle raises the floor. If it is too high, idle supply accumulates and
it is lowered. This is the surge mechanism of ride-hailing, applied to the floor
parameter rather than to each call.

### 3.4 Worked example (illustrative)

A chat model, reference GPU class at an oracle-median 1.20 USDC/GPU-hour, measured
at 0.9 GPU-seconds per 1k tokens, margin 1.4:

```
cost_per_1k = 1.20 / 3600 × 0.9 ≈ 0.0003 USDC
floor       = 0.0003 × 1.4      ≈ 0.00042 USDC / 1k tokens
posted (calm market, surge 1.0) ≈ 0.00042
posted (scarce peak, surge 1.8) ≈ 0.00076
```

The consumer sees a stable per-1k-tokens number on the card; it only moves up
under real scarcity, and never below a cost-plus floor.

## 4. Matching: compete on reputation, latency, uptime, not price

With a single posted price per model, price is uniform across providers, so:

- **`CheapestPrice` becomes a no-op** in the default marketplace policy (all
  candidates carry the same posted price). It is dropped from the default
  weights, not deleted from the engine (RFC-0005 keeps scorers pluggable).
- **`SpotScorer`** (prefer nodes discounting idle capacity) is likewise off by
  default, because providers do not discount, they participate or not.
- The default policy for a Models request becomes, in RFC-0005 terms:

```rust
Pipeline::new()
    .filter(ResourceFit).filter(CapabilityMatch).filter(Healthy)
    .scorer(TrackRecordScorer, 0.5)   // earned reputation (RFC-0006 receipts)
    .scorer(NearbyScorer,      0.3)   // latency
    .scorer(UptimeScorer,      0.2);  // reliability
```

- **`max_price_micro_usdc`** stays as a consumer ceiling filter, now compared
  against the posted price, so a consumer can still refuse a surged price.
- The scheduler still **reads** the posted price; it does not set it. The pricing
  layer (§3) is the authority. This preserves the RFC-0005 invariant.

Net effect: the only way a provider wins more jobs is to be **more reputable,
closer, and more available**, which is exactly the incentive RFC-0006 wants,
because reputation, not price, becomes the asset a provider accrues.

## 5. The provider's decision

A provider no longer announces a per-request rate. It announces:

- **capability + capacity** (which models it can serve, VRAM, throughput), and
- **participation**: for each served model, "I will serve at the posted price."

`provider.price_micro_usdc_per_hour` is **demoted** from a matching input to, at
most, an **internal cost hint** the pricing layer can use when calibrating the
floor/surge. It is no longer shown to consumers and no longer decides placement.

## 6. Settlement integration

The settlement quote is the posted price, not a provider quote:

- `Settlement::quote(workload, provider)` (`crates/protocol/src/settlement.rs`)
  returns a `PaymentQuote` whose **price is `posted_price(model, t) × CU_count`**,
  with `payee = provider`, asset = USDC.
- The x402 flow, escrow, and `release_verified` (RFC-0006) are unchanged: the
  quote is paid into escrow and released on the signed, verified result. Only the
  **source of the number** changes (posted price, not `ENDPOINTS[].price` or a
  provider hourly rate).
- In `web/os.html`, the card price and the x402 quote both read the posted price
  for that model, so the sticker price and the charged price are the same number
  by construction.

## 7. Interaction with RFC-0006 (why the floor matters twice)

The cost-plus floor reinforces economic security on both sides:

- **It closes the underprice-to-cheat vector.** Nobody can undercut an honest
  provider, because price is uniform and floored: a cheater cannot express its
  lower real cost as a lower price to win jobs. Its only lever left is
  reputation, which the canary/ramp machinery (RFC-0006 §5, §6) is designed to
  destroy on the first caught cheat.
- **It guarantees the honest provider a margin.** Honesty is sustainable, not
  charity: at the floor the honest cost of doing the real work is covered plus a
  margin, so there is no economic pressure to cut corners to stay solvent.

## 8. Honest limits (state them plainly)

- **The anchor is a trust point.** Whoever sets `cost_per_CU` (governance table
  now, oracle later) can move the price. Mitigation: at devnet it is a
  governance parameter; to decentralize, the floor becomes a **median of several
  feeds attested by a reputation/attester set**, the same shape RFC-0006 wants
  for its challenge authority, and it stays governance-updatable, never hardcoded.
- **Price discovery has lag.** Adjusting the floor by fill rate is slower than a
  live auction. This is an accepted trade for stability and for not handing the
  price to a race to the bottom.
- **Uniform price forgoes consumer price competition.** Consumers do not get
  undercut prices. In exchange they get a predictable price, a verified honest
  provider, and a supply base that does not evaporate. For a commodity under
  economic-only security, that is the better trade.
- **Metering must be honest too.** Per-token / per-second metering is itself a
  claim the provider makes. It is bounded by the same canary/redundancy checks as
  the output (a wildly wrong token count on a canary is a caught cheat), but exact
  metering verification is out of scope here and shares RFC-0006's limits.

## 9. Build list (mapped to current code)

| # | Change | Where |
|---|---|---|
| 1 | New **pricing layer**: `posted_price(model, t) = floor × surge`, `floor` from a governance table, `surge` from fill rate | new `crates/pricing` (or `crates/protocol::pricing`) |
| 2 | Define **CU metering** per model class (tokens / generation / second) | `crates/protocol::workload` |
| 3 | `Settlement::quote` returns **posted price × CU**, not a provider rate | `crates/protocol/src/settlement.rs`, `crates/cloudiy::payments` |
| 4 | Default marketplace policy **drops `CheapestPrice`/`SpotScorer`**, weights `TrackRecordScorer` + `NearbyScorer` + `UptimeScorer` | `crates/scheduler/src/lib.rs` |
| 5 | `max_price_micro_usdc` compared against **posted price** (ceiling only) | `crates/scheduler`, `crates/protocol::workload` |
| 6 | Provider announces **capability + participation**; `price_micro_usdc_per_hour` demoted to internal cost hint | `crates/protocol::provider`, `crates/cloudiy::client` |
| 7 | Frontend card price and x402 quote both read the **posted price** | `web/os.html` (`ENDPOINTS`, quote path) |
| 8 | (later) swap governance table for a **GPU-cost oracle** (median of feeds) | pricing layer, no structural change |

## 10. Open questions

1. **Who governs the floor at devnet, and how is `margin` set?** A single
   parameter now; what is the path to a decentralized attester set?
2. **Surge function shape.** What is `M_max`, and how fast should the floor
   track fill rate (to avoid oscillation)?
3. **Per-model vs per-class floors.** Do we anchor each model, or a GPU class
   times a model's measured compute? The latter scales better.
4. **Oracle source set.** Which public feeds compose the GPU-cost median, and how
   are they attested on-chain without leaking a manipulable single source?
5. **Metering trust.** How far do we verify claimed token/second counts beyond
   the canary bound, if at all?

## 11. Evolution

| Phase | Delivers |
|---|---|
| 0 (today) | Provider hourly rate + `CheapestPrice`; flat per-model card price reused as quote |
| 1 | Pricing layer with **governance floor + surge**; per-request CU metering; settlement quote reads posted price |
| 2 | Default policy drops price scorers; matching is reputation × latency × uptime |
| 3 | Floor migrates to a **GPU-cost oracle** (median of feeds, attester set); provider rate fully demoted to internal hint |
