# The Cloudiy Open Compute Protocol — Specification v0.1 (draft)

> HTTP standardized access to information. This protocol standardizes access
> to **computation**. It is not a cloud provider, not a marketplace, not a GPU
> rental platform. It is a communication layer: any identity can provide
> compute, any identity can consume it, through one universal API.

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

## 7. SDK contract

SDKs (Rust, Python, TypeScript, Go) hide everything below the workload:

```python
compute.run(cpu=8, memory=32, gpu_vram=24, template="ollama",
            command="ollama serve")
```

Never: create VM, install CUDA, configure networking.

## 8. Reference implementation map (this repo)

| Layer | Crate / dir | Status |
|---|---|---|
| Protocol domain (pure types, no infra) | `crates/protocol` | v0.1 |
| Scheduler (modular filters + scorers) | `crates/scheduler` | v0.1 |
| Runtime abstraction (+ Docker driver) | `crates/runtime` | v0.1 |
| Node daemon (discovery, P2P, HTTP, wgpu kernels as a built-in runtime) | `crates/cloudiy` | shipping |
| Wire protocol / identity / signing | `crates/common` | shipping |
| Consumer SDKs | `crates/sdk`, `sdk/python`, `sdk/js` | shipping |
| Settlement interfaces (Solana/USDC/escrow behind traits) | `crates/protocol::settlement` | interfaces only, by design |

Versioning: the protocol schema is versioned (`compute/0`); breaking changes
bump the version, implementations negotiate.
