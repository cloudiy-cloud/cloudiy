# Cloudiy as an ODS extension — draft, not submitted

Draft of a [Osmantic/ODS](https://github.com/Osmantic/ODS) service extension that
turns an ODS install into a Cloudiy provider. **Nothing here has been proposed
upstream** — it exists so the conversation can start from working files instead
of an idea.

## Why the two projects fit

ODS turns one machine into a private AI server. Cloudiy is the network for when
one machine is not enough — or when yours is idle and could be earning. The
overlap is exact: an ODS user already has Docker configured, hardware detected,
GPU drivers working, and a box that sits unused most of the day. That is
precisely the provider Cloudiy lacks.

The complementarity runs both ways:

- **ODS → Cloudiy**: enable the extension, share the slice you choose, earn USDC
  per job. Nothing leaves the machine except the workloads you accepted.
- **Cloudiy → ODS**: an ODS user whose local box can't fit a model can burst the
  workload onto the network instead of buying hardware.

Neither project has to change its thesis for this to work. ODS stays local-first
and private; Cloudiy stays a protocol. The extension is the seam.

## What is in here

| File | What it is |
|---|---|
| `manifest.yaml` | Service manifest against `ods.services.v1` — modelled on the `tailscale` extension, which shares the shape (outbound-only networking, no mapped port, opt-in, `category: optional`). |
| `compose.yaml` | The container: pinned image, persistent identity volume, healthcheck via `cloudiy id`, capped logs. |

## Open questions for the ODS maintainers

Written down honestly, because they are the parts a reviewer would rightly push
back on:

1. **Docker socket.** The node launches consumer workloads as sibling
   containers, so it mounts `/var/run/docker.sock`. That is real privilege on
   the host. Mitigations already in the node: validated image + command (no
   leading-dash argument injection), pids limit, `no-new-privileges`, CPU/memory
   caps taken from the accepted job, published ports bound to `127.0.0.1` only,
   and a hard gate refusing untrusted tenants unless a sandboxed OCI runtime
   (gVisor/Kata) is configured. Still, if ODS would rather not ship a
   socket-mounting service, the alternative is a rootless/sysbox topology or
   running the node outside the compose stack — worth deciding together.
2. **Money in a local-first project.** This is the first ODS service that pays
   the user. Does that belong in the catalog at all, and if so, does it need
   different framing (a warning, a distinct category, a confirmation step)?
3. **Devnet default.** The extension defaults to Solana **devnet**, i.e. test
   money, because Cloudiy's escrow has not been audited or deployed to mainnet.
   Shipping a service that "earns" test USDC needs to be unmistakable in the UI
   so nobody expects real income yet.
4. **Image distribution.** `compose.yaml` points at `ghcr.io/cloudiy-cloud/cloudiy`,
   which does not exist yet — the release pipeline currently publishes binaries,
   not a container image. That is a prerequisite on the Cloudiy side.

## Prerequisites on the Cloudiy side (before proposing anything)

- [ ] Publish a container image (the Release workflow builds five binary targets
      today; a container target has to be added).
- [ ] A public directory node online, and its id baked into the release, so the
      extension works with no `CLOUDIY_DIRECTORY` set — see
      [`docs/GO-PUBLIC-BETA.md`](../../docs/GO-PUBLIC-BETA.md).
- [ ] Verify the env-var names above against the CLI as shipped; some are
      documented here as the intended surface and need a pass against
      `cloudiy share --help`.
- [ ] Decide whether devnet-only is acceptable for a public extension, or
      whether this waits for mainnet.

## How this would be proposed

Not as a drive-by PR. The honest sequence is: open a discussion/issue describing
the complementarity and the socket question, get a read from the maintainers,
and only then send a PR with these files under `ods/extensions/services/cloudiy/`.
The repository owner here already contributes to ODS, which is the right footing
to start from — and also the reason to be careful not to trade that standing for
a self-serving patch.
