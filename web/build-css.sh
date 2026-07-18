#!/usr/bin/env sh
# Rebuild the site's Tailwind stylesheet (assets/tailwind.css).
# Run this whenever you add or change Tailwind classes in any *.html page.
# Requires Node/npx; pins Tailwind v3 (the version the old Play CDN served).
set -e
cd "$(dirname "$0")"
npx --yes tailwindcss@3 \
  --config ./tailwind.config.js \
  --input ./tailwind.input.css \
  --output ./assets/tailwind.css \
  --minify
echo "Built assets/tailwind.css"
