#!/usr/bin/env bash
#
# 编译带 zmod 功能的 codex 二进制（先按序打 patches/，编完反向取消所有补丁）。
#
# 用法:
#   build_codex.sh [mac|windows|linux] [选项]
#
#   平台参数（可选，默认编译当前主机平台）:
#     mac      -> aarch64-apple-darwin / x86_64-apple-darwin（按主机架构）
#     windows  -> x86_64-pc-windows-msvc
#     linux    -> x86_64-unknown-linux-gnu
#
#   选项:
#     --debug      编译 debug 版本（默认 release）
#     --release    编译 release 版本
#     --out DIR    产物输出目录（默认 <仓库根>/release）
#     -h|--help    显示帮助
#
# 说明:
#   - patches/ 下所有 .patch 按文件名升序（001 → 002 → 003 …）顺序 `git apply`，
#     编译结束（成功/失败/中断）后按逆序 `git apply -R` 反向取消，保持 codex-rs
#     子树干净（符合 CLAUDE.md 约定）。所有 patch 一起打，不支持单独编译某个。
#   - 001-build.patch 专门承载 zmod crate 对 codex-rs 构建的改动（core/Cargo.toml
#     的 path 依赖等）；新增 crate 的构建改动请追加进 001-build.patch，不要再开新
#     的构建 patch，以免多个 patch 在同一处插入依赖产生冲突。
#   - 交叉编译到非主机平台需先安装对应工具链/链接器。

set -euo pipefail

PROFILE="release"
PLATFORM=""
OUT_DIR=""

usage() {
  sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

# 解析参数（支持带值的 --out）
while [ $# -gt 0 ]; do
  case "$1" in
    mac|windows|linux) PLATFORM="$1" ;;
    --debug)           PROFILE="debug" ;;
    --release)         PROFILE="release" ;;
    --out)             shift; OUT_DIR="${1:-}" ;;
    --out=*)           OUT_DIR="${1#*=}" ;;
    -h|--help)         usage 0 ;;
    *) echo "未知参数: $1" >&2; usage 1 ;;
  esac
  shift
done

# 仓库根 / codex-rs 工作区相对脚本位置定位（scripts/build/ -> 仓库根 -> codex-rs）
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKSPACE_DIR="$REPO_DIR/codex-rs"
PATCH_DIR="$REPO_DIR/patches"
[ -z "$OUT_DIR" ] && OUT_DIR="$REPO_DIR/release"

# 解析目标三元组
case "$PLATFORM" in
  mac)
    case "$(uname -m)" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      *)             TARGET="x86_64-apple-darwin" ;;
    esac
    ;;
  windows) TARGET="x86_64-pc-windows-msvc" ;;
  linux)   TARGET="x86_64-unknown-linux-gnu" ;;
  "")      TARGET="" ;;  # 主机默认目标
esac

# ── 收集 patches/ 下全部 patch（按文件名升序：001 → 002 → 003 …）────────
PATCH_FILES=()
while IFS= read -r f; do PATCH_FILES+=("$f"); done \
  < <(find "$PATCH_DIR" -maxdepth 1 -name '*.patch' | sort)
[ "${#PATCH_FILES[@]}" -gt 0 ] || { echo "错误: patches/ 下没有 .patch 文件" >&2; exit 1; }

# ── 还原函数：按逆序反向取消已打的补丁（trap 调用）─────────────────────
# APPLIED_FILES 记录已成功应用的 patch（按应用顺序）；逆序 `git apply -R` 反向取消。
# Cargo.lock 的 zmod 条目属构建副产物，一并还原。保留构建产物 target/。
APPLIED_FILES=()
restore_worktree() {
  cd "$REPO_DIR" 2>/dev/null || return 0
  local i
  for (( i=${#APPLIED_FILES[@]}-1; i>=0; i-- )); do
    git apply -R "${APPLIED_FILES[$i]}" 2>/dev/null || true
  done
  git checkout -q -- codex-rs/Cargo.lock 2>/dev/null || true
  find codex-rs -name '*.orig' -delete 2>/dev/null || true
}

# ── 应用 patch（按序 git apply；编完由 trap 反向取消）──────────────────
cd "$REPO_DIR"

# 收集所有 patch 将触及的文件（从 +++ b/ 行解析，相对仓库根），前置检查必须干净
PATCHED_PATHS="$(sed -n 's|^+++ b/||p' "${PATCH_FILES[@]}" | sort -u | tr '\n' ' ')"
# shellcheck disable=SC2086
if ! git diff --quiet -- $PATCHED_PATHS || ! git diff --cached --quiet -- $PATCHED_PATHS; then
  echo "错误: 以下 patch 目标文件有未提交改动，先清理再编译:" >&2
  # shellcheck disable=SC2086
  git status --short -- $PATCHED_PATHS >&2
  exit 1
fi
trap restore_worktree EXIT INT TERM

echo "应用 patch:"
for f in "${PATCH_FILES[@]}"; do
  if git apply "$f" 2>/tmp/codez_patch_err; then
    APPLIED_FILES+=("$f")
    echo "  ✓ $(basename "$f")"
  else
    echo "  ✗ $(basename "$f") 应用失败:" >&2
    sed 's/^/    /' /tmp/codez_patch_err >&2
    exit 1
  fi
done

# 校验关键接入点确实在源码里（防止静默漏打）
grep -q "codez_llm_switch::route" "$WORKSPACE_DIR/core/src/client.rs" \
  || { echo "错误: llm-switch 接入点未注入 client.rs" >&2; exit 1; }

# ── 组装 cargo 参数并编译 ──────────────────────────────────────────────
CARGO_ARGS=(build -p codex-cli)
[ "$PROFILE" = "release" ] && CARGO_ARGS+=(--release)

if [ -n "$TARGET" ]; then
  if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    echo "正在安装 Rust 目标: $TARGET"
    rustup target add "$TARGET"
  fi
  CARGO_ARGS+=(--target "$TARGET")
fi

echo "工作区: $WORKSPACE_DIR"
echo "平台:   ${PLATFORM:-host}  目标: ${TARGET:-default}  profile: $PROFILE"
echo "执行:   cargo ${CARGO_ARGS[*]}"

cd "$WORKSPACE_DIR"
cargo "${CARGO_ARGS[@]}"

# ── 拷贝产物到 release 目录 ────────────────────────────────────────────
SRC_OUT="target/${TARGET:+$TARGET/}$PROFILE"
BIN_NAME="codex"
[ "$PLATFORM" = "windows" ] && BIN_NAME="codex.exe"
SRC_BIN="$SRC_OUT/$BIN_NAME"

[ -f "$SRC_BIN" ] || { echo "错误: 未找到产物 $WORKSPACE_DIR/$SRC_BIN" >&2; exit 1; }

mkdir -p "$OUT_DIR"
cp -f "$SRC_BIN" "$OUT_DIR/$BIN_NAME"
echo "完成: $OUT_DIR/$BIN_NAME"
# restore_worktree 由 trap 在退出时执行
