#!/usr/bin/env bash
# Install / manage the personal CloudiyOS gateway (`cloudiy gateway`) as a per-user
# background service, so CloudiyOS keeps working without a terminal window open
# and comes back after a crash or reboot.
#
#   ./gateway-service.sh install     # write + enable + start the service, verify
#   ./gateway-service.sh status      # is it running? is /api/id answering?
#   ./gateway-service.sh stop        # stop now (stays installed; returns at login)
#   ./gateway-service.sh start
#   ./gateway-service.sh restart
#   ./gateway-service.sh logs        # follow the gateway log
#   ./gateway-service.sh uninstall   # stop + remove the service (keeps your key)
#
# Per-user by design: no root, no system service, no sudo. On Linux it installs
# a systemd *user* unit; on macOS a launchd *LaunchAgent*. Both run as you and
# serve only 127.0.0.1.
#
# Config (env, all optional):
#   CLOUDIY_BIN          path to the cloudiy binary (default: found on PATH)
#   CLOUDIY_OS_BIND      gateway address        (default: 127.0.0.1:4600)
#   CLOUDIY_OS_WEB_DIR   serve the full CloudiyOS UI from this dir (default: none,
#                        built-in terminal only)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BIND="${CLOUDIY_OS_BIND:-127.0.0.1:4600}"
WEB_DIR="${CLOUDIY_OS_WEB_DIR:-}"
LABEL="cloud.cloudiy.os"           # launchd label
UNIT="cloudiy-os"                  # systemd unit name

die()  { echo "!! $*" >&2; exit 1; }
info() { echo "==> $*"; }

# Running this under sudo would install the service into root's home, not yours.
[ "$(id -u)" = "0" ] && die "run WITHOUT sudo — this is a per-user service (installs into your home, serves your loopback)."

resolve_bin() {
    if [ -n "${CLOUDIY_BIN:-}" ]; then
        command -v "$CLOUDIY_BIN" >/dev/null 2>&1 && { command -v "$CLOUDIY_BIN"; return; }
        [ -x "$CLOUDIY_BIN" ] && { echo "$CLOUDIY_BIN"; return; }
        die "CLOUDIY_BIN=$CLOUDIY_BIN is not executable"
    fi
    if command -v cloudiy >/dev/null 2>&1; then command -v cloudiy; return; fi
    for c in "$HOME/.local/bin/cloudiy" /usr/local/bin/cloudiy /opt/homebrew/bin/cloudiy; do
        [ -x "$c" ] && { echo "$c"; return; }
    done
    die "could not find the 'cloudiy' binary — install it, or set CLOUDIY_BIN=/path/to/cloudiy"
}

# Poll /api/id until the gateway answers (iroh relay init takes ~1s).
verify() {
    info "verifying the gateway answers on http://$BIND/api/id"
    i=0
    while [ "$i" -lt 20 ]; do
        if curl -fsS "http://$BIND/api/id" >/dev/null 2>&1; then
            id="$(curl -fsS "http://$BIND/api/id" 2>/dev/null)"
            echo "    up — $id"
            return 0
        fi
        i=$((i + 1)); sleep 1
    done
    echo "!! gateway did not answer /api/id within 20s — check '$0 logs'" >&2
    if [ "$(uname -s)" = "Darwin" ]; then
        cat >&2 <<'HINT'
   macOS note: if the process shows as running but never binds, the binary is
   likely being blocked by the code-signing subsystem (amfid) when launchd
   execs it — an unsigned / ad-hoc-signed binary can hang at load in the
   launchd context even though it runs fine from a terminal. The fix is a
   Developer-ID-signed, notarized release binary (see SIGNING.md). Confirm with:
     log show --last 2m --predicate 'process == "amfid"' | tail
HINT
    fi
    return 1
}

# --- Linux: systemd user unit --------------------------------------------------
sd_dir="$HOME/.config/systemd/user"
sd_unit="$sd_dir/$UNIT.service"

# systemctl --user needs a user bus; over SSH XDG_RUNTIME_DIR is often unset.
sd_env() { export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; }

linux_install() {
    local bin webarg
    bin="$(resolve_bin)"
    webarg=""
    [ -n "$WEB_DIR" ] && webarg=" --web-dir $WEB_DIR"
    mkdir -p "$sd_dir"
    sed -e "s|__CLOUDIY_BIN__|$bin|g" \
        -e "s|__CLOUDIY_BIND__|$BIND|g" \
        -e "s|__CLOUDIY_WEBDIR_ARG__|$webarg|g" \
        "$HERE/$UNIT.service" > "$sd_unit"
    info "wrote $sd_unit (cloudiy: $bin, bind: $BIND${WEB_DIR:+, web-dir: $WEB_DIR})"
    sd_env
    systemctl --user daemon-reload
    systemctl --user enable --now "$UNIT" \
        || die "systemctl --user failed — is a user session/bus available? (see README: enable-linger / XDG_RUNTIME_DIR)"
    # Run at boot without an active login session (best-effort; may prompt polkit).
    if loginctl enable-linger "$USER" 2>/dev/null; then
        info "lingering enabled — the gateway runs at boot, not only after login"
    else
        echo "   (note) could not enable-linger; the gateway starts at login. Enable boot-time with: loginctl enable-linger $USER" >&2
    fi
    verify
}
linux_start()   { sd_env; systemctl --user start "$UNIT"; verify; }
linux_stop()    { sd_env; systemctl --user stop "$UNIT"; info "stopped (still installed; returns at login/boot)"; }
linux_restart() { sd_env; systemctl --user restart "$UNIT"; verify; }
linux_status()  { sd_env; systemctl --user status "$UNIT" --no-pager || true; }
linux_logs()    { sd_env; journalctl --user -u "$UNIT" -f; }
linux_uninstall() {
    sd_env
    systemctl --user disable --now "$UNIT" 2>/dev/null || true
    rm -f "$sd_unit"
    systemctl --user daemon-reload 2>/dev/null || true
    info "removed $sd_unit (your node key under ~/.config/cloudiy is untouched)"
}

# --- macOS: launchd LaunchAgent ------------------------------------------------
la_dir="$HOME/Library/LaunchAgents"
la_plist="$la_dir/$LABEL.plist"
la_log="$HOME/Library/Logs/cloudiy-os.log"

mac_install() {
    local bin webargs
    bin="$(resolve_bin)"
    webargs=""
    [ -n "$WEB_DIR" ] && webargs="<string>--web-dir</string><string>$WEB_DIR</string>"
    mkdir -p "$la_dir" "$(dirname "$la_log")"
    sed -e "s|__CLOUDIY_BIN__|$bin|g" \
        -e "s|__CLOUDIY_BIND__|$BIND|g" \
        -e "s|__CLOUDIY_WEBDIR_ARGS__|$webargs|g" \
        -e "s|__CLOUDIY_LOG__|$la_log|g" \
        "$HERE/$LABEL.plist" > "$la_plist"
    info "wrote $la_plist (cloudiy: $bin, bind: $BIND${WEB_DIR:+, web-dir: $WEB_DIR})"
    # bootout first so re-install is idempotent (ignore "not found").
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$la_plist" || die "launchctl bootstrap failed"
    verify
}
mac_start()   { launchctl bootstrap "gui/$(id -u)" "$la_plist" 2>/dev/null || launchctl kickstart "gui/$(id -u)/$LABEL"; verify; }
mac_stop()    { launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true; info "stopped (plist stays; returns at next login)"; }
mac_restart() { launchctl kickstart -k "gui/$(id -u)/$LABEL" 2>/dev/null || mac_start; verify; }
mac_status()  { launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null | grep -E "state|pid" || echo "not loaded"; }
mac_logs()    { touch "$la_log"; tail -f "$la_log"; }
mac_uninstall() {
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
    rm -f "$la_plist"
    info "removed $la_plist (your node key under ~/.config/cloudiy is untouched)"
}

# --- dispatch ------------------------------------------------------------------
case "$(uname -s)" in
    Linux)  OS=linux ;;
    Darwin) OS=mac ;;
    *)      die "unsupported OS $(uname -s) — see README for Windows (Scheduled Task)." ;;
esac

cmd="${1:-}"
case "$cmd" in
    install|start|stop|restart|status|logs|uninstall) "${OS}_${cmd}" ;;
    "" ) die "usage: $0 {install|start|stop|restart|status|logs|uninstall}" ;;
    * )  die "unknown command '$cmd' — usage: $0 {install|start|stop|restart|status|logs|uninstall}" ;;
esac
