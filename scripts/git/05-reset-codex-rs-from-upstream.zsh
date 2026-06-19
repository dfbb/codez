#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/git/05-reset-codex-rs-from-upstream.zsh <CODEZ_BRANCH> [CODEX_BRANCH] --yes

CODEZ_BRANCH="${1:?Missing CODEZ_BRANCH}"
CODEX_BRANCH="${2:-main}"
CONFIRM="${3:-}"
UPSTREAM_REMOTE="upstream-codex"
PREFIX="codex-rs"
SPLIT_BRANCH="sync/codex-rs-reset"

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

if [[ "$CONFIRM" != "--yes" ]]; then
  echo "Danger: this replaces codez/$PREFIX with $UPSTREAM_REMOTE/$CODEX_BRANCH:$PREFIX."
  echo "Run:"
  echo "  $0 $CODEZ_BRANCH $CODEX_BRANCH --yes"
  exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Error: not inside a git repository."
  exit 1
fi

require_clean_worktree
git remote set-url --push "$UPSTREAM_REMOTE" DISABLED

git switch "$CODEZ_BRANCH"
git fetch "$UPSTREAM_REMOTE" "$CODEX_BRANCH"

git branch -D "$SPLIT_BRANCH" >/dev/null 2>&1 || true
git subtree split --prefix="$PREFIX" "$UPSTREAM_REMOTE/$CODEX_BRANCH" -b "$SPLIT_BRANCH"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

git archive "$SPLIT_BRANCH" | tar -x -C "$tmp_dir"

rm -rf "$PREFIX"
mkdir -p "$PREFIX"
cp -R "$tmp_dir"/. "$PREFIX"/

git add "$PREFIX"
git commit -m "Reset codez/$PREFIX from openai/codex $PREFIX"

git branch -D "$SPLIT_BRANCH" >/dev/null 2>&1 || true

echo
echo "Reset complete. Review the commit, then push with:"
echo "  git push origin $CODEZ_BRANCH"
