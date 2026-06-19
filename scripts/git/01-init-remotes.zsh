#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/git/01-init-remotes.zsh [CODEZ_REPO_URL] [CODEX_REPO_URL]

CODEZ_REPO_URL="${1:-git@github.com:dfbb/codez.git}"
CODEX_REPO_URL="${2:-https://github.com/openai/codex.git}"
UPSTREAM_REMOTE="upstream-codex"

SCRIPT_PATH="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Initializing git repository..."
  git init -b main
fi

if git remote get-url origin >/dev/null 2>&1; then
  echo "Updating origin to codez repo..."
  git remote set-url origin "$CODEZ_REPO_URL"
else
  echo "Adding origin as codez repo..."
  git remote add origin "$CODEZ_REPO_URL"
fi

if git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
  echo "Updating $UPSTREAM_REMOTE to openai/codex repo..."
  git remote set-url "$UPSTREAM_REMOTE" "$CODEX_REPO_URL"
else
  echo "Adding $UPSTREAM_REMOTE as openai/codex repo..."
  git remote add "$UPSTREAM_REMOTE" "$CODEX_REPO_URL"
fi

echo "Disabling push to $UPSTREAM_REMOTE..."
git remote set-url --push "$UPSTREAM_REMOTE" DISABLED

if ! git rev-parse --verify HEAD >/dev/null 2>&1; then
  git symbolic-ref HEAD refs/heads/main
  echo "Creating initial codez commit..."
  git add scripts/git
  git commit -m "Add codez git sync scripts"
fi

echo
echo "Current remotes:"
git remote -v
