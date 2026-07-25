# Go public (devnet beta) — the launch checklist

> The counterpart of `MAINNET-RUNBOOK.md`. That one moves real money and is
> gated on an external audit; **this one carries no financial risk** — devnet
> USDC only — and is what makes the network exist for someone who is not you.
>
> The code is ready. What is missing is infrastructure that is actually running.

---

## The problem this solves

Today a visitor who runs the command on the landing page gets a binary that:

- was built from **v0.1.2 (10 Jul 2026)** — before replicated settlement with
  payment (RFC-0008), the cluster config, the isolation hardening and the MCP
  surface all landed; and
- has **no directory baked in**, so `cloudiy share` announces into the void and
  `cloudiy run` discovers nobody.

And a visitor who opens `/explorer.html` sees an empty page, because it resolves
the gateway to `http://127.0.0.1:4600` on *their* machine.

So: the protocol works, the network does not exist. Four steps fix that.

---

## Step 1 — Put the always-on infra online  *(only you can do this)*

One free VM runs both services. Full instructions: [`deploy/README.md`](../deploy/README.md).

```
scp deploy/vps-setup.sh opc@<vm-ip>:~ && ssh opc@<vm-ip> 'sudo bash vps-setup.sh'
```

- Needs an **Oracle Cloud Free Tier** account (US$0, Ampere or AMD micro) and
  `cloudiy.cloud` on **Cloudflare** for the tunnel. Both are yours to create —
  they need your credentials, so no tooling can do this for you.
- **Copy `CLOUDIY_DIRECTORY_KEY` from the summary into your vault.** The
  Directory ID is derived from it; losing it changes the ID and breaks everyone
  pointing at the old one.
- Output you need for the next steps: the **Directory ID** and the tunnel URL
  (e.g. `https://gateway.cloudiy.cloud`).

Verify before moving on: `cloudiy providers --via <directory-id>` from your Mac
returns something, and `curl https://gateway.cloudiy.cloud/api/id` answers.

## Step 2 — Bake the Directory ID into the released binary  *(wired; one click)*

`.github/workflows/release.yml` already reads the repo variable and compiles it
in (`bootstrap.rs` precedence: flag → env → compile-time default).

- GitHub → **Settings → Secrets and variables → Actions → Variables** → new
  repository variable `CLOUDIY_DEFAULT_DIRECTORY` = the Directory ID.
- The build prints `baking default directory: <id>`. Unset, it emits a CI
  warning and ships today's behaviour (needs `--via`) — never a failed build.

## Step 3 — Cut the release that carries all of it  *(prepared; you fire it)*

The last published release predates everything from the recent rounds. After
step 2:

```bash
bash scripts/bump-version.sh 0.1.4   # single source of version across the SDKs
git commit -am "release: v0.1.4" && git tag v0.1.4 && git push origin main --tags
```

The Release workflow builds five targets (Linux x86_64/arm64, macOS
Intel/Apple Silicon, Windows) and publishes them under
`releases/latest/download/…` — which is exactly what `install.sh` fetches, so
the landing page's one-liner starts serving current software.

> Publishing is an outward-facing action: cutting the tag is deliberately left
> to you, not to tooling.

## Step 4 — Point the site at the live network  *(mine)*

- `web/explorer.html` (and `os.html`) resolve the gateway as
  `?gw=` → localStorage → same-origin → **`PUBLIC_GATEWAY`** → `127.0.0.1:4600`.
  Set that constant to the tunnel URL from step 1 and the Explorer shows the
  real network to any visitor, with no query string.
- Until it is set, the empty state explains what the page needs instead of
  looking broken.

## Step 5 — Have something to show  *(shared)*

A network with zero providers looks dead no matter how good the protocol is.

- Keep **2–3 providers online** — the VM itself can share CPU
  (`cloudiy share --share-cpu 2 --share-memory-mb 2048 --no-gpu`), your Mac adds
  the GPU capability, a third box adds diversity.
- With the directory baked in (step 2), a provider is literally
  `curl -fsSL https://cloudiy.cloud/install.sh | sh && cloudiy share`.

---

## Acceptance: the beta is live when a stranger can do this

On a machine that has never seen the project, with no flags and no env vars:

```bash
curl -fsSL https://cloudiy.cloud/install.sh | sh
cloudiy providers            # lists live nodes, discovered through the baked directory
cloudiy run --kernel vector_add --input "1,2,3;10,20,30"   # signature-verified result
```

…and `https://cloudiy.cloud/explorer.html` shows those same nodes to anyone who
opens it.

## Explicitly NOT in scope here

Mainnet, real USDC, the external audit, the fee-authority multisig and the
`release_verified` posture decision. Those live in `MAINNET-RUNBOOK.md` and are
gated on decisions only the owner can make. The devnet beta needs none of them.
