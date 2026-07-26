# Contributing to Cloudiy

Thanks for your interest in improving Cloudiy. This guide covers how to set up,
what we expect from a change, and how CI gates pull requests.

## Project layout

```
crates/protocol   # Open Compute Protocol types (Identity, Resource, Workload, …)
crates/scheduler  # placement engine
crates/runtime    # execution backends (WGSL via wgpu, Docker/OCI)
crates/common     # shared types, wire protocol, node keys, result signing
crates/sdk        # Rust consumer library
crates/cloudiy    # the `cloudiy` binary (provider + consumer CLI + gateway + MCP)
crates/cloudiy/manifests/  # the model catalog as data: image, license, requirements
sdk/python, sdk/js, sdk/go # zero-dependency consumer SDKs
conformance/      # black-box spec suite — run it against any node, ours or yours
contracts/        # Anchor USDC escrow program (separate Cargo workspace)
workers/          # Dockerfiles for the inference workers
deploy/           # directory + gateway on a VPS; gateway as a per-user service
proto/            # gRPC service definition (legacy/reference)
web/              # static site (landing, docs, Explorer, CloudiyOS)
docs/rfcs/        # design history and open decisions
```

See [`PROTOCOL.md`](PROTOCOL.md) and [`docs/rfcs/`](docs/rfcs) for design intent —
substantial protocol changes should start as an RFC.

**The spec is the contract, not this implementation.** If you change observable
behavior, the spec changes with it and `conformance/` should still pass. A check
you cannot write from `PROTOCOL.md` alone is a gap in the spec, not in the suite —
that is how the normative wire specification came to be written.

## Adding a model to the catalog

Catalog entries are data (`crates/cloudiy/manifests/*.json`), and two CI gates
guard them. Both exist because the mistake shipped once:

- **License allowlist** — permissive only. AGPL is refused because its network
  clause is triggered by serving a model to third parties, which is exactly what
  this network does; CC-BY-NC is refused because the network charges for
  compute. Check the license at the source and **per variant**: `FLUX.1
  [schnell]` is Apache-2.0 while `FLUX.1 [dev]` is not, and MusicGen's code is
  MIT while its weights are non-commercial.
- **Image verifier** — an entry may only be `available` if its container image
  really exists in the registry, pinned by digest. Everything else stays
  `planned`.

Announce what you serve: never label an entry with one model's name and run a
different model underneath.

## Development setup

Requires Rust (https://rustup.rs). For the on-chain program you also need the
Solana CLI and Anchor `0.32.1`.

```bash
cargo check
cargo build
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

# on-chain program (separate workspace)
cd contracts && anchor build && anchor test
```

## Making a change

1. Fork and branch from `main` (e.g. `feat/scheduler-affinity`,
   `fix/x402-retry`).
2. Keep the diff focused; unrelated cleanups belong in separate PRs.
3. Add or update tests for any behavior change.
4. Update docs (`README.md`, `PROTOCOL.md`, RFCs) when behavior or interfaces
   change.
5. Run `fmt`, `clippy -D warnings`, and the test suite locally before pushing.

## Commit and PR conventions

- Use [Conventional Commits](https://www.conventionalcommits.org): `feat:`,
  `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- Write a clear PR description: what changed, why, and how it was tested.
- Link any related issue or RFC.

## CI gates

Every PR must pass:

- **Workspace CI** ([`ci.yml`](.github/workflows/ci.yml)) — `fmt`, `clippy -D
  warnings`, `build`, `test`.
- **Contracts CI** ([`contracts.yml`](.github/workflows/contracts.yml)) — runs
  when anything under `contracts/` changes: `anchor build` + `anchor test`.
- **Audit** ([`audit.yml`](.github/workflows/audit.yml)) — `cargo audit` and
  `cargo deny` for advisories, licenses, and banned/duplicate dependencies.

At release time, **Fleet smoke**
([`fleet-smoke.yml`](.github/workflows/fleet-smoke.yml)) additionally validates
that the *shipped binary* starts and runs on the distros and architectures
providers actually use (Ubuntu, Debian, Fedora, Alpine, arm64, macOS). It runs
on a `v*` tag or manual dispatch, not on every push. See [`docs/CI.md`](docs/CI.md)
for the full coverage matrix and the known gaps (notably Alpine/musl).

New dependencies should be justified in the PR and must satisfy the license
policy in [`deny.toml`](deny.toml).

## Security

Please report vulnerabilities privately — see [`SECURITY.md`](SECURITY.md). Do
not open public issues for security problems.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
