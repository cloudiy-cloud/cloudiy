# RFC-0005 — Scheduling: the Brain of the Protocol

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.1 |
| **Requires** | RFC-0001 (Vision), RFC-0003 (Discovery), PROTOCOL.md (nouns) |
| **Reference implementation** | `crates/scheduler` (`Pipeline`, `Filter`, `Scorer`, `rank`/`place`) |

## 1. Problem

RFC-0001 promises *"the consumer describes computation; the protocol resolves
execution"*. Discovery (RFC-0003) answers *who exists*. Scheduling answers the
harder question: **of everyone who exists, who should run THIS workload?**

Every mature orchestrator eventually discovers that placement is where all the
intelligence concentrates — Kubernetes' scheduler, Borg's scoring, Nomad's
bin-packing. In an open network the stakes are higher: nodes are heterogeneous,
untrusted, priced differently, geographically scattered, and come and go. The
scheduler is not a detail of cloudiy. It is the brain.

## 2. Invariants

1. **Client-side and sovereign.** The scheduler runs on the consumer's side
   (CLI, SDK, MCP server, marketplace, agent). No central placement service
   exists, so no one can bias, censor or front-run placements. Two consumers
   with different policies get different — both correct — answers.
2. **Declarative input only.** The scheduler consumes a `WorkloadSpec` (WHAT)
   and verified `ProviderAnnouncement`s (RFC-0003). It never sees machines,
   IPs or vendor names — only resources, capabilities and metadata.
3. **No hardcoded policy.** The engine is a pure pipeline; every opinion is a
   pluggable component with a weight. `Pipeline::default_policy()` is one
   preset, not the protocol.

## 3. Design

Two component kinds, one pipeline, executed locally in milliseconds:

```text
verified announcements
        │
        ▼
   ┌─────────┐   hard constraints — a node either qualifies or not
   │ Filters │   (resource fit, capability match, health, price ceiling…)
   └─────────┘
        │  survivors
        ▼
   ┌─────────┐   soft preferences — each scores 0.0–1.0, weighted sum
   │ Scorers │   (price, reputation, utilization, health bonus…)
   └─────────┘
        │
        ▼
 rank() → ordered placements · place() → the winner
```

```rust
pub trait Filter: Send + Sync {
    fn keep(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> bool;
}
pub trait Scorer: Send + Sync {
    fn score(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> f64; // 0.0–1.0
}
```

### 3.1 Shipped today

| Component | Kind | Judgment |
|---|---|---|
| `ResourceFit` | Filter | node's `available` covers the spec's `resources` |
| `CapabilityMatch` | Filter | every required capability is advertised |
| `HealthyOnly` | Filter | drops `Unhealthy` nodes |
| `PriceCeiling` | Filter | drops nodes above the spec's budget |
| `CheapestPrice` | Scorer | cheaper is better |
| `HighReputation` | Scorer | announced reputation (until RFC-0006 makes it earned) |
| `LowUtilization` | Scorer | headroom now predicts latency later |
| `HealthBonus` | Scorer | `Healthy` beats `Degraded` |

### 3.2 The axes to grow into

The pipeline shape is finished; the intelligence is not. Each axis below is a
future `Filter`/`Scorer` pair — none requires touching the engine, the wire
format, or any SDK:

| Axis | Component sketch | Signal source |
|---|---|---|
| **Latency / geography** | `NearbyScorer` — prefer nodes with low measured RTT | iroh path RTT during discovery dial, or self-announced region |
| **Reputation (earned)** | `TrackRecordScorer` — jobs completed × signature validity × dispute history | on-chain receipts: every `release_verified` is a public, signed, paid unit of work — reputation nobody can fake |
| **Energy** | `GreenScorer` / `EnergyFilter` — prefer renewable-powered or low-idle nodes | self-announced `energy:*` capability, later attested |
| **Availability** | `UptimeScorer` — historical announcement continuity (TTL gaps = flakiness) | directory heartbeat history |
| **Workload affinity** | `AffinityScorer` — data gravity (near a volume, RFC-0004), model already warm, or anti-affinity for replicas | spec hints + provider cache announcements |
| **Trust tier** | `IsolationFilter` — untrusted workloads require `isolation:gvisor|kata`; sensitive ones require attestation | advertised isolation capability (shipped), TEE attestation (future) |
| **Historical performance** | `ThroughputScorer` — tokens/sec, samples/sec observed on past runs | consumer-local telemetry; optionally shared |
| **Future capacity** | `ReservationFilter` — only nodes that can honor a lease window | reserved-lease protocol (partially shipped in the VM plane) |
| **Price dynamics** | `SpotScorer` — prefer nodes discounting idle capacity | announced price + utilization curve |

### 3.3 Composition and presets

Policies are weighted compositions, and different frontends ship different
presets — all on the same engine:

```rust
// a batch-training consumer: cost above all
Pipeline::new()
    .filter(ResourceFit).filter(CapabilityMatch).filter(HealthyOnly)
    .scorer(CheapestPrice, 0.7)
    .scorer(UptimeScorer, 0.3);

// an interactive agent: latency and reliability, price second
Pipeline::new()
    .filter(ResourceFit).filter(CapabilityMatch).filter(IsolationFilter::container())
    .scorer(NearbyScorer, 0.4)
    .scorer(TrackRecordScorer, 0.4)
    .scorer(CheapestPrice, 0.2);
```

`rank()` (top-N) already powers quorum execution (`run --replicas N`): the same
pipeline that picks one node picks N independent ones and the consumer demands
agreement on the signed output — placement and verification composing.

## 4. What the scheduler must never become

- **A service.** The moment placement moves server-side, the network has an
  operator. Presets may be *published* (a marketplace can recommend weights);
  execution stays local.
- **A price oracle.** Scorers read announced prices; they don't set them.
  Price formation stays between providers (announcements) and consumers
  (ceilings and weights).
- **Vendor-aware.** `RTX 4090` must never appear in a policy. If a workload
  needs an ability, that ability is a capability (`cuda:12.8`, `vram≥24576`),
  announced and matched — hardware names stay out of the protocol.

## 5. Open questions

1. **Scoring transparency** — should a placement carry a per-scorer breakdown
   so agents can explain *why* node X won? (Cheap: `Placement` already carries
   the total score.)
2. **Reputation bootstrap** — a new node has no track record; how much weight
   may `TrackRecordScorer` carry before it entrenches incumbents? Possible
   answer: exploration quota (ε-greedy placement) as a first-class scorer.
3. **Collusion resistance for shared telemetry** — locally observed throughput
   is trustworthy; *shared* telemetry is an attack surface. Likely gated on
   signed receipts only.
4. **Multi-node workloads** — gang scheduling (clusters) needs all-or-nothing
   placement across N nodes; today's pipeline places independently. Design
   space: two-phase reserve/commit over the reserved-lease protocol.

## 6. Evolution

| Phase | Deliverable |
|---|---|
| 0 (shipped) | `Pipeline` engine, 4 filters, 4 scorers, `rank`/`place`, quorum integration |
| 1 | `NearbyScorer` (iroh RTT), `UptimeScorer` (directory history), scoring breakdown |
| 2 | `TrackRecordScorer` on on-chain `release_verified` receipts (with RFC-0006 reputation) |
| 3 | Reservations (`ReservationFilter`) + gang scheduling for multi-node workloads |
