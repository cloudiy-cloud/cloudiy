# RFC-0012 — Browser access to a VM's web UI (the deploy-access problem)

| | |
|---|---|
| **Status** | Partly shipped. Item 1: `port`/`ui_path` in the manifest. Item 2: `/api/vm/forward` (raw TCP port-forward, WebSocket-capable) **works**; `/api/vm/proxy` (HTTP proxy) renders static UIs but is **blocked for dynamic apps** by the sandbox null-origin issue (§6b) — fix is the RFC-0013 token. WS proxy (§6c) is designed, blocked on the same token. |
| **Requires** | RFC-0009 (Persistent Volume / VMs), the existing `Request::Tunnel` P2P forward, `guard_local_origin` |
| **Contract change** | None on-chain. Reuses `Request::Tunnel` unchanged (provider side needs no change). |

## 1. The problem

`openApp()` in `os.html` dispatches by `kind`; every dev/tool app falls into
`shellUI`, which is **static HTML** — a drawn prompt, a `[demo] <image>` line, a
pulsing cursor. It doesn't type, run, or connect. Meanwhile CloudiyOS has a *real*
terminal (xterm.js over WS `/api/shell`, PTY, resize, vim/htop working).

But the terminal is not the answer for most apps either: Jupyter wants JupyterLab
on 8888, File Browser its UI on 80, code-server on 8080, ComfyUI 8188, Grafana
3000, n8n 5678, Qdrant 6333, MinIO 9001, Label Studio 8080. **What a deploy gives
access to is a property of the app**, and that access type belongs to the app.

## 2. What shipped (Item 1)

The worker/app manifest (§18) now carries the access target:

- **`port`** — the TCP port the app serves its web UI on inside the VM. `0` = no
  web UI, which is the signal for the frontend to open the **Terminal** instead
  of a panel (ubuntu, pytorch-base stay port-less).
- **`ui_path`** — the path to open on that port (default `/`; Jupyter wants
  `/lab`).

Canonical values for the current apps are in `HANDOFF.md` for the web agent's
`APPS` array. So the frontend can already decide *terminal vs panel* per app and
knows *which port* to reach. What's missing is the reachability of that port from
the browser — Item 2.

## 3. What `cloudiy tunnel` already gives us (and what it doesn't)

`Request::Tunnel { port }` (P2P) forwards a raw TCP stream to `127.0.0.1:port` on
the provider — a port the caller's VM published. The provider handler
(`p2p.rs::handle_tunnel`) already enforces the two invariants that matter:

- **Ownership** is bound to the authenticated QUIC peer identity
  (`conn.remote_id()`), never a request field — the gateway can only tunnel into
  a VM **it owns**, exactly like `/api/shell`.
- **Port allowlist**: only a port in `vm.status().ports` (what that VM published)
  is reachable; plus a spent-lease refusal.

So the transport is done and audited. **The provider side needs no change.** What
`tunnel` does *not* give is a path the *browser* can use: it's a CLI TCP forward,
not an HTTP endpoint.

## 4. Design: a gateway HTTP proxy over the tunnel

A gateway-only route, reusing everything above:

```
GET|POST|… /api/vm/proxy/:to/:port/*path   →   HTTP over Tunnel(to, port) → 127.0.0.1:port in the VM
```

Per browser request the gateway: (1) opens a QUIC stream to provider `:to`, sends
`Request::Tunnel { port }`, reads `Ack` (provider validates ownership + port);
(2) speaks HTTP/1.1 over the raw stream — writes the request line, forwarded
headers and body; (3) reads the response and returns it to the browser. A fresh
tunnel per request keeps it to one request/response (no keep-alive multiplexing).

Ownership, the port allowlist, the lease check, and the `guard_local_origin`
middleware (anti-CSRF / anti-DNS-rebinding, applied to the whole router) are all
**inherited** — no new trust boundary on the provider.

## 5. Security — the blocking issue and the required contract

The proxy exposes an **arbitrary service inside a tenant's VM to the browser**.
Treated as hostile input:

- **[handled by design] Cross-tenant / host reach.** Impossible: the tunnel only
  reaches `127.0.0.1:port` where `port ∈` the caller's own VM's published ports,
  and ownership is the peer identity. No path lets one owner reach another's VM
  or a non-published provider port.
- **[handled] Header/CRLF injection.** `:port` is parsed as `u16`; `*path` is
  rejected if it contains CR/LF or control bytes before being placed in the
  request line. Pure, unit-testable (`sanitize_proxy_path`).
- **[handled] Origin/Host guard** is inherited (loopback-only, no cross-site).

- **[BLOCKING — needs a frontend contract] Confused-deputy across origins.** The
  proxy serves the VM app's HTML/JS. If the frontend embeds that content
  **same-origin** with the gateway, a malicious tenant app runs JS in the gateway
  origin and can call `/api/shell`, `/api/vm/down`, etc. And a backend-only fix
  does **not** exist here: `guard_local_origin` deliberately allows *any loopback
  origin*, so serving the proxy on a second localhost port doesn't isolate it
  (a fetch from `localhost:OTHER` to `localhost:GATEWAY/api` still carries a
  loopback `Origin` and passes the guard). A CSRF token doesn't help either — same-origin
  content can read it from the DOM.

  **The only robust isolation is at the frontend: render proxied content in a
  `<iframe sandbox="allow-scripts allow-forms allow-popups">` WITHOUT
  `allow-same-origin`.** That gives the iframe an *opaque* origin: its JS cannot
  reach the gateway's `/api`, cannot read the parent's storage, and cannot ride
  the loopback-Origin allowance. This is a hard requirement, not a nicety — the
  proxy MUST NOT be shipped without it.

- **[follow-up] WebSockets.** Jupyter and code-server drive their UIs over WS.
  Plain request/response proxying loads simpler UIs (File Browser, a Grafana
  dashboard, MinIO console for the most part) but not the WS-heavy ones. WS-over-
  tunnel (bridging the browser WS ⇄ the tunnel byte stream through the app's WS
  upgrade) is a larger, separable addition.

## 6. Why this is delivered as design + report

The secure path is **not purely backend**: it depends on the frontend sandboxed
iframe (§5), which is the web agent's territory, and it needs a `/solana-audit`
of the finished cross-territory surface. Shipping the backend proxy alone would
be a half that is *unsafe until* the frontend piece lands — an invitation to wire
it up insecurely. So per the mission's own fallback, Item 1 ships and Item 2 is
specified here.

**To implement Item 2 once the contract is agreed:**
1. Web agent confirms the sandboxed-iframe embedding (no `allow-same-origin`) and
   the URL shape `/api/vm/proxy/:to/:port/*path` (see HANDOFF.md).
2. On-chain agent adds the gateway route: `sanitize_proxy_path` (+ tests), the
   HTTP-over-tunnel client (fresh `Request::Tunnel` per request, `Connection:
   close`, httparse the response), streaming the response back.
3. Add `x-frame-options`/CSP hygiene on proxy responses as defense-in-depth.
4. `/solana-audit` the diff before declaring done.
5. WS proxying as a tracked follow-up.

## 6b. What shipped for Item 2 (the proxy)

`GET|POST|… /api/vm/proxy/:to/:port/*path` is implemented in `gateway.rs`:

- `sanitize_proxy_path` rejects a request-target that isn't absolute or carries
  any control/whitespace byte (CR, LF, space, tab, DEL, C0) — closing header/
  request-line injection and smuggling. Unit-tested.
- `proxy_over_tunnel` opens a **fresh** `Request::Tunnel` per request, waits for
  the provider `Ack` (which is where ownership + the published-port allowlist +
  the lease are enforced — unchanged), writes the rebuilt HTTP/1.1 request with
  `Connection: close` and `Host: 127.0.0.1:port`, finishes the send side, and
  reads the whole response (capped at 64 MiB).
- `build_proxy_response` parses the response head, de-chunks a
  `Transfer-Encoding: chunked` body, drops hop-by-hop/framing headers (axum
  re-frames), and adds `X-Content-Type-Options: nosniff`.

**Header trade-off (deliberate).** The RFC called for `x-frame-options`/CSP as
defense in depth. In practice the proxied content is *meant* to be framed by our
own UI, and many apps need their own origin (a Jupyter cookie, say). A restrictive
`X-Frame-Options: DENY` or CSP `sandbox`/`frame-ancestors` would break either the
embedding or the app. So the proxy instead **strips the app's own
`x-frame-options`/`content-security-policy`** (so they can't fight our embedding)
and adds only `nosniff`. The **isolation that matters is the frontend's sandboxed
iframe without `allow-same-origin`** (§5), which is now in place. This is called
out for the audit and the orchestrator to confirm.

**Two paths, on purpose.** `/api/vm/forward` (raw TCP, WebSocket-capable) is the
right path for Jupyter/code-server; `/api/vm/proxy` (HTTP request/response) is the
lighter path for embedding a simple UI in an OS window. Both inherit the same
provider-side trust boundary and add no bypass.

> **⚠️ Known limitation discovered after shipping (blocks dynamic apps).** The
> proxy is served **under the gateway origin** and the frontend embeds it in a
> sandboxed iframe **without** `allow-same-origin` (correct — §5). That gives the
> iframe an **opaque origin**, so every `fetch`/`XHR`/WebSocket the app's JS makes
> sends `Origin: null`, which `guard_local_origin` rejects. Net effect: the proxy
> serves the first HTML paint, then **every dynamic request 403s** — Jupyter,
> code-server, Grafana, MinIO don't work through `/api/vm/proxy`. Static/simple
> UIs (initial HTML + `script`/`img`/`link` subresources, which send no `Origin`)
> do render. **The fix is not an origin rule** (`Origin: null` is forgeable by any
> site's own sandboxed iframe → allowing it would be CSRF against the pod); it is
> a **capability token** — see RFC-0013. Until then, `/api/vm/forward` is the path
> that actually works for dynamic apps (it serves the app on its own
> `127.0.0.1:<port>` origin, so it never touches the gateway guard).

## 6c. WebSocket proxy (`/api/vm/wsproxy`) — design, blocked on RFC-0013

To open Jupyter/code-server *inside* an OS window (not a forwarded-port tab), the
gateway needs to bridge a browser WebSocket to the VM's WS server over the tunnel.

**Shape.** `GET /api/vm/wsproxy/:to/:port/*path` (a WS upgrade):

1. Validate `to`/`port`, `sanitize_proxy_path` the target (reused).
2. Open a fresh `Request::Tunnel(to, port)`, read `Ack` (ownership + allowlist +
   lease enforced by the provider, unchanged).
3. Run a WS **client** handshake over the raw tunnel stream to
   `ws://127.0.0.1:port<path>` (the tunnel stream is `AsyncRead+AsyncWrite` via
   `tokio::io::join(recv, send)`; `tokio_tungstenite::client_async` does the
   handshake — the crate is already in the tree via axum, so no new dependency).
4. Terminate the browser WS with axum's `WebSocketUpgrade`, then bridge frames
   both ways, translating `axum::extract::ws::Message` ⇄
   `tungstenite::Message` (Text/Binary/Ping/Pong/Close are ~1:1). Two split
   halves forwarded in a `select!`; closing either side tears down the other.

**Subprotocols.** Connect the VM side **first** so we learn the selected
subprotocol, then echo it to the browser via `ws.protocols([selected])` — needed
for Jupyter's `v1.kernel.websocket.jupyter.org` binary protocol to agree on both
ends (a mismatch silently corrupts frames).

**Why it is blocked.** A WS handshake from the sandboxed iframe carries
`Origin: null` (the §6b limitation), so `/api/vm/wsproxy` would 403 exactly like
the HTTP proxy's dynamic calls. Building it before the RFC-0013 token lands would
ship a route that is broken by construction. So this is delivered as **design**;
implementation follows the token. (`/api/vm/forward` already carries WebSockets
today for the apps that need them — this is about the *embedding*, not
capability.)

## 7. Addendum — the confused-deputy was already live (guard hardening)

The §5 confused-deputy is **not** hypothetical or proxy-only. The path Cloudiy
recommends *today* — `cloudiy tunnel --to <node> --port 8080` — serves the VM app
on `localhost:8080`, and the old `guard_local_origin` accepted **any loopback
Origin**. So that tunnelled app's JS could `fetch('http://localhost:4600/api/shell')`
(or `/api/vm/down`) and pass the guard, because the Origin was loopback. A real,
independent vector.

**Fixed** (`gateway.rs`): the guard now requires a present `Origin` to be
**same-origin** with the request's `Host` — same loopback host (localhost ≡
127.0.0.1 ≡ ::1) *and the same port*. The gateway owns its port, so nothing else
can serve on it; a page on any other loopback port is rejected. Unchanged:
no-`Origin` requests (curl, direct navigation) pass; non-loopback `Host` is
rejected (anti-DNS-rebinding); the `?gw=` public-gateway mode was already
incompatible with this loopback-only guard (a public `Host` is rejected), so
there is no regression. Unit-tested: same-origin OK, other loopback port
rejected, no-Origin OK, non-loopback rejected.
