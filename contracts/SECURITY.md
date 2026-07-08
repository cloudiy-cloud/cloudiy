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
| **C1** | **Critical** | `verify_ed25519` did not check the precompile's three `instruction_index` fields. An attacker could point them at a second instruction (a valid signature by a key they control) while leaving the provider's key + expected message inline — forging a proof and getting the provider paid for a result never signed. | Require all three `instruction_index == u16::MAX` ("this instruction"), so the precompile verifies exactly the inline bytes the contract compares. Proven: the forgery tx now fails in `ReleaseVerified` with `BadSignature (6008)`, *after* the precompile accepted the attacker signature. |
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

## Mainnet checklist (before launch)

- [ ] **Upgrade authority → multisig (e.g. Squads) or set immutable** after a
      final audit. Today it is a single hot key (`FcZH…`) — highest-priority
      operational risk. *(Requires an owner decision.)*
- [ ] Independent professional audit of `programs/cloudiy-escrow`.
- [ ] Point clients at the **mainnet USDC mint** and re-verify the full loop.
- [ ] Add `cargo audit` (RUSTSEC) to CI for the program's dependency tree.
- [ ] Port the devnet proofs (`examples/permissionless_release.rs`,
      `examples/spoof_release.rs`) into an automated regression suite.
