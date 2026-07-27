# The CloudiyOS gateway as a managed service

The **gateway** (`cloudiy gateway`) is the local bridge between your browser and the
P2P network — it's what lets CloudiyOS work in a browser tab. By default you run
it in a terminal and leave the window open; close the terminal and CloudiyOS
dies with it. For anyone who isn't babysitting a terminal, that's a reason to
give up.

These units run the gateway as a **per-user background service**: it starts at
login (and, on Linux, optionally at boot), restarts if it crashes, and logs
where you can find it — no open terminal required.

> **Per-user, not a system service.** The gateway serves only on `127.0.0.1` and
> uses *your* node key (`~/.config/cloudiy`), so it runs as you, in your session
> — no root, no dedicated user, no `sudo`. This is deliberately different from
> the VPS/system `cloudiy-gateway.service` in [`../README.md`](../README.md),
> which is a public gateway behind a Cloudflare Tunnel.

## When to use a service vs. run it manually

| Use a service when… | Just run `cloudiy gateway` when… |
|---|---|
| You want CloudiyOS always available on your machine | You're trying it out or debugging |
| You don't want to keep a terminal open | You want to watch the logs live in the foreground |
| You want it back automatically after a reboot or crash | It's a one-off session |

Manual is always fine: `cloudiy gateway` (Ctrl-C to stop). The service just automates
keeping it up.

## Linux (systemd user service)

```bash
cd deploy/gateway
./gateway-service.sh install     # write the unit, enable+start it, verify /api/id
./gateway-service.sh status
./gateway-service.sh logs        # journalctl --user -u cloudiy-os -f (rotated by journald)
./gateway-service.sh stop        # stop now; it returns at next login/boot
./gateway-service.sh uninstall   # remove the service (your node key is kept)
```

`install` also tries `loginctl enable-linger $USER` so the gateway runs at boot,
not only after you log in — useful for a headless provider box. If lingering
can't be enabled, it falls back to start-at-login and tells you.

**Log rotation** is handled by journald natively — nothing to configure.

**Headless / SSH note.** `systemctl --user` needs a user D-Bus session. Over SSH
you may need `export XDG_RUNTIME_DIR=/run/user/$(id -u)` (the script sets it if
unset). If you see "Failed to connect to bus", enable lingering first:
`loginctl enable-linger $USER`.

## macOS (launchd LaunchAgent)

```bash
cd deploy/gateway
./gateway-service.sh install     # ~/Library/LaunchAgents/cloud.cloudiy.os.plist, load it, verify
./gateway-service.sh status
./gateway-service.sh logs        # tail -f ~/Library/Logs/cloudiy-os.log
./gateway-service.sh stop
./gateway-service.sh uninstall
```

A LaunchAgent runs in your GUI session: it starts at login and restarts on
crash. It does **not** run before login (that needs a root LaunchDaemon), which
is the right scope for a desktop gateway.

> ⚠️ **Code signing matters on macOS.** launchd execs the binary in a stricter
> context than your shell. A **Developer-ID-signed, notarized** release binary
> starts cleanly. An **unsigned / ad-hoc-signed** binary (a locally-built one,
> or a release built without signing — see [`../../SIGNING.md`](../../SIGNING.md))
> can be blocked by macOS's code-signing subsystem (`amfid`) when launchd starts
> it — the process shows as *running* but never binds, even though the same
> binary runs fine from a terminal. **The install script catches this**: it
> verifies `/api/id` and reports the failure with a pointer, instead of
> pretending the service came up. If you hit it, use a signed release (or run
> `cloudiy gateway` in a terminal). This was observed on the reference dev machine and
> is why signing the macOS release is recommended — tracked in `HANDOFF.md`.

**Log rotation:** launchd does not rotate `~/Library/Logs/cloudiy-os.log`. For a
long-running install, add an optional `newsyslog` rule (needs one-time root):

```
# /etc/newsyslog.d/cloudiy.conf   (create as root)
# logfilename                             mode count size when flags
/Users/<you>/Library/Logs/cloudiy-os.log  644  5     1024 *    NJ
```

## Windows

**Covered as a documented Scheduled Task, not a full service.** A per-user task
that starts `cloudiy gateway` at logon needs no admin and no extra tooling:

```powershell
# Create — starts the gateway at logon, in the background.
schtasks /Create /TN "CloudiyOS" /SC ONLOGON `
  /TR "\"$env:USERPROFILE\.local\bin\cloudiy.exe\" os --bind 127.0.0.1:4600" /F

# Start it now (or just log out and back in):
schtasks /Run /TN "CloudiyOS"

# Verify it's up:
curl.exe http://127.0.0.1:4600/api/id

# Stop / remove:
schtasks /End    /TN "CloudiyOS"
schtasks /Delete /TN "CloudiyOS" /F
```

Adjust the path to wherever `cloudiy.exe` lives. **What this does not do**
(honestly, vs. systemd/launchd): a plain ONLOGON task does not auto-restart on
crash and has no built-in log rotation. Restart-on-failure is possible with
`schtasks` XML settings or by using a service wrapper (NSSM/`sc.exe`), but that
is more setup than this mission covers — a native Windows service is **not**
provided here. Start-at-boot-before-login would also need a true service (admin).

## Security note

The gateway binds **loopback only** (`127.0.0.1:4600`) and has an anti-CSRF /
DNS-rebinding guard (`guard_local_origin`) so a web page can't reach it across
origins. But **any local process running as your user can call it today** —
there is no local authentication on `/api/*` yet. On a single-user machine
that's fine. On a **shared or multi-user machine**, running the gateway as a
background service means it's always reachable by everything else you're running
— weigh that before installing it there, and prefer running it manually only
when you need it.

> An RFC on local gateway authentication is in progress (agent `onchain`). Until
> it lands, treat the gateway as "trusted local process", not an authenticated
> endpoint.

## What the service does *not* change

Running as a service only changes *lifecycle* (start/stop/restart/logging). It
does not change what the gateway is or how it's reached: same loopback bind, same
node key, same `/api/*` surface. To expose it to a browser on another device,
that's still a tunnel ([`../README.md`](../README.md)), not this service.
