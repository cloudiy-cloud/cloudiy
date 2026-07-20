# Cloudiy Escrow — Security Notes & Mainnet Readiness

Program: `9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN` (devnet)
Anchor 0.32.1 · fee 4% (`PROTOCOL_FEE_BPS = 400`) · fee authority `GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP`

## What the program does

A per-job USDC escrow. The consumer locks funds in a vault owned by a job PDA
(`seeds = ["job", consumer, job_id]`); funds leave only via one of:

- **`release`** — consumer signs, pays the provider minus fee (voluntary/trusted).
- **`release_verified`** — *permissionless*: anyone may settle if the transaction
  carries an Ed25519 precompile proof that the provider's node key signed
  `RESULT_DOMAIN ‖ 0 ‖ uuid(job_id) ‖ 0 ‖ sha256(output)`. Payout is fixed to the
  provider + fee authority, so a settler can only complete the honest payment.
- **`refund`** — provider may cancel any time; consumer may reclaim after the
  deadline.

All three close the vault **and** the job account, returning rent to the consumer.

## Threat model

Untrusted: the settler, the transaction assembler, the `output_hash` argument,
and every instruction in the transaction. Trusted: the SPL Token program, the
Ed25519 precompile, the Solana runtime, and the program's upgrade authority.

## Findings addressed

| ID | Sev | Issue | Fix |
|----|-----|-------|-----|
| A3 | High | `release_verified` required the consumer's signature — a completed job couldn't be settled without them. | `payer: Signer` (any) + `consumer: UncheckedAccount` receiving only vault rent; payout is fixed on-chain. Proven live with `settler ≠ consumer`. |
| **C1** | **Critical** | `verify_ed25519` did not check the precompile's three `instruction_index` fields. An attacker could point them at a second instruction (a valid signature by a key they control) while leaving the provider's key + expected message inline — forging a proof and getting the provider paid for a result never signed. | Require all three `instruction_index == u16::MAX` ("this instruction"), so the precompile verifies exactly the inline bytes the contract compares. Proven: the forgery tx now fails in `ReleaseVerified` with `BadSignature (6009)`, *after* the precompile accepted the attacker signature. |
| C2 | Med | `release*/refund` closed the vault but not the job account; ~0.002 SOL rent per job was stranded and `job_id` could never be reused. | `close = consumer` on the job account in all three flows. |

Node/consumer-side findings (A1 replay, A2 deadline margin, A4 runner binding,
M1/M2 DoS/CSRF, B1/B2 robustness) are handled off-chain — see the `cloudiy`
crate and its commit history.

## Residual risks (accepted / by design)

- **Deadline race on refund.** Between job completion and the provider claiming,
  a consumer can `refund` once the deadline passes, denying payment for real
  work. Mitigated off-chain: providers refuse escrows whose deadline is within
  `WORKLOAD_TIMEOUT + 120s` (finding A2). Set deadlines with margin.
- **Legacy SPL only.** The program binds `token::Token`; Token-2022 mints
  (transfer fees / hooks) will not deserialize and are effectively unsupported.
  Fine for canonical USDC.
- **Fee rounds down** (floor). Negligible dust favoring the payer; no dust is
  left in the vault (`payout + fee == amount` exactly).
- **`provider_node_key` is consumer-supplied.** A wrong key only locks the
  consumer's own funds until refund; not exploitable against others.

## Testing

- `anchor test` — hermetic suite against a local validator (`tests/`, 11 cases):
  core flows (create_job/release/release_verified/refund), the permissionless
  settle (settler ≠ consumer), and the **C1 spoof forgery is rejected**, plus
  edge cases (timeout bounds, refund authorization, double-release, wrong
  signer). Runs in CI (`.github/workflows/contracts.yml`).
- Advisory scan of the program's separate dependency tree via
  `cargo-deny check advisories` in CI (`.github/workflows/audit.yml`,
  `contracts/deny.toml`) — 0 vulnerabilities; 3 unfixable informational
  advisories from the Solana stack are explicitly ignored.
- Live devnet proofs: `examples/permissionless_release.rs`, `examples/spoof_release.rs`.

## Build notes

- **`driftsort_main` stack-offset warning** (`anchor build`): `Stack offset of
  4104 exceeded max offset of 4096 by 8 bytes`. Investigated and **benign**.
  Source is `borsh`'s canonical `HashMap`/`HashSet` serialization
  (`ser/mod.rs`, `vec.sort_by(...)`), which monomorphizes `core::slice`'s
  stable sort. The program uses **no** sort / `HashMap` / `HashSet` / `BTreeMap`
  and every borsh type here is fixed-layout scalars/arrays, so the sort is a
  transitive codegen instantiation, unreachable from any instruction. The
  release profile already maxes dead-code elimination (`lto = "fat"`,
  `codegen-units = 1`). Even hypothetically, the modern SBF stack guard aborts
  the transaction cleanly on overflow — it does not corrupt state. No action.

## Mainnet checklist (before launch)

- [x] Automated regression suite (`anchor test`) + advisory scan in CI.
- [ ] **Upgrade authority → multisig (e.g. Squads) or set immutable** after a
      final audit. Today it is a single hot key (`FcZH…`) — highest-priority
      operational risk. *(Owner deferred this decision to nearer launch.)*
- [ ] Independent professional audit of `programs/cloudiy-escrow`.
- [ ] Point clients at the **mainnet USDC mint** and re-verify the full loop.
