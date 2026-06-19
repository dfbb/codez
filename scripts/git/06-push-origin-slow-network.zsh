#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/git/06-push-origin-slow-network.zsh [BRANCH]

SCRIPT_PATH="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Error: not inside a git repository."
  exit 1
fi

BRANCH="${1:-$(git branch --show-current)}"

if [[ -z "$BRANCH" ]]; then
  echo "Error: cannot determine branch. Pass it explicitly, for example:"
  echo "  scripts/git/06-push-origin-slow-network.zsh main"
  exit 1
fi

origin_push="$(git remote get-url --push origin 2>/dev/null || true)"
if [[ "$origin_push" != *"github.com:dfbb/codez"* && "$origin_push" != *"github.com/dfbb/codez"* ]]; then
  echo "Warning: origin push does not look like dfbb/codez:"
  echo "  $origin_push"
fi

echo "Pushing $BRANCH to origin with SSH keepalive..."

GIT_SSH_COMMAND="ssh -o ServerAliveInterval=30 -o ServerAliveCountMax=10" \
  git push --set-upstream --progress origin "$BRANCH"
