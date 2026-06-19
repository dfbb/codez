#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/git/03-import-codex-rs.zsh [CODEZ_BRANCH] [CODEX_BRANCH]

CODEZ_BRANCH="${1:-main}"
CODEX_BRANCH="${2:-main}"
UPSTREAM_REMOTE="upstream-codex"
PREFIX="codex-rs"
SPLIT_BRANCH="sync/codex-rs-import"

SCRIPT_PATH="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

require_clean_worktree() {
  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Error: working tree is not clean. Commit or stash changes first."
    exit 1
  fi
}

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Error: not inside a git repository."
  echo "Run scripts/git/01-init-remotes.zsh first, or clone the codez repo."
  exit 1
fi

require_clean_worktree

if [[ -e "$PREFIX" ]]; then
  echo "Error: $PREFIX already exists."
  echo "Use scripts/git/04-sync-codex-rs.zsh for later updates."
  exit 1
fi

git remote set-url --push "$UPSTREAM_REMOTE" DISABLED

echo "Fetching $UPSTREAM_REMOTE..."
git fetch "$UPSTREAM_REMOTE" "$CODEX_BRANCH"

echo "Switching to $CODEZ_BRANCH..."
if git show-ref --verify --quiet "refs/heads/$CODEZ_BRANCH"; then
  git switch "$CODEZ_BRANCH"
else
  git switch -c "$CODEZ_BRANCH"
fi

echo "Splitting $UPSTREAM_REMOTE/$CODEX_BRANCH:$PREFIX..."
git branch -D "$SPLIT_BRANCH" >/dev/null 2>&1 || true
git subtree split --prefix="$PREFIX" "$UPSTREAM_REMOTE/$CODEX_BRANCH" -b "$SPLIT_BRANCH"

echo "Importing into codez/$PREFIX with subtree squash..."
git subtree add --prefix="$PREFIX" "$SPLIT_BRANCH" \
  --squash \
  -m "Import openai/codex $PREFIX into codez"

git branch -D "$SPLIT_BRANCH" >/dev/null 2>&1 || true

echo
echo "Import complete. Review the result, then push with:"
echo "  git push --set-upstream --progress origin $CODEZ_BRANCH"
