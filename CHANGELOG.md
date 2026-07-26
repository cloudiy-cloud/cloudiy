# Changelog

Notable changes to the **thin-client SDKs** (Python, JavaScript, Go) that live
under [`sdk/`](sdk). Format loosely follows
[Keep a Changelog](https://keepachangelog.com/1.1.0/); versions are
[semver](https://semver.org/).

> **Two version lines.** The thin clients share one version (this file, tagged
> `sdk-v*`). The Rust workspace — the `cloudiy` provider node and the
> `crates/*` libraries — has its own version and its own tags (`v*`), because
> the node and the clients ship on different cadences. See
> [`sdk/README.md`](sdk/README.md#releasing) for the release flow.

## [0.3.0] — unreleased

The first version intended for publication to PyPI and npm. Everything below
describes the state of the thin clients as of this version rather than a diff
against a published predecessor, since no earlier version was ever published.

### Security

- **Result signatures are verified by default in every SDK.** `submit()` checks
  the provider's ed25519 signature over `(job_id, sha256(input), sha256(output))`
  (domain `cloudiy/result/v2`) before returning, and raises `SignatureError` /
  throws / returns `*SignatureError` when it is missing, invalid, or from a node
  other than a pinned `expect_pubkey`. An agent never acts on unverified compute.
  Closes the HIGH finding in [`docs/SECURITY-AUDIT.md`](docs/SECURITY-AUDIT.md).
- The signature binds the **exact input** submitted, so a provider that ran a
  different prompt cannot produce a signature that verifies.
- Verification stays dependency-free: Python and JS ship a self-contained
  ed25519 verify (stdlib / BigInt + WebCrypto), Go uses `crypto/ed25519`.

### Added

- **Go SDK** (`sdk/go`) — zero third-party dependencies, same surface as the
  Python and JS clients.
- **TypeScript declarations** (`sdk/js/cloudiy.d.ts`) and the PEP 561 marker
  (`sdk/python/cloudiy_sdk/py.typed`), so both packages are typed for consumers.
- **Agent quickstarts** in each SDK's `examples/` — an AI agent discovers a
  provider, calls the tool itself, settles the x402 quote, and verifies the
  signature. Runs with or without an Anthropic API key.
- **`as_tool_schema()` / `asToolSchema()` / `AsToolSchema()`** emit an
  OpenAI/Anthropic-style function-tool definition for LLM agents.
- **HTTP end-to-end tests** (`scripts/e2e-sdk.sh`) drive all three SDKs against
  a live node: `info()`, the 402→quote→pay→retry flow, and signature
  verification. Wired into CI.
- **`scripts/pack-sdks.sh`** builds and verifies the publishable artifacts;
  **`scripts/bump-version.sh`** keeps the three SDK versions in sync.

### Changed

- Idempotent reads (`info`, `health`, `status`) retry transient failures
  (connection error, timeout, HTTP 5xx) with exponential backoff, tunable via
  `retries`. **`submit()` is never auto-retried** — a paid job must not be
  resent and double-charged.
- Network and protocol failures surface as `CloudiyError` (added to the JS SDK
  for parity) instead of raw transport errors.

### Fixed

- Documentation across the SDKs described the v1 signature format
  `(job_id, sha256(output))`; the implemented format is v2 and binds the input.

[0.3.0]: https://github.com/cloudiy-cloud/cloudiy/tree/main/sdk
