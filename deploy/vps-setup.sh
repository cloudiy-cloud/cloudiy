#!/usr/bin/env bash
# Cloudiy VPS setup — brings up the always-on infra on ONE box:
#   • cloudiy directory  (P2P provider registry; reachable by its Directory ID)
#   • cloudiy os          (browser↔P2P gateway; HTTP/WS on 127.0.0.1:4600)
#   • cloudflared         (optional Cloudflare Tunnel → gateway.<domain> over HTTPS)
#
# Target: a fresh systemd Linux VM (Oracle Free Tier Ampere/AMD, or any distro).
# Self-contained: scp just this file and run it — it needs nothing from the repo.
#
#   sudo bash vps-setup.sh
#   # optionally, to also install the Cloudflare Tunnel connector as a service:
#   sudo CF_TUNNEL_TOKEN="<token from the Cloudflare dashboard>" bash vps-setup.sh
set -euo pipefail

BIN=/usr/local/bin/cloudiy
GATEWAY_BIND="127.0.0.1:4600"
CLOUDIY_HOME=/var/lib/cloudiy

[ "$(id -u)" -eq 0 ] || { echo "run as root:  sudo bash $0" >&2; exit 1; }

log() { echo "==> $*"; }

# ---- 1. install the cloudiy binary (no Rust needed) ----------------------
log "installing cloudiy to /usr/local/bin"
CLOUDIY_INSTALL_DIR=/usr/local/bin sh -c 'curl -fsSL https://cloudiy.cloud/install.sh | sh'
"$BIN" --version

# ---- 2. dedicated user + persistent home (holds directory.key!) ----------
if ! id -u cloudiy >/dev/null 2>&1; then
  useradd --system --create-home --home-dir "$CLOUDIY_HOME" --shell /usr/sbin/nologin cloudiy
fi
install -d -o cloudiy -g cloudiy "$CLOUDIY_HOME/.config/cloudiy"
install -d /etc/cloudiy

# ---- 3. systemd units ----------------------------------------------------
log "writing systemd units"
cat > /etc/systemd/system/cloudiy-directory.service <<UNIT
[Unit]
Description=Cloudiy directory (P2P provider registry)
After=network-online.target
Wants=network-online.target

[Service]
User=cloudiy
Environment=HOME=$CLOUDIY_HOME
Environment=RUST_LOG=info
ExecStart=$BIN directory
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
UNIT

cat > /etc/systemd/system/cloudiy-gateway.service <<UNIT
[Unit]
Description=Cloudiy gateway (browser<->P2P bridge)
After=network-online.target cloudiy-directory.service
Wants=network-online.target

[Service]
User=cloudiy
Environment=HOME=$CLOUDIY_HOME
Environment=RUST_LOG=info
# CLOUDIY_DIRECTORY is written here after the directory reports its id:
EnvironmentFile=-/etc/cloudiy/gateway.env
ExecStart=$BIN os --bind $GATEWAY_BIND
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload

# ---- 4. start the directory, capture its stable Directory ID -------------
log "starting directory"
systemctl enable --now cloudiy-directory
DIR_ID=""
for _ in $(seq 1 30); do
  DIR_ID=$(journalctl -u cloudiy-directory -o cat --no-pager 2>/dev/null \
           | grep -oE "Directory ID: [a-f0-9]{64}" | awk '{print $3}' | tail -1 || true)
  [ -n "$DIR_ID" ] && break
  sleep 1
done
[ -n "$DIR_ID" ] || { echo "!! could not read Directory ID — check: journalctl -u cloudiy-directory" >&2; exit 1; }

# ---- 5. point the gateway at that directory, start it -------------------
log "starting gateway (directory=$DIR_ID)"
printf 'CLOUDIY_DIRECTORY=%s\nRUST_LOG=info\n' "$DIR_ID" > /etc/cloudiy/gateway.env
systemctl enable --now cloudiy-gateway

# ---- 6. cloudflared (optional) -----------------------------------------
if [ -n "${CF_TUNNEL_TOKEN:-}" ]; then
  log "installing cloudflared connector"
  arch=$(uname -m); case "$arch" in aarch64|arm64) cfa=arm64;; x86_64|amd64) cfa=amd64;; *) cfa="";; esac
  if [ -n "$cfa" ] && [ ! -x /usr/local/bin/cloudflared ]; then
    curl -fsSL "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-$cfa" -o /usr/local/bin/cloudflared
    chmod +x /usr/local/bin/cloudflared
  fi
  /usr/local/bin/cloudflared service install "$CF_TUNNEL_TOKEN"
fi

# ---- 7. summary ---------------------------------------------------------
cat <<SUMMARY

============================================================
  Cloudiy infra is up.

  Directory ID:  $DIR_ID
  Gateway:       http://$GATEWAY_BIND   (expose via Cloudflare Tunnel)

  Status:   systemctl status cloudiy-directory cloudiy-gateway
  Logs:     journalctl -u cloudiy-directory -f

  BACK UP THIS FILE (losing it changes the Directory ID and breaks
  everyone pointing at it):
     $CLOUDIY_HOME/.config/cloudiy/directory.key

  Next:
   1. Cloudflare dashboard → Zero Trust → Networks → Tunnels → Create.
      Public hostname:  gateway.<your-domain>  →  http://localhost:4600
      Copy the token, then:  sudo cloudflared service install <TOKEN>
      (or re-run this script with CF_TUNNEL_TOKEN=<TOKEN>).
   2. Bake the Directory ID into releases for zero-config discovery:
      set repo secret / build env  CLOUDIY_DEFAULT_DIRECTORY=$DIR_ID
   3. Test discovery:
      provider:  CLOUDIY_DIRECTORY=$DIR_ID cloudiy share
      consumer:  cloudiy run --via $DIR_ID --kernel vector_add --data "1,2,3,4;5,6,7,8"
============================================================
SUMMARY
