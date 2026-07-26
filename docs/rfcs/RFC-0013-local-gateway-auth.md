# RFC-0013 — Local gateway authentication (capability token)

| | |
|---|---|
| **Status** | **Design — not implemented.** This is a decision for the owner (auth is UX-sensitive). Written to be built from once a direction is chosen. |
| **Requires** | The `guard_local_origin` middleware, RFC-0012 (VM web access). |
| **Contract change** | None on-chain. Gateway-local only. |
| **Unblocks** | The `/api/vm/proxy` (and a future `/api/vm/wsproxy`) actually working for **dynamic** apps behind the sandboxed iframe — see §2. |

## 1. The two problems

**Problem A — any local process can drive the gateway.** The gateway serves
`/api/*` on `127.0.0.1:4600` with **no credential**. `guard_local_origin` stops a
*web page of another origin* (anti-CSRF) and a non-loopback `Host`
(anti-DNS-rebinding), but it does **nothing** against a *local program*: any
process on the machine can `curl http://127.0.0.1:4600/api/vm/down` and it works.
On a single-user laptop that is usually fine; on a shared box, or with untrusted
local software, it is a real hole — the gateway holds the machine's P2P identity
and drives real VMs.

**Problem B — the sandboxed-iframe proxy can't make dynamic calls.** RFC-0012 §5
requires the frontend to embed proxied VM content in
`<iframe sandbox="allow-scripts allow-forms allow-popups">` **without**
`allow-same-origin`. That is the correct isolation — but it gives the iframe
document an **opaque origin**, so every `fetch`/`XHR`/WebSocket its JS makes sends
`Origin: null`. `guard_local_origin` rejects `null` (it is neither same-origin
loopback nor allowlisted). Result: the proxy serves the first HTML paint, then
**every dynamic request 403s**. Jupyter, code-server, Grafana, MinIO — all
fetch/WS-driven — are effectively non-functional through `/api/vm/proxy` today.
(The `/api/vm/forward` path does **not** have this problem: it serves the app on
its *own* `127.0.0.1:<port>` origin, so the app is a normal same-origin app and
never touches the gateway guard.)

The two problems have **one** clean solution: a per-session **capability token**
that authorizes a request regardless of `Origin`. It closes A (a local process
without the token can't drive the gateway) and B (the sandboxed iframe carries the
token in its URL, so its null-origin sub-requests are authorized by the token, not
the origin).

## 2. Why a token, specifically

- `Origin: null` is **forgeable**: *any* site can put the gateway's proxy URL in
  its own `<iframe sandbox>` and get a null origin too. So "allow null on proxy
  paths" would let `evil.com` drive the user's VM apps (CSRF against the pod). An
  origin rule cannot fix Problem B safely.
- A token the frontend holds and `evil.com` cannot read **is** the CSRF defense,
  and simultaneously the local-process defense. This is exactly how Jupyter
  (`?token=`) and VS Code tunnels secure a localhost server.

## 3. Design

### 3.1 The token

- On start, the gateway loads-or-creates a 256-bit random token, stored at
  `~/.config/cloudiy/gateway.token` with `0600` perms (like the client key). It is
  **not** printed in full to logs.
- Compared in **constant time** (`subtle`, already a dependency) to avoid timing
  oracles.

### 3.2 How the browser gets it

The gateway serves `os.html` itself (same origin), so it can inject the token into
the page at serve time — e.g. a `<meta name="cloudiy-token">` or a small
`window.__CLOUDIY_TOKEN__` the served HTML carries. The SPA reads it once. (A
public/hosted UI over a tunnel can't be handed the token this way; see §5.)

### 3.3 How a request presents it

Two carriers, because the two surfaces differ:

- **Control API** (`/api/vm/*`, `/api/models*`, `/api/shell`, …): an
  `Authorization: Bearer <token>` header (or `?token=` for the WebSocket, which
  can't set headers from the browser).
- **Proxy paths** (`/api/vm/proxy`, `/api/vm/wsproxy`): the token as a **path
  segment prefix** —
  `//api/vm/proxy/<token>/:to/:port/*path`. This is the key trick: a relative
  `fetch('api/kernels')` from an iframe document at
  `/api/vm/proxy/<token>/<node>/<port>/lab` resolves to
  `/api/vm/proxy/<token>/<node>/<port>/api/kernels` — **the token rides along in
  every relative sub-request automatically**, with no cooperation from the app's
  JS. (A query `?token=` would be dropped by relative sub-requests; a path prefix
  is not.)

### 3.4 The new guard rule

```
authorized(req) =
    valid_token(req)                      // header, ?token=, or path prefix
    OR ( no_token_configured               // opt-in; default off preserves today
         AND same_origin_or_allowlisted(req) )   // the current RFC-0012 rule
```

- With a token present and valid → allow, **whatever the Origin** (this is what
  makes the null-origin sandbox work).
- The current `guard_local_origin` same-origin/allowlist logic stays as the
  fallback when the feature is **off** (default), so nothing regresses.
- Anti-DNS-rebinding (`Host` must be loopback or allowlisted) stays **always on**,
  token or not.

### 3.5 Alternatives considered

- **Unix domain socket** (no TCP, filesystem-permission auth). Strongest for
  Problem A and needs no token, but: browsers can't `fetch` a unix socket, so the
  browser path still needs a TCP listener — this doesn't solve Problem B, and
  splits the transport. Reasonable as a *complement* for CLI↔gateway, not a
  replacement.
- **OS keychain / per-request signature.** Heavier; no better than a file token
  for a localhost server; worse UX.
- **Do nothing (status quo).** Leaves Problem B unsolved — the proxy stays broken
  for dynamic apps, and only `/api/vm/forward` works. Defensible *if* we decide
  the forward is the only supported embed path and drop the HTTP/WS proxy.

## 4. Recommendation

Adopt the **capability token** (§3): a file token, injected into the
gateway-served UI, presented as a Bearer header on the control API and as a
**path-prefix** on the proxy paths, with the token-or-same-origin guard rule.
It is the minimum that makes the sandboxed-iframe proxy usable **and** closes the
local-process hole, and it mirrors a pattern users already trust (Jupyter). Keep
it **opt-in** at first (`--auth-token` / auto-on when a token file exists) so the
single-user laptop flow is unchanged until the owner flips it on.

## 5. Open questions (owner decides)

1. **Default on or off?** Off preserves today's zero-friction local flow but
   leaves the proxy broken for dynamic apps and the local hole open. On is safer
   but adds a token to every surface and complicates the public-gateway story.
2. **Public/hosted UI.** A Vercel UI over a tunnel can't be handed a file token
   the way the gateway-served `os.html` can. Options: the operator pastes the
   token once (stored in `localStorage`, like the `?gw=` override), or the public
   gateway uses a different, operator-provisioned secret. Needs a call.
3. **Rotation.** Rotate on restart (simple; invalidates open tabs) vs. persistent
   (stable; survives restart). Jupyter rotates per start.

## 6. If/when built

1. Token load/create (`subtle` constant-time compare) + the guard rule (§3.4),
   behind a flag — **on-chain agent territory** (`crates/cloudiy`), with unit
   tests mirroring the existing guard tests (valid token any-origin → allow;
   bad/absent token + foreign origin → 403; anti-rebinding preserved).
2. Inject the token into the served `os.html`; add the `<token>` path-prefix route
   for the proxy; teach the SPA to send the Bearer header / build proxied iframe
   URLs with the prefix — **web agent territory**.
3. `/solana-audit` the finished cross-territory surface (this is the same class of
   confused-deputy the guard work has been circling — it deserves the pass).
