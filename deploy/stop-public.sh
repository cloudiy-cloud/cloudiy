#!/usr/bin/env bash
# Stop everything serve-public.sh started (directory, gateway, share, tunnel).
# Use this if serve-public.sh was killed abruptly and left orphans running
# (a normal Ctrl+C cleans up on its own).
set -uo pipefail
echo "Stopping Cloudiy public stack..."
pkill -f "cloudflared tunnel --url http://127.0.0.1:4600" 2>/dev/null && echo "  cloudflared: stopped" || echo "  cloudflared: none"
pkill -f "cloudiy os"        2>/dev/null && echo "  gateway: stopped"   || echo "  gateway: none"
pkill -f "cloudiy share"     2>/dev/null && echo "  share: stopped"     || echo "  share: none"
pkill -f "cloudiy directory" 2>/dev/null && echo "  directory: stopped" || echo "  directory: none"
if command -v lsof >/dev/null 2>&1 && lsof -ti :4600 >/dev/null 2>&1; then
  lsof -ti :4600 | xargs kill 2>/dev/null || true
fi
echo "Done."
