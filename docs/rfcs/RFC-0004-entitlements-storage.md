# RFC-0004 — Stateless Providers, External Storage & Entitlements

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.1 |
| **Requires** | RFC-0001 (Vision), RFC-0003 (Discovery), PROTOCOL.md (nouns, wire) |
| **Reference implementation** | `crates/protocol`, `crates/scheduler`, `crates/runtime`, `crates/cloudiy::vm`, `crates/protocol::settlement` |

> **Positioning note.** This RFC refines the *resource model* of the Compute
> Protocol. It does not bind the protocol to any storage backend, chain or
> runtime; it defines the language for stateless compute, external storage,
> and consumer entitlements. Cloudiy is one implementation.

---

## Abstract

This RFC resolves two coupled questions the reference implementation left
implicit:

1. **Where does state live?** Providers should sell *cycles*, not *durability*.
   All persistent data must live in an external **storage** resource, so a
   provider can be stateless, interchangeable and safe to reap at any moment.
2. **What does a consumer actually reserve?** Not a machine. A consumer holds an
   **entitlement** — a resource envelope (quota) it may burst into — and the
   scheduler places each workload on a fitting node *per run*. The "parts" a
   user picks are a UI metaphor over fungible network resources, not a physical
   box and not a specific provider.

Together these make placement, pricing and payment decompose cleanly by
resource, and let a user's environment survive any single provider going away.

---

## 1. Problem

RFC-0001 says *"the consumer describes computation; the protocol resolves
execution"* and *"a workload is scheduled, executed in isolation, and
destroyed — resources return to the pool."* Two gaps remained:

- **Statefulness leaked onto providers.** "Installing an app" implied writing an
  image and its data onto a provider's disk. But a provider that must retain a
  consumer's bytes is no longer stateless: it cannot be reaped freely, it now
  owes durability and privacy guarantees it was never paid for, and it becomes a
  sticky dependency for that consumer.
- **"Pick a machine" contradicted "distribute across providers."** An early
  point-to-point model let a consumer choose a provider. A later model bursts
  each workload onto the best available node. If a user also picks "parts"
  (specific hardware), those parts are meaningless unless they bind to one
  provider — which reintroduces the single-provider dependency the network
  exists to remove.

This RFC picks the coherent combination: **stateless providers + external
storage + entitlements + per-workload placement**, with affinity as an opt-in.

---

## 2. Principles

1. **A provider sells cycles, never durability.** No consumer state persists on
   a node beyond a running workload.
2. **State is a resource, addressed independently of compute.** `storage` is a
   first-class resource kind, provided and paid for separately.
3. **Consumers reserve capacity, not machines.** An entitlement is a quota over
   fungible network resources.
4. **Placement is per-workload and policy-driven.** The scheduler (RFC-0001 §5,
   RFC-0003) picks a fitting node each run. Machines do not randomly compete;
   the scheduler chooses.
5. **Affinity is opt-in.** Floating is the default; pinning is a request for
   workloads that need locality.

---

## 3. Stateless providers & external storage

### 3.1 `storage` as a resource kind

`storage` joins `cpu`, `memory`, `gpu`, `vram`, `bandwidth` in the open resource
set (PROTOCOL.md §1). A storage provider announces `storage` capacity the same
way a GPU provider announces `gpu`; the two need not be the same node.

### 3.2 Volumes

A **Volume** is external, content-addressed persistent state:

| Field | Meaning |
|---|---|
| `volume_id` | Stable handle owned by an identity. |
| `backend` | Opaque storage-network hint (e.g. `s3`, `walrus`, `irys`). The protocol never hardcodes one. |
| `root_digest` | Content hash of the current volume state (enables integrity + dedup). |
| `size_bytes` | Billed capacity. |
| `encryption` | Client-side; the storage provider stores ciphertext only. |

A `WorkloadSpec` references volumes to mount:

```
mounts: [ { volume_id, path: "/home", mode: "rw" } ]
```

At `PREPARING`, the runtime materialises the mount from the storage backend
(stream or fetch-on-read); at termination it flushes writes back and updates
`root_digest`. The compute node keeps nothing.

### 3.3 Images by digest, never as state

Environments are referenced by **content digest**, not by mutable tag. The
runtime pulls layers into an ephemeral sandbox, runs, and destroys it. Nodes MAY
cache layers for performance — a layer cache holds no consumer data and is not
state. "Installing an app" therefore means *adding an image reference (by
digest) + provisioning a volume in the consumer's storage* — not writing to a
provider.

---

## 4. Entitlements

### 4.1 Definition

An **Entitlement** is a resource envelope an identity may draw from:

| Field | Meaning |
|---|---|
| `holder` | Owning identity (ed25519). |
| `limits` | Per-kind ceilings, e.g. `{ gpu: 1×class-A, cpu: 16, memory: 64GB }`. |
| `budget` | Optional prepaid balance (micro-USDC) the envelope can spend. |
| `mode` | `on_demand` or `reserved` (see §4.2). |
| `expiry` | Optional validity window. |

The entitlement is **not** bound to a provider. Every `WorkloadSpec` a holder
submits is validated against — and its usage debited from — the entitlement. The
UI may render this as "parts" or a machine build; underneath it is a quota.

### 4.2 Billing modes

| Mode | Meaning | Placement (see §5) |
|---|---|---|
| `on_demand` | Pay per workload / per second of actual use, drawn from `budget`. | Floats: best-fitting node per run. |
| `reserved` | Prepaid capacity held for a session (`rate`/hour + budget). Maps to the lease already in `crates/cloudiy::vm`. | Pinned for the lease window (§5.2). |

On-demand maximises price competition and utilisation; reserved buys
predictability. A holder can run both at once (a reserved interactive VM plus
on-demand burst jobs).

---

## 5. Placement & affinity

### 5.1 Default: float

For `on_demand` workloads the scheduler ranks admissible nodes (RFC-0003 §
client-side scheduling: filter → score → dial winner, or top-N for redundancy)
and places each run independently. Because state is external, any winning node
can serve the next run.

### 5.2 Affinity (opt-in)

`WorkloadSpec.affinity` requests locality when needed:

| Value | Meaning |
|---|---|
| `none` (default) | Float — schedule freely each run. |
| `soft: <node>` | Prefer this node (cache/data locality); fall back if unavailable. |
| `hard: <node>` | Pin to this node; fail if unavailable. The original point-to-point mode. |
| `session` | Stay on the node chosen at session start for the lease duration. |

Use cases for affinity: interactive shells, stateful training with local
scratch, local multi-GPU/NVLink topology, or a large dataset already cached on a
node.

### 5.3 Migration

Because durable state lives in external volumes, a `session`/reserved workload
can be **migrated** between nodes (provider drain, failure, cheaper capacity)
with no data on the critical path — restart against the same `volume_id`. This
is impossible under the stateful-provider model and is a direct benefit of §3.

---

## 6. Payment decomposition

A settled workload splits across up to four payees, by resource contributed —
never by "which machine":

| Payee | Paid for |
|---|---|
| **Compute provider(s)** | Cycles (per second / per job). Multiple under redundancy. |
| **Storage provider** | Volume capacity (GB-month) + egress. |
| **Author / publisher** | Optional royalty for an app image or model (RFC for the App Store to follow). |
| **Protocol** | Fixed 4% fee. |

The escrow's job account carries the payee set and split. Today the reference
escrow implements **compute provider + protocol fee** only
(`contracts/programs/cloudiy-escrow`); this RFC specifies the generalised
multi-payee `release`. Backwards-compatible: a job with a single compute payee
and no storage/author payee behaves exactly as today.

---

## 7. Security & trust (summary)

Statelessness improves the trust model: a reaped node cannot leak retained data
because it retains none, and client-side volume encryption means storage
providers hold ciphertext only. Untrusted execution is handled where it belongs
— isolation of the runtime (PROTOCOL.md §4), signed results (RFC-0001), optional
quorum execution (top-N placement, compare signed outputs), and the App Store
trust model (a later RFC): publisher-signed manifests pinned by digest, tiered
reputation, and optional staking/slashing.

---

## 8. Reference-implementation impact

| Change | Where |
|---|---|
| Add `storage` resource kind + `Volume`/`mounts` types | `crates/protocol` (`resource.rs`, `workload.rs`) |
| Add `Entitlement` type + validation | `crates/protocol`, enforced in `crates/scheduler` |
| Storage backend behind a trait (no hardcoded provider) | `crates/runtime` |
| Materialise/flush volume mounts around execution | `crates/runtime` (docker driver) |
| `affinity` field honoured in placement | `crates/scheduler` |
| Reserved-lease pin already exists; wire on-demand float | `crates/cloudiy::vm`, scheduler |
| Multi-payee `release` (compute + storage + author + fee) | `contracts/programs/cloudiy-escrow`, `crates/protocol::settlement` |

---

## 9. Open questions

1. **Storage backend(s).** Which external storage to ship first (S3-compatible
   for speed vs. a decentralised network for credible neutrality)? Trait now,
   default later.
2. **Volume consistency during migration.** Snapshot-on-flush vs. a
   copy-on-write log; how to fence a stale node from writing after migration.
3. **Entitlement enforcement.** Purely client/scheduler-side, or attested
   on-chain against `budget`? Affects double-spend of a shared budget.
4. **Author royalty carrier.** Manifest-embedded pubkey vs. an on-chain app
   registry (ties into the App Store RFC).
5. **Cache-locality scoring.** Should the scheduler score for image/volume
   layers already cached on a node to cut cold-start?

---

## 10. Compatibility

Additive. Nodes that do not announce `storage` remain pure compute providers.
Workloads with no `mounts`, no `affinity` and a single compute payee behave
exactly as in v0.1. The escrow change is a superset of the current `release`.
