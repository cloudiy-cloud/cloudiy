#!/bin/bash
set -e

echo "🔧 Corrigindo remote do Git..."

cd "$(dirname "$0")"

# Remove possible lock
rm -f .git/index.lock 2>/dev/null || true

echo "📍 Remote atual:"
git remote -v

echo ""
echo "🔄 Trocando remote para https://github.com/w3-surfer/gpuasas.git ..."
git remote set-url origin https://github.com/w3-surfer/gpuasas.git

echo ""
echo "✅ Novo remote:"
git remote -v

echo ""
echo "📦 Adicionando arquivos..."
git add .

echo ""
echo "📝 Commitando mudanças..."
git commit -m "feat: gpuasas dashboard + landing page v0.1 + rust crates" || echo "(Nada novo para commitar)"

echo ""
echo "🚀 Enviando para https://github.com/w3-surfer/gpuasas ..."
git push -u origin main

echo ""
echo "✅ Deploy concluído! O Vercel agora deve apontar para o repo correto."
