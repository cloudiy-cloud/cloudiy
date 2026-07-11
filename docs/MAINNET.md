# Cloudiy — Devnet → Mainnet Runbook

This is the checklist to take Cloudiy's payment layer from **devnet** to **mainnet**.

> **EVERY step in this document is HUMAN-executed.** It involves deploying a program
> to mainnet, moving real funds, and flipping production config. Nothing here should
> be automated by tooling or an agent. Treat this as a runbook a person follows
> deliberately, ideally with a second reviewer.

## Current devnet facts (baseline)

| Item | Devnet value |
|------|--------------|
| Escrow program name | `cloudiy_escrow` |
| Escrow program id | `9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN` |
| RPC | `https://api.devnet.solana.com` |
| Mint | devnet test SPL mint |

## Target mainnet facts

| Item | Mainnet value |
|------|---------------|
| RPC | `https://api.mainnet-beta.solana.com` |
| Mint | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` (real circle USDC) |
| `--require-payment` | enabled on all providers |

---

## 1. Audit + review the Anchor program

- Get an independent security review of the escrow program in `contracts/`.
- Review escrow account math, PDA seeds, authority checks, and the
  release/refund/close paths for edge cases (double-spend, early release, stuck funds).
- Confirm there is no test-only / devnet-only code path that weakens checks.
- Lock the toolchain: record the exact `anchor` and `solana` CLI versions used, and
  build with `anchor build --verifiable` so the on-chain bytecode is reproducible.

## 2. Build & deploy to mainnet

Fund a mainnet **deploy keypair** with enough SOL for the program's rent-exempt
deployment (a real cost — plan for a few SOL depending on program size).

Two ways to handle the program id:

- **Keep the same id** — reuse the existing program keypair as a mainnet keypair so
  the id `9zMBC7JD...` carries over. Requires the original program keypair.
- **New id** — deploy with a fresh keypair and record the new program id.

```bash
anchor build --verifiable
anchor deploy --provider.cluster mainnet \
  --provider.wallet /path/to/mainnet-deploy-keypair.json
```

**Record the resulting program id.** You'll wire it into the provider flags and the
web config below.

## 3. Configure providers for mainnet

Run each provider (`cloudiy share`) with mainnet payment flags:

```bash
cloudiy share \
  --rpc-url https://api.mainnet-beta.solana.com \
  --mint EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v \
  --require-payment
```

- Set the escrow program id to the mainnet id from step 2 (via config/flag as the
  provider expects it).
- `--require-payment` MUST be on so providers only serve paid, escrowed jobs.

## 4. Flip the web app escrow config (reference only)

In `web/os.html` there is an `ESCROW` config block. A human updates it to point at
mainnet:

- `programId` → the mainnet escrow program id (step 2)
- `mint` → `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`
- `rpcUrl` → `https://api.mainnet-beta.solana.com`

> Reference only — **a human flips this in `web/os.html`.** The ops scaffolding does
> not edit `web/`.

## 5. Authority handling

- **Upgrade authority**: decide who can upgrade the program. For a trust-minimized
  launch, transfer the upgrade authority to a multisig (e.g. Squads) or set it to a
  known, secured key — or make the program immutable once audited
  (`solana program set-upgrade-authority --final`). Immutability is irreversible.
- **Fee authority**: confirm which key collects/controls protocol fees and that it's
  a secured mainnet key (ideally a multisig), not the deploy keypair.
- Record every authority key and where its secret lives.

## 6. Fund + test with small real amounts first

- Before opening to users, run an end-to-end payment on mainnet with **tiny real USDC
  amounts** (cents): create an escrow, run a job, release payment, and verify the
  refund path.
- Confirm balances move correctly on a mainnet explorer.
- Only after a clean small-value run should you announce/scale up.

---

## Reminder

You (a human) are executing all of the above: the audit, the mainnet deploy, moving
real SOL/USDC, the authority transfers, and the web config flip. This document is the
runbook, not an automation.
