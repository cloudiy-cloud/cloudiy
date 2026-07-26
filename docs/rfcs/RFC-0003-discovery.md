# RFC-0003 — Provider Discovery & Client-Side Scheduling

| | |
|---|---|
| **Status** | Shipped (reference implementation in this repo; design history below) |
| **Version** | 0.1 |
| **Requires** | RFC-0001 (Vision), PROTOCOL.md (nouns, wire) |
| **Reference implementation** | `cloudiy directory`, `cloudiy share --directory`, `cloudiy run/launch/deploy --via` |

## 1. Problem

RFC-0001 promises *"the consumer describes computation; the protocol resolves
execution"*. That requires answering: **how does a consumer find providers it
has never met?** Until this RFC, consumers dialed providers by Node ID copied
out-of-band — correct, but not discovery.

## 2. Design

Three roles, one invariant: **the directory is an untrusted relay.**

```text
Provider ──signed announcement (heartbeat)──►  Directory
Consumer ──Providers?──────────────────────►  Directory
Consumer ◄──[signed announcements]──────────  Directory
Consumer:  verify every signature → filter/score locally → dial winner P2P
```

### 2.1 Signed announcements

A provider periodically publishes a `SignedAnnouncement`:

| Field | Meaning |
|---|---|
| `payload` | JSON `ProviderAnnouncement` (identity, resources, capabilities, price, utilization, health) — the exact signed bytes |
| `issued_at` | Unix seconds; announcement expires after **180 s** (TTL) |
| `signed_by` | Announcer EndpointId (ed25519) |
| `signature` | ed25519 over `"cloudiy/announce/v1" ‖ issued_at ‖ payload` (domain-separated) |

Verification (done by directories **at the door** and again by consumers):

1. freshness: `now − issued_at ≤ 180 s`, forward skew ≤ 30 s;
2. signature valid for `signed_by`;
3. `payload.identity == signed_by` — a node can never announce resources on
   behalf of another node.

Heartbeat = re-announcing every 60 s; a silent provider ages out of every
directory within the TTL. There is no unregister message — absence is the
signal, which is robust against crashes.

### 2.2 Directory nodes

A directory node (`cloudiy directory`) stores fresh announcements keyed by
announcer, bounded to 10 000 entries, and serves two wire messages over the
same ALPN (`cloudiy/0`):

- `Announce(SignedAnnouncement)` → `Ack` | `Error`
- `Providers` → `Providers([SignedAnnouncement])`

Because entries are signed end-to-end, a malicious directory can at worst
**omit** providers (censorship) — it cannot forge capacities, prices or
identities, and staleness is bounded by the TTL. Censorship resistance comes
from running many directories: providers may announce to several, consumers
may query several and merge. The directory's own identity is a separate key
(`~/.config/cloudiy/directory.key`) so one machine can play both roles.

### 2.3 Client-side scheduling

Consumers fetch announcements, verify them, and run the scheduling pipeline
**locally** (`cloudiy-scheduler`: filters — health, resource fit, capability
match, price ceiling; weighted scorers — price, reputation, utilization,
health). `cloudiy run/launch/deploy --via <directory-id>` replaces `--to`:

```console
$ cloudiy run --via <dir> --kernel matrix_mul --data "2,2,2;…" --token <code>
📡 Scheduled onto 9846fe73… (score 0.70, 10000 micro-USDC/h) — 1 candidate(s)
🔏 Signature verified — result signed by the node you dialed
```

Scheduling client-side keeps directories dumb and policies sovereign: an AI
agent, a marketplace and a CLI user may rank the same announcements
differently, and none of them has to trust a scheduler service.

## 3. Threat model (summary)

| Threat | Mitigation |
|---|---|
| Directory forges providers | impossible — announcements signed by node keys |
| Directory inflates a provider's specs | impossible — payload is signed byte-for-byte |
| Replay of old announcements | TTL + `issued_at` inside the signed payload |
| Provider lies about its own hardware | out of scope here — RFC-0001 §18 verifiability track (benchmarks, TEE attestation, reputation) |
| Directory censors providers | run/query multiple directories; gossip or on-chain registry are drop-in successors |
| Announcement flooding | verification at the door + store cap; economic rate limiting is future work |

## 4. Evolution

This bootstrap directory is deliberately the *simplest correct* discovery
layer. Planned successors — none of which change provider or consumer code
paths, only where announcements travel:

1. **Multi-directory** federation (client-side merge — already possible).
2. **Gossip** (e.g. iroh-gossip) for directory-less swarms.
3. **On-chain registry** binding announcements to stake/reputation, feeding
   the `reputation` field that the scheduler already consumes.

*End of RFC-0003 — Provider Discovery & Client-Side Scheduling*
