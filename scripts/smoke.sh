#!/bin/sh
# Minimal smoke test of a built `cloudiy` binary — the kind of thing that must
# work on a *stranger's* machine, because that is what a provider is. It runs a
# handful of commands that don't need a GPU, a network, or any config, and
# starts+stops the provider daemon once. If any step fails, the binary does not
# run on this host — exactly the failure we want to catch before a public
# release, not after a provider gives up and never comes back.
#
#   scripts/smoke.sh /path/to/cloudiy
#
# POSIX sh on purpose: this runs inside bare distro containers (alpine, debian,
# fedora) that may have no bash. No bashisms, no external tools beyond coreutils.
#
# Exit 0 iff every step passed.
set -eu

BIN="${1:-./target/release/cloudiy}"

if [ ! -x "$BIN" ]; then
    echo "!! not an executable: $BIN" >&2
    # Distinguish "missing dynamic loader" (the classic glibc-on-musl failure)
    # from a genuinely absent file, because on Alpine the gnu binary IS present
    # but cannot exec — and that is the single most useful thing to report.
    [ -e "$BIN" ] && echo "   file exists but is not runnable here (dynamic loader / libc mismatch?)" >&2
    exit 1
fi

fail=0
step() { printf '  %-26s' "$1"; }
ok()   { echo "ok${1:+  ($1)}"; }
bad()  { echo "FAIL${1:+  ($1)}"; fail=1; }

# Best-effort libc identification — the whole point of testing on Alpine is to
# see musl here (a glibc binary won't run against it).
libc="$(ldd --version 2>&1 | head -1 || true)"
case "$libc" in
    *musl*) libc="musl" ;;
    *GNU*|*GLIBC*|*glibc*) libc="glibc" ;;
    *) libc="unknown" ;;
esac

echo "smoke: $BIN"
echo "  host: $(uname -s) $(uname -m); libc: $libc"

# 1. --version — also the canary for "can the loader even start this binary".
#    On a libc mismatch this is where you get "not found" / "No such file".
step "cloudiy --version"
if v="$("$BIN" --version 2>&1)" && echo "$v" | grep -q "cloudiy"; then
    ok "$v"
else
    bad "$v"
fi

# 2. id — prints the node's ed25519 identity (64 hex chars). No network needed.
step "cloudiy id"
if id="$("$BIN" id 2>&1)" && echo "$id" | grep -Eq '^[0-9a-f]{64}$'; then
    ok "$(echo "$id" | cut -c1-16)…"
else
    bad "$id"
fi

# 3. info --help — a subcommand help path (clap wiring intact).
step "cloudiy info --help"
if "$BIN" info --help >/dev/null 2>&1; then ok; else bad; fi

# 4. top-level --help.
step "cloudiy --help"
if "$BIN" --help >/dev/null 2>&1; then ok; else bad; fi

# 5. share — the daemon a provider actually runs. Start it headless (P2P only,
#    no GPU required), confirm it announces a Node ID, then stop it. This is the
#    real "does it run on this box" test: hardware detection, key handling, the
#    P2P endpoint all have to initialize.
step "cloudiy share up/down"
log="$(mktemp)"
RUST_LOG=info "$BIN" share --no-http >"$log" 2>&1 &
pid=$!
up=0
i=0
while [ "$i" -lt 20 ]; do
    if grep -q "Node ID:" "$log" 2>/dev/null; then up=1; break; fi
    kill -0 "$pid" 2>/dev/null || break   # process died early
    i=$((i + 1))
    sleep 1
done
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
if [ "$up" = 1 ]; then
    ok "announced Node ID"
else
    bad "no Node ID in $(wc -l <"$log") lines"
    echo "----- share output -----" >&2
    cat "$log" >&2
    echo "------------------------" >&2
fi
rm -f "$log"

echo
if [ "$fail" = 0 ]; then
    echo "SMOKE PASSED"
else
    echo "SMOKE FAILED"
fi
exit "$fail"
