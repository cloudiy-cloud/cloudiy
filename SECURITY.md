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
  `keys.rs`) — ed25519 signatures are domain-separated (`cloudiy/result/v1`)
  to prevent cross-protocol signature reuse.
- **Auth tokens** — compared in constant time; request bodies are capped to
  bound memory use. Never ship the default dev token.
- **On-chain escrow** (`contracts/`) — the Anchor program guards USDC. Review
  arithmetic, PDA ownership, and state transitions carefully.
- **Runtimes** (`crates/runtime`) — the WGSL runtime runs only fixed, shipped
  kernels; the Docker/OCI runtime executes untrusted images and must stay
  opt-in behind proper isolation.

Automated dependency and license auditing runs in CI via `cargo audit` and
`cargo deny` (see [`.github/workflows/audit.yml`](.github/workflows/audit.yml)).
