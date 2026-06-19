#!/usr/bin/env bash
#
# 编译 codex-rs 的 codex 二进制，可指定目标平台。
#
# 用法:
#   build_codex.sh [mac|windows|linux] [--debug]
#
#   平台参数（可选，默认编译当前主机平台）:
#     mac      -> aarch64-apple-darwin / x86_64-apple-darwin（按主机架构）
#     windows  -> x86_64-pc-windows-msvc
#     linux    -> x86_64-unknown-linux-gnu
#
#   选项:
#     --debug    编译 debug 版本（默认 release）
#     --release  编译 release 版本
#     -h|--help  显示帮助
#
# 注意：交叉编译到非主机平台需要先安装对应工具链/链接器。

set -euo pipefail

PROFILE="release"
PLATFORM=""

usage() {
  sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

for arg in "$@"; do
  case "$arg" in
    mac|windows|linux) PLATFORM="$arg" ;;
    --debug)           PROFILE="debug" ;;
    --release)         PROFILE="release" ;;
    -h|--help)         usage 0 ;;
    *) echo "未知参数: $arg" >&2; usage 1 ;;
  esac
done

# codex-rs 工作区相对脚本位置定位（scripts/build/ -> 仓库根 -> codex-rs）
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../../codex-rs" && pwd)"

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

# 组装 cargo 参数
CARGO_ARGS=(build -p codex-cli)
[ "$PROFILE" = "release" ] && CARGO_ARGS+=(--release)

if [ -n "$TARGET" ]; then
  # 确保目标已安装
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

# 输出产物路径
OUT_DIR="target/${TARGET:+$TARGET/}$PROFILE"
BIN="$OUT_DIR/codex"
[ "$PLATFORM" = "windows" ] && BIN="$OUT_DIR/codex.exe"
echo "完成: $WORKSPACE_DIR/$BIN"
