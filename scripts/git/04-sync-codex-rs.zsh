#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/git/04-sync-codex-rs.zsh [CODEZ_BRANCH] [CODEX_REF]
#
# CODEX_REF 默认留空 -> 自动检测上游最新稳定 release tag(rust-vX.Y.Z,
# 排除 alpha/beta/rc 预发布),只同步到该 release,不跟踪 main 的中间提交。
# 如需固定到某个具体 tag 或分支,把它作为第二个参数显式传入。

CODEZ_BRANCH="${1:-main}"
CODEX_REF="${2:-}"
UPSTREAM_REMOTE="upstream-codex"
PREFIX="codex-rs"
SPLIT_BRANCH="sync/codex-rs-latest"
# 上游稳定 release tag 形如 rust-v0.141.0;排除带 -alpha/-beta/-rc 的预发布
# 以及 rust-vrust-v / rust-vv 之类的脏 tag。
RELEASE_TAG_REGEX='^rust-v[0-9]+\.[0-9]+\.[0-9]+$'

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

has_subtree_metadata() {
  git log --grep="git-subtree-dir: $PREFIX" --format=%H -1 -- "$PREFIX" | grep -q .
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

echo "Fetching origin..."
git fetch origin "$CODEZ_BRANCH" || true

echo "Fetching $UPSTREAM_REMOTE tags..."
git fetch --tags "$UPSTREAM_REMOTE"

# 未显式指定 ref 时,自动选出上游最新稳定 release tag。
if [[ -z "$CODEX_REF" ]]; then
  echo "Detecting latest upstream stable release tag..."
  CODEX_REF="$(git ls-remote --tags "$UPSTREAM_REMOTE" 2>/dev/null \
    | sed 's#.*refs/tags/##; s#\^{}$##' \
    | grep -E "$RELEASE_TAG_REGEX" \
    | sort -V \
    | tail -n 1)"
  if [[ -z "$CODEX_REF" ]]; then
    echo "Error: no stable release tag matching $RELEASE_TAG_REGEX found on $UPSTREAM_REMOTE."
    exit 1
  fi
  echo "Latest stable release: $CODEX_REF"
fi

echo "Fetching $UPSTREAM_REMOTE ref $CODEX_REF..."
git fetch "$UPSTREAM_REMOTE" "$CODEX_REF"

echo "Splitting upstream $CODEX_REF:$PREFIX..."
git branch -D "$SPLIT_BRANCH" >/dev/null 2>&1 || true
git subtree split --prefix="$PREFIX" "$CODEX_REF" -b "$SPLIT_BRANCH"

echo "Merging upstream $PREFIX ($CODEX_REF) into codez/$PREFIX with subtree squash..."
if has_subtree_metadata; then
  git subtree merge --prefix="$PREFIX" "$SPLIT_BRANCH" \
    --squash \
    -m "Sync openai/codex $PREFIX into codez ($CODEX_REF)"
else
  echo "No subtree metadata found for $PREFIX; bootstrapping subtree merge metadata."
  split_commit="$(git rev-parse "$SPLIT_BRANCH")"
  git merge --squash --allow-unrelated-histories \
    -s recursive \
    -Xsubtree="$PREFIX" \
    "$SPLIT_BRANCH"
  git commit \
    -m "Sync openai/codex $PREFIX into codez ($CODEX_REF)" \
    -m "git-subtree-dir: $PREFIX
git-subtree-split: $split_commit"
fi

git branch -D "$SPLIT_BRANCH" >/dev/null 2>&1 || true

echo
echo "Sync complete. Resolve conflicts if any, run checks, then push with:"
echo "  git push --progress origin $CODEZ_BRANCH"
