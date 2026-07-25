# CI & release validation

What the automated pipelines check, and — just as important — what they
deliberately do **not**. The guiding principle: `cloudiy` runs on a stranger's
machine (that is what a provider is), so the bar is "does the shipped binary
start and behave on hardware and distros we don't control", not just "does it
compile here".

## Workflows

| Workflow | Trigger | What it does |
|---|---|---|
| `ci.yml` | every push / PR to `main` | Build + `cargo test`; SDK tests (Python/JS/Go); packaging dry-run (PyPI/npm); protocol conformance; P2P + HTTP-SDK e2e under gVisor; Windows build. |
| `fleet-smoke.yml` | tag `v*` / manual | Smoke the **release binary** across Linux distros, arm64, and macOS. ← this doc's focus. |
| `release.yml` | tag `v*` / manual | Build + package provider binaries for 5 targets; optional signing; GitHub Release. |
| `release-sdks.yml` | tag `sdk-v*` / manual | Publish the thin-client SDKs (PyPI/npm) + crates.io. |
| `contracts.yml`, `audit.yml`, `workers.yml` | — | Anchor tests; supply-chain audit; worker images. |

## Fleet smoke matrix (`fleet-smoke.yml`)

The smoke itself (`scripts/smoke.sh`) is GPU-free, network-config-free, and
POSIX-sh, so it runs anywhere: `cloudiy --version`, `cloudiy id`,
`cloudiy info --help`, `cloudiy --help`, and one start+stop of the `share`
daemon (which exercises hardware detection, key handling, and the P2P endpoint).

| Target | Runner / image | Status | Why |
|---|---|---|---|
| Ubuntu 22.04 | `ubuntu:22.04` (docker) | **required** | most common provider distro |
| Ubuntu 24.04 | `ubuntu:24.04` (docker) | **required** | current LTS |
| Debian 12 | `debian:12-slim` (docker) | **required** | server default, minimal image |
| Fedora 41 | `fedora:41` (docker) | **required** | a different base (rpm, newer glibc) |
| Alpine | `alpine:3` (docker) | **diagnostic** | musl — see Known gaps |
| Linux arm64 | `ubuntu-24.04-arm` (native) | **required** | Raspberry Pi / ARM servers actually *execute* it |
| macOS arm64 | `macos-latest` (native) | **required** | half of dev providers are Macs |

**Cost.** The x86_64 Linux binary is built **once** and reused inside every
distro image via `docker run` — distro variety is free. Only three real compiles
happen (linux x86_64, linux arm64, macOS). The whole workflow runs only on a
release tag or manual dispatch, never on every push.

**Failure policy.** A failure on a *required* target fails the workflow — that is
the signal we want before a public release. The Alpine row is a non-blocking
diagnostic (`continue-on-error`) and emits a `::warning::` instead.

## Known gaps (surfaced on purpose, not hidden)

- **Alpine / musl.** The released Linux binary is `x86_64-unknown-linux-gnu` —
  dynamically linked against **glibc**. It does **not** run on Alpine or any
  musl-only host; the loader isn't there, so it fails with "not found" at exec.
  The fleet smoke runs Alpine anyway, as a diagnostic, so the gap is visible on
  every release rather than discovered by a frustrated Alpine user. **The fix is
  a `x86_64-unknown-linux-musl` release target** (a static musl build runs on
  both glibc and musl hosts); adding it to `release.yml`'s matrix is a
  release-surface decision — flagged in `HANDOFF.md`.
- **glibc floor.** The gnu binary needs the runner/image glibc to be new enough.
  Building on `ubuntu-latest` links against a recent glibc; a much older host
  (e.g. CentOS 7) could fail the version check. Not currently tested; a musl
  build sidesteps this too.
- **No real GPU.** CI runners have no GPU, so the smoke exercises the CPU/P2P
  startup path only. GPU kernel execution and the signed-result happy-path are
  covered on a GPU host by the conformance suite and the SDK e2e, not here.
- **32-bit / other arches.** Only x86_64 and arm64 are built and smoked. No
  32-bit, RISC-V, or other targets.

## What the smoke does NOT assert

It is a **liveness** check, not a functional or conformance test:

- It does not run a paid job, settle escrow, or verify a result signature — that
  is the [conformance suite](../conformance/README.md) and the SDK e2e.
- It does not test the scheduler, discovery over a real network, or GPU kernels.
- It confirms the daemon *announces a Node ID and stays up*, not that it can
  reach relays or serve a remote job.

The layers compose: `ci.yml` proves it builds and passes tests and conformance
on Linux; `fleet-smoke.yml` proves the shipped binary *starts and runs* on the
distros and architectures providers actually use.
