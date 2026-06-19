#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/git/04-sync-codex-rs.zsh [CODEZ_BRANCH] [CODEX_BRANCH]

CODEZ_BRANCH="${1:-main}"
CODEX_BRANCH="${2:-main}"
UPSTREAM_REMOTE="upstream-codex"
PREFIX="codex-rs"

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
  exit 1
fi

require_clean_worktree

if [[ ! -d "$PREFIX" ]]; then
  echo "Error: $PREFIX does not exist."
  echo "Run scripts/git/03-import-codex-rs.zsh first."
  exit 1
fi

git remote set-url --push "$UPSTREAM_REMOTE" DISABLED

echo "Switching to $CODEZ_BRANCH..."
git switch "$CODEZ_BRANCH"

echo "Fetching origin and $UPSTREAM_REMOTE..."
git fetch origin "$CODEZ_BRANCH" || true
git fetch "$UPSTREAM_REMOTE" "$CODEX_BRANCH"

echo "Replacing codez/$PREFIX with latest $UPSTREAM_REMOTE/$CODEX_BRANCH:$PREFIX snapshot..."
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

git archive "$UPSTREAM_REMOTE/$CODEX_BRANCH:$PREFIX" | tar -x -C "$tmp_dir"

rm -rf "$PREFIX"
mkdir -p "$PREFIX"
cp -R "$tmp_dir"/. "$PREFIX"/

git add "$PREFIX"

if git diff --cached --quiet; then
  echo "No changes in $PREFIX."
else
  git commit -m "Sync openai/codex $PREFIX snapshot"
fi

echo
echo "Sync complete. Resolve conflicts if any, run checks, then push with:"
echo "  git push --progress origin $CODEZ_BRANCH"
