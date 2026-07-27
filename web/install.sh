#!/bin/sh
# Cloudiy provider node installer (macOS / Linux). No Rust, no compiler.
#   curl -fsSL https://cloudiy.cloud/install.sh | sh
#
# Rather read it before running? It is short and does nothing surprising:
#   curl -fsSL https://cloudiy.cloud/install.sh | less
#
# Downloads the prebuilt `cloudiy` binary for your OS/arch from the latest
# GitHub Release and drops it in ~/.local/bin (override with CLOUDIY_INSTALL_DIR).
#
# The whole installer lives in main(), called only on the LAST line, so a
# truncated download (dropped connection mid-pipe) can never run a half command.
set -eu

INSTALLER_BUILD="1.1"

main() {
    # Public distribution repo (binaries only; source lives in the private repo).
    REPO="cloudiy-cloud/cloudiy-dist"
    BIN="cloudiy"
    DEST="${CLOUDIY_INSTALL_DIR:-$HOME/.local/bin}"

    # ---- presentation layer: colour + Unicode only when it's safe ----------
    # Degrade to plain text when stdout is not a TTY, NO_COLOR is set, or the
    # terminal is dumb. Colour NEVER carries meaning on its own — every status
    # also uses a word (OK / ERROR / …), so a plain-text run loses nothing.
    esc=$(printf '\033')
    if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != "dumb" ]; then
      FANCY=1
      B="${esc}[38;2;204;255;51m"   # brand green (#ccff33), truecolor
      D="${esc}[2m"; BOLD="${esc}[1m"; RED="${esc}[1;31m"; RS="${esc}[0m"
      M_STEP="▸"; M_OK="✓"; BAR_CH="━"; DASH="—"; ARROW="→"
    else
      FANCY=0
      B=''; D=''; BOLD=''; RED=''; RS=''
      M_STEP=">"; M_OK="+"; BAR_CH="-"; DASH="--"; ARROW="->"
    fi

    # Terminal width (for a bar that fits and art that degrades when narrow).
    cols=$(stty size 2>/dev/null | awk '{print $2}') || cols=''
    [ -n "$cols" ] || cols="${COLUMNS:-80}"
    case "$cols" in *[!0-9]*|'') cols=80 ;; esac
    barw=$cols; [ "$barw" -gt 72 ] && barw=72
    bar=$(awk -v n="$barw" -v c="$BAR_CH" 'BEGIN{s="";for(i=0;i<n;i++)s=s c;print s}')

    say()  { printf '%s\n' "$*"; }
    rule() { printf '%s%s%s\n' "$D" "$bar" "$RS"; }
    step() { printf '  %s%s%s %s\n' "$B" "$M_STEP" "$RS" "$*"; }
    ok()   { printf '  %s%s OK%s %s\n' "$B" "$M_OK" "$RS" "$*"; }
    die()  { printf '  %sX ERROR%s %s\n' "$RED" "$RS" "$*" >&2; exit 1; }

    # ---- header ------------------------------------------------------------
    say ''
    rule
    if [ "$cols" -ge 54 ]; then
      printf '%s' "$B"
      cat <<'ART'
      ___  _                 _  _
     / __|| | ___  _  _   __| |(_) _  _
    | (__ | |/ _ \| || | / _` || || || |
     \___||_|\___/ \_,_| \__,_||_| \_, |
                                   |__/
ART
      printf '%s' "$RS"
    else
      printf '  %s%scloudiy%s\n' "$BOLD" "$B" "$RS"
    fi
    printf '  %sthe open compute network %s provider node installer%s\n' "$D" "$DASH" "$RS"
    rule
    printf '  Signal acquired. I will guide the installation.\n'
    printf '  %sAbort any time with Ctrl-C. Nothing is changed until you confirm.%s\n' "$D" "$RS"
    say ''

    # ---- detect the target -------------------------------------------------
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
      Linux)
        case "$arch" in
          x86_64|amd64)   target="x86_64-unknown-linux-gnu" ;;
          aarch64|arm64)  target="aarch64-unknown-linux-gnu" ;;
          *) die "unsupported architecture '$arch' on Linux." ;;
        esac ;;
      Darwin)
        case "$arch" in
          arm64)   target="aarch64-apple-darwin" ;;
          x86_64)  target="x86_64-apple-darwin" ;;
          *) die "unsupported architecture '$arch' on macOS." ;;
        esac ;;
      *)
        printf '  %sX ERROR%s unsupported OS %s. On Windows use the PowerShell installer:\n' "$RED" "$RS" "'$os'" >&2
        printf '          irm https://cloudiy.cloud/install.ps1 | iex\n' >&2
        exit 1 ;;
    esac

    # Best-effort: name the exact release that will be installed (never fatal).
    version=''
    if command -v curl >/dev/null 2>&1; then
      version=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | head -n1 | sed 's/.*"tag_name":[ ]*"//; s/".*//') || version=''
    fi
    [ -n "$version" ] || version="latest"

    step "Host        $(printf '%s%s / %s%s' "$BOLD" "$os" "$arch" "$RS")"
    step "Target      $target"
    step "Version     $(printf '%s%s%s' "$B" "$version" "$RS")"
    step "Install to  $DEST/$BIN"
    step "Installer   build $INSTALLER_BUILD"
    say ''
    printf '  Next: download the verified %scloudiy%s binary and place it above.\n' "$B" "$RS"

    # ---- confirm (only when a real terminal is attached) -------------------
    # curl | sh has no interactive stdin (stdin IS the script), so read from
    # /dev/tty. With no tty (CI, container) we proceed non-interactively.
    if [ -t 1 ] && [ -e /dev/tty ]; then
      printf '  %sPress Enter to install, or Ctrl-C to abort:%s ' "$B" "$RS"
      read _ans < /dev/tty || true
      say ''
    else
      printf '  %s(no terminal attached %s installing non-interactively)%s\n' "$D" "$DASH" "$RS"
      say ''
    fi

    url="https://github.com/${REPO}/releases/latest/download/${BIN}-${target}.tar.gz"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    step "Downloading  $target ..."
    if ! curl -fsSL "$url" -o "$tmp/pkg.tar.gz"; then
      printf '  %sX ERROR%s download failed (%s).\n' "$RED" "$RS" "$url" >&2
      printf '          No release yet for this platform? See github.com/%s/releases\n' "$REPO" >&2
      exit 1
    fi

    # Verify the SHA-256 the release publishes next to the binary. A tampered or
    # truncated archive — or a missing checksum — aborts the install (fail closed).
    if ! curl -fsSL "$url.sha256" -o "$tmp/pkg.sha256"; then
      die "could not fetch checksum ($url.sha256) — refusing to install unverified."
    fi
    expected="$(awk '{print $1}' "$tmp/pkg.sha256" | tr 'A-F' 'a-f')"
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "$tmp/pkg.tar.gz" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
      actual="$(shasum -a 256 "$tmp/pkg.tar.gz" | awk '{print $1}')"
    else
      die "no sha256 tool (sha256sum/shasum) found — cannot verify download."
    fi
    actual="$(printf '%s' "$actual" | tr 'A-F' 'a-f')"
    if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
      printf '  %sX ERROR%s checksum mismatch — refusing to install.\n' "$RED" "$RS" >&2
      printf '            expected %s\n' "$expected" >&2
      printf '            actual   %s\n' "$actual" >&2
      exit 1
    fi
    ok "Checksum verified (sha256)"

    tar -xzf "$tmp/pkg.tar.gz" -C "$tmp"
    binpath="$(find "$tmp" -type f -name "$BIN" | head -n1)"
    [ -n "$binpath" ] || die "binary not found in archive."

    mkdir -p "$DEST"
    install -m 0755 "$binpath" "$DEST/$BIN" 2>/dev/null || { cp "$binpath" "$DEST/$BIN"; chmod 0755 "$DEST/$BIN"; }
    ok "Installed to $DEST/$BIN"

    # ---- what happened + the real next step --------------------------------
    say ''
    rule
    printf '  %s%sInstalled.%s  cloudiy %s %s %s\n' "$BOLD" "$B" "$RS" "$version" "$ARROW" "$DEST/$BIN"
    rule
    on_path=0
    case ":$PATH:" in *":$DEST:"*) on_path=1 ;; esac
    if [ "$on_path" -eq 0 ]; then
      printf '  %s!%s Not on PATH yet. Add it:\n' "$RED" "$RS"
      printf '      export PATH="%s:$PATH"\n' "$DEST"
      say ''
    fi
    printf '  Next step %s start earning by offering this machine:\n' "$DASH"
    printf '      %s%s share%s\n' "$B" "$BIN" "$RS"
    printf '  %s`%s share` walks you through the receiving setup (wallet, price, limits).%s\n' "$D" "$BIN" "$RS"
    printf '  See everything it can do:  %s --help\n' "$BIN"
    say ''
}

main "$@"
