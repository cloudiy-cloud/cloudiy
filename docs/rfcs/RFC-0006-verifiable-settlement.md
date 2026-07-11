# RFC-0006 — Trust-Minimized Settlement Without Stake

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.1 |
| **Requires** | RFC-0001 (Vision), RFC-0005 (Scheduling), PROTOCOL.md, `docs/SECURITY-AUDIT.md` (HIGH-1) |
| **Reference implementation (today)** | `crates/cloudiy::payments`, `crates/common::sig`, `contracts/programs/cloudiy-escrow`, `crates/sdk`, `sdk/*` |

> **Positioning note.** This RFC answers the open design question behind
> **HIGH-1** in `docs/SECURITY-AUDIT.md`: `release_verified` proves *provenance*,
> not *delivery* — a funded provider can self-settle without doing the work. It
> defines how the network decides a job was really done and pays for it, given
> a deliberate hardware target. It is a design spec, not yet implemented.

---

## Abstract

How do we pay a provider for model inference **without the provider being able
to cheat** — run a cheaper model, alter the consumer's prompt, or return canned
output and still settle — when the network deliberately targets **consumer-grade
hardware and up**, uses **no stake**, and must **not add friction to becoming a
provider**?

There is no mechanism that gives *cryptographic impossibility* of cheating for
arbitrary LLM inference on hardware the provider owns. The honest, buildable
answer is **economic / game-theoretic security**: cheating is made *detectable
with high probability* and *unprofitable*, and cheaters are *self-eliminated*
over time. This RFC specifies that layer: a **reputation ramp** and an
**earnings holdback** (the "stake without stake"), a **canary** verification
mechanism, and the **cryptographic binding of input → model → output** that
makes the whole thing enforceable.

Explicit non-goal: prompt/output **confidentiality** from the provider. That
needs a hardware root of trust (TEE) and is out of scope here (see §8).

---

## 1. Problem & constraints

`release_verified` today releases escrowed USDC on a provider-signed result. The
signature proves *which node produced which output for which job* (provenance).
It does **not** prove the work was actually done, the requested model was used,
or the consumer's exact prompt was fed to it (delivery / correctness). A funded
provider can self-settle without doing the work.

The constraints that shape the solution (all decided in design):

- **Target hardware: consumer-grade and up.** This rules out requiring a TEE
  (confidential computing exists only on datacenter GPUs — H100/H200/Blackwell),
  because that would exclude the core provider audience.
- **No tiers.** One settlement mechanism for the whole network.
- **No stake.** No capital deposit required to become a provider.
- **No added access friction.** Onboarding a provider stays instant and free.

## 2. The trilemma, and the decision

For LLM inference you can have at most two of *{trust-minimized proof, any GPU,
LLM support}*:

| Single mechanism | Trust-minimized | Any GPU | LLM |
|---|:---:|:---:|:---:|
| TEE / confidential computing | ✅ | ❌ (datacenter only) | ✅ |
| Optimistic + reputation + audit | ⚠️ economic only | ✅ | ✅ |
| Deterministic quorum | ✅ | ✅ | ❌ (deterministic only) |

The constraints in §1 select the **economic** row: any GPU + LLM, at the cost of
an *economic* guarantee rather than cryptographic impossibility. This RFC makes
that row concrete. (Deterministic quorum still applies, for free, to the
deterministic wgpu kernels — see §5.2.)

## 3. What is verified — and what is not

You **cannot** cheaply verify "is this answer correct": the only way to know an
LLM's correct output is to run the model, which costs as much as the provider's
work. So the network never verifies every output. It verifies, **statistically**,
a different claim:

> *Is this provider running the requested model, honestly, on the consumer's
> exact input?* — measured on the subset of jobs where the right answer is known
> (canaries) or reproducible (redundancy).

Because a provider cannot tell a checked job from a paying one (§5.1), its
behavior on checked jobs is an unbiased estimator of its behavior on paying jobs.

## 4. Cryptographic substrate — binding input → model → output

> **Status: implemented (v2), including on-chain.** Changes 1–3 have landed —
> the run-auth and result signatures bind `sha256(input)` (domains bumped to
> `…/v2`), the provider verifies the run-auth over the received input, and the
> Rust/Python/JS SDK verifiers check the input. **The escrow contract's
> `release_verified` was updated to the v2 message too** (`RESULT_DOMAIN` v2 +
> `input_hash` param), so on-chain settlement now enforces
> output-for-*this-input*, not just output-provenance — `cloudiy release`, the
> MCP tool, `solana.rs`, the spoof harness and the TS tests all pass the input;
> the program compiles under `anchor build`. **Human step:** redeploy the
> program to devnet (new instruction ABI) — existing devnet escrows predate it.
>
> Change 4 (commit `input_hash` on-chain at *create_job*/funding) is
> intentionally **not** done: the exact input is only known at run time, not at
> funding time (a prepaid escrow funds before the prompt exists), so input
> binding belongs at settlement — which `release_verified` now does — rather than
> at funding. The rest of this RFC (canary→reputation wiring, holdback
> enforcement) remains.

**Original state (gap, verified in code before the change):**
- Consumer run-auth signature (`payments::run_auth_message`) covers
  `"cloudiy/escrow-run/v1" ‖ 0 ‖ job_id` — **the input is not bound.**
- Provider result signature (`common::sig::sign_result`) covers
  `"cloudiy/result/v1" ‖ 0 ‖ job_id ‖ 0 ‖ sha256(output)` — **input not bound.**
- `EscrowJob` on-chain stores `job_id, consumer, provider, mint, amount,
  deadline, state` — no input commitment; `job_id` is a random UUID.

Nothing signed pins the prompt, so today neither transit tampering nor a
provider swapping the prompt is detectable at the crypto layer.

**Change (do these in lockstep — breaking):**
1. Consumer computes `input_hash = sha256(input_data)`; run-auth becomes
   `"cloudiy/escrow-run/v1" ‖ 0 ‖ job_id ‖ 0 ‖ input_hash`. The consumer's
   signature now commits the exact prompt (defeats transit tampering — hard).
2. Provider recomputes `sha256(input_data)` on receipt and **rejects** if it
   does not match `input_hash`; it must acknowledge the exact bytes or refuse.
3. Result signature extends to
   `"cloudiy/result/v1" ‖ 0 ‖ job_id ‖ 0 ‖ input_hash ‖ 0 ‖ sha256(output)`,
   binding *input → output*: a signed result proves "for **this** input I
   produced **this** output" — what canaries and disputes need.
4. *(Optional, stronger)* commit `input_hash` on-chain in `EscrowJob` at
   creation, anchoring the binding on-chain (costs contract space + redeploy).

**What this gives:** transit integrity + non-repudiation + input↔output binding.
**What it does not:** it does **not** force the provider to actually feed that
input into the model on its own box — that stays the canary's job (§5.1). The two
halves together close the picture.

> Note: change (3) revises the result-signature format that the Python/JS SDK
> verifiers (RFC-shipped in `sdk/*`) already check, so their verifiers, test
> vectors and `crates/common/examples` generator must be updated together.

## 5. Verification mechanisms

### 5.1 Canary jobs (primary)
> **Status: evaluable core implemented** (`crates/cloudiy/src/canary.rs`): the
> canary bank, the tolerant/fingerprint comparison (exact / normalized-contains /
> number), the pass-fail verdict and a local self-probe (`cloudiy canary`) that
> runs the bank through the real worker. Validated against the served model
> (llama-3.2:1b: 4/4). **Now also probes remotes**: `canary::probe_remote` dials
> a provider over iroh and runs the bank as endpoint jobs; `cloudiy canary --to`
> exposes it, and a directory background prober periodically probes fresh
> providers and folds verdicts into the reputation registry (§6) — its own probes
> are the trust source (no external verdict submission to spoof). Unreachable /
> can't-pay probes are **skipped, never penalized** (couldn't-evaluate ≠ cheated).
> Validated over a real two-node run (dial → RunEndpoint → llama → 4/4).
> Remaining: *indistinguishable* injection needs real consumer traffic to blend
> into (active probing is distinguishable by pattern); who funds paid canaries is §10.

A bank of **reference `input → known-output` pairs** per model (run once through
the genuine model at `temperature=0`). These are **mixed into the provider's job
stream, indistinguishable** from real jobs — same format, same API path, same
size distribution — so the provider cannot tell which jobs are checked. On
return, the answer is compared to the reference:
- match → running the right model on the right input → reputation up;
- mismatch → wrong/cheaper model, altered prompt, or fake → reputation down +
  clawback (§6.2).

A wrong model **or** an altered prompt both corrupt the canary output, so this
one mechanism covers both the "wrong model" and "altered input" cheats.

**Comparison is tolerant, not byte-exact** (heterogeneous consumer GPUs produce
slightly different logits). Use:
- **low-entropy canaries** whose greedy answer is stable across hardware
  (`"Reply with only the number: 17+26" → "43"`);
- **model-discriminating canaries** — prompts where the claimed model and common
  cheaper substitutes diverge characteristically (detects *model identity*, not
  exact tokens; robust to hardware nondeterminism).

**Security-critical property: indistinguishability.** If canaries look different
(special prompts, different route, timing), a cheater passes them and cheats the
rest. Canaries must be drawn from the real traffic distribution — ideally *real
past consumer prompts with known-good answers* — and routed identically.

### 5.2 Redundancy (high-value jobs, no known answer)
Send the same job to N independent providers; compare with tolerant/semantic
match. Agreement → pay; disagreement → withhold, escalate to a third, penalize
the outlier. No known answer and no stake needed; cost is N×, so consumer-opt-in
for jobs that want it. Deterministic wgpu kernels get exact-match quorum for free.

### 5.3 Consumer probe (point of payment)
The consumer embeds a known-answer probe in its session, indistinguishable, and
releases escrow only if it passes. Verification at the moment of payment, with no
network involvement.

## 6. Incentive layer — without stake

> **Status: ramp core implemented** (`crates/cloudiy/src/reputation.rs`): the
> per-provider reputation model (slow climb on clean canaries, sharp crater on a
> failure), the tier gate (New/Building/Trusted/Veteran, gated on both score and
> history depth so trust can't be faked), the ramp policy (max job value ·
> canary rate · holdback duration per tier), and a `Registry` that folds canary
> verdicts (§5.1) into scores. `cloudiy canary` shows the resulting tier/ramp.
> Unit-tested (fresh=bottom, clean record climbs, one cheat craters a veteran,
> history gates the tier). **Now persisted and served authoritatively**: the
> `Registry` serializes to disk (atomic write, survives restarts); a directory
> node loads it and answers a new `Request::Reputation` with the canary-derived
> `(node_id, score)` map; the consumer's `fetch_providers` overrides each
> provider's *self-reported* reputation with the directory's authoritative score
> before scheduling, so the existing `HighReputation` scorer ranks on earned
> trust. The directory is trusted only for this ranking hint — payment safety is
> the escrow + signatures, never reputation. Remaining: fully-trustless
> reputation (on-chain, §10); scheduler *hard-gating* on `may_take`; holdback
> enforcement (§6.2 → the escrow challenge window).

### 6.1 Reputation ramp
Providers join instantly, free, at **zero trust**: small jobs only, high canary
rate, longer earnings holdback. A clean track record **earns** higher trust →
bigger jobs, lower canary rate, faster payout. The collateral is *earned
reputation*, not deposited capital — so "no stake, no access friction" holds; the
only thing that grows over time is *trusted capacity*, not a capital requirement.

### 6.2 Holdback / challenge window ("stake without stake")
Instead of an upfront deposit, a provider's **own pending earnings** sit in
escrow for a challenge window. A canary/audit failure inside the window claws
them back. The amount at risk is recent unpaid revenue — no external capital.

> **Status: on-chain mechanism implemented, disabled by default.** The escrow
> (`contracts/programs/cloudiy-escrow`) gained `created_at` on the job, a
> `CHALLENGE_WINDOW_SECS` constant (**default 0**), and two guards:
> `release`/`release_verified` refuse to settle before `created_at + window`, and
> `refund` gained a **clawback** path — a `CHALLENGE_AUTHORITY` may refund a job
> to the consumer *inside the window* (bounded to it, so it can't touch settled
> or out-of-window jobs). With the window at 0 this is a complete no-op — settle
> is immediate, clawback is inert — so nothing changes until a redeploy raises
> the window. Chosen over a two-phase `Releasing` state to avoid restructuring
> the atomic `close = consumer` payout lifecycle in deployed money code.
> Compiles under `anchor build`; off-chain parser unaffected (field appended
> last).
>
> **Two activation decisions remain (yours):** (1) the window length, and (2)
> **who the challenge authority is** — the open §10 oracle question. It defaults
> to the fee authority (a trust element); RFC-0006 §10 replaces it with a
> decentralized attester / on-chain reputation quorum before this should be
> switched on. Redeploy required to activate.

### 6.3 Sybil / whitewashing containment
Without stake, identities are cheap, so a banned cheater can spin up a fresh one.
Containment: a fresh identity **starts at zero** → reaches only small,
heavily-audited jobs → hit-and-run is bounded to low value and made *irrelevant*
by keeping entry jobs too small to be worth cheating. Whitewashing costs the
re-accumulation of reputation, not capital.

### 6.4 Game-theoretic basis
This is the repeated game / folk theorem: honesty is the profit-maximizing
strategy when

> `gain_per_cheat  <  discounted_future_earnings_if_honest  +  earned_reputation_lost`.

The collateral is the *future*, not capital — which requires identity to carry
value over time (earned reputation, §6.1). A provider that wants volume can only
profit by building reputation, and cheating burns it, so cheating is −EV. Because
canaries are indistinguishable, the provider's best response is to run the right
model on the right input **on every job**.

## 7. Honest limits (state these plainly)

- **Statistical, not absolute.** A one-shot cheat on a job that happens not to be
  a canary can slip through. It is bounded by the canary rate and holdback size;
  tune both until any single cheat is −EV. This is *not* cryptographic
  impossibility, and the design must never claim it is.
- **Confidentiality is not covered.** The provider can read the prompt/output.
  Only a TEE fixes this (§8).
- **Heterogeneous determinism.** No byte-exact re-execution across consumer GPUs;
  rely on tolerant + fingerprint comparison, not exact `output_hash` match.
- **Throwaway identity.** A zero-reputation identity has nothing to lose beyond
  the current job's payment; it can only be *denied payment* and *capped in
  reach*, not *punished*.

## 8. Optional future (not tiers)

- **Opportunistic TEE attestation** as a **reputation/priority bonus** — a
  provider that happens to have confidential-computing hardware can attach an
  attestation to rank higher. This is a *provider-side signal*, not a
  consumer-facing tier, so it does not violate "no tiers".
- **On-chain `input_hash` commitment** (§4.4).
- **zkML** when it becomes economical for large models.

## 9. Build list (mapped to current code)

**Crypto substrate (§4):**
- `crates/cloudiy::payments::run_auth_message` — add `input_hash`.
- Consumer signing sites — `crates/cloudiy::client`, `crates/cloudiy::mcp`, and
  the browser signer in `web/vm.html` — sign over `job_id ‖ input_hash`.
- Provider verify — `payments::verify_escrow` / `core` — check received input
  hashes to the committed `input_hash`.
- `crates/common::sig::{sign_result, verify_result}` — add `input_hash`; then
  update `sdk/python` + `sdk/js` verifiers, regenerate vectors, update tests.

**Reputation & holdback (§6):**
- A reputation registry (open question §10: signed append-only log on the
  directory node, or a lightweight on-chain reputation account).
- Escrow/contract: earnings holdback, challenge window, clawback instruction;
  optional on-chain `input_hash`.

**Canary subsystem (§5.1):**
- Canary bank generation per model; indistinguishable injection into the job
  stream; tolerant + fingerprint comparison; scoring → reputation update.

**Worker determinism (§5.1):**
- Pin `temperature=0`, fixed seed, pinned image/quantization so canaries are
  comparable.

## 10. Open questions

1. **Where does reputation live** so it is tamper-resistant without a stake
   chain? *(partly addressed)* The directory now **signs** its scores
   (`SignedReputation`, verified against the dialed directory) — non-repudiable
   and relay-safe. Fully trustless still wants an on-chain rep account / quorum.
2. **Who pays for canary compute?** *(control built, funding open)* A per-cycle
   probe cap (`CanaryBudget`, `CLOUDIY_CANARY_MAX_RUNS_PER_CYCLE`) bounds prober
   cost; the *funding source* for paid (`--require-payment`) providers — a
   protocol-fee pool or a funded prober wallet — is still to be chosen.
3. **Redundancy pricing and collusion resistance** (§5.2).
4. **Holdback duration vs. provider cash-flow** — tension with "no access
   friction"; how long can earnings sit before it deters honest providers?
5. **Cross-hardware canary thresholds** — how tolerant is tolerant, per model?

## 11. Closing: how delivery is verified end-to-end, and when to revisit

*(Decision record, July 2026 — closes the design question this RFC opened.)*

### The answer, layer by layer

There is no single "verify". Delivery is established by layers, each marked by
the kind of guarantee it gives — 🔒 cryptographic certainty, 📊 economic/
statistical assurance:

1. **🔒 The request is locked.** The consumer signs the exact input
   (`run_auth v3`: job_id ‖ sha256(input) ‖ expiry); the provider must
   acknowledge those exact bytes or refuse. Nothing in transit — and not the
   provider — can swap the prompt undetected.
2. **🔒 The result is bound.** The provider signs
   (job_id, sha256(input), sha256(output)); `release_verified` re-checks it
   on-chain. Non-repudiable proof of *which node produced which output for
   which input*. This does **not** yet prove honest work.
3. **📊 Honest work is verified statistically** — never by judging the real
   answer (unknowable cheaply), but by: **canaries** (indistinguishable
   known-answer jobs), **redundancy** (N providers, majority is truth — no
   known answer needed), and **model fingerprinting** (prompts where the
   claimed model and cheap substitutes diverge characteristically).
4. **📊 Reputation converts detection into deterrence.** Clean record climbs
   the ramp (bigger jobs, lighter audit, faster payout); one caught cheat
   craters it. Cheating is −EV for anyone who wants volume; cheaters
   self-eliminate.
5. **📊 Holdback (built, dormant)** adds per-job clawback for high-value work —
   deliberately inactive until a challenge authority is decided (§6.2/§10).

In one sentence: **WHAT was asked and WHAT was returned are locked by
mathematics; WHETHER it was honest work on the right model is verified
statistically and enforced economically — so the provider's only winning
strategy is to actually deliver.** Per-job mathematical certainty of honest
execution does not exist today for large-model inference on consumer hardware;
claiming otherwise would be dishonest.

### Alternatives considered and why not (now)

| Path | Why not now | What would change that |
|---|---|---|
| **opML / fraud proofs** (optimistic + on-chain bisection of a disputed step) | Requires bit-exact deterministic reference execution across verifiers — collides with the heterogeneous consumer-GPU fleet; adds challenge windows + watchers | Willingness to mandate a standardized deterministic runtime |
| **Mandatory deterministic runtime** (batch-invariant kernels → exact re-execution works) | Restricts what software/hardware a provider may run — against "any machine" | Could ship later as an *optional* certified-deterministic mode |
| **TEE everywhere** | Datacenter-only GPUs (H100+) — excludes the target audience | Already kept as an opportunistic reputation bonus (§8), never a tier |
| **zkML** (succinct proof of the full inference) | Proving cost is orders of magnitude above running the model — uneconomical for large LLMs today | Proving cost dropping ~100×; the §4 input/output binding is exactly the interface a zk proof would consume, so the swap is drop-in |
| **Consumer-consent release** | The consumer can grief (withhold consent for delivered work) | Rejected earlier in design |

### Revisit triggers

Re-open this design only when one of these becomes true:

1. **Recurring high-value jobs** → activate the holdback: pick the challenge
   window and a decentralized challenge authority (M-of-N attester federation
   first; the mechanism is signature-counting, not consensus).
2. **Willingness to standardize the runtime** → opML / exact re-execution
   becomes available as a verifiable mode.
3. **zkML proving costs collapse** → replace the statistical layer with
   mathematical certainty; everything else (escrow, binding, reputation,
   scheduling) is unchanged.
4. **Live consumer traffic at volume** → indistinguishable canary injection
   into real streams, and sampled redundancy (§5.2 B2/B3) funded per §10.

Until a trigger fires, further mechanism here is complexity without return —
the implemented stack (binding + canary + reputation + signed authoritative
scores + dormant holdback) is the honest ceiling for the chosen constraints.
