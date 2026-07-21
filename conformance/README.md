# Cloudiy protocol conformance suite

A black-box test suite that validates **any** implementation of the Cloudiy
compute protocol — not just this repo's reference node — against the spec.
Point it at a running node and it checks the observable wire contract a
consumer depends on, with every check citing the clause it enforces.

This is the concrete answer to *"is Cloudiy a protocol, or just one
implementation?"* A protocol is something a second team can implement and have
their node pass this suite. If it passes, a Cloudiy SDK can talk to their node
without knowing it isn't ours.

## Run it

Zero dependencies — stdlib Python 3 only. Nothing to install.

```bash
# against a node you started (`cloudiy share`, or your own implementation)
python3 conformance/cloudiy_conformance.py 127.0.0.1:8080

# a dev node that accepts an access token opens the payment gate for the
# signed-result checks:
python3 conformance/cloudiy_conformance.py 127.0.0.1:8080 --token <access-code>

# also exercise the >16 MiB frame limit (uploads a large body):
python3 conformance/cloudiy_conformance.py 127.0.0.1:8080 --slow
```

Or boot the reference node and run the suite in one step:

```bash
scripts/conformance.sh          # starts `cloudiy share`, runs the suite, tears down
scripts/conformance.sh --slow
```

## What each verdict means

```
[PASS] RFC-0006 §4  result signature verifies over (job_id, sha256(input), sha256(output))
[SKIP] RFC-0006 §4  no GPU on this node — cannot exercise a signed result
[FAIL] PROTOCOL §6  unpaid submit is answered 402 Payment Required
```

- **PASS** — the node honored that spec clause on this run.
- **FAIL** — the node **violated** the clause. The suite exits non-zero if any
  check fails.
- **SKIP** — the clause could not be exercised in this environment, and that is
  **not** a conformance failure. Two legitimate reasons:
  - **No compute here.** A CPU-only node can't produce a signed GPU result, so
    the signature checks skip. Run against a node with a real accelerator to
    exercise them.
  - **Real settlement required.** A node running with on-chain payment enforced
    (`--require-payment`) won't open the gate for the suite's demo payment, and
    a black-box probe can't mint a real escrow. Pass `--token` for a dev node,
    or run the signed checks against a node in dev mode.

The final line is the headline: `conformance: N/M checks passed`.

## What it checks (and the clause each cites)

| Area | Check | Spec |
|---|---|---|
| Discovery | `/info` returns a node descriptor | PROTOCOL §6, §12 |
| Identity | `/info` carries the node's 32-byte ed25519 identity | PROTOCOL §2 |
| Versioning | `/info` carries the protocol tag and a version string | PROTOCOL §16 |
| Payment terms | `/info` advertises price, asset (mint), network, escrow, scheme | PROTOCOL §6, §12 |
| Price model | `price_usdc × 1e6` equals the quote's `maxAmountRequired` | PROTOCOL §12.1 (R12.2) |
| Capabilities | `/info` lists capabilities and resource accounting | PROTOCOL §2.4, §3 |
| x402 | an unpaid submit is answered `402 Payment Required` | PROTOCOL §6 |
| x402 | the 402 body is a valid x402 quote (price + payee + asset) | x402 / §6, §13 |
| x402 | retrying with payment (or a dev token) lifts the gate | PROTOCOL §6 |
| Errors | a **job** failure is HTTP `200` with `status:"error"`, never 5xx | PROTOCOL §14.2 (R14.2) |
| Signature | a completed result is signed by the node's own identity | PROTOCOL §2 |
| Signature | the signature verifies over `(job_id, sha256(input), sha256(output))` | RFC-0006 §4 |
| Signature | tampering the **output** breaks verification (binding is real) | RFC-0006 §4 |
| Signature | changing the **input** breaks verification (input-binding is real) | RFC-0006 §4 |
| Kernel | the deterministic `vector_add` output is correct | PROTOCOL §17 |
| Errors | a malformed request is rejected with a stable 4xx (not 500, not 200) | PROTOCOL §14.1 (R14.1) |
| Limits | an oversized body is rejected (frame limit) — `--slow` | PROTOCOL §15 |

Every check cites the exact clause of the spec it enforces — as of `PROTOCOL.md`
v0.2, whose normative wire specification (Part II, §12–17) pins the node
descriptor, the quote, the error taxonomy, size limits, versioning and the
kernel encodings. There are no `reference (de facto)` verdicts left: the suite
and the spec are in lockstep, and PROTOCOL §17 names this file as the
`vector_add` interop anchor.

The signature checks are the heart of it: they don't just confirm a signature
is *present*, they prove it is **bound** — flipping one output byte, or swapping
the input, must make the same signature fail. A node that returns a
well-formed-but-unbound signature fails these, which is the point (RFC-0006 §4
is exactly the input→output binding that on-chain `release_verified` relies on).

Two of the newer checks are worth calling out. The **price-model** check
(§12.1) catches the drift the spec now forbids — `price_usdc` is a display float
derived from the canonical integer micro-USDC, and the two must reconcile. The
**job-failure** check (§14.2) submits an unknown kernel to force an
admitted-but-failed job and asserts it comes back as HTTP `200` with
`status:"error"` — never a 4xx/5xx — so a consumer can tell "your request was
bad" apart from "your job ran and failed".

## Expected results

Against the reference node, with the payment gate open (`--token`):

| Environment | Result |
|---|---|
| GPU node, `--slow` | `conformance: 22/22 checks passed` |
| CPU-only node, `--slow` | `17/17 checks passed, 1 skipped` (signed result needs a GPU) |
| CPU-only node (default) | `16/16 checks passed, 2 skipped` (also skips the frame limit) |

SKIPs are environmental, never failures. A node that requires real on-chain
settlement (no `--token`, no accepted demo payment) additionally skips the
gate-dependent checks — the black-box probe can't mint a real escrow.

## Out of scope

The suite tests only what is **observable at the wire**. It deliberately says
nothing about:

- **Scheduler policy** — how a node or directory picks placements, scores
  providers, or ranks by reputation (PROTOCOL §5, RFC-0006 §6). Two conforming
  implementations may schedule completely differently.
- **Internal isolation** — namespaces, cgroups, seccomp, the OCI runtime
  (PROTOCOL §4). That a workload *ran* is observable; *how* it was sandboxed is
  the implementation's business.
- **Settlement internals** — the escrow program, fee split, holdback
  (RFC-0004, RFC-0006 §6). The suite checks that a quote is *offered* and that a
  result is *signed*; it does not settle on-chain.
- **Economic security** — canaries, the reputation ramp, holdback enforcement
  (RFC-0006 §5–6). Those are statistical/economic guarantees measured over many
  jobs, not a single black-box run.

## History

The first version of this suite predated the normative wire spec, so several
checks cited `reference (de facto)` — parts of the observable contract that only
the reference implementation pinned (the `/info` field names, the error
taxonomy, the frame limit). Those gaps were written up and became **Part II of
`PROTOCOL.md` v0.2** (§12–17), and the checks now cite those sections instead.
There are no `reference (de facto)` verdicts left — the suite validating a
second implementation is validating it against the written spec, not against us.
