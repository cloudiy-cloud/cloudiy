# Security Policy

Cloudiy handles cryptographic keys, signed compute results, and on-chain USDC
settlement, so we take security reports seriously.

## Supported versions

The project is pre-1.0 and under active development. Only the latest `main` is
supported — please report issues against the current tip.

## Reporting a vulnerability

**Do not open a public issue for security problems.**

Instead, use GitHub's private reporting: open the repository's **Security** tab →
**Report a vulnerability** (GitHub Security Advisories). If that is unavailable,
contact the maintainers privately through the address listed on the GitHub org
profile.

Please include:

- a description of the issue and its impact,
- the affected component (`crates/*`, `contracts/`, `sdk/*`, or `web/`),
- steps to reproduce or a proof of concept,
- any suggested remediation.

We aim to acknowledge reports within **72 hours** and to provide a remediation
timeline after triage. We will credit reporters who wish to be named once a fix
is released.

## Scope and hardening notes

Areas that are especially sensitive:

- **Node identity and result signing** (`crates/common/src/sig.rs`,
  `keys.rs`) — ed25519 signatures are domain-separated (`cloudiy/result/v2`,
  binding `job_id ‖ sha256(input) ‖ sha256(output)`) to prevent cross-protocol
  signature reuse. Announcements, run-authorization and volume keys each carry
  their own domain.
- **Auth tokens** — compared in constant time; request bodies are capped to
  bound memory use. Never ship the default dev token.
- **On-chain escrow** (`contracts/`) — the Anchor program guards USDC. Review
  arithmetic, PDA ownership, and state transitions carefully. VM ownership is
  bound to the **authenticated peer identity**, never to a field in the request.
- **Runtimes** (`crates/runtime`, `crates/cloudiy/src/vm.rs`) — the WGSL runtime
  runs only fixed, shipped kernels; the Docker/OCI runtime executes untrusted
  images. A provider that declares tenants untrusted
  (`CLOUDIY_UNTRUSTED_TENANTS`) refuses to start one without a sandboxed OCI
  runtime (gVisor/Kata) or an egress-filtered network.
- **Browser surface** (`crates/cloudiy/src/gateway.rs`) — the origin guard
  requires **same origin** (host *and* port), not merely any loopback: an app
  reached through a forwarded port must not be able to drive `/api/*`. Tenant
  content is only embedded in a sandboxed iframe **without** `allow-same-origin`
  ([RFC-0012 §5](docs/rfcs/RFC-0012-vm-web-proxy.md)) — removing that flag
  reintroduces an origin hijack.

## Audits and automated gates

- Internal audit rounds and findings: [`docs/SECURITY-AUDIT.md`](docs/SECURITY-AUDIT.md);
  escrow-specific findings: [`contracts/SECURITY.md`](contracts/SECURITY.md).
- Failure semantics — who detects what, what is retryable, and the invariant
  that **money is never stuck past the escrow deadline** —
  [RFC-0010](docs/rfcs/RFC-0010-failure-handling.md).
- CI gates: `cargo audit` and `cargo deny`
  ([`audit.yml`](.github/workflows/audit.yml)); a **license allowlist** that
  fails the build on AGPL / CC-BY-NC / non-permissive catalog entries
  (`crates/cloudiy/src/license.rs`); and an **image verifier** that refuses to
  advertise a model whose container image does not exist.

## Known limits (documented trade-offs, not vulnerabilities)

- **Inputs are visible to the provider** that runs the job. Signatures prove
  provenance, not secrecy. Confidentiality from the provider needs attested
  execution (TEE), which is roadmap.
- **`release_verified` is permissionless**, so a divergent replica can settle
  its own escrow — bounded to one replica's price, deterred by reputation
  ([RFC-0008 §5](docs/rfcs/RFC-0008-replicated-settlement.md)).
- **The local gateway has no authentication yet**: any process on the same
  machine can call it. The origin guard stops *web pages*, not local programs.
  Design under discussion in [RFC-0013](docs/rfcs/RFC-0013-local-gateway-auth.md).
