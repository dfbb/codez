#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/git/02-check-remotes.zsh

UPSTREAM_REMOTE="upstream-codex"
EXPECTED_ORIGIN="dfbb/codez"
EXPECTED_UPSTREAM="openai/codex"

SCRIPT_PATH="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Error: not inside a git repository."
  echo "Run scripts/git/01-init-remotes.zsh first, or clone the codez repo."
  exit 1
fi

origin_fetch="$(git remote get-url origin 2>/dev/null || true)"
origin_push="$(git remote get-url --push origin 2>/dev/null || true)"
upstream_fetch="$(git remote get-url "$UPSTREAM_REMOTE" 2>/dev/null || true)"
upstream_push="$(git remote get-url --push "$UPSTREAM_REMOTE" 2>/dev/null || true)"

matches_github_repo() {
  local url="$1"
  local repo="$2"

  [[ "$url" == *"github.com/$repo"* || "$url" == *"github.com:$repo"* ]]
}

echo "origin fetch:          ${origin_fetch:-N/A}"
echo "origin push:           ${origin_push:-N/A}"
echo "$UPSTREAM_REMOTE fetch: $upstream_fetch"
echo "$UPSTREAM_REMOTE push:  $upstream_push"
echo

if ! matches_github_repo "$origin_fetch" "$EXPECTED_ORIGIN"; then
  echo "Warning: origin does not look like dfbb/codez."
fi

if ! matches_github_repo "$origin_push" "$EXPECTED_ORIGIN"; then
  echo "Warning: origin push does not look like dfbb/codez."
fi

if ! matches_github_repo "$upstream_fetch" "$EXPECTED_UPSTREAM"; then
  echo "Warning: $UPSTREAM_REMOTE does not look like openai/codex."
fi

if [[ "$upstream_push" != "DISABLED" ]]; then
  echo "Warning: $UPSTREAM_REMOTE push is not disabled. Fixing..."
  git remote set-url --push "$UPSTREAM_REMOTE" DISABLED
fi

echo "OK: $UPSTREAM_REMOTE push is disabled."
