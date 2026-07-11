# Cloudiy infra — directory + gateway on one VPS

Two always-on services make the network usable beyond direct-dial-by-Node-ID:

- **Directory** (`cloudiy directory`) — the discovery registry. Providers announce
  to it; consumers discover through it. Pure P2P (iroh), reachable by its
  **Directory ID** from anywhere — no public IP, DNS or open ports.
- **Gateway** (`cloudiy os`) — the browser↔P2P bridge. Serves `/api/*` (HTTP/WS)
  so a **no-install** consumer can use CloudiyOS from a browser. Needs a public
  **HTTPS** URL, which Cloudflare Tunnel provides.

Both run on a single box:

```
Oracle Free Tier VM (Ampere/AMD, US$0)
├─ cloudiy-directory.service   → reachable via <Directory ID>   (P2P, no ingress)
├─ cloudiy-gateway.service     → http://127.0.0.1:4600           (localhost only)
└─ cloudflared (tunnel)        → https://gateway.cloudiy.cloud → 127.0.0.1:4600
```

Cost: **US$0** (Oracle Free Tier + Cloudflare Free). Only the domain is paid (yearly).

## 1. Provision the VM (Oracle Cloud Free Tier)

- Create an **Always Free** compute instance — Ubuntu 22.04, either **Ampere A1
  (arm64)** or the **AMD micro (amd64)**. Both have prebuilt cloudiy binaries.
- **No inbound ports needed** (Cloudflare Tunnel is outbound-only). Keep the
  security list closed except SSH.

## 2. Run the setup

```
scp deploy/vps-setup.sh opc@<vm-ip>:~
ssh opc@<vm-ip>
sudo bash vps-setup.sh
```

It installs the `cloudiy` binary (no Rust), creates a `cloudiy` system user,
generates the directory key, installs the two systemd services, starts the
directory, prints the **Directory ID**, then starts the gateway pointed at it.

> **Back up the directory key.** The Directory ID is derived from a 32-byte key.
> The setup stores it as `CLOUDIY_DIRECTORY_KEY=<64 hex>` in `/etc/cloudiy/directory.env`
> and prints it in the summary — **copy that line into your vault**. Losing it
> changes the Directory ID and breaks everyone pointing at the old one.
>
> To reproduce the same ID on a rebuilt/second box, pass the key in:
> `sudo CLOUDIY_DIRECTORY_KEY=<64 hex> bash vps-setup.sh`.
>
> Migrating an existing box that used the old file-based key? Read it once with
> `sudo cat /var/lib/cloudiy/.config/cloudiy/directory.key` and use that hex as
> `CLOUDIY_DIRECTORY_KEY` — same ID, now managed as an env secret.

## 3. Expose the gateway with Cloudflare Tunnel

Requires `cloudiy.cloud` on Cloudflare (nameservers). Then:

1. Dashboard → **Zero Trust → Networks → Tunnels → Create a tunnel** → *Cloudflared*.
2. Name it (e.g. `cloudiy-gateway`), **copy the connector token**.
3. On the VM: `sudo cloudflared service install <TOKEN>` (or re-run
   `sudo CF_TUNNEL_TOKEN=<TOKEN> bash vps-setup.sh`).
4. Back in the tunnel → **Public Hostname** → Add:
   - Subdomain `gateway`, domain `cloudiy.cloud`
   - Service **HTTP** → `localhost:4600`

`https://gateway.cloudiy.cloud` is now live (TLS + WebSocket, no open ports).

## 4. Zero-config discovery (bake the Directory ID)

So every installed `cloudiy` auto-announces/discovers with no flags, build the
releases with the Directory ID compiled in:

- Add repo secret **`CLOUDIY_DEFAULT_DIRECTORY`** = the Directory ID, and pass it
  to the build in `.github/workflows/release.yml`
  (`env: CLOUDIY_DEFAULT_DIRECTORY: ${{ secrets.CLOUDIY_DEFAULT_DIRECTORY }}`),
  then cut a new tag. (Ask and I'll wire this step.)

Until then, point at it explicitly: `CLOUDIY_DIRECTORY=<id>` / `--via <id>`.

## 5. Test discovery (no Node ID needed)

```
# provider (your Mac)
CLOUDIY_DIRECTORY=<Directory ID> cloudiy share

# consumer (any machine)
cloudiy run --via <Directory ID> --kernel vector_add --data "1,2,3,4;5,6,7,8"
```

The scheduler finds the provider through the directory and returns a
signature-verified result.

## Browser zero-install path (wired)

The deployed web app reaches a gateway via a `?gw=<https-url>` override (also
persisted as `localStorage.cloudiy_gw`). Point the Vercel site at this box's
tunnel:

```
https://cloudiy-cloud.vercel.app/os.html?gw=https://gateway.cloudiy.cloud
```

No domain yet? Skip this whole VPS guide and use the one-command quick tunnel —
directory + gateway + a public `*.trycloudflare.com` URL, with the ready-to-open
link printed for you:

```
./deploy/serve-public.sh          # SHARE=1 to also share this machine
```

See `deploy/relay/README.md` for both tunnel paths. Still worth hardening before
mainnet: a **CORS**/allowed-origin layer on the gateway and browser-side
**re-verification of provider signatures**, so the hosted gateway stays a
convenience, not a trust point (`TODO` in `crates/cloudiy/src/http.rs`).

## Operations

- Status: `systemctl status cloudiy-directory cloudiy-gateway`
- Logs: `journalctl -u cloudiy-directory -f` / `-u cloudiy-gateway -f`
- Update the binary: re-run the installer, then `systemctl restart cloudiy-*`.
- Redundancy: run a second directory on another box and announce/discover on
  both (`--directory a --directory b`, `--via a --via b`).
