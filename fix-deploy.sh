#!/bin/bash
set -e

echo "🔧 Fixing Git lock and deploying..."

cd "$(dirname "$0")"

# Remove possible git lock
rm -f .git/index.lock 2>/dev/null || true

# Check status
echo "📦 Current git status:"
git status --short

echo ""
echo "🚀 Adding all changes..."
git add .

echo ""
echo "📝 Creating commit..."
git commit -m "feat: dashboard v2 + vercel.json + web files" || echo "Nothing to commit"

echo ""
echo "⬆️ Pushing to GitHub..."
git push origin main

echo ""
echo "✅ Done! Vercel should redeploy automatically."
