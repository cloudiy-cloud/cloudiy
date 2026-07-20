# Cloudiy — Devnet → Mainnet Runbook

> **Every step here is HUMAN-executed.** It deploys a program to mainnet, moves
> real funds and flips production config. Nothing in this document should be run
> by tooling or an agent. Follow it deliberately, with a second reviewer.

This supersedes the earlier `docs/MAINNET.md`, which now points here.

---

## 0. Verified baseline (checked against devnet, July 2026)

| Item | Value | How to re-check |
|---|---|---|
| Escrow program id | `9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN` | `solana program show <id> --url devnet` |
| ProgramData account | `Agx612FBvjhPBRL2Ys488V8AeprLiayFHx4JMx63o3PJ` | idem |
| **Upgrade authority** | `FcZHkZgz4PR7UhjwS995vBseW7mC4LvUAua4V8YrsNFF` (single hot key) | idem |
| Program size | 359,768 bytes | idem |
| Fee authority | `GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP` | `FEE_AUTHORITY` in `contracts/programs/cloudiy-escrow/src/lib.rs` |
| Protocol fee | 400 bps (4%) | `PROTOCOL_FEE_BPS` |
| Challenge window | `CHALLENGE_WINDOW_SECS = 0` (holdback **inert**) | same file |
| Anchor | 0.32.1 | `contracts/programs/cloudiy-escrow/Cargo.toml` |
| Mainnet USDC | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` | Circle |

**Two structural facts that shape everything below:**

1. **`FEE_AUTHORITY` is a compile-time constant**, and `CHALLENGE_AUTHORITY =
   FEE_AUTHORITY`. Moving fee collection to a multisig is a **program change +
   upgrade**, not a transaction. It must therefore happen *before* the program is
   made immutable — after that it is frozen forever.
2. **There is no pause / emergency instruction.** The program exposes only
   `create_job`, `create_job_split`, `release`, `release_verified`, `refund`.
   There is no on-chain kill switch, so the upgrade authority is the *only*
   code-level lever. Going immutable removes it permanently (see §6).

---

## 1. What the code already does (so you don't hunt for constants)

The Rust side resolves cluster, RPC, mint and program id from one config
(`crates/common/src/cluster.rs`), precedence **CLI flag → env → cluster default**:

```bash
CLOUDIY_CLUSTER=mainnet          # switches RPC + mint + x402 label together
CLOUDIY_RPC_URL=...              # optional override
CLOUDIY_USDC_MINT=...            # optional override
CLOUDIY_ESCROW_PROGRAM=...       # REQUIRED on mainnet until step 3 lands
```

Mainnet is deliberately **not** fully baked in: `MAINNET_ESCROW_PROGRAM` is
`None`, so selecting mainnet without a program id fails loudly rather than
pointing real USDC at an address with no program:

```
no escrow program is baked in for mainnet — the Cloudiy escrow has not been
deployed there yet. Pass --escrow-program <id> or set CLOUDIY_ESCROW_PROGRAM.
```

**After the mainnet deploy (§3), filling in that one constant is what makes
mainnet a pure flag flip.**

The MCP server refuses mainnet twice over — by RPC host *and* by selected
cluster — unless `--allow-mainnet` is passed. Keep that gate.

---

## 2. Pre-deploy blockers (do not skip)

- [ ] **Independent audit** of `contracts/programs/cloudiy-escrow` by a firm that
      did not write it. The internal audit (`docs/SECURITY-AUDIT.md`) is a first
      pass, explicitly not a substitute.
- [ ] **Decide the `release_verified` posture.** Known limit (RFC-0008 §5):
      `release_verified` is permissionless, so a *malicious* provider can
      self-settle its own escrow with a signed-but-wrong output. Quorum protects
      the answer, not that replica's stake. Either accept it (loss bounded to one
      replica's price, deterrent is reputation) or activate the holdback — which
      requires answering **who the challenge authority is** (RFC-0006 §10).
      Today `CHALLENGE_AUTHORITY = FEE_AUTHORITY`, i.e. us: a centralized
      clawback. Decide consciously; do not ship the default by accident.
- [ ] **Fee-authority decision** — if fees should be collected by a multisig,
      that is a program change (see §0). Land it *before* deploying, or accept
      `GnaUN…` as the permanent fee key.
- [ ] **Toolchain pinned + verifiable build**: Anchor 0.32.1, the edition2024
      pins already in `contracts/Cargo.lock`. Build with
      `anchor build --verifiable` and record the resulting hash.
- [ ] `cargo deny check` clean; `cargo audit` clean.

---

## 3. Deploy to mainnet

### Cost (measured, not estimated)

Rent is cluster-independent, so the devnet numbers hold on mainnet:

| Item | SOL | Note |
|---|---|---|
| ProgramData rent, current size (359,768 B) | **2.5049** | `solana rent 359768` |
| ProgramData rent with 2× upgrade headroom | **5.0089** | `solana program deploy --max-len 719536` |
| Deploy transaction fees | ~0.005 | hundreds of chunked writes at 5k lamports |
| Transient buffer account | ≈ rent above | reclaimed when the deploy finalizes |

**Budget ~5.5 SOL** for an exact-size deploy or **~10.5 SOL** with 2× headroom,
so the buffer and the final account can coexist without a mid-deploy failure.
Headroom matters: without `--max-len`, a future upgrade that grows the program
past the allocated size cannot be applied in place.

### Program id

- **Same id** (`9zMBC7JD…`) — reuse the program keypair. Clients that hardcoded
  it keep working; the id is already in the audit trail.
- **New id** — fresh keypair. Cleaner separation between the devnet test
  deployment and production. Record it and put it in `MAINNET_ESCROW_PROGRAM`.

Prefer a **new id** unless you specifically want continuity: it makes "is this
tx on the test program or the real one?" unambiguous forever.

```bash
anchor build --verifiable
solana program deploy \
  --url mainnet-beta \
  --program-id /secure/path/mainnet-escrow-keypair.json \
  --max-len 719536 \
  --keypair /secure/path/mainnet-deploy-keypair.json \
  target/verifiable/cloudiy_escrow.so

solana program show <NEW_ID> --url mainnet-beta   # record everything it prints
```

Then, in `crates/common/src/cluster.rs`:

```rust
pub const MAINNET_ESCROW_PROGRAM: Option<&str> = Some("<NEW_ID>");
```

Re-run `cargo test -p cloudiy-common` — the precedence tests cover the mainnet
branch.

---

## 4. Move the upgrade authority off the hot key

`FcZH…` is a single hot key that can replace the program and void every on-chain
guarantee. This is the highest real-world risk in the whole system. Two paths:

| | **Squads multisig** | **Immutable (`--final`)** |
|---|---|---|
| Can fix a bug | Yes, with M-of-N approval | **No, ever** |
| Can rug / be stolen | Only by M-of-N collusion or M key compromises | No |
| Fee authority changeable later | Yes (upgrade) | **No — frozen forever** |
| Rollback lever exists | Yes | **None** (§6) |
| Right when | Launching, code may still need fixes | Long-lived, audited, no pending decisions |

**Recommendation: Squads first, immutable much later (if ever).** With no pause
instruction and an unresolved challenge-authority question, immutability now
would freeze both the code *and* the fee/challenge authority with no recourse.

### Squads transfer

1. Create the multisig at [squads.so] on **mainnet**; add signers; pick a
   threshold (3-of-5 is a reasonable floor — 2-of-3 concentrates too much).
2. Get the Squads **vault/authority PDA** address from the UI.
3. **Verify the address by having a second person read it back independently.**
   A wrong address here permanently destroys upgradeability.
4. Transfer:

```bash
solana program set-upgrade-authority <PROGRAM_ID> \
  --url mainnet-beta \
  --keypair /secure/path/current-authority.json \
  --new-upgrade-authority <SQUADS_VAULT_PDA> \
  --skip-new-upgrade-authority-signer-check
```

(The flag is required because a PDA cannot sign — which is exactly why step 3
matters.)

5. Confirm: `solana program show <PROGRAM_ID> --url mainnet-beta` reports the
   Squads PDA as Authority.
6. Do a **dry-run upgrade proposal** through Squads on a throwaway program first,
   so you learn the flow before you need it under pressure.

---

## 5. Cut over clients

Providers:

```bash
CLOUDIY_CLUSTER=mainnet cloudiy share --require-payment \
  --runtime runsc                      # untrusted-tenant isolation stays on
```

`--require-payment` MUST be on: providers should serve only paid, escrowed jobs.
`--usdc-mint` and the x402 label now follow the cluster automatically; override
only if you have a reason.

Consumers / MCP:

```bash
CLOUDIY_CLUSTER=mainnet cloudiy run ... --pay --release
CLOUDIY_CLUSTER=mainnet cloudiy mcp --allow-mainnet   # gate is deliberate
```

Web (`web/os.html`, **outside this crate's ownership** — a human edits it, or it
goes through the web agent): the `CONFIG` object's program id, mint and RPC.
File the request in `HANDOFF.md` rather than editing `web/` from here.

---

## 6. Re-test on mainnet before opening up

Run these against the **mainnet** program, with tiny real amounts (cents):

- [ ] `anchor test` — hermetic suite, 11 cases, against the built artifact.
- [ ] `examples/permissionless_release.rs` — permissionless settle works.
- [ ] `examples/spoof_release.rs` — **the C1 forgery is still rejected**
      (`BadSignature 6009`). This is the single most important negative test; a
      pass here on the mainnet program is non-negotiable.
- [ ] `scripts/e2e.sh` — P2P + container path under gVisor.
- [ ] `scripts/e2e-quorum-escrow.sh` — RFC-0008 replicated settlement, adapted to
      mainnet USDC and cent-sized amounts. Verify both cases: quorum pays only
      the agreeing replicas, and a divergent replica earns no release.
- [ ] One manual **refund** after deadline expiry — the path a consumer needs
      when a provider vanishes.
- [ ] Confirm every balance movement on a mainnet explorer, including the 4% fee
      landing at the fee authority ATA.

The fee authority and every provider need an **ATA for mainnet USDC before the
first settlement** — `release_verified` takes `provider_token`/`fee_token` as
existing accounts and fails if either is missing.

---

## 7. Go / no-go

**Go only if all of these are true:**

- Independent audit complete, findings fixed or explicitly accepted in writing.
- Upgrade authority is a multisig with ≥3-of-5 and tested signer access.
- The `release_verified` posture (§2) is a written decision, not a default.
- Every §6 test passed on the mainnet program, including the spoof rejection.
- Fee-authority key custody documented; ATAs exist.
- A named human is on call with the multisig signers reachable.

**No-go if any of these:**

- Upgrade authority still a single hot key.
- Any §6 test failed, flaked, or was skipped "because devnet passed".
- Challenge authority undecided *and* `CHALLENGE_WINDOW_SECS` non-zero (that
  combination hands a centralized clawback to an unowned key).
- Program deployed without `--max-len` headroom while fixes are still expected.

---

## 8. Rollback

There is **no on-chain pause**, so rollback is layered and mostly off-chain:

1. **Stop the bleeding (seconds, no chain access needed).**
   Take providers off mainnet — restart without `CLOUDIY_CLUSTER=mainnet`, or
   stop them. No provider accepting mainnet escrows means no new funds at risk.
   Existing escrows are unaffected.
2. **In-flight escrows.** Funds already locked stay recoverable: the provider can
   `refund` voluntarily at any time, and the consumer can `refund` after the
   deadline. Publish this instruction to users immediately — it is their lever,
   not ours.
3. **Code fix (needs the multisig).** Redeploy the previous verified build via
   Squads. Keep the last-known-good `.so` and its verifiable hash archived
   *before* every upgrade, or this step has nothing to roll back to.
4. **If the program is immutable:** steps 1 and 2 are the *entire* rollback. No
   code fix is possible, ever. Plan communications accordingly — this is the
   concrete cost of immutability and the main reason §4 recommends multisig.

**Pre-commit to the artifacts that make rollback possible:**
archive each deployed `.so` + verifiable build hash + the `solana program show`
output, tagged with the git commit, before every mainnet deploy or upgrade.
