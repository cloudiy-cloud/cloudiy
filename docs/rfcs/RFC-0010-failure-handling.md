# RFC-0010 — Failure Handling

| | |
|---|---|
| **Status** | Normative (describes shipped behavior; findings tracked inline) |
| **Version** | 0.1 |
| **Requires** | RFC-0006 (Verifiable Settlement), RFC-0008 (Replicated Settlement), RFC-0009 (Persistent Volume) |
| **Contract change** | **None.** This RFC documents and audits; it does not change on-chain code. |
| **Summary** | How Cloudiy behaves when things break — provider crash, consumer disappearance, payment/settlement failure, network partition — and the one invariant that holds across all of them: *funds are never locked past the escrow deadline.* |

---

## 0. Why this document exists

Every other failure story in Cloudiy is a paragraph inside another RFC or a
comment next to the code that implements it. That's fine until a skeptical infra
engineer — or a second implementation — asks "what happens when X fails?" and has
to reverse-engineer the answer from five files. This RFC consolidates it, with
**the current code as the normative reference** and a citation on every claim, so
the answer is auditable in one place.

It is also an **active audit**: every claim below was checked against the tree at
the time of writing. Where the code was undefined, inconsistent, or untested,
it's marked **[FINDING]** and, if the fix was small and safe, fixed in the same
branch (noted `[fixed]`); larger ones are left as `[follow-up]`.

## 1. The invariant

> **I1 — Funds are never locked beyond the escrow deadline.**
> An escrow `Job` is in exactly one of three states — `Active (0)`, `Released (1)`,
> `Refunded (2)` (`contracts/programs/cloudiy-escrow/src/lib.rs:23`). From
> `Active`, the consumer can **always** reclaim the full amount by calling `refund`
> once `now >= job.deadline` (`lib.rs:216`, `consumer_timeout`). `create_job`
> forces `60 ≤ timeout ≤ MAX_TIMEOUT_SECS` (`TimeoutTooShort`/`TimeoutTooLong`),
> so a bounded deadline always exists. `Released`/`Refunded` are terminal and
> mean the money already resolved — not stuck.

Everything below is measured against I1. A path that could violate it is a
finding. **One documented carve-out exists** (F-08 malicious self-settle, §6);
no *new* violation was found.

**The universal backstop.** Independent of every failure mode in this document,
an `Active` escrow is refundable by its consumer after the deadline. When you
reach the end of a failure branch and ask "where's the money?", the answer is
almost always "in the vault, refundable after the deadline" — that is the design
working, not a bug.

## 2. Provider fails during execution

### 2.1 Two timeouts, two paths

| Path | Timeout | Const | Consumer sees |
|---|---|---|---|
| Kernel `submit` (`submit_guarded`) | 60s | `JOB_TIMEOUT_SECS` (`core.rs:21`) | `Response::Error{ "job timed out after 60s" }` (`core.rs:627`) |
| Workload `run_workload` (containers + WGSL templates) | 300s default, or `spec.max_duration_secs.min(3600)` | `WORKLOAD_TIMEOUT_SECS` (`core.rs:695`) | `Response::Job{ status:"error", error_message:"workload timed out after Ns" }` (`core.rs:845-861`) |

The effective workload budget: `if spec.max_duration_secs > 0 { it.min(3600) } else { 300 }`
(`core.rs:824-828`) — a consumer request is honored but hard-capped at one hour.

On the workload path, `runtime.destroy` runs unconditionally ("Destroy no matter
what", `core.rs:852`) and `state.resources.lock().release(&spec.resources)` frees
the reservation (`core.rs:866`) whether the job succeeded, failed, or timed out.
On the kernel path, the GPU permit deliberately lives inside the `spawn_blocking`
task (`core.rs:621-624`), so it's released when the work actually finishes even
though the caller was already cut loose at 60s.

**[FINDING F-01 — inconsistent failure shape] `[follow-up]`.** The two paths report
failure differently: `submit_guarded` returns `Err(String)` → `Response::Error`,
while `run_workload` returns `Ok(SubmitOutcome::Completed(job))` with
`status:"error"`. A consumer that wants to detect "did my job fail?" must handle
**both** a top-level `Response::Error` and a `Response::Job` whose `status ==
"error"`. Not a money bug (I1 holds — see 2.2), but a wire-contract wart worth
unifying. Left as follow-up because unifying it touches the response type used by
both fronts and the SDKs.

### 2.2 Money state on execution failure

The escrow is marked *served* (`state.served_escrows.insert`, `core.rs:479`)
**right after on-chain verification succeeds, before execution**. So a job that
then times out or crashes has already consumed the escrow's one admission on this
provider. But the on-chain escrow is untouched — no `release` happened — so it
stays `Active` and the consumer refunds after the deadline (I1). 

**Normative consequence (by design, not a finding):** an escrow funds *exactly one
execution attempt*. A transient provider-side failure burns that attempt; to retry,
the consumer refunds (after deadline) and funds a fresh escrow. This is the A1
anti-replay posture (`core.rs:445-455`), not a defect — but it is the behavior an
operator must expect, so it is stated here explicitly.

- **Who detects:** the consumer (receives the timeout/error response).
- **Retryable:** yes, with a *new* escrow after refund; not against the same one.
- **Final money state:** `Active` → consumer-refundable after deadline. **I1 holds.**

## 3. Consumer disappears (orphaned VM)

A CloudiyOS VM (`cloudiy vm up`) outlives the consumer's connection. Two mechanisms
reclaim it.

### 3.1 Lease reaping

A metered VM carries a prepaid compute lease (`VmRecord`: `rate_micro_usdc_per_hour`,
`budget_micro_usdc`). `exhausted(now)` is true iff `budget > 0 && rate > 0 &&
accrued(now) >= budget` (`vm.rs:311-315`) — so **unmetered/dev VMs (budget or rate
0) are never reaped**. `reap_expired` (`vm.rs:599-617`) runs on a timer, calls
`stop(owner, false)` for each exhausted VM, and returns its `ResourceVector` to
node accounting. A race window (lease spent, reaper not yet run) is covered by
`lease_exhausted` checks before opening a shell/tunnel (`p2p.rs:244,294`), which
refuse with `"VM lease exhausted — top up and re-provision"`.

### 3.2 Provider restart recovery

`reconcile` (`vm.rs:774-860`) rebuilds VM state from Docker by the
`cloudiy.managed` label, reading owner/image/cpu/mem/gpu/ports/volume/rate/budget/
created from labels written at creation (`vm.rs:497-524`). Stopped managed VMs are
`docker start`-ed; ones that can't be revived are `rm --force`-ed and dropped
(`vm.rs:800-811`). `created_at` comes from the `cloudiy.created` label so a
restart doesn't extend the lease (`vm.rs:517-518`).

**[FINDING F-02 — label-less VM resets its lease] `[follow-up]`.** If
`cloudiy.created` is missing/malformed, `reconcile` falls back to
`Utc::now()` (`vm.rs:840`), resetting the lease clock to restart time — a fresh
full window. Worse, missing `rate`/`budget` labels become `unwrap_or(0)`
(`vm.rs:834-835`), which makes `exhausted()` return false forever → an
**unreapable, effectively free** VM. This only affects VMs created by a provider
version *predating* the lease labels (every current-version VM gets them at
`vm.rs:497-524`), so it's a cross-version-upgrade edge, not a same-version one.
Left as follow-up: the safe remediation (quarantine or expire label-less metered
VMs) is a judgment call — it could also orphan a legitimately-old dev VM — and
belongs with the operator, not a silent code change. **Does not touch I1:** this
is a *compute-lease* accounting gap, not escrow funds; the escrow refund backstop
is independent.

### 3.3 The volume on stop/reap

`stop(owner, false)` persists the working volume to the durable store, then drops
the local copy — but **only if the persist succeeded** (`vm.rs:749-765`):

```rust
match self.volume_sync(owner, &rec.volume, true).await {
    Ok(())  => { self.cli(&["volume","rm","--force",&rec.volume]).await.ok(); }
    Err(e)  => tracing::error!("external volume persist failed for {} — keeping local copy: {e}", short(owner)),
}
```

On persist failure the local Docker volume is **kept as a fallback**, logged at
`error`. Data is never dropped on a failed sync. (`wipe=true` unconditionally
deletes, `vm.rs:745-748` — that's an explicit destroy, not a failure path.)

- **Who detects:** the provider (reaper / restart); the consumer only notices next
  `vm up` (state restored, or a fresh VM if nothing persisted).
- **Retryable:** the volume persist is best-effort each stop; a kept local copy is
  retried on the next stop.
- **Final money state:** unrelated to escrow — a VM lease is prepaid; I1 is about
  escrows and is unaffected.

## 4. Volume failures (RFC-0009 modes)

`volume_sync` no-ops without a remote, else dispatches by `CLOUDIY_VOLUME_MODE`
(`vm.rs:628-642`): default `rclone` (plaintext copy) or `snapshot` (restic).

- **Persist (`to_remote`) failure**, both engines → `Err` → `stop` keeps the local
  copy (§3.3). restic `backup` failure is strict (`anyhow::ensure!`, `vm.rs:730`);
  restic `init` is best-effort (idempotent, `vm.rs:723`).
- **Restore (from store) failure** is tolerated as "fresh VM": rclone restore in
  `start` warns (`vm.rs:439-441`); restic restore was silent.

**[FINDING F-03 — silent restic restore failure masks data loss] `[fixed]`.** In
`volume_sync_restic` the restore was `let _ = self.cli(&refs).await;` with **no
log** (`vm.rs:707-719`), while the rclone path `warn!`s. A restore that fails for
a *real* reason — wrong key (`CLOUDIY_VOLUME_KEY_SIG` mismatch), corrupt repo,
network — is then indistinguishable from a legitimately-empty fresh VM: the tenant
silently boots an empty `/root` over the top of state that *does* exist in the
store. Fixed in this branch: the restic restore now emits a `warn!` on a non-empty
repo that fails to restore, matching the rclone path, so the operator can tell
"new VM" from "restore broke". (A truly-empty repo still restores cleanly and
stays quiet.)

- **Who detects:** the provider (now via the warn); the consumer sees an empty home.
- **Retryable:** yes on the next start, once the underlying cause (key/repo/net) is
  fixed — and because persist keeps the local copy on failure, the authoritative
  state is not lost by a failed restore alone.
- **Final money state:** n/a (volume, not escrow).

## 5. Payment failures

All rejections here surface to the consumer as the x402 `payment_requirements`
JSON with the error string in `req["error"]` (`core.rs:483-486`,
`payment_requirements_with_error`).

| Failure | Where | Consumer sees |
|---|---|---|
| Deadline too near | `payments.rs:169` (A2), `min_remaining = WORKLOAD_TIMEOUT_SECS + 120 = 420s` (`core.rs:459`) | `"escrow deadline too near (Ns left, need 420s) — refund risk"` |
| Escrow not `Active` | `payments.rs:149` | `"escrow is not Active (already released or refunded)"` |
| Underfunded | `payments.rs:161` | `"escrow underfunded: X < Y micro-USDC"` |
| Replay (already served) | `core.rs:446-455` (A1) | `"escrow already consumed by a prior job (replay)"` |
| Run-auth lapsed | `payments.rs:186` (MEDIUM-2) | `"run authorization has expired"` |
| Run-auth expiry too far | `payments.rs:189`, cap `RUN_AUTH_MAX_WINDOW_SECS = 3600` | `"run authorization expiry is too far in the future"` |
| Run-auth wrong/forged sig | `payments.rs:195-197` (A4) | `"consumer authorization signature is invalid"` |
| No escrow, provider requires one | `core.rs:490-499` | `"provider requires an on-chain escrow payment (attach escrow)"` |

Two backstops make the anti-replay set safe even though `ServedEscrows` is
in-memory and bounded (`MAX_SERVED_ESCROWS = 4096`, LRU-evicted, `core.rs:214`):
the chain is the authority — an evicted-then-resubmitted escrow either fails
on-chain re-verification because it's no longer `Active` (`payments.rs:149`) or is
already closed. So a provider restart clearing the set cannot cause a double-spend
or free execution.

**Partial funding in `run --replicas --pay`** (`client.rs:363-396`): escrows are
funded one-per-replica **before any compute**; the first funding failure fails
fast and `print_refundable` (`client.rs:306-319`) prints one
`cloudiy refund --escrow <ACCT>` line per already-funded escrow, gated on "after
the escrow deadline". No auto-refund — the funds sit `Active` and refundable (I1).

**[FINDING F-04 — payment-rejection logic untested] `[fixed]`.** The A2 deadline
margin (`payments.rs:169`) and the two run-auth freshness bounds
(`payments.rs:186-191`) had **no unit test** — `payments.rs`' tests covered only
parsing and the run-auth *message* binding, and `verify_escrow` itself needs a
live RPC so it isn't unit-testable whole. Fixed in this branch: the two checks are
extracted into pure helpers (`deadline_has_margin`, `auth_within_window`) that
`verify_escrow` now calls, with unit tests covering the boundaries (exactly-at,
just-under, lapsed, absurd-future). Behavior is byte-identical; the logic is now
pinned.

- **Who detects:** the provider (at admission); the consumer gets the x402 error.
- **Retryable:** yes — fix the cause (fund more, extend deadline, re-sign) and
  resubmit; a rejected escrow is *not* marked served (`core.rs:477-481` only
  reserves on success), so a legitimately-failed attempt can retry the same escrow.
- **Final money state:** the escrow was never touched on a pre-execution rejection
  → `Active`, fully refundable. **I1 holds.**

## 6. Settlement failures

`release_verified` re-checks the provider's result signature on-chain via
`verify_ed25519` (`lib.rs:636-685`). Rejections, with **corrected** Anchor codes:

| Code | Variant | Trigger |
|---|---|---|
| 6003 | `NotActive` | escrow already settled/refunded (`lib.rs:148`) |
| **6008** | `MissingSignature` | no Ed25519 precompile ix at index 0, or wrong program (`lib.rs:641-647`) |
| **6009** | `BadSignature` | malformed precompile data, instruction-index spoof, bad offsets, or message-byte mismatch (`lib.rs:655,669-683`) |
| **6010** | `WrongSigner` | precompile-verified pubkey ≠ `job.provider_node_key` (`lib.rs:676-679`) |
| 6013 | `ChallengeWindowOpen` | settle attempted inside the holdback window — inert while `CHALLENGE_WINDOW_SECS == 0` (`lib.rs:23`) |

**[FINDING F-05 — stale error numbers in tests and docs] `[fixed]`.** The Anchor
discriminants are auto-assigned `6000 + ordinal`, and a variant (`MathOverflow`,
6007) sits ahead of the signature errors, so the real codes are **6008/6009/6010**
— but several places said 6008 for `BadSignature`:
`contracts/tests/cloudiy-escrow.ts:186` (`/BadSignature|6008|0x1778/`),
`contracts/tests/edge-cases.ts:206` (`/WrongSigner|6009/`), `contracts/SECURITY.md`,
and `docs/MAINNET-RUNBOOK.md:218`. The tests still pass because the regex also
matches the error *name*, so the wrong numeric fallback was dead and the drift went
unnoticed. Fixed in this branch: numbers corrected to 6009/6010 (and `0x1778 →
0x1779`) in both tests and both docs. Six other test assertions (6001, 6002, 6005,
6006, 6011, 6012) independently confirm the ordinal mapping. *The `#[msg]` name is
the stable identifier; the number shifts on any reordering — cite by name.*

**Divergent replica (RFC-0008):** in a quorum run, a replica whose signed output
disagrees with the majority is flagged, excluded from the quorum, and **not
released by us** (`client.rs`, RFC-0008). Its escrow stays `Active` → refundable.

**[FINDING F-06 — malicious self-settle, the one carve-out to I1] `[documented,
by design]`.** `release_verified` is *permissionless* (`payer: Signer` = any
caller, `lib.rs` `ReleaseVerified`). A **malicious** divergent replica can settle
its **own** escrow with a valid signature over its own (wrong) output, before the
deadline — so that one escrow may pay the liar instead of being refundable
(`client.rs:578-580`, RFC-0008 §5). This is the single case where an escrow can
leave `Active` other than by honest release/refund. It is **bounded to one
replica's price**, never a drain or a redirection (payout is pinned to the
provider the consumer chose), and the deterrent is reputation (RFC-0006). The
clean fix already exists in-contract and is disabled: a non-zero
`CHALLENGE_WINDOW_SECS` puts the self-settlement in a holdback long enough to claw
back — blocked on the RFC-0006 §10 decision of *who the challenge authority is*.
Documented, not "fixed", because activating it is a governance decision, not a
code change.

- **Who detects:** the consumer (verifies each signature locally; sees the divergence).
- **Retryable:** the *answer* is delivered by the honest majority; the divergent
  escrow is refundable unless self-settled (F-06).
- **Final money state:** honest replicas paid; divergent replica refundable (or,
  in the F-06 case, self-paid). **I1 holds except the documented F-06 carve-out.**

## 7. Network / P2P failures

- **Dial/accept failure:** logged and dropped per-connection; the accept loop
  continues (`p2p.rs:24,27`). No retry at this layer — the *consumer* re-dials.
- **Inbound stream flood:** two-tier cap — node-wide `MAX_CONCURRENT_INBOUND_STREAMS
  = 64`, per-connection `MAX_INBOUND_STREAMS_PER_CONN = 16` (`core.rs:41,46`). A
  refused stream gets one `node_busy()` frame — `"provider is at inbound-stream
  capacity — retry shortly"` — without consuming a permit (`p2p.rs:56-93`).
  Interactive sessions have a separate `MAX_CONCURRENT_SESSIONS = 16` cap →
  `"provider is at interactive-session capacity — try again shortly"`.
  **Retryable:** yes — it's explicit backpressure, not an error.
- **Directory unreachable:** `fetch_providers` skips it with a stderr warning and
  merges the reachable ones (`client.rs:220`); reputation fetch failures degrade to
  an empty map, not an abort (`client.rs:173`). Redundant directories give
  resilience against a dead one.

**[FINDING F-07 — "all directories down" is indistinguishable from "no providers"]
`[follow-up]`.** `fetch_providers` returns `Ok(merged)` even when *every* directory
was unreachable — the caller gets an empty list, a success with zero results
(`client.rs:161-224`). Only the stderr `⚠️` warnings tell "the network is down"
from "nobody is online", so a caller can't retry intelligently on a partition.
Left as follow-up: returning an error when *all* directories failed *and* none
succeeded would let callers back off, but it changes the function's contract and
its callers — a deliberate change, not a drive-by.

## 8. Findings summary

| ID | Severity | Class | Status |
|---|---|---|---|
| F-01 | Low | inconsistent timeout failure shape | follow-up |
| F-02 | Low | label-less VM resets lease (cross-version edge) | follow-up |
| F-03 | Medium | silent restic restore masks data loss | **fixed** |
| F-04 | Low | payment-rejection logic untested | **fixed** |
| F-05 | Low | stale settlement error numbers (tests + docs) | **fixed** |
| F-06 | Medium | malicious self-settle — the I1 carve-out | documented (governance) |
| F-07 | Low | partition indistinguishable from empty | follow-up |

No finding violates I1 except F-06, which is bounded, pre-documented (RFC-0008 §5),
and has an in-contract remedy gated on a governance decision. **The core claim
holds: funds are never locked past the deadline.**

## 9. What a second implementation must preserve

1. An `Active` escrow MUST be consumer-refundable after `deadline` (I1). Do not add
   an escrow exit that leaves funds unreachable.
2. Mark an escrow served only on *successful* verification, never on rejection, or
   a transient failure permanently burns a legitimate escrow.
3. Keep the local volume on a failed persist; never drop the only copy on a sync error.
4. On a failed restore, tell the operator (don't silently serve an empty home over
   real state).
5. Refuse escrows without deadline margin (`WORKLOAD_TIMEOUT + 120`) — otherwise a
   consumer can run then refund, getting free work.
6. Cite settlement errors by `#[msg]` name; the numeric code shifts on reordering.
