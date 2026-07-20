#!/usr/bin/env bash
# Single source of truth for the thin-client SDK version.
#
# The Python, JavaScript and Go SDKs share one version line and are released
# together from one tag, but each language keeps its version in its own file.
# This script is what keeps those four files honest.
#
#   scripts/bump-version.sh            # print the current version
#   scripts/bump-version.sh --check    # exit 1 if the files disagree (CI gate)
#   scripts/bump-version.sh 0.4.0      # set all four to 0.4.0
#
# NOTE: the Rust crates (crates/*) are a SEPARATE version line — the workspace
# is at its own version in the root Cargo.toml and is not touched here. See
# `sdk/README.md` → Releasing for why.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# file:sed-pattern pairs — one per place a version is written.
PY_TOML="$ROOT/sdk/python/pyproject.toml"
PY_INIT="$ROOT/sdk/python/cloudiy_sdk/__init__.py"
JS_PKG="$ROOT/sdk/js/package.json"
GO_SRC="$ROOT/sdk/go/cloudiy.go"

read_py_toml() { sed -n 's/^version = "\(.*\)"$/\1/p' "$PY_TOML" | head -1; }
read_py_init() { sed -n 's/^__version__ = "\(.*\)"$/\1/p' "$PY_INIT" | head -1; }
read_js_pkg()  { sed -n 's/^  "version": "\(.*\)",$/\1/p' "$JS_PKG" | head -1; }
read_go_src()  { sed -n 's/^const Version = "\(.*\)"$/\1/p' "$GO_SRC" | head -1; }

# in-place sed that works on both BSD (macOS) and GNU sed
sed_i() { sed -i.bak "$1" "$2" && rm -f "$2.bak"; }

report() {
    printf '  %-34s %s\n' "sdk/python/pyproject.toml" "$(read_py_toml)"
    printf '  %-34s %s\n' "sdk/python/cloudiy_sdk/__init__.py" "$(read_py_init)"
    printf '  %-34s %s\n' "sdk/js/package.json" "$(read_js_pkg)"
    printf '  %-34s %s\n' "sdk/go/cloudiy.go" "$(read_go_src)"
}

check() {
    local a b c d
    a="$(read_py_toml)"; b="$(read_py_init)"; c="$(read_js_pkg)"; d="$(read_go_src)"
    for v in "$a" "$b" "$c" "$d"; do
        if [ -z "$v" ]; then
            echo "!! could not read a version — did a file's format change?" >&2
            report >&2
            exit 1
        fi
    done
    if [ "$a" = "$b" ] && [ "$b" = "$c" ] && [ "$c" = "$d" ]; then
        echo "SDK version $a (python, js, go in sync)"
        return 0
    fi
    echo "!! SDK versions disagree — run scripts/bump-version.sh <version>" >&2
    report >&2
    exit 1
}

case "${1:-}" in
    ""|--check)
        check
        ;;
    -h|--help)
        sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
        ;;
    *)
        NEW="$1"
        # Reject anything that isn't a bare semver: a stray `v` prefix or a
        # typo here would land in published package metadata.
        if ! printf '%s' "$NEW" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)*$'; then
            echo "!! not a semver version: '$NEW' (expected e.g. 0.4.0, no leading 'v')" >&2
            exit 1
        fi
        echo "==> bumping SDKs to $NEW"
        sed_i "s/^version = \".*\"$/version = \"$NEW\"/" "$PY_TOML"
        sed_i "s/^__version__ = \".*\"$/__version__ = \"$NEW\"/" "$PY_INIT"
        sed_i "s/^  \"version\": \".*\",$/  \"version\": \"$NEW\",/" "$JS_PKG"
        sed_i "s/^const Version = \".*\"$/const Version = \"$NEW\"/" "$GO_SRC"
        report
        check >/dev/null
        echo "    ok — all four in sync at $NEW"
        echo
        echo "Next: commit, then tag and push to release:"
        echo "  git commit -am \"release: SDKs v$NEW\""
        echo "  git tag sdk-v$NEW && git push origin sdk-v$NEW"
        ;;
esac
