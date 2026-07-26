# RFC-0008 — Replicated Settlement: `run --replicas N` with real escrow

| | |
|---|---|
| **Status** | Shipped — `run --replicas N --pay` (one escrow per replica), quorum in `crates/sdk/src/quorum.rs`, verified e2e on devnet |
| **Version** | 0.1 |
| **Requires** | RFC-0005 (Scheduling), RFC-0006 (Verifiable Settlement) |
| **Contract change** | **None.** Reuses `create_job` / `release_verified` as-is. |
| **Reference implementation** | `crates/cloudiy::client` (quorum path), `crates/sdk` (pure tally helper) |

---

## Abstract

`run --replicas N` executes a deterministic kernel on N independent providers and
accepts the output only if a strict majority returns the *same* signed bytes.
Today it refuses to run with `--escrow`: payment is modelled as one escrow for
one job, and a quorum is N jobs.

This RFC closes that gap with **one Job/escrow per replica**. It needs no on-chain
change, and it is the setting in which permissionless `release_verified` is
*legitimately* safe — the audit's HIGH-1 direction (c): restrict trustless
settlement to replicated/deterministic workloads where a third party can compare
results.

It also states plainly what quorum does **not** buy: it protects the *answer*, not
the *money* of a divergent replica. See §5.

---

## 1. Why one escrow per replica (and why the alternative doesn't exist)

An escrow pins, at `create_job` time, **both** the provider's payout pubkey and
the provider's `provider_node_key` (the key `release_verified` checks the result
signature against). A single escrow therefore names exactly one provider. N
replicas means N providers, so it means N escrows. There is no "one escrow, many
providers" shape without a contract change, and we don't want one (§3).

A consequence that decides the CLI surface: **the consumer cannot pre-fund the
escrows.** Replicas are chosen by the scheduler from the directory at run time,
so the provider set isn't known until targets resolve. Funding must therefore
happen *after* target resolution, inside the run — which is why the quorum path
gets a `--pay` flag rather than N `--escrow`/`--job-id` pairs.

## 2. Flow

```
1. resolve N targets from the directory            (existing resolve_targets)
2. for each target i:  create_job  → (escrow_i, job_id_i)   ← pins provider_i
3. for each target i:  submit(job_id_i, escrow_i, run_auth_sig_i)
4. verify each result signature locally            (SDK, already)
5. tally sha256(output) over signature-verified results; majority wins
6. for each provider in the winning set: release_verified(escrow_i, …)
7. everyone else: no release → consumer refunds after the deadline
```

Each replica's run-auth signature binds `job_id_i ‖ sha256(input) ‖ expiry` (v3),
and each provider signs `RESULT_DOMAIN ‖ job_id_i ‖ sha256(input) ‖ sha256(output)`.
Because every replica has its own `job_id`, the per-replica signatures are
naturally domain-separated: provider A's result proof cannot settle provider B's
escrow.

**Funding is all-up-front, fail-fast.** All N escrows are created before any
execution starts, so a funding failure (insufficient USDC, RPC error) aborts
before compute is spent. If funding fails partway, the already-funded escrow
accounts are printed with refund instructions — they are recoverable after the
deadline, never lost.

**Deadline.** The escrow timeout must cover N sequential runs plus settlement.
Providers already refuse escrows whose deadline is within `WORKLOAD_TIMEOUT + 120s`
(audit finding A2), so the default is kept generous and scales with N.

**Cost.** N replicas cost N × price. The CLI prints the total before funding.

## 3. Why no contract change

`create_job` already takes a per-job `job_id` and `provider_node_key`;
`release_verified` already validates a per-job signature and pays a payout fixed
on-chain. N independent jobs are just N independent uses of existing
instructions. The quorum is a **client-side policy over N independent
settlements** — nothing about it needs to be on-chain for each settlement to be
individually correct, because each escrow can only ever pay the one provider it
named.

Putting quorum on-chain (an "N-of-M job" instruction) would mean a program
upgrade, a larger trusted surface, and an on-chain notion of "the right answer" —
precisely the oracle problem RFC-0006 §10 defers. We don't need it, so we don't
do it.

## 4. What this buys

Quorum makes permissionless `release_verified` *honest*. The audit (HIGH-1) found
that `release_verified` proves provenance, not delivery: a funded provider can
sign a hash of anything and self-settle. The audit's own carve-out is that it is
"genuinely safe when the output is independently verifiable — a deterministic
kernel plus `run --replicas N` quorum". This RFC is that carve-out becoming real
with money attached:

- the consumer gets an answer **corroborated by a majority of independent
  providers**, not one provider's word;
- divergence is **attributable** — the CLI names which providers disagreed, which
  is the input the RFC-0006 reputation ramp needs to crater a cheat's score.

## 5. Honest limit: quorum protects the answer, not the liar's escrow

**The stated trade-off "a divergent replica gets no release, the consumer refunds
after the deadline" is only true for a passive or honest-but-faulty provider.**

`release_verified` is permissionless (`payer: Signer` — *any* caller, contract
`ReleaseVerified`). A **malicious** provider does not need the consumer to
release: it can submit `release_verified` for its *own* escrow, with its own
signature over its own (wrong) output, and get paid — regardless of whether it
agreed with the quorum. The consumer cannot prevent this and cannot refund an
escrow that has already settled.

So, precisely:

| Divergent replica behaviour | Outcome |
|---|---|
| Passive / faulty (never settles) | No release; consumer refunds after deadline ✅ |
| Malicious (self-settles) | Provider is paid its replica's amount ❌ |

What the consumer still gets in the malicious case: the **correct output** (from
the majority) and the **identity of the liar**. What it loses is bounded at *one
replica's price* — it is not a drain, and it is not a redirection (payouts stay
pinned to the provider the consumer chose).

The deterrent is economic and lives in RFC-0006: a caught divergence craters the
provider's reputation score, which gates the job value it may take at all. Trust
is the collateral.

**The clean fix already exists in the contract and is currently disabled**:
`CHALLENGE_WINDOW_SECS` (RFC-0006 §6.2) is `0`, so settlement is immediate and the
clawback path in `refund` is inert. With a non-zero window, a divergent
provider's self-settlement would sit in a holdback long enough for a challenge to
claw it back — turning the ❌ row above into ✅. Activating it needs the §10
decision on *who the challenge authority is*, which is out of scope here and must
not be rushed (a centralized challenge authority would be a worse trade than the
current bounded loss).

This RFC therefore ships quorum-with-payment as a **result-integrity** guarantee,
and explicitly does not claim it recovers funds from a malicious replica.

## 6. CLI surface

```
cloudiy run --kernel <k> --data <d> --via <directory> \
            --replicas N --pay [--amount <usdc>] [--release] \
            [--keypair <path>] [--rpc-url <url>]
```

- `--pay` — fund one escrow per replica automatically (after target resolution).
  Only meaningful with a directory; mutually exclusive with `--escrow`/`--job-id`,
  which remain the single-provider, pre-funded path.
- `--release` — after a quorum is reached, settle each **agreeing** provider via
  `release_verified`. Without it, the run reports the escrows and leaves
  settlement to the consumer.
- Divergent / unsigned / unreachable replicas are never released by us; their
  escrow accounts are printed with refund instructions.

## 7. SDK

`crates/sdk` gains a **pure, unit-testable tally**: given
`(node, output, signature_verified)` triples and a threshold, return the winning
output hash, the agreeing set, the divergent set and the unsigned set. Pure
because the interesting failure modes (ties, all-unsigned, no majority) deserve
tests without a network. Agents embedding the SDK get the same quorum policy the
CLI uses instead of re-implementing it.

## 8. Build list

1. `crates/sdk`: `quorum::tally` + tests (pure).
2. `crates/cloudiy::client`: per-replica funding, per-replica run-auth, quorum via
   the SDK helper, per-provider `release_verified`, refund reporting.
3. `crates/cloudiy::main`: `--pay` flag; drop the `--replicas` + escrow block.
4. E2E on devnet: 2 providers (distinct `HOME`s), 2/2 quorum with a real test-mint
   payment; adversarial case: a divergent replica gets no release from us.

## 9. Open questions

1. **Challenge window** — activating `CHALLENGE_WINDOW_SECS` closes §5's malicious
   row. Blocked on RFC-0006 §10 (who is the challenge authority). Not this RFC.
2. **Replica count vs cost** — N × price with no discount. A "pay the majority
   only" model would need the contract to hold one pot for N providers, i.e. a
   program change. Deferred deliberately.
3. **Partial quorum settlement** — today a failed quorum releases nothing. An
   alternative (release the plurality anyway) is rejected: it would pay for an
   answer we don't trust.
