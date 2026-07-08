# Cloudiy Gateway Relay / Bridge

## The problem

The local web gateway runs as:

```
cloudiy os --web-dir web    # binds 127.0.0.1:4600
```

It listens on **localhost only**. The hosted web app (deployed to Vercel) runs in the
user's browser on a public origin, and **a browser cannot reach `127.0.0.1:4600` on
someone else's machine**. So the published web app can't talk to a provider's gateway
unless you bridge the gap.

## Two options

### (a) Run the gateway locally — recommended for now

Run `cloudiy os --web-dir web` on the same machine as the browser and open the app at
`http://127.0.0.1:4600`. No tunnel, no DNS, no TLS, no public exposure. This is the
simplest and safest path today.

### (b) Expose the gateway over HTTPS via a tunnel

If you need the *hosted* web app to reach a gateway running elsewhere, put the gateway
behind an HTTPS endpoint. Two configs are provided:

- **`Caddyfile`** — reverse-proxy `:443` on a domain → `127.0.0.1:4600` with automatic
  TLS. Run with `caddy run --config ./Caddyfile`. Requires ports 80/443 open and a
  domain pointing at the host.
- **`cloudflared-config.yml`** — a Cloudflare Tunnel mapping a public hostname to
  `http://127.0.0.1:4600`, with Cloudflare-managed TLS and no inbound firewall changes.

## HUMAN steps

Hosting, DNS, and TLS are **human steps** — they require a domain, a server, and an
account with Cloudflare/Let's Encrypt. The ops scaffolding only provides the configs;
someone has to:

- point a domain's DNS at the host (Caddy) or create+route a Cloudflare Tunnel,
- run the reverse proxy / tunnel process,
- keep it up (systemd, a process manager, etc.).

## ⚠️ Security — auth hardening required before public exposure

Option (b) puts the gateway on the public internet. **Do not do this on mainnet
without hardening it first.** There is a `TODO` in `crates/cloudiy/src/http.rs` about
locking down allowed origins before mainnet. Until origin/auth controls are in place,
anyone who finds the URL can drive the gateway. Prefer option (a) for now.
