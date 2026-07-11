# Exposing a Cloudiy gateway over HTTPS (cloudflared)

## The problem

The local web gateway runs as:

```
cloudiy os --web-dir web    # binds 127.0.0.1:4600
```

It listens on **localhost only**. The hosted web app (deployed to Vercel, e.g.
`https://cloudiy-cloud.vercel.app`) runs in the user's browser on a public,
HTTPS origin, and **a browser cannot reach `127.0.0.1:4600` on someone else's
machine** — nor may an HTTPS page fetch a plain-`http://` gateway. So the
published app can't talk to a provider's gateway unless you bridge the gap with
a public HTTPS endpoint.

We use **Cloudflare Tunnel** (`cloudflared`) for this. It is outbound-only (no
inbound firewall ports), gives Cloudflare-managed TLS, and proxies WebSockets
(needed for `/api/shell`). Two paths, depending on whether you have a domain.

## Path A — quick tunnel (no domain, great for testing)

One command brings up directory + gateway + a public `*.trycloudflare.com` URL
and prints the exact link to open the deployed app against it:

```bash
./deploy/serve-public.sh          # add SHARE=1 to also share THIS machine
```

It prints, for example:

```
Public gateway : https://random-words-1234.trycloudflare.com
Open the deployed app against it:
  https://cloudiy-cloud.vercel.app/os.html?gw=https://random-words-1234.trycloudflare.com
```

Open that link and the web app talks to your gateway (the `?gw=` override, also
persisted in `localStorage` as `cloudiy_gw`). `Ctrl+C` — or `./deploy/stop-public.sh`
if it was killed abruptly — stops everything. The URL is random and rotates each
run; it is fine for testing but not a stable address.

## Path B — named tunnel (stable hostname, for production)

Requires a domain on Cloudflare. Gives a fixed `gateway.<your-domain>`:

```bash
cloudflared tunnel login
cloudflared tunnel create cloudiy-gateway            # writes <TUNNEL_ID>.json creds
cloudflared tunnel route dns cloudiy-gateway gateway.example.com
# edit cloudflared-config.yml with your <TUNNEL_ID>, hostname and creds path, then:
cloudflared tunnel --config ./cloudflared-config.yml run
```

See `cloudflared-config.yml` in this directory. For an always-on VPS that runs
the directory + gateway as systemd services and installs the tunnel connector,
use `deploy/vps-setup.sh` (pass `CF_TUNNEL_TOKEN=<token>` to wire the connector).

With a stable hostname the deployed-app link is stable too:

```
https://cloudiy-cloud.vercel.app/os.html?gw=https://gateway.example.com
```

## Wiring the deployed web app

The frontend already reads a gateway override, in priority order:

1. `?gw=<https-url>` query param (also saved to `localStorage.cloudiy_gw`),
2. `localStorage.cloudiy_gw`,
3. same-origin (works when you open the gateway's own origin directly),
4. `http://127.0.0.1:4600` fallback.

So no code change is needed to point the Vercel site at a tunnel — just append
`?gw=<url>`. Without a reachable gateway the app stays in demo/preview mode.

## Alternative — run the gateway locally

If the browser is on the **same machine** as the gateway, skip tunnels entirely:
run `cloudiy os --web-dir web` and open `http://127.0.0.1:4600/os.html`. No DNS,
no TLS, no public exposure. Simplest and safest for solo use.

## ⚠️ Security — auth hardening required before public exposure

A tunnel puts the gateway on the public internet, including `/api/vm/*` and the
`/api/shell` WebSocket. **Do not leave one up unattended on mainnet without
hardening it first.** There is a `TODO` in `crates/cloudiy/src/http.rs` about
locking down allowed origins. Until origin/auth controls are in place, anyone
who learns the URL can drive the gateway. The quick tunnel's random URL is
obscurity, not security — treat it as a short-lived demo, and tear it down when
done.
