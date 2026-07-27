# RFC-0009 — Persistent Volume v2: encrypted, incremental, portable state

| | |
|---|---|
| **Status** | Prototype shipped behind a flag (key derivation + manifest, `crates/cloudiy/src/volume.rs`); DECISION POINTS A–E still open (E = who chooses the store, §5.5) |
| **Version** | 0.1 |
| **Requires** | RFC-0004 (Entitlements/Storage), RFC-0008 (Replicated Settlement — the pattern this mirrors for *state*) |
| **Contract change** | **None.** Volume state is off-chain; nothing here touches the escrow. |
| **Reference implementation** | `crates/cloudiy::vm` (`volume_sync`), prototype behind `CLOUDIY_VOLUME_MODE=snapshot` |

---

## Abstract

Today a CloudiyOS VM's `/root` is a Docker volume that `volume_sync` pushes to
`CLOUDIY_VOLUME_REMOTE` with `rclone copy` on stop and pulls back on start
(`crates/cloudiy/src/vm.rs`). The UI calls this an *"Encrypted, replicated
network volume"* (`web/os.html`) — but encryption and replication are properties
of **the operator's rclone remote**, not of the protocol. A consumer cannot
verify either, and a curious provider or remote operator sees plaintext `/root`.

This RFC evolves that into **client-encrypted, incremental, content-addressed
snapshots** keyed from the **consumer's wallet**, plus a lightweight
**declarative manifest** that reconstructs most environments with no volume sync
at all. It ends with the roadmap piece — **the network itself as the store**, the
state analogue of RFC-0008.

The whole design turns on one honest question, asked in §3 and answered
explicitly: *the sync runs on the provider — so where, and when, is the key
exposed?*

---

## 1. What's wrong with `rclone copy` today

- **No protocol-level confidentiality.** `rclone copy` moves plaintext bytes. If
  the operator's remote isn't a `crypt` remote, `/root` is readable at rest by
  whoever holds the remote; even with `crypt`, the key is the *operator's*, so
  the consumer still trusts the provider for confidentiality. The marketing
  wording oversells what the protocol guarantees.
- **No deduplication / history.** `copy` re-walks the tree every stop; a 5 GB
  home with a 2 MB change re-uploads whatever rclone's size/mtime check thinks
  moved, with no block-level dedup and no point-in-time history. A corrupted
  sync overwrites the only copy.
- **No integrity binding.** Nothing ties the restored bytes to the consumer.
  A malicious remote could serve a *different* consumer's tree (or a tampered
  one) and the VM would boot it.
- **Coarse trust.** "Encrypted, replicated" are the operator's promises to keep,
  invisible to the consumer.

The volume is treated as a **pet** (a mutable home synced whole). Two better
primitives: encrypted **snapshots** (a better pet) and a declarative **manifest**
(cattle). We want both.

---

## 2. Snapshot engine: restic vs borg vs roll-our-own

Requirements: client-side encryption with a key *we* derive, block/chunk
deduplication, content-addressed history, restore-by-snapshot, and a backend
abstraction that can target an rclone remote today and the network later.

| | **restic** | **borg** | **roll-our-own** |
|---|---|---|---|
| Client-side encryption | AES-256-CTR + Poly1305-AES MAC, key never leaves client | AES-256-CTR + HMAC-SHA256 | whatever we build (new crypto = new risk) |
| Dedup | content-defined chunking, cross-snapshot | content-defined chunking | must implement |
| History | content-addressed snapshots, prune policies | archives + prune | must implement |
| Backends | local, SFTP, S3, GCS, Azure, **rclone** (any remote), REST | local, SSH | must implement each |
| **Key input we control** | `RESTIC_PASSWORD` / `--password-command` (a passphrase we can feed) | passphrase / keyfile | native |
| Ops model | single static Go binary, easy to ship in a sidecar container | Python + msgpack, heavier to containerize | — |
| Audited | yes, widely deployed | yes, widely deployed | no |

**Recommendation: restic.** Decisive factors:

1. Its **rclone backend** (`restic -r rclone:REMOTE:path`) means the prototype
   reuses the operator's *existing* `CLOUDIY_VOLUME_REMOTE` and rclone config
   with zero new operator setup — restic drives rclone instead of us calling
   `rclone copy`. The migration is a swap of the sync engine, not the backend.
2. It ingests a **passphrase we supply** (`--password-command`), which is exactly
   the seam a wallet-derived key needs — no fork, no patched crypto.
3. Single static binary → a clean sidecar container, same shape as the current
   `rclone/rclone` transient container.

borg is comparable cryptographically but has no first-class rclone backend and a
heavier runtime. Rolling our own means writing and auditing chunking + AEAD +
a repo format — new cryptographic surface for no benefit over a battle-tested
tool. We reject it.

> **DECISION POINT A — snapshot engine.** Recommendation: **restic**. Alternative:
> borg (only if a restic-specific blocker appears). Own-implementation is not
> recommended and would itself need a separate audit.

---

## 3. The key, and the central dilemma

### 3.1 Deriving a symmetric key from the wallet

The consumer already holds an ed25519 wallet keypair (`~/.config/solana/id.json`,
loaded by `crate::solana::Keypair`). We derive the volume key from it the same
domain-separated way the rest of the protocol signs (`cloudiy/result/v2`,
`cloudiy/escrow-run/v3`, …):

```
sig   = Ed25519_sign(wallet_sk, "cloudiy/volume-key/v1")   // 64 bytes
key   = HKDF-SHA256(ikm = sig, salt = "cloudiy/volume-key/v1", info = owner_id) // 32 bytes
```

Then `key` is the restic repository password (fed via `--password-command`, never
written to disk).

**Why this is stable.** ed25519-dalek implements RFC 8032, where the signature is
a *deterministic* function of key and message — signing `"cloudiy/volume-key/v1"`
yields the same 64 bytes every time, on any machine, so the same wallet always
derives the same volume key. (Verified: `crates/cloudiy/src/solana.rs` uses
`ed25519_dalek::SigningKey`, no randomized-signing feature.) A non-deterministic
signer would silently make every session's key different and every restore fail —
so this property is load-bearing and the prototype asserts it in a test.

**Why sign a constant, not just hash the secret key.** The wallet secret never
appears in the derivation input; only a signature over a fixed domain string
does. That keeps the key material one non-invertible step away from the wallet
seed, and the domain string means this signature can't be replayed as any other
protocol signature (they all carry distinct domains). `info = owner_id` binds the
key to *this identity*, so two identities on one wallet (if ever supported) don't
share a volume key.

### 3.2 The dilemma: the sync runs on the provider

Encryption is only worth anything if the **key is never visible to the
provider**. But the volume lives on the provider and the sync has historically
run there. Two architectures resolve this differently.

**Architecture A — encrypt inside the VM, on the provider.**
A sidecar (restic) runs on the provider host at stop, reads plaintext `/root`,
encrypts, pushes ciphertext to the remote. For it to encrypt with the consumer's
key, **the key must reach the provider** (passed in at stop, or derived from a
signature the consumer hands over).

- *Exposure:* the key — and plaintext `/root` — are on the provider host at
  snapshot time. A malicious provider captures both. Encryption-at-rest on the
  remote is achieved; **confidentiality against the provider is not.**
- *Pro:* minimal moving parts, reuses today's transient-container shape, fast
  (local disk → restic → remote, no round-trip through the consumer).
- This is barely better than today for the threat that matters (the provider),
  though it does fix at-rest exposure on the remote and adds dedup/history.

**Architecture B — client-side sync over the tunnel.**
The consumer already has an authenticated QUIC tunnel to its VM
(`cloudiy tunnel`, `handle_tunnel` in `p2p.rs`). The snapshot runs **on the
consumer's machine**: it reads `/root` *through the tunnel*, and restic encrypts
locally with the wallet key, which **never leaves the consumer**. Ciphertext then
goes to the remote (or, later, the network).

- *Exposure:* the key never touches the provider. The provider still sees
  plaintext `/root` *while the VM runs* (it executes the workload — unavoidable
  without a TEE, and already true today), but the **durable, portable state is
  confidential to the consumer**: at rest and in transit it is ciphertext the
  provider can't read, and a different provider restoring it next time gets
  ciphertext it can't read either.
- *Con:* slower (bytes traverse the tunnel), and it needs a consumer-side agent
  running at stop — a behavioral change from "fire and forget on the provider".
- *This is the only architecture that makes the protocol's confidentiality claim
  true against the provider.*

**Recommendation: Architecture B is the target; a clearly-labeled Architecture A
is an acceptable interim** for the prototype where a consumer-side agent isn't
present — but its weaker guarantee must be stated in the UI, not hidden behind
"encrypted". The honest framing:

- *A:* "encrypted at rest on the store" (protects against the remote/operator, **not** the provider).
- *B:* "end-to-end encrypted with your wallet" (protects against the provider and the remote).

Neither protects the *running* VM's memory/disk from its host — that needs a TEE
and is out of scope (RFC-0006 §5 transparency note already says prompts/inputs
are visible to the executing provider).

> **DECISION POINT B — architecture.** Recommendation: ship **B** as the real
> product; allow **A** only as an explicitly-labeled interim, never described as
> protecting against the provider. Do we want the prototype to implement A
> (provider-side, faster to stand up) or go straight to B (consumer-side,
> correct)? Recommendation: prototype the **key derivation + restic repo format
> against a local backend first** (pure/testable), then wire B.

### 3.3 Key loss and rotation — the hard truths

- **Lose the wallet → lose the state.** The volume key is derived *only* from the
  wallet. There is no recovery mailbox, no provider-side copy, no reset. This is
  the price of provider-blind confidentiality and it must be said plainly in the
  UI, once, in words a non-cryptographer understands: *"Your VM's saved state is
  locked to this wallet. Lose the wallet, lose the state. Back up the wallet."*
- **Rotation is not re-encryption of the past.** Because the key is a pure
  function of the wallet, "rotating" it means either (a) moving to a new wallet
  and re-snapshotting from a live restore (old snapshots stay readable only by
  the old wallet), or (b) introducing a key *version* in the domain string
  (`cloudiy/volume-key/v2`) and re-encrypting existing snapshots during a live
  session. restic supports multiple repo keys, which eases (b).

> **DECISION POINT C — key model.** (1) Accept "lose wallet = lose state" as the
> default (recommended — it is the honest cost of E2E), or add an *optional*
> escrow-of-key-to-a-second-wallet feature later? (2) Rotation via new-wallet
> re-snapshot (simple) vs versioned domain + restic multi-key (more moving
> parts)? Recommendation: default to (1)=lose-means-lose with a loud UI warning;
> defer rotation to a follow-up, reserving `cloudiy/volume-key/vN` now.

---

## 4. Declarative manifest ("cattle")

Most dev environments don't need a 5 GB home synced — they need *the recipe*.
A manifest reconstructs the environment on any node with **no volume sync**:

```toml
# cloudiy.volume.toml — declarative environment (v1)
[env]
image = "debian:12-slim"          # base, as today

[packages]
apt = ["build-essential", "git", "ripgrep"]
pipx = ["poetry"]

[dotfiles]
# fetched over the tunnel from the consumer, or from a public URL
".gitconfig" = "inline:..."
".config/nvim/" = "git:https://github.com/user/nvim-config"

[repos]
"~/work/app" = { url = "https://github.com/org/app", ref = "main" }

[secrets]
# a single blob, encrypted with the wallet key (§3.1), decrypted into the VM
# at build time over the tunnel — never stored plaintext on the provider
blob = "restic:secrets"           # or an inline age-encrypted ciphertext
```

Properties:

- **Portable and tiny.** The manifest is kilobytes; it rebuilds from upstream
  sources (apt, git, pipx) rather than shipping bytes. A new node reaches parity
  in the time it takes to `apt install`, not to download a home.
- **The secrets blob is the only ciphertext**, encrypted with the wallet key like
  §3.1, so credentials aren't in the plaintext manifest and aren't visible to the
  provider (decrypted into the VM over the tunnel at build time).
- **Composes with snapshots.** Manifest builds the *base*; a snapshot layers the
  *mutable delta* on top for people who do want their exact home. Manifest-only
  is the light default; manifest + snapshot is the heavy option.
- **Reviewable.** Unlike an opaque volume, a manifest is human-readable and
  diffable — you can see what an environment will contain before you run it.

This is the path that covers the majority of "I want my tools on whatever node
runs my job" cases without any of §3's exposure trade-offs, because there's no
plaintext home to sync at all.

> **DECISION POINT D — manifest format.** TOML (matches Cargo/rustup ergonomics,
> recommended) vs YAML (familiar to devops) vs JSON (machine-first)? And: is the
> secrets blob restic-backed or a standalone `age` ciphertext? Recommendation:
> **TOML**, secrets as a standalone wallet-encrypted blob so the manifest is
> useful even without the snapshot engine.

---

## 5. Future: the network as the store

RFC-0008 made *compute* trustless by replicating a job across N providers and
settling on agreement. The state analogue: **replicate the encrypted volume
across N providers with erasure coding**, so no single provider (or remote
operator) is trusted for durability *or* confidentiality.

Sketch only — not designed here:

- Snapshot → client-encrypted (§3) → **erasure-coded** (e.g. Reed-Solomon `k`-of-`n`)
  into `n` shards; any `k` reconstruct. Shards are ciphertext, so a shard-holder
  learns nothing.
- Shards distributed to `n` providers, **paid per shard per epoch** through the
  same escrow primitive RFC-0008 uses for compute (storage as a metered
  workload — RFC-0004 already reserves `storage_bps`).
- Retrieval fetches any `k` shards, verifies each against a content-addressed
  manifest root, reconstructs, decrypts locally.
- **Durability without a trusted remote**: lose up to `n−k` providers and the
  state survives; add proof-of-storage challenges (à la Filecoin/Arweave) so
  providers can't claim to hold a shard they dropped.

This is where "encrypted, replicated network volume" becomes literally true at
the protocol level rather than a property of one operator's rclone remote. It is
a large piece of work (shard placement, storage proofs, repair, payment epochs)
and is deliberately out of scope for this RFC beyond stating the direction.

---

## 5.5 Who chooses the store (the centralization the thesis actually forbids)

§3.2 answered *where the sync runs*; this answers *who picks the destination* —
and it is the more load-bearing question for the project's thesis.

**Today it's the operator.** `CLOUDIY_VOLUME_REMOTE` is an env var the **provider
operator** sets. So the durable copy of a consumer's state lands in a bucket the
consumer never chose and doesn't control — a central point, picked by someone
else, that the consumer must trust for both confidentiality and availability. For
a protocol whose whole pitch is "no central intermediary," that is the wrong
default hiding in an env var.

**The fix is not "no store" — it's "the consumer picks, and the protocol imposes
none."** Two things make this safe:

1. **The consumer declares the store**, not the operator. The store address is a
   consumer input (part of the volume manifest / a per-consumer setting), passed
   to the VM the same way ownership already is — never a provider global.
2. **Client-side encryption with the wallet-derived key (§3.1) before bytes leave
   the machine** (Architecture B, §3.2). This is the unlock: once the state is
   ciphertext only the wallet can read, **the store stops being a trust point.**
   It can be a commercial S3, a friend's box, anything — it holds bytes it cannot
   read and cannot selectively corrupt without detection (content-addressed
   manifest root). What stays "central" is then *only availability* — one bucket
   is a single point of *failure*, not of *trust* — and availability is solved by
   pointing at more than one backend, not by trusting any one.

> **Decentralized is not "there is no central store." It is "the protocol does not
> impose one."** A consumer who wants the convenience of AWS should get it; a
> consumer who wants a trustless network store (§5) should get that; neither is
> baked in. The protocol's job is to make the *choice* the consumer's and to make
> every choice safe by encrypting before egress.

**Options, in increasing order of effort:**

- **(a) Consumer points at their own rclone backend — almost here.** rclone
  already speaks S3/R2/B2/GDrive/SFTP/… The wallet-derived key and the restic
  repo format already exist (`volume.rs`, §3.1/§6). What's missing is small and
  concrete: (i) take the remote from a **consumer** setting instead of the
  operator env, (ii) do the encrypt/sync **consumer-side over the tunnel** (§3.2-B)
  so the key never reaches the provider, (iii) a one-field UI ("where should your
  VM's state live? paste an rclone remote / connect a bucket"). This is the
  recommended first deliverable after this RFC — it turns the store from an
  operator secret into a consumer choice with the confidentiality claim actually
  true.
- **(b) A decentralized default for the consumer who has nothing.** Most users
  won't have a bucket. They need a sane default that isn't "trust this one
  operator." **Shadow Drive deserves first analysis** here: it is Solana-native
  (so it settles against the *same wallet* the user already has — no second
  account, no second payment rail) and is built for exactly this. Others
  (Arweave for permanence, IPFS/Filecoin for availability-priced storage) are
  worth a comparison pass. Open question: is the default *provisioned for* the
  user (protocol pays, meters it back) or *pointed at* (user brings their own
  Shadow Drive account)? **Not built here** — this is the design note that says
  "evaluate Shadow Drive before defaulting anyone anywhere."
- **(c) The network itself as the store (§5).** Erasure-coded, client-encrypted
  shards paid per epoch through the RFC-0008 escrow primitive. This is the end
  state where "no imposed central store" is literally true at the protocol level.
  Vision, sketched in §5, **not now** — large (shard placement, storage proofs,
  repair).

> **DECISION POINT E — who chooses the store.** Recommendation: make the store a
> **consumer** choice, encrypted client-side (a) as the first real deliverable;
> evaluate **Shadow Drive** as the zero-config default (b) before shipping any
> default; keep the network store (c) as the tracked end state. The operator env
> `CLOUDIY_VOLUME_REMOTE` stays only as an operator-convenience fallback, clearly
> labeled as *not* consumer-confidential until (a) lands (it's Architecture A).

---

## 6. Build list (this RFC)

1. **This document** + DECISION POINTS A–D for the user.
2. `crates/cloudiy` **key derivation** (`volume_key`): sign `cloudiy/volume-key/v1`
   → HKDF → 32-byte key. Pure, with a determinism test and a domain-separation
   test. (Prototype.)
3. **Manifest parse** (`cloudiy.volume.toml` → typed struct). Pure, with tests
   for the shapes above and malformed input. (Prototype.)
4. **`CLOUDIY_VOLUME_MODE=snapshot`** opt-in in `vm.rs`: when set, use the restic
   engine (Architecture A interim for the prototype, clearly labeled) instead of
   `rclone copy`; **default unchanged** (`rclone copy` stays the default and the
   code path is untouched). E2E behind Docker; documented manual test otherwise.
5. Leave §3.2-B (consumer-side tunnel sync), §5 (network store), and rotation as
   follow-ups gated on the DECISION POINTS.

## 7. Non-goals / locked

- **No contract change.** Volume state is off-chain.
- **No removal of the rclone path.** It stays the default; `snapshot` is purely
  additive and opt-in.
- **No TEE / running-VM confidentiality.** The executing provider sees the live
  VM; only durable/portable state is made provider-blind. Consistent with the
  RFC-0006 transparency note.

## 8. Open questions (beyond the decision points)

1. **When does B's consumer-side agent run?** A VM can be reaped for lease
   expiry while the consumer is offline (`reap_expired` in `vm.rs`) — then no
   consumer-side sync can happen. Does lease-expiry reaping fall back to A
   (provider-side, labeled) or to "manifest-only, no snapshot"? Leaning:
   manifest-only survivable state + a warning that unsynced deltas are lost on an
   offline reap.
2. **restic repo per owner vs shared repo with per-owner keys.** Per-owner repo
   is simpler and isolates blast radius; a shared repo dedups across tenants but
   leaks cross-tenant chunk-existence. Leaning: per-owner repo.
3. **Integrity/anti-substitution** — bind the restic repo id (or snapshot root)
   to the owner so a remote can't serve another consumer's ciphertext. restic's
   repo is already keyed, so a wrong-key restore fails closed; still, the owner
   namespacing from today's `volume_sync` should carry over.
