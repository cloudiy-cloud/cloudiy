# Cloudiy — End-to-End Audit & Go-Public Readiness

*Internal audit, July 2026. Covers the escrow contract, the provider/networking
layer, the client surfaces (CloudiyOS, MCP, SDKs), and the protocol core. Ranked
by severity. Items marked **[fixed]** were addressed in this pass; **[flag]**
require a decision or a larger change; **[ops]** are launch prerequisites.*

## Executive summary

No unauthorized-drain, payout-redirection, double-release, or refund-block bug
exists in the money layer, and the core authorization invariant (VM/session
ownership bound to the authenticated QUIC peer identity, never a request field)
holds. `cargo clippy` is clean workspace-wide; there is no `TODO`/`unimplemented!`
debt in `crates/`.

The single most important finding is conceptual, not a coding bug:
**`release_verified` proves *provenance*, not *delivery*** — a funded provider can
self-settle without doing the work (HIGH-1). This narrows what "trustless
settlement" can honestly claim and is the top design decision before mainnet.

The remaining substantive items are input-validation and DoS hardening (mostly
**[fixed]** this pass) and a set of well-understood operational blockers
(upgrade authority, mainnet mint, third-party audit).

---

## Money layer (escrow contract + client tx builder + provider verify)

### HIGH-1 — `release_verified` proves provenance, not delivery *(flag — design)*
`contracts/programs/cloudiy-escrow/src/lib.rs:122-172` (proof) + `:331` (permissionless payer).

The Ed25519 proof only establishes "the provider's node key signed *some*
`output_hash`." The provider holds that key, so it can sign a hash of anything
and, because `release_verified` is permissionless, submit the settlement itself —
extracting the escrow without delivering a result to the consumer, who cannot
refund until the deadline. The consumer's real protection (that plain `release`
needs *their* signature after they verify delivery) is bypassed.

This is not third-party theft (payouts still go to the on-chain-recorded provider
the consumer chose), but within the deadline window the escrow gives the consumer
**no** protection against a provider who takes prepaid funds and does nothing.

`release_verified` is genuinely safe **when the output is independently
verifiable** — a deterministic kernel plus `run --replicas N` quorum, or any case
where a third party can recompute/compare the result. It is *not* a
proof-of-delivery for opaque single-provider work.

**Actions:**
- **[fixed, docs]** Corrected the public "trustless settlement" wording so it
  claims provenance + consumer-side verification, not delivery.
- **[flag]** Choose a direction before mainnet: (a) require the *consumer's*
  signature over the accepted `output_hash` in `release_verified` (keeps it from
  being provider-forceable); (b) a dispute/challenge window before funds move;
  or (c) restrict permissionless `release_verified` to replicated/deterministic
  workloads and keep consumer-consent `release` as the default for opaque work.

### MEDIUM-2 — `run_auth_message` binds only `job_id` *(flag — protocol v2)*
`crates/cloudiy/src/payments.rs:61-67`, consumed `:159-169`.

The consumer's run-authorization signature covers `"cloudiy/escrow-run/v1" ‖ 0 ‖
job_id` and nothing else — not the workload, provider, escrow account, amount, or
an expiry/nonce. A captured run-auth sig can be replayed to spend the consumer's
prepaid compute on a *different* workload, and never expires (so it is reusable
after a `job_id` is recycled via close+reuse).

**Action [flag]:** bump the domain to `v2` and bind `sha256(workload) ‖
provider_node_id ‖ escrow_account ‖ expiry` into the signed message; provider
checks all fields. Coordinated change across `payments.rs`, the consumer, and the
SDKs — do it as one commit, not piecemeal.

### LOW — `settle` fee math uses `u64`, not the `u128` the client uses *(flag)*
`lib.rs:504-518` vs `solana.rs:24-26`. `amount * bps` overflows `u64` above
~1.8e15 micro-USDC and returns `MathOverflow` (safe-by-erroring, funds still
refundable) — not exploitable, but align to `u128` on the next contract revision
to match the client and the "no overflow" framing.

### Verified clean (money layer)
- **C1 Ed25519 anti-spoof** (`verify_ed25519`, `:597-646`): correct and complete —
  all three instruction-index fields pinned to `u16::MAX`, `d[0]==1` rejects a
  second inline signature, offsets bounds-checked, inline pubkey/message compared;
  `solana.rs::ed25519_verify_ix` builds the matching ix. The bundled spoof harness
  is the right negative test.
- **A3 permissionless settle**: payees/bps come only from the on-chain `Job`; a
  settler ≠ consumer can only complete the honest payout.
- **PDA/account constraints, double-release, refund lockup, cross-provider replay,
  client tx builder** (`find_program_address`, ATA, shortvec/legacy message,
  `uuid_string` matching the client): all correct.
- **Browser `create_job`** (`web/vm.html::escrowCreateJob`): byte-faithful to
  `solana.rs` and the contract (discriminator, `disc(8)++job_id(16)++amount(u64
  LE)++timeout(i64 LE)++node_key(32)`, seeds, 8-account order, devnet target). No
  fund-locking mismatch.

---

## Provider / networking layer

### MEDIUM — Argument injection: `image`/`command` starting with `-` *(fixed)*
`crates/cloudiy/src/vm.rs`. A gateway-supplied `image = "--privileged"` (or
`command = ["--..."]`) was pushed as a positional and could be parsed by docker as
a flag, defeating the isolation choices. **Fixed:** `validate_image` /
`validate_command` reject a leading `-` and constrain the image charset before
`pull`/`run`/`exec`.

### MEDIUM — Tenant VM has unrestricted host/LAN egress *(flag — infra)*
`vm.rs:205-272`. A dev container can reach the provider's private LAN and cloud
metadata (`169.254.169.254`) to steal the host's cloud credentials. `no-new-privileges`
+ dropped caps don't stop *routed traffic*. **Action:** for untrusted/metered
tenants attach an egress-filtered internal network (block link-local + RFC1918) or
require the gVisor/Kata `--runtime` path; document that as the untrusted-tenant
posture. (Loopback-bound ports and the 127.0.0.1 gateway are already unreachable
from the container bridge — good.)

### MEDIUM — Unbounded pre-auth stream concurrency + 8 MiB pre-auth alloc *(flag)*
`p2p.rs:36-48`. A peer opens N bi-streams, each reading/JSON-parsing up to 8 MiB
before any auth/permit; only interactive streams take the `sessions` semaphore.
**Action:** add a global inbound-stream semaphore (or per-connection cap) in
`handle_conn`, and a smaller frame cap for the first request frame.

### LOW — endpoint workers unbounded/uncapped; media & workers never reaped *(flag)*
`gateway.rs`. The ollama/sdxl/ltx worker `docker run`s set no `--memory`/`--cpus`/
`--pids-limit`/`--no-new-privileges`; `/api/endpoint` has no concurrency cap beyond
`worker_lock`; `serve_media` files and idle worker containers are never cleaned.
**Action:** add resource caps + an idle reaper + media TTL.

### LOW — 16-char owner truncation collides container/volume names *(flag)*
`vm.rs:30-38`. Names use `owner[..16]` while the map keys on the full id; a shared
64-bit prefix (infeasible to grind, but latent) would let one tenant's `rm --force`
kill another's container and mount their `/root` volume. **Action:** name on a hash
of the *full* owner id.

### LOW — `std::sync::Mutex` `.lock().unwrap()` poisoning is a DoS primitive *(flag)*
Any panic under a held lock poisons it and every later `.lock().unwrap()` panics →
provider dead. No panic-under-lock path exists today, but prefer `parking_lot::Mutex`
(no poisoning) on a network-facing daemon.

### Verified clean (networking)
Ownership binding (`conn.remote_id()`), constant-time token compare
(`subtle::ct_eq`), 8 MiB frame cap, M2 Origin/Host guard (rejects non-loopback
Host, cross-site/`null` Origin — CSRF + DNS-rebinding safe), PTY lifetime, argv-based
docker (no shell injection), `serve_media` path traversal, tunnel SSRF. The legacy
HTTP API on `0.0.0.0:8080` with permissive CORS is by-design (payment-gated) but
carries a standing "lock down before mainnet" note — see ops list.

---

## Client surfaces (CloudiyOS, MCP, SDKs)

### HIGH — Consumer SDKs (Python/JS) do NOT verify result signatures *(fixed)*
`sdk/python/cloudiy_sdk/__init__.py`, `sdk/js/cloudiy.mjs`. Both used to return the
node's `output_data` verbatim — no signature parsed, no `require_signature` — so the
two SDKs marketed for agents trusted whatever the node returned; a malicious provider
or MITM could return forged output that a paying agent then acts on. **Fixed:** both
SDKs now parse the result's `signature`/`signed_by` and ed25519-verify it against the
same domain-separated payload as `crates/common/src/sig.rs`
(`"cloudiy/result/v1" ‖ 0 ‖ job_id ‖ 0 ‖ sha256(output)`), **default `verify=True`** —
a missing or tampered signature raises `SignatureError` instead of returning output.
An optional `expect_pubkey`/`expectPubkey` pins the provider identity (matching the
Rust SDK, which pins the dialed node). The verify is self-contained (a stdlib/BigInt
Ed25519 verify + the runtime's SHA-256/512), so it **keeps the zero-dependency
promise** rather than pulling `pynacl`/`@noble` — verification is public-key-only, so
a non-constant-time implementation is safe. Test vectors generated from the Rust
signer confirm cross-language agreement (`sdk/*/…test…`).
**[residual, flag]:** remote nodes are still reached over plaintext `http://` by
default; signatures now defeat output *tampering* over the wire, but TLS is still
wanted for prompt/output *confidentiality* against a passive MITM.

### HIGH — Wallet page loaded third-party scripts without SRI *(fixed)*
`web/vm.html:8-17`. The origin that connects Phantom and builds the signed escrow
tx pulled `@solana/web3.js` and others from CDNs with no integrity check. **Fixed:**
pinned versions now carry SHA-384 SRI + `crossorigin`. **[flag]** Tailwind's
unversioned CDN build can't take a stable hash — replace it with a compiled
stylesheet before a production launch.

### HIGH — DOM XSS from gateway/provider-controlled output *(fixed)*
`web/vm.html`. `media_url`, `image_b64`, and provider-announcement fields
(`gp.payout`/`gp.nodeKey`) were interpolated into `innerHTML`. A hostile provider
(via a local gateway) could inject markup into the wallet-connected origin and read
`localStorage`/drive escrow signing. **Fixed:** media nodes built via
`createElement` with a scheme allowlist (`safeMediaUrl`), all attribute
interpolations run through `escAttr`, `image_b64` sanitized to the base64 charset.

### MEDIUM — MCP devnet guard was a substring match *(fixed)*
`crates/cloudiy/src/mcp.rs:42`. `rpc_url.contains("devnet")` let a mainnet URL with
`devnet` in a path/query bypass the guard. **Fixed:** `rpc_is_non_mainnet` classifies
by *host*, refuses anything naming `mainnet`, and treats unknown custom RPCs as
potentially-mainnet (requires `--allow-mainnet`). Unit-tested.

### LOW — Prompts silently sent to a third-party relay *(flag)*
`web/vm.html`. Image-endpoint runs route the prompt (and any reference-image URL) to
`image.pollinations.ai` with only a source-comment disclosure. Relay output is only
rendered/cached (never signed or written to escrow — trust-beyond-display is clean).
**Action:** disclose the relay in the run UI or gate it behind explicit opt-in.

### Verified clean (client)
MCP spend caps (checked after amount resolution, before signing; sequential
dispatch; `read_only` strips + re-checks signing tools; keypair never returned; no
arbitrary-message signing oracle; tracing→stderr keeps the JSON-RPC channel clean),
wallet amount/provider parity, terminal bytes to `xterm.write` (not HTML), static
pages (hardcoded innerHTML only).

---

## Protocol core

### MEDIUM — Integer overflow in `verify_announcement` on wire `issued_at` *(fixed)*
`crates/common/src/sig.rs:99-104`. `now - issued_at` on an attacker-supplied
`i64::MIN` panicked in debug (poisoning the directory `Mutex` → remote DoS from one
unauthenticated frame, since the arithmetic runs *before* signature verification)
and silently wrapped in release. **Fixed:** `saturating_sub`.

### MEDIUM — WGSL arbitrary-shader path panicked on unaligned buffers *(fixed)*
`crates/runtime/src/wgsl.rs`. Caller-supplied `output_len`/input lengths not aligned
to 4 bytes tripped a wgpu validation error whose default handler `panic!`s (contained
by `spawn_blocking`, but an opaque failure). **Fixed:** reject sizes not a multiple of
`COPY_BUFFER_ALIGNMENT` up front as a clean job error. **[flag]** the remaining
uniform/unused-group-0-binding mismatch still surfaces as a contained panic — wrap the
device in a wgpu error scope for a clean error.

### LOW — scheduler NaN score / `checked_add` not checked *(fixed)*
`scheduler/src/lib.rs`: a NaN scorer output sorted a node to the top under `total_cmp`
— **fixed** by sanitizing non-finite scores to 0. `protocol/src/resource.rs::checked_add`
did unchecked `+=` despite its name — **fixed** to `saturating_add`.

### LOW — other latent items *(flag)*
`Resources::release` can't detect an unmatched/double release (oversubscription if a
call site mis-pairs; today symmetric); `SessionFrame::Exit(i32::MIN)` sentinel collides
with a genuine `i32::MIN` exit code (unreachable in practice).

### Dead code *(fixed / noted)*
- **[fixed]** Deleted `ProviderInfo` (`common/src/types.rs`) — zero references.
- **[fixed]** Reconciled the `PROTOCOL_VERSION` doc comment vs the ALPN constant.
- **[noted]** `Reputation` trait (`protocol/src/settlement.rs`) is an intentional
  future seam (like `Settlement`); kept. `solana.rs:122 unreachable!` is the standard
  `find_program_address` idiom; fine.

---

## What's left to run publicly, end-to-end

### Blockers — do not launch on mainnet without these
1. **Upgrade authority** is a single hot key (`FcZH…`). Move to a Squads multisig or
   set the program immutable — until then the authority can replace the program and
   every on-chain guarantee above is void. *(Highest real-world risk.)*
2. **Independent contract audit** of `cloudiy-escrow` before it holds real USDC.
3. **Mainnet USDC mint + program redeploy**: switch clients (`solana.rs`, `vm.html`,
   provider `--usdc-mint`) from the devnet test mint to the real Circle mint, and
   redeploy the escrow to mainnet.
4. **Settle the `release_verified` design** (HIGH-1) — decide consumer-consent vs
   dispute window vs replication-gated, or restrict its use.
5. ~~**SDK signature verification** (HIGH) — agents must not trust unsigned output.~~
   **Done:** Python/JS SDKs now ed25519-verify the result by default (`verify=True`),
   raising `SignatureError` on missing/tampered signatures, zero-dependency. Remaining:
   TLS for remote-node confidentiality (integrity is now covered by the signature).

### Hardening before untrusted tenants run real workloads
6. Tenant VM egress filtering / metadata block (MEDIUM), or mandate gVisor/Kata.
7. Pre-auth stream concurrency cap (MEDIUM); worker resource caps + reaper + media
   TTL (LOW); full-owner-hash names (LOW); `parking_lot` mutexes (LOW).
8. Lock down / authenticate the legacy `0.0.0.0:8080` HTTP API (or bind loopback).
9. Replace the Tailwind CDN dev build with a compiled stylesheet; add a CSP where the
   toolchain allows.
10. `run_auth_message` v2 binding (MEDIUM-2).

### Product completeness for a working end-to-end public network
11. **Published GPU worker images + first NVIDIA nodes** — image/video endpoints and
    real GPU inference need `ghcr.io/cloudiy/*` workers to exist and a GPU provider
    online. Until then those endpoints correctly report "needs a GPU node."
12. **A hosted directory + a baked default** (`CLOUDIY_DEFAULT_DIRECTORY`) so a
    zero-config consumer discovers providers without flags.
13. **Reputation/quorum in practice** — surface `run --replicas` and on-chain
    `release_verified` receipts as the reputation signal (RFC-0005/0006), which also
    makes `release_verified` safe for opaque work.
14. CI: add `cargo audit` (advisory DB) and the escrow proof-harness examples to the
    gated pipeline.

### Already solid (don't re-litigate)
Ownership binding, constant-time auth, framing bounds, Origin/Host guard, PTY, escrow
state machine, C1/A3, PDA constraints, client tx builder, MCP spend caps, clippy-clean
workspace, no committed secrets/artifacts.
