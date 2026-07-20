# The Cloudiy Open Compute Protocol — Specification v0.2 (draft)

> HTTP standardized access to information. This protocol standardizes access
> to **computation**. It is not a cloud provider, not a marketplace, not a GPU
> rental platform. It is a communication layer: any identity can provide
> compute, any identity can consume it, through one universal API.

> **How to read this document.** §§1–9 are the design model (axioms, nouns,
> verbs). **§§10–17 are the *normative wire specification*** — the concrete
> bytes on the wire, pinned to the behavior of the reference node (`crates/cloudiy`),
> which is the source of truth: it passes the black-box conformance suite
> (`conformance/`) 18/18. A second team can implement an interoperable node from
> §§10–17 alone. Where a rule says **MUST/MUST NOT/SHOULD** it is testable against
> that suite. Each normative claim cites the reference code (`file:line`) so it is
> auditable. **This version specifies existing behavior**; wire changes that would
> be desirable but are not yet implemented are called out as *Future (breaking)*.

## 1. Design axioms

1. **Everything is a Resource.** Never a machine, a server or "a GPU box".
   Resource kinds are an open set: `cpu`, `memory`, `gpu`, `vram`, `storage`,
   `bandwidth`, and any future `custom:*` kind — adding one must never require
   an architecture change.
2. **Everything is an Identity.** Human, AI agent, company, backend, DAO,
   another protocol — the protocol does not care and must never ask.
   An identity is an opaque, verifiable string (today: an ed25519 key).
3. **Consumers describe WHAT, the protocol decides HOW.** Docker, VMs,
   drivers, CUDA installs, SSH, Kubernetes, physical machines are
   implementation details that must never leak through the API.
4. **Capabilities are first-class.** Resources describe *quantity*
   (`24GB vram`); capabilities describe *functionality* (`cuda:12.8`,
   `pytorch`, `ffmpeg`). Consumers request capabilities, not machines.
5. **The unit of execution is the Workload.** Never rent servers. A workload
   is declared, scheduled, executed in isolation, measured, and destroyed —
   resources return to the pool automatically.
6. **Runtimes are interchangeable.** Docker is the first runtime, not part of
   the protocol. Firecracker, Kata, Podman, WASM, microVMs must slot in
   behind the same interface.
7. **Settlement is an implementation detail.** Payments bind through the
   x402 quote flow (`Payment Required` → settle → retry). Solana/USDC/escrow/
   streaming/proof-of-compute live behind interfaces; the protocol never
   hardcodes a chain.

## 2. Nouns

| Noun | Meaning |
|---|---|
| **Identity** | Opaque verifiable actor. May consume, provide, publish, get paid. |
| **Resource** | Quantifiable capacity: kind + amount (+ unit convention). |
| **Capability** | Named functionality a node offers (`docker`, `cuda:12.8`, `pytorch`, `arm64`). |
| **Provider** | An identity contributing a *chosen slice* of its resources. What is not shared stays private. |
| **Node** | A provider's daemon + its announced resources/capabilities. Nodes are heterogeneous by design (GPU, compute, storage, hybrid, future accelerators). |
| **Workload** | A declared unit of work: image/template/recipe + command + resource requests + capability requirements + ports + storage + limits. |
| **Template / Recipe** | Reusable environment descriptions (`pytorch`, `ollama`, `postgres`…) that expand into workload specs. |
| **Placement** | Scheduler output: workload → node binding with a price. |

## 3. Resource accounting

Every node tracks, per resource kind: `total` (what the hardware has),
`shared` (what the provider chose to contribute), `allocated` (in use by
workloads), `available = shared − allocated`. Allocation happens at placement;
release is automatic at workload termination — no manual returns, ever.

## 4. Workload lifecycle

```
DECLARED → SCHEDULED(node) → PREPARING (pull image / build env)
        → RUNNING (metrics + logs streaming)
        → SUCCEEDED | FAILED(reason) | CANCELLED
        → environment destroyed, resources released   (always, no exceptions)
```

Isolation is mandatory: workloads never execute directly on the host
(namespaces, cgroups, seccomp, read-only fs where possible).

## 5. Scheduler

The scheduler is the kernel of the network. It schedules **workloads**, not
CPUs. Inputs: resource fit, capabilities, price, region/latency, provider
health, utilization and reputation. The pipeline is modular —
filters (hard constraints) then weighted scorers (preferences) — and no
policy is ever hardcoded.

## 6. Universal API (transport-agnostic verbs)

Carried over HTTP/JSON and P2P QUIC today; any future transport must map 1:1.

```
CreateWorkload(spec)        → workload_id | PaymentRequired(quote)
GetWorkload(id)             → state, placement, metrics summary
StopWorkload(id) / DeleteWorkload(id)
Logs(id) / Metrics(id)                       (+ WebSocket/stream events)
ListResources() / ListCapabilities()         (network- or node-scoped)
ListTemplates() / ListRecipes()
RegisterProvider(announcement) / Heartbeat(utilization, health)
```

Payment binding: any verb may answer `Payment Required` with an x402 quote
(price in micro-USDC, payee, asset, settlement hint). Settle and retry — no
accounts, no API keys.

The concrete transport encodings of these verbs — HTTP methods/paths/bodies and
P2P frames — are normative in **§10 (HTTP)** and **§11 (P2P)**. The reference
node implements only the subset marked *shipping* there; the rest of §6 is the
forward-looking verb surface.

## 7. SDK contract

SDKs (Rust, Python, TypeScript, Go) hide everything below the workload:

```python
compute.run(cpu=8, memory=32, gpu_vram=24, template="ollama",
            command="ollama serve")
```

Never: create VM, install CUDA, configure networking.

## 8. Entitlements, external storage & placement affinity

**Providers are stateless compute.** A node contributes cycles, not durable
state. It never "installs" or keeps a consumer's data. Anything persistent —
home directory, packages, datasets, model weights, outputs — lives in an
external **storage** resource, addressed independently of any node.

**Images are pulled by digest, on demand, never persisted as state.** A
workload references an environment by content digest; the runtime pulls it into
an ephemeral sandbox, runs it, and destroys it. Nodes MAY cache image layers
for performance — a cache is not state and carries no consumer data.

**Consumers hold an Entitlement, not a machine.** An entitlement is a *resource
envelope* — a quota the identity may burst into (e.g. `gpu:1×class-A, cpu:16,
memory:64GB`, plus an optional prepaid budget). It is not bound to any provider.
Each workload the consumer submits is bounded by, and drawn from, its
entitlement; the scheduler picks a fitting node **per workload** at run time.
This is the AWS-instance-type pattern: the UI may present "parts" (a build-your-
machine metaphor), but underneath a configuration is a quota over fungible
network resources, not a physical box.

**Billing follows the two modes already in the reference implementation:**

| Mode | Meaning | Placement |
|---|---|---|
| **On-demand** | Pay per workload/second of actual use, drawn from the entitlement. | Floats: best-fitting node chosen per run. |
| **Reserved (lease)** | Prepaid capacity held for a session (`rate`/hour + budget). | Pinned: bound to one node for the lease window. |

**Affinity is opt-in, floating is the default.** Stateless jobs float across the
network. For workloads that need locality — interactive sessions, stateful runs,
local multi-GPU topology, or a large dataset already cached — the consumer MAY
request affinity: a *soft* preference (prefer the same node for cache reuse) or a
*hard* pin (the original point-to-point mode). Because state is external, a
pinned session can still be **migrated** to another node between bursts without
data movement on the critical path.

**Payment decomposes by resource, not by machine.** A settled workload can split
across up to four payees: the **compute** provider(s), the **storage** provider,
an optional **author/publisher** royalty (for an app or model), and the
**protocol fee** (4%). The escrow's job account carries the payee set; today it
implements provider + protocol only (see `crates/protocol::settlement` and the
escrow program). The multi-payee split is specified in RFC-0004.

## 9. Reference implementation map (this repo)

| Layer | Crate / dir | Status |
|---|---|---|
| Protocol domain (pure types, no infra) | `crates/protocol` | v0.1 |
| Scheduler (modular filters + scorers) | `crates/scheduler` | v0.1 |
| Runtime abstraction (+ Docker driver) | `crates/runtime` | v0.1 |
| Node daemon (discovery, P2P, HTTP, wgpu kernels as a built-in runtime) | `crates/cloudiy` | shipping |
| Wire protocol / identity / signing | `crates/common` | shipping |
| Consumer SDKs | `crates/sdk`, `sdk/python`, `sdk/js` | shipping |
| Settlement interfaces (Solana/USDC/escrow behind traits) | `crates/protocol::settlement` | interfaces only, by design |
| Entitlements (resource envelope / quota) | `crates/protocol`, `crates/scheduler` | planned — RFC-0004 |
| External storage resource + volume mounts | `crates/protocol`, `crates/runtime` | planned — RFC-0004 |
| Reserved lease (pinned session) vs on-demand float | `crates/cloudiy::vm` | lease shipping; float via scheduler |

Versioning: the protocol schema is versioned; breaking changes bump the version.
The **normative** rules for the version field, its current value and mismatch
behavior are in **§16** (the aspirational `compute/0` tag is reconciled there
with what the reference node actually reports).

---

# Part II — Normative wire specification (v0.2)

Everything below pins the **observable contract** of the reference node. Field
names, byte layouts, status codes and limits are normative. Citations point at
the reference implementation so each rule is verifiable.

## 10. HTTP transport (shipping)

The reference node serves JSON over HTTP for consumers that cannot speak QUIC
(the browser marketplace, simple clients). Router:
`crates/cloudiy/src/http.rs:79-90`.

### 10.1 Verb → endpoint map

| Verb (§6) | Method & path | Request body | Success | Quote/err |
|---|---|---|---|---|
| — (liveness) | `GET /health` | — | `200` `{"status":"ok","uptime_secs":<i64>}` | — |
| node descriptor | `GET /info` | — | `200` NodeInfo (§12) | — |
| `CreateWorkload` | `POST /submit` | JobRequest (§11.1) JSON; optional `X-PAYMENT` header | `200` JobResponse (§11.2) | `402` quote (§13) / `503` `{"error":<str>}` |
| `GetWorkload` | `GET /status/:job_id` | — | `200` StatusResponse (§11.3) | — |

Rules:
- **R10.1** A conforming HTTP node MUST expose exactly these four routes with
  these methods. `GET /info` and `GET /health` MUST require no authentication or
  payment (`http.rs:81-82`, `health`/`node_info` handlers).
- **R10.2** On `POST /submit`, the x402 payment payload travels in the
  `X-PAYMENT` request header; a settlement receipt, when present, is returned in
  the `X-PAYMENT-RESPONSE` response header (`http.rs:41-66`). The same payload
  MAY instead be carried inline in `JobRequest.payment` (§11.1) — the node reads
  the header first.
- **R10.3** Request bodies MUST be capped (§15). CORS on the reference node is
  permissive for the MVP browser client (`http.rs:89`) — this is *not* a protocol
  requirement and SHOULD be locked to known origins in production.

## 11. Message schemas (shipping)

All bodies are JSON. Byte arrays (`input_data`, `output_data`) are JSON arrays of
`u8`. Types: `crates/common/src/types.rs`.

### 11.1 JobRequest (`types.rs:4-16`)

| Field | Type | Req. | Meaning |
|---|---|---|---|
| `job_id` | string | **yes** | Consumer-chosen id. The reference binds escrow to a UUID; a conforming consumer SHOULD use a UUID string. |
| `kernel` | string | **yes** | Kernel/template name (§17). Empty for VM-plane requests. |
| `input_data` | `[u8]` | **yes** | Input bytes (may be empty). |
| `params` | object(string→string) | **yes** | Free-form kernel params (may be empty `{}`). |
| `auth_token` | string | **yes** | Provider access code, or empty for the payment path. |
| `consumer_pubkey` | string\|null | no | Consumer Solana pubkey, when relevant. |
| `payment` | string\|null | no | x402 payload, base64-encoded JSON (§13.2). HTTP alternative to `X-PAYMENT`. |

### 11.2 JobResponse (`types.rs:18-36`)

| Field | Type | Req. | Meaning |
|---|---|---|---|
| `job_id` | string | **yes** | Echoes the request id. |
| `output_data` | `[u8]` | **yes** | Result bytes (empty on error). |
| `status` | string | **yes** | `"completed"` or `"error"` (§14.2). |
| `error_message` | string\|null | **yes** | Set iff `status=="error"`. |
| `provider_pubkey` | string\|null | no | Provider payout pubkey. |
| `payment_receipt` | string\|null | no | x402 receipt, base64 JSON. |
| `signature` | string\|null | no* | Hex ed25519 result signature (§11.4). *Present on every successfully executed job on the reference node; a consumer requiring trust MUST reject a missing one. |
| `signed_by` | string\|null | no* | EndpointId that produced `signature`. |

### 11.3 StatusResponse (`types.rs:38-44`)

`job_id` (string), `status` (string), `progress` (f32 in `0.0..=1.0`),
`provider_pubkey` (string\|null).

### 11.4 Result signature (normative — the core guarantee)

This is the protocol's central cryptographic guarantee: an **offline-verifiable
proof of which node produced which output for which input under which job**. It is
what the on-chain escrow's `release_verified` re-checks, and what an SDK verifies
before trusting output. It belongs in the protocol, not only in a design RFC
(promoted here from RFC-0006 §4). Reference: `crates/common/src/sig.rs:14-58`.

**Signed message** (`sig.rs:16-28`). The provider signs, with its node key (the
ed25519 key behind its EndpointId), the byte string:

```
"cloudiy/result/v2"  ‖  0x00  ‖  job_id (UTF-8)  ‖  0x00  ‖  sha256(input)  ‖  0x00  ‖  sha256(output)
```

- `"cloudiy/result/v2"` is a domain separator (`DOMAIN`, `sig.rs:14`): `v2` binds
  the input; `v1` did not. Domain separation ensures these signatures can never be
  confused with iroh TLS handshake signatures or other Cloudiy message types.
- `0x00` bytes are field separators. `job_id` is the raw UTF-8 of the request id.
  `sha256(input)`/`sha256(output)` are the 32-byte digests of the exact
  `input_data` submitted and `output_data` returned.

**Wire form.** `JobResponse.signature` is the **hex-encoded** 64-byte ed25519
signature; `JobResponse.signed_by` is the signer's EndpointId
(`sig.rs:32-38`, `types.rs:29-35`).

**Verification** (`sig.rs:43-58`). A consumer:
1. reconstructs the message above from the `job_id`, the input **it submitted**,
   and the returned `output_data`;
2. checks the ed25519 signature against the EndpointId in `signed_by` (which, for
   a directly-dialed provider, MUST equal the node it connected to).

- **R11.4a** A conforming provider that executes a job MUST return a
  `signature`/`signed_by` computed exactly as above.
- **R11.4b** A consumer requiring trust MUST reject a result whose signature is
  missing or does not verify against the input **it** sent — a provider that ran a
  different input cannot produce a signature that verifies (input-binding is the
  point of `v2`).
- **Future (breaking).** A `v3` domain would be a breaking change and MUST use a
  new domain string so old and new signatures never collide.

## 11bis. P2P transport (shipping)

The primary transport for CLI/agent consumers is iroh QUIC, ALPN `cloudiy/0`.
The peer identity is the ed25519 EndpointId (§2 identity), authenticated by the
QUIC handshake — so ownership of a VM/session binds to the connection, never to a
request field.

### Framing (`crates/common/src/proto.rs:143-181`)

Each message is a length-prefixed frame: **4-byte big-endian `u32` length**,
then that many bytes of **UTF-8 JSON**. One request frame → one response frame,
except `OpenSession`/`Tunnel`, which take over the stream after the first
response (binary `SessionFrame`s / raw bytes). Frame length MUST be `≤ MAX_FRAME`
(§15).

### Request → Response (`proto.rs:37-141`)

| Request | Carries | Response |
|---|---|---|
| `Info` | — | `Info(NodeInfo)` |
| `Submit(JobRequest)` | a job | `Job(JobResponse)` / `PaymentRequired{requirements}` / `Error{message}` |
| `RunWorkload{request,spec}` | OCP workload | `Job` / `PaymentRequired` / `Error` |
| `Status{job_id}` | — | `Status(StatusResponse)` |
| `Announce(SignedAnnouncement)` | to a directory | `Ack` / `Error` |
| `Providers` | to a directory | `Providers([SignedAnnouncement])` |
| `Reputation` | to a directory | `Reputation(SignedReputation)` |

`PaymentRequired.requirements` is the same x402 quote object as the HTTP `402`
body (§13). The `Error{message}` frame is the P2P analogue of the HTTP `503`
(§14). VM-plane requests (`StartVm`, `OpenSession`, `Tunnel`, …) are a separate
surface (RFC-0009) and out of scope for this section.

## 12. Node descriptor — `/info` schema (shipping)

Normative field set of the node descriptor (`GET /info` body / `Response::Info`).
Type `NodeInfo` (`types.rs:46-70`); values assembled in `core::node_info`
(`crates/cloudiy/src/core.rs:342-366`).

| Field | Type | Req. | Meaning |
|---|---|---|---|
| `protocol` | string | **yes** | Constant `"cloudiy"` (§16). |
| `version` | string | **yes** | Release/schema version (§16). |
| `endpoint_id` | string | **yes** | The node's ed25519 EndpointId (§2 identity). |
| `solana_pubkey` | string\|null | **yes** | Payout pubkey; `null` when no wallet is configured. |
| `gpu_model` | string | **yes** | Advertised accelerator (`"auto"`/model, or CPU). |
| `vram_mb` | u64 | **yes** | Advertised VRAM. |
| `jobs_completed` | usize | **yes** | Monotone completed-job counter. |
| `price_usdc` | number (float) | **yes** | Per-job price in **whole USDC** — a *derived display* value (§12.1). |
| `usdc_mint` | string | **yes** | SPL mint of the accepted asset. |
| `network` | string | **yes** | x402 settlement network label (`"solana-devnet"`/`"solana"`). |
| `payment` | string | **yes** | Payment scheme name (reference: `"x402"`). |
| `escrow_program` | string | **yes** | Settlement hint: escrow program id. |
| `fee_bps` | u16 | **yes** | Protocol fee in basis points (reference: `400`). |
| `resources` | object\|absent | no | OCP resource accounting (§3); omitted when the node has none. |
| `capabilities` | [string] | **yes** | Functionality offered (§2.4, §17); MAY be empty. |

### 12.1 Price is one number, in micro-USDC

There are two price representations on the wire, and they MUST agree. The
**canonical** value is the **integer micro-USDC** the node stores
(`state.price_micro_usdc`), which is also what the quote carries
(`maxAmountRequired`, §13) and what settles on-chain. `/info.price_usdc` is a
**convenience float** derived as `price_micro_usdc / 1_000_000.0`
(`core.rs:352`). Therefore:

- **R12.1** A consumer MUST settle the exact integer micro-USDC from the quote
  (`maxAmountRequired`), never a float reconstructed from `price_usdc` (float
  round-trips lose precision). `price_usdc` is for display only.
- **R12.2** A conforming node's `/info.price_usdc * 1_000_000`, rounded, MUST
  equal the quote's `maxAmountRequired`.

## 13. x402 payment quote (shipping)

When a job needs payment, the node answers with an x402 quote — HTTP `402` body,
or `Response::PaymentRequired.requirements`. Canonical form
(`core::payment_requirements`, `core.rs:308-329`):

```json
{
  "x402Version": 1,
  "error": "X-PAYMENT header is required",
  "accepts": [{
    "scheme": "exact",
    "network": "solana-devnet",
    "maxAmountRequired": "10000",
    "resource": "/submit",
    "description": "GPU job execution on <gpu_model>",
    "mimeType": "application/json",
    "payTo": "<provider Solana pubkey>",
    "maxTimeoutSeconds": 300,
    "asset": "<USDC mint>",
    "extra": { "escrowProgram": "<program id>", "feeBps": 400 }
  }]
}
```

| Field | Type | Meaning |
|---|---|---|
| `x402Version` | int | `1`. |
| `accepts[0].scheme` | string | `"exact"`. |
| `accepts[0].network` | string | settlement network (matches `/info.network`). |
| `accepts[0].maxAmountRequired` | **string** | price in **micro-USDC** (canonical, §12.1). |
| `accepts[0].resource` | string | the paid resource (`"/submit"`). |
| `accepts[0].payTo` | string | provider payout pubkey. |
| `accepts[0].maxTimeoutSeconds` | int | quote validity / job time budget, **relative** (§14bis). |
| `accepts[0].asset` | string | USDC mint. |
| `accepts[0].extra.escrowProgram` | string | escrow program id for settlement. |
| `accepts[0].extra.feeBps` | int | protocol fee (bps). |

- **R13.1** `maxAmountRequired` MUST be a base-10 **string** of an integer number
  of micro-USDC.
- **R13.2** A consumer retries by attaching a payment payload (§13.2) that
  settles against `extra.escrowProgram` for `payTo` in `asset` on `network`.

### 13.2 Payment payload

The payload is **base64-encoded JSON**, carried in the `X-PAYMENT` header (HTTP)
or `JobRequest.payment` (P2P/inline). It is untrusted input, parsed defensively
(`core::decode_payment`, `core.rs:331-340`). For the real escrow scheme it
references a funded escrow account plus the consumer's run-authorization
signature (§14bis); a demo scheme exists for flow tests. Size is capped (§15).

## 14. Error taxonomy (shipping)

The reference node distinguishes **transport/admission** failures from **job**
failures, and this distinction is normative.

### 14.1 Admission / transport errors (HTTP status)

| Status | When | Body | P2P analogue |
|---|---|---|---|
| `402 Payment Required` | job needs payment | x402 quote (§13) | `PaymentRequired{requirements}` |
| `503 Service Unavailable` | admission refused (capacity, bad escrow, replay, deadline, provider policy) | `{"error":"<message>"}` | `Error{message}` |
| `400 Bad Request` | body is not valid JSON / wrong shape | (framework) | frame decode error |
| `413 Payload Too Large` | body exceeds the cap (§15) | (framework) | frame > MAX_FRAME → closed |

(`http.rs:46-70` for 402/503/200; `RequestBodyLimitLayer` for 413,
`http.rs:86`; the axum `Json` extractor for 400.)

- **R14.1** A malformed request MUST be rejected with a stable 4xx (the reference
  returns `400` for unparseable JSON). It MUST NOT be answered `500`, and MUST NOT
  crash the node.

### 14.2 Job failure is `200` with `status:"error"` (normative, with rationale)

A job that is **admitted and runs but fails** (e.g. a container non-zero exit, a
timeout, "no GPU on this node") is reported as **HTTP `200`** with a
`JobResponse` whose `status == "error"` and `error_message` set
(`core::run_workload` builds this via `error_response`; `types.rs:22-23`).

- **R14.2** A conforming node MUST report a *job-level* failure as a `200`
  `JobResponse` with `status:"error"`, **not** as a 4xx/5xx. The HTTP/frame layer
  succeeded (the request was well-formed, paid, and executed); only the workload
  failed. Overloading a transport status for a job outcome would conflate "your
  request was bad" with "your job ran and failed", which a consumer must tell
  apart to decide retry vs re-fund.
- **Note (Future, breaking).** An alternative that carries job failure in a `5xx`
  with a structured body is a reasonable future design, but it is a breaking wire
  change and would require SDKs to migrate; it is out of scope for v0.2, which
  pins the shipped `200`-with-`status` convention.

## 14bis. Time semantics (shipping)

Two independent time concepts live on **opposite sides** of the flow; conflating
them is a common implementer error.

- **`maxTimeoutSeconds` — relative, provider side.** In the quote (§13), a
  *relative* budget (reference: `300`). It is the provider's declared job time
  window, not an absolute clock.
- **Run-auth `expiry` — absolute, consumer side.** In the *payment payload*, the
  consumer signs a run authorization over
  `"cloudiy/escrow-run/v3" ‖ 0 ‖ job_id ‖ 0 ‖ sha256(input) ‖ 0 ‖ expiry`, where
  `expiry` is an **absolute unix timestamp** (`payments::run_auth_message`,
  `crates/cloudiy/src/payments.rs`). The provider rejects a lapsed authorization
  and one whose expiry is more than `RUN_AUTH_MAX_WINDOW_SECS` (3600s) in the
  future (`payments::auth_within_window`).

- **R14bis** A consumer MUST send an **absolute** `expiry` in the run-auth
  signature and MUST NOT reuse `maxTimeoutSeconds` as that value. The escrow's own
  deadline (on-chain) is a third, separate clock governing refunds (RFC-0010 §5).

## 15. Frame & size limits (shipping — spec minimums)

| Limit | Value | Where | Rule |
|---|---|---|---|
| Max request/response frame | **8 MiB** (`MAX_FRAME`) | `proto.rs:23` | A conforming node MUST accept frames up to this and MAY reject larger. |
| Max HTTP body | **16 MiB** (`MAX_BODY_BYTES`) | `core.rs`, applied `http.rs:86` | idem, via `413`. |
| Max payment payload | **8 KiB** (base64 string length) | `core::decode_payment`, `core.rs:335` | A larger payment payload MUST be rejected without parsing. |

These are **spec minimums**: a conforming node MUST support at least these sizes
so a portable consumer can rely on them. A node MAY accept larger, but MUST NOT
advertise conformance while rejecting inputs *below* these thresholds.

## 16. Versioning (shipping + future)

- **Field.** The schema version travels in `NodeInfo.version` (§12). The
  reference reports the **release version** (`env!("CARGO_PKG_VERSION")`,
  `core.rs:346`) and `protocol == "cloudiy"` (`core.rs:345`).
- **Current reality (normative).** There is **no negotiation handshake** today:
  a consumer reads `/info` and decides for itself. The aspirational `compute/0`
  schema tag (§9) is **not** the value currently on the wire; §16 is the
  authority, and a second implementation MUST read the version from
  `NodeInfo.version`, not assume `compute/0`.
- **R16.1 (mismatch behavior).** Until a negotiation mechanism ships, a consumer
  that requires a schema it cannot confirm from `/info` MUST fail closed —
  refuse to submit and surface the version mismatch — rather than proceed
  optimistically. A node MUST NOT silently reinterpret a request from an
  unrecognized schema.
- **Future (breaking).** A real negotiation (client sends accepted versions;
  node selects or 4xx-rejects) plus a stable `schemaVersion` distinct from the
  release version. Specifying it is deferred; this section exists so the
  behavior is *defined* (fail-closed) rather than undefined in the meantime.

## 17. Kernels & capabilities (shipping)

- **R17.1** **No kernel is mandatory.** A conforming node declares what it can do
  via `capabilities` (§2.4) — e.g. `wgsl`, `kernel:vector_add`, `kernel:matrix_mul`,
  `docker`. A consumer MUST discover capability from `/info`, never assume a
  kernel exists. (`KERNELS` on the reference GPU runtime:
  `crates/runtime/src/wgsl.rs:21`.)
- The reference node advertises the built-in GPU kernels below when a GPU is
  present; a CPU-only node advertises none of them and the conformance suite
  correctly SKIPs the signed-result check there.

### Appendix — reference kernel input/output encodings

Non-normative for *which* kernels exist (R17.1), but normative for the *encoding*
of the reference kernels, so a second implementation of `vector_add` interoperates
(`wgsl.rs:475-516`):

| Kernel | Input (`kernel` = name, bytes = string) | Output |
|---|---|---|
| `vector_add` | `"a1,a2,…;b1,b2,…"` — two equal-length float vectors, `;`-separated | comma-joined element-wise sum, e.g. `"1,2,3;10,20,30"` → `"11,22,33"` |
| `matrix_mul` | `"m,k,n;a1,…(m*k);b1,…(k*n)"` — dims then two row-major matrices | comma-joined row-major product |
| `wgsl` | a JSON job (`WgslJob`): an arbitrary compute shader; inputs/outputs base64 | base64 output buffer |

The conformance suite pins `vector_add("1,2,3;10,20,30") == "11,22,33"`
(`conformance/cloudiy_conformance.py:249`) as the interop anchor.
