# RFC-0014 — Provider payout address (an address, not a wallet)

| | |
|---|---|
| **Status** | Design + implementation (items 1–3 shipped; item 4 node-side primitive shipped, browser transport is a web contract in HANDOFF). |
| **Requires** | `cloudiy share`, `cloudiy_common::wallet`, the ed25519 signing/verification already used for result/announce/volume-key. |
| **Contract change** | None on-chain. Provider onboarding + config only. |

## 1. The bug

`cloudiy share` calls `load_pubkey()`, which reads `~/.config/solana/id.json`. If
the file is absent it does **not** fail — it warns and announces
`<no-wallet-configured>` as the payout address. A provider can therefore go
online, accept jobs, and **compute for free**: the escrow has nowhere to pay. The
warning says to run `solana-keygen new`, which assumes the Solana CLI — which a
`curl | sh` user does not have. Silent free compute is a bug, not a mode.

## 2. The principle: an address, not a wallet

**A provider needs an address, not a wallet.** Receiving USDC needs only the
**public** key; the private key only ever *spends*. Today the code reads a whole
keypair file just to take the last 32 bytes — the private key sits on the disk of
a machine that is online accepting strangers' code, and is never used. That is
risk with no upside.

This mirrors a pattern already correct in the repo: the **node key** (P2P
identity) is auto-generated and controls no money — losing it costs reputation,
not funds (`crates/common/src/keys.rs`). **Money stays outside the box.** The
payout address is the same shape: a public string the node stores, never a key.

## 3. Design

### Item 1 — declared address, no key on the machine (default)

- The payout **address** lives at `~/.config/cloudiy/payout` — a plain file
  holding one base58 pubkey, **never a keypair**. Selling point: *your node never
  stores your private key.*
- **Compat:** an existing `~/.config/solana/id.json` is still accepted as a
  source (we read only its public half), so nothing breaks for current users —
  but it is no longer *required*.
- Resolution order: `--payout <addr>` / `CLOUDIY_PAYOUT_ADDRESS` → the payout
  file → the legacy `id.json` → (interactive or fail, below).

### Item 2 — fail loud

`cloudiy share` with no resolvable payout address **must not** boot announcing
`<no-wallet-configured>`. It either (a) refuses with a clear message naming the
three ways to set an address, or (b) requires the explicit **`--no-payout`**
donation mode for someone who truly wants to give compute away. Free compute is
only ever reachable behind that explicit flag.

### Item 3 — interactive setup on first `share`

One question, once: *which address receives your payments?* Validated **for
real**:

- base58 → exactly 32 bytes,
- **on the ed25519 curve** — an off-curve key is a PDA and **cannot own a token
  account**; paying it would burn funds. (`is_on_curve` already exists in
  `solana.rs`.)

Saved to `~/.config/cloudiy/payout`. Brand colour `#ccff33`
(`\033[38;2;204;255;51m`, the exact sequence `os.html` uses) sparingly, and
**degrade to no colour** when stdout isn't a TTY or `NO_COLOR` is set.

**Non-TTY (systemd, container):** never block on a prompt. If stdin is not a
terminal and no address is configured, **fail with instructions** naming the flag
/ env / file — don't hang.

### Item 4 — browser pairing, for someone who doesn't know their address

In interactive setup, an option *"I don't know / connect my wallet"*:

1. The node mints a short **pairing code** and prints a CloudiyOS URL carrying it
   (`https://cloudiy.cloud/pair#code=<CODE>` or the local gateway `/pair`).
2. The user opens it, connects the wallet they already use, and **signs a
   message** — domain-separated, `cloudiy/payout-bind/v1 || <CODE>` — proving
   control of the address. A signature (not just a pasted address) is the point:
   it stops a typo or a copied-someone-else's-address, and proves the operator
   controls the destination.
3. The browser emits a compact **binding token** =
   `base58( address(32) || signature(64) )`. The user pastes it back into the
   terminal. The node verifies the signature over `DOMAIN || code` by `address`,
   then saves the confirmed address.

The node side (domain, token parse, signature verification) is implemented and
unit-tested here. The **browser side** (the `/pair` page: read the code, connect
wallet, sign `DOMAIN||code`, show the token) is the web agent's — the exact
message bytes and token format are the contract, in `HANDOFF.md`.

The paste-back transport needs **no relay**: it works even when the browser is on
a different machine from the headless node, and adds no online endpoint. A future
UX nicety (the gateway polling for the token) can replace the paste without
changing the security core (the signed `DOMAIN||code`).

## 4. Security notes

- The node **never** holds a private key for the payout address. Worst case if
  the node is fully compromised: the attacker learns a public address and could
  announce a *different* payout address — but that only redirects *that node's
  own* future earnings, and is visible. No user funds are at risk from node
  compromise, by construction.
- Off-curve rejection is a funds-safety check, not hygiene: USDC to a PDA with no
  owner is unspendable.
- The `payout-bind` domain is separate from result/announce/volume-key/escrow-run
  so a signature for one purpose can never be replayed as another.
- `--no-payout` is the only path to free compute, and it is loud.

## 5. Out of scope / follow-ups

- The `/pair` browser page and any gateway-polling transport (web territory).
- Rotating the payout address while online (just re-run the setup; announce picks
  up the new address on the next cycle).
