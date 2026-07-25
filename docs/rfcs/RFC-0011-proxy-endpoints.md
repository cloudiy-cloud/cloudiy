# RFC-0011 — Proxy Endpoints (skeleton)

| | |
|---|---|
| **Status** | **Skeleton / open question** — not designed, not implemented. Do NOT build from this. |
| **Requires** | RFC-0006 (Verifiable Settlement), RFC-0008 (Replicated Settlement) |
| **Question** | Should Cloudiy ever serve **closed, proprietary** models (OpenAI, Gemini, closed FLUX pro, Kling, …) by *proxying the owner's API*, and if so, under what — explicitly non-trustless — settlement model? |

> This file exists because the catalog cleanup (removing proprietary brands that
> were secretly served by open workers) raised the question "then how *would* we
> ever offer a closed model?" The honest answer is "not the way everything else
> works", and that deserves to be written down rather than improvised later. This
> is a **skeleton**: it frames the tension and the decision, and stops. It is not
> a green light.

## 1. The tension (why this can't reuse the normal path)

Every other endpoint on the network is **open-weight**: the provider holds the
weights and *computes* the result. The result signature (`cloudiy/result/v2`,
PROTOCOL §11.4) then proves a strong thing — *this node computed this output for
this input*. `release_verified` pays against that proof, and RFC-0008 quorum can
even make it trustless for deterministic work.

A **proxy** provider does not have the weights. It can only forward the request
to the model owner's API (OpenAI, Google, BFL) and relay the answer. So the most
its signature can attest is *"I forwarded this and relayed what came back"* —
**provenance of a relay, not of a computation**. Concretely, this breaks two
guarantees:

- **No independent verifiability.** A quorum of proxies all calling the same
  upstream API is not N independent computations; it's one computation observed N
  times (and the upstream could return different things, or nothing). RFC-0008's
  "3 providers agree ⇒ trust the answer" collapses — they agree because they all
  asked the same closed oracle.
- **The signature means less.** A malicious proxy can relay a *cached* or
  *fabricated* answer and sign "I relayed this"; the consumer cannot tell a real
  upstream call from a forged one. The escrow would pay for a relay that may not
  have happened.

So proxy endpoints are **not** a config flag on the existing path — they are a
different trust model, and must be labelled as such or they poison the meaning of
every signature on the network.

## 2. If pursued — the shape it would have to take (sketch only)

Not a design. A sketch of the *constraints* any real design must satisfy:

- **A separate, explicitly-labelled settlement lane.** Proxy endpoints MUST be
  marked `trust: proxied` (or similar) in the catalog and the quote, and MUST NOT
  reuse the `cloudiy/result/v2` provenance claim as if it meant computation. A
  consumer opting into a proxy endpoint is knowingly trusting the relay.
- **Attestation, not proof.** The strongest honest primitive is an *attestation*
  from the upstream (a signed response from the model owner, TLS-notary /
  oracle-style evidence that the relay really called the API). Absent that, it is
  pure trust in the proxy's reputation — acceptable only if stated plainly.
- **Escrow still works, but for a weaker claim.** The escrow can pay for "a relay
  was performed" gated on the consumer's acceptance (plain `release`, consumer
  signs), never on permissionless `release_verified` — because there is no
  computation proof to re-check on-chain.
- **Isolation from the trustless lane.** The reputation ramp (RFC-0006), the
  quorum path (RFC-0008), and the open-weight catalog MUST stay uncontaminated:
  a proxy endpoint's behavior can never raise a provider's trustless-lane score.

## 3. DECISION POINT (for the user / orchestrator)

**Do we want proxy endpoints at all?** Three honest positions:

1. **No, ever.** Cloudiy is a network of real compute; if you want OpenAI, call
   OpenAI. This keeps every signature on the network meaning exactly one thing.
   *(Cleanest; the current de-facto stance after the catalog cleanup.)*
2. **Yes, but only as an explicitly-labelled, trust-based lane** with the §2
   constraints — a convenience product, never sold as trustless.
3. **Only with hard attestation** (upstream-signed responses / TLS-notary), and
   not before that tech is real — i.e. deferred indefinitely.

This RFC does not choose. It records that the choice exists, that option 2 must
never be shipped *looking like* the trustless path, and that the catalog cleanup
(open-weight only) is what makes the question answerable cleanly rather than by
accident. **Until a decision lands, the answer is option 1 by default and nothing
is built.**
