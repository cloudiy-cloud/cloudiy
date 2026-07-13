# RFC-0007 — Pricing & Matching: the Protocol Posts the Price

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.4 |
| **Requires** | RFC-0001 (Vision), RFC-0005 (Scheduling), RFC-0006 (Verifiable Settlement) |
| **Reference implementation (today)** | **phase 1 shipped:** `crates/protocol::pricing` (`PricingTable`, CU metering), gateway `/api/quote` quotes the posted price, `web/os.html` derives card + quote prices from the same table. Still pending: `crates/scheduler` default policy (phase 2), provider announcement demotion |

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
   no competition to discover a price, and any reactive mechanism has no traffic
   to learn from. Endogenous supply/demand pricing drives the price down until it
   stops being **worth providing compute** before the network reaches critical
   mass.

This RFC replaces provider-set pricing with a **protocol-posted, per-request,
compute-metered price**. At launch that price is **stable**: a cost-plus number
anchored to an external cost base, revised only by a deliberate governance (later
oracle) action, never moving per request or per market tick. A **dynamic demand
premium** on top is explicitly **deferred** to a later phase, gated by a revisit
trigger (real scarcity plus enough traffic to calibrate it). Providers do not
price each call: they decide whether to **participate** at the posted price, and
they compete on **reputation, latency and uptime**, not price.

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
- **Stable and predictable at launch.** In a thin market, a price both sides can
  count on is what attracts both sides. No per-request or per-tick movement.
- **Viable in a thin market.** Few users during bootstrap must not push the price
  below the cost of providing compute, or supply leaves.
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
price. That participation (or its absence) is the supply signal that informs how
governance revises the posted price (§3.2), and it is a cleaner signal than a
per-call bid.

## 3. The pricing model: a stable posted price

At launch the posted price is stable and cost-plus:

```
posted_price(model) = cost_per_CU(model) × margin      (margin > 1)
```

It is metered per request by compute consumed (§3.1) and anchored to an external
cost base (§3.2). **"Stable" means it does not move per request or per market
tick.** It is revised only by a **deliberate, announced governance action**
(later, an oracle update) when the underlying cost actually changes, for example
when GPU spot prices fall.

Two parameters of this formula are **decided** (v0.3):

- **`margin` is a single, transparent, network-wide multiplier**, the same for
  every model, held as a **governance parameter** and revised rarely. It is not a
  free number: `margin = 1 + overhead_fraction + target_profit`, where
  `overhead_fraction` is the real non-compute cost (idle between jobs, retries on
  failure, bandwidth, orchestration) and `target_profit` is the profit the
  network guarantees an honest provider. With overhead ≈ 20% and a ~15-20% profit
  target, the initial value is **≈ 1.4×**. Per-class margins are a later option
  only if measured overhead diverges by class.
- **`cost_per_CU` decomposes into a GPU-market cost and the model's measured
  compute (§3.2)**, priced against a fixed reference GPU class so the number stays
  uniform regardless of which provider serves the request.

Predictability is the point. In a thin market, a price a provider can count on
and a consumer can count on is what attracts both, far more than a price
"optimized" by a mechanism that has no traffic to learn from yet. A reactive
price with no data oscillates, and a race-to-the-bottom price drives supply out.
Stable-and-cost-plus avoids both.

The **dynamic demand premium** (raising price above cost-plus when supply is
scarce) is **deferred**: see §3.3.

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

### 3.2 The cost base: two orthogonal axes

`cost_per_CU` is **not** a single opaque number per model. It **decomposes** into
a market axis and a physical axis (decided, v0.3):

```
cost_per_CU(model) = gpu_class_cost_per_hour(ref_class) / 3600 × compute_per_CU(model)
                     └────────── market axis ──────────┘         └─── physical axis ───┘
```

- **`gpu_class_cost_per_hour(class)`** is a small table keyed by GPU class (a
  consumer 4090, an A100, an H100, ...), on the order of 5-10 entries total. It is
  objective market data (the spot cost of that GPU class), so it is exactly what
  an **oracle** can feed, and it is shared by every model at once.
- **`compute_per_CU(model)`** is how much compute the model consumes per CU
  (GPU-seconds per 1k tokens, per generation, per second of audio). It is a
  **measured physical property**, benchmarked once per model and updated only when
  the model or its implementation changes. It is not a price and needs no vote.

**Priced against a fixed reference class.** A model runs at different
GPU-seconds on a 4090 vs an H100, so the price is computed for a chosen
`ref_class`, **not** the class the job happens to land on. This keeps the posted
price **uniform and stable** no matter which provider serves it. A provider on
faster hardware than the reference simply earns more margin; on slower hardware,
less. Hardware efficiency becomes the **provider's margin lever, not the
consumer's price**, which pulls better hardware onto the network without breaking
uniform pricing.

**The reference class is the consumer class (RTX 4090 tier), decided v0.4.**
This follows RFC-0006's hardware target ("consumer-grade and up"): the price must
be calibrated to the network's *typical* provider, not its best one. Anchoring on
a datacenter class would post a per-GPU-second cost most providers do not have,
squeezing the typical provider's margin below target while overpaying the few
with datacenter hardware. GPU-generation upgrades to the reference class are a
governance revision like any other cost change.

**Implementation refinement (phase 1).** Some catalog models physically cannot
run on the consumer class at all (video generation needs datacenter VRAM). For
those, `compute_per_CU` is undefined on the 4090 tier, so each model is priced
against its **minimum viable class** (the consumer class whenever it runs
there). The property that matters is preserved: the price is still **uniform per
model**, every consumer pays the same number, and hardware above the model's
pricing class remains the provider's margin lever.

**Benchmark process for `compute_per_CU` (decided v0.4).** A canonical, public
benchmark harness per model class (fixed prompts/shapes; model *and* runtime
versions pinned) runs N times on reference-class hardware; `compute_per_CU` is
the **median** GPU-seconds per CU. Re-benchmark only on a model/runtime version
bump. Attestation is phased like everything else: at devnet, governance runs it
and publishes the harness + inputs so anyone can reproduce and contest; later, an
attester set re-runs the same harness and the median of signed observations
wins (the RFC-0006 attester shape). RFC-0006 canaries double as free continuous
re-validation: they are known-answer jobs running in production whose timing
exposes `compute_per_CU` drift.

Why decomposed (vs an explicit per-model price table):

- **A new model is a benchmark, not a governance vote:** measure its
  `compute_per_CU` once; the market axis is already shared.
- **When the GPU market moves, all model prices recompute automatically** by
  updating ~10 class entries, instead of re-voting N per-model prices.
- **Minimal oracle surface:** the oracle attests ~10 objective GPU-class costs,
  not N model prices, which is smaller and far harder to manipulate.

**Sourcing the market axis, phased:**

| Source | How | Trade-off |
|---|---|---|
| **Governance table** | the ~10 GPU-class costs maintained and revised by governance | trivial at devnet, no oracle; manual at first |
| **GPU-cost oracle** | an oracle publishes each class cost as a median of public-cloud spot feeds | anchored in real cost, manipulation-resistant as a median; needs an oracle |
| **Incumbent peg** | (fallback) sanity-check against the same model's public-API price × a discount | market-validated; depends on third parties, less sovereign |

**Recommendation: start on the governance table for the ~10 class costs, designed
to be swapped for the GPU-cost oracle.** The decomposition and the posted-price
formula do not change; only the source of the market axis changes. A revision is
a deliberate governance action, not a market movement, which is what "stable"
requires.

### 3.3 Deferred: a dynamic demand premium

**Not in the launch design.** Once there is real supply/demand and enough traffic
to calibrate, a premium `≥ 1` may later be layered on top of the cost-plus base:

```
posted_price(model, t) = cost_per_CU(model) × margin × premium(model, t)   (premium ≥ 1)
```

driven by supply scarcity (fill rate), clamped to some `M_max`, and only ever
pushing the price **up**, never below the cost-plus base. It is deferred because:

- a reactive mechanism with **no traffic oscillates** (raises and drops price on
  noise), and
- a thin market keeps the premium at ~1 anyway, so it buys nothing at launch.

**Revisit trigger:** sustained scarcity on a model (fill rate below target at the
posted price) with enough volume to tune `M_max` and the tracking speed without
oscillation. Until then, the response to scarcity is a deliberate governance
revision of the base (§3.2), not an automatic premium.

### 3.4 Worked example (illustrative)

A chat model. Reference GPU class cost (governance/oracle): 1.20 USDC/GPU-hour.
Measured compute: 0.9 GPU-seconds per 1k tokens. Global margin: 1.4.

```
market axis   = 1.20 / 3600      ≈ 0.000333 USDC / GPU-second
cost_per_1k   = 0.000333 × 0.9   ≈ 0.0003   USDC / 1k tokens   (market × physical)
posted        = 0.0003 × 1.4     ≈ 0.00042  USDC / 1k tokens   (× margin, stable)
```

The consumer sees this per-1k-tokens number on the card. It changes only when
governance revises a GPU-class cost (or the model is re-benchmarked), not per
request and not with demand. A provider serving this on an H100 (faster than the
reference class) does the work in fewer GPU-seconds and keeps the difference as
extra margin; the consumer still pays the same posted number.

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
  against the posted price, so a consumer can still refuse a price.
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
posted price. It is no longer shown to consumers and no longer decides placement.

## 6. Settlement integration

The settlement quote is the posted price, not a provider quote:

- `Settlement::quote(workload, provider)` (`crates/protocol/src/settlement.rs`)
  returns a `PaymentQuote` whose **price is `posted_price(model) × CU_count`**,
  with `payee = provider`, asset = USDC.
- The x402 flow, escrow, and `release_verified` (RFC-0006) are unchanged: the
  quote is paid into escrow and released on the signed, verified result. Only the
  **source of the number** changes (posted price, not `ENDPOINTS[].price` or a
  provider hourly rate).
- In `web/os.html`, the card price and the x402 quote both read the posted price
  for that model, so the sticker price and the charged price are the same number
  by construction.

## 7. Interaction with RFC-0006 (why cost-plus matters twice)

The cost-plus posted price reinforces economic security on both sides:

- **It closes the underprice-to-cheat vector.** Nobody can undercut an honest
  provider, because price is uniform and cost-plus: a cheater cannot express its
  lower real cost as a lower price to win jobs. Its only lever left is
  reputation, which the canary/ramp machinery (RFC-0006 §5, §6) is designed to
  destroy on the first caught cheat.
- **It guarantees the honest provider a margin.** Honesty is sustainable, not
  charity: the posted price covers the honest cost of doing the real work plus a
  margin, so there is no economic pressure to cut corners to stay solvent.

## 8. Honest limits (state them plainly)

- **The anchor is a trust point, split by nature.** The price has an **objective**
  part (the ~10 GPU-class costs, measurable market data) and a **policy** part
  (the `margin`). Whoever controls them can move the price. Mitigation: the
  objective part decentralizes first, becoming a **median of several feeds
  attested by a reputation/attester set** (the shape RFC-0006 wants for its
  challenge authority); the `margin` stays the last governance lever because it is
  a policy choice, not a measurement. Both stay governance-updatable, never
  hardcoded.
- **A stable price cannot react to real-time scarcity.** Until the deferred
  premium (§3.3) exists, a suddenly-hot model cannot price to ration demand or
  pull supply in real time; the response is a deliberate governance revision,
  which is slower. Accepted at launch for predictability and to avoid oscillation
  with no data.
- **Uniform price forgoes consumer price competition.** Consumers do not get
  undercut prices. In exchange they get a predictable price, a verified honest
  provider, and a supply base that does not evaporate. For a commodity under
  economic-only security, that is the better trade.
- **Metering is consumer-verifiable by construction, with one residue.** The CU
  units of §3.1 were chosen to be observable at the edge: the consumer can count
  tokens locally from its own input + output with the tokenizer, knows the
  duration of its own audio, and image/video are fixed-shape with CU = 1. The
  network never bills on a provider's *claimed GPU-seconds*, only on CUs the
  consumer can check, so there is nothing for a provider to inflate. The residue
  is interrupted/partial streaming, bounded by the x402 quote cap
  (`max_tokens` etc.): settlement charges `min(claimed, client-verified, cap)`.

## 9. Build list (mapped to current code)

| # | Change | Where |
|---|---|---|
| 1 | New **pricing layer**: `posted = (gpu_class_cost(ref)/3600 × compute_per_CU) × margin`, stable; `gpu_class_cost` a ~10-entry governance table, `margin` a single global governance scalar (no surge at launch) | new `crates/pricing` (or `crates/protocol::pricing`) |
| 2 | **CU metering** per model class (tokens / generation / second) + a **`compute_per_CU` benchmark** per model against the reference class | `crates/protocol::workload`, benchmark harness |
| 3 | `Settlement::quote` returns **posted price × CU**, not a provider rate | `crates/protocol/src/settlement.rs`, `crates/cloudiy::payments` |
| 4 | Default marketplace policy **drops `CheapestPrice`/`SpotScorer`**, weights `TrackRecordScorer` + `NearbyScorer` + `UptimeScorer` | `crates/scheduler/src/lib.rs` |
| 5 | `max_price_micro_usdc` compared against **posted price** (ceiling only) | `crates/scheduler`, `crates/protocol::workload` |
| 6 | Provider announces **capability + participation**; `price_micro_usdc_per_hour` demoted to internal cost hint | `crates/protocol::provider`, `crates/cloudiy::client` |
| 7 | Frontend card price and x402 quote both read the **posted price** | `web/os.html` (`ENDPOINTS`, quote path) |
| 8 | (later) swap governance table for a **GPU-cost oracle** (median of feeds) | pricing layer, no structural change |
| 9 | (triggered) optional **dynamic demand premium** above the cost-plus base (§3.3) | pricing layer, no structural change |

## 10. Decisions & open questions

**Decided (v0.3):**

- **Margin (§3).** A **single, network-wide, transparent multiplier**, same for
  every model, held as a **governance parameter**, `margin = 1 + overhead +
  target_profit`, initial value **≈ 1.4×**. Per-class margins are a later option
  only if measured overhead diverges by class. The path to decentralization is:
  keep `margin` as the last governance lever (it is policy, not measurement).
- **Base granularity (§3.2).** **Per GPU-class cost × the model's measured
  compute**, priced against a fixed **reference class**, not a per-model price
  table. New models need only a benchmark; a market move updates ~10 class
  entries and recomputes all prices.

**Decided (v0.4):**

- **Reference class + benchmark (§3.2).** The pricing reference is the
  **consumer class (RTX 4090 tier)**, per RFC-0006's hardware target: calibrate
  to the typical provider, not the best one. `compute_per_CU` comes from a
  **canonical public harness** (versions pinned, median of N runs on the
  reference class), re-run only on version bumps; governance-published and
  reproducible at devnet, attester-set medians later. Canaries provide free
  continuous drift detection.
- **Metering trust (§8).** Resolved by construction: CUs are consumer-verifiable
  (tokens countable locally, audio duration known, fixed-shape CU = 1), and the
  network never bills on claimed GPU-seconds. Residue (partial streaming) is
  bounded by the quote cap: settlement charges `min(claimed, client-verified,
  cap)`.
- **Oracle format (phase 3; source list stays open).** A **trimmed median of at
  least 3 public spot/on-demand feeds** per GPU class, normalized to USDC/hour,
  published as signed attester observations; low revision cadence (weekly, or on
  large deviation) with a **step guard** (max ±20% per revision) to preserve
  stability. The concrete feed list is picked at phase 3.
- **Deferred-premium parameters (phase 4, all governance-adjustable).**
  Introduce the premium for a model only on **fill rate < 95% sustained for 14+
  days AND volume ≥ ~1000 req/day** (below that there is no data to calibrate);
  clamp `M_max = 2.0`; premium updates at most daily with a **±10%/day step
  limit** to prevent oscillation.

**Still open:** only the phase-3 concrete oracle feed list, and whatever
implementation feedback surfaces. Nothing blocks phase 1.

## 11. Evolution

| Phase | Delivers |
|---|---|
| 0 (today) | Provider hourly rate + `CheapestPrice`; flat per-model card price reused as quote |
| 1 | Pricing layer with a **stable cost-plus posted price** (governance base); per-request CU metering; settlement quote reads posted price |
| 2 | Default policy drops price scorers; matching is reputation × latency × uptime |
| 3 | Base migrates to a **GPU-cost oracle** (median of feeds, attester set); provider rate fully demoted to internal hint |
| 4 (triggered) | Optional **dynamic demand premium** above the cost-plus base, once scarcity is real and calibratable (§3.3) |
