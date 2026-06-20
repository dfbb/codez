
This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> 语言规范：本仓库所有对话与文档统一使用中文，文档使用 markdown 格式。

## 项目概览

codez 是基于 [openai/codex](https://github.com/openai/codex) 仓库 `codex-rs` 目录的二次开发项目。它把上游的 Rust 工作区原样同步进来，再在其之上叠加 codez 自己的脚本、文档和新增功能。

目录职责：

| 目录 | 内容 | 谁维护 |
| --- | --- | --- |
| `codex-rs/` | 上游 `openai/codex` 的 `codex-rs` 目录快照（约 494 个 crate 的 Rust workspace） | 通过同步脚本从上游拉取，**不要手改** |
| `scripts/` | codez 自己的脚本（目前是 `scripts/git/` 同步脚本） | codez |
| `docs/` | codez 自己的文档 | codez |
| `zmod/` | codez 新增功能的 Rust crate（当前为空） | codez |
| `patches/` | 每个 `zmod` crate 对应、打到 `codex-rs` 上的 patch（当前为空） | codez |

## 核心设计：patch + 同步

**编译模型**：在 `codex-rs` 上打上所有 `zmod` 对应的 patch，然后 `codex-rs` 和 `zmod` 一起编译。`zmod/` 与 `patches/` 目前为空，这套打补丁的构建流程尚未落地，是后续要实现的设计目标——新增功能时优先放进 `zmod` 的独立 crate，对 `codex-rs` 的侵入性改动用 `patches/` 表达，而不是直接修改 `codex-rs` 源码（否则下次同步会冲突）。

**为什么不直接改 `codex-rs`**：`codex-rs/` 是用 `git subtree --squash` 从上游同步的快照。直接改动会在每次 `04-sync-codex-rs.zsh` 同步时产生三方合并冲突。把改动隔离到 `zmod/` + `patches/` 才能让同步保持顺畅。

## zmod crate 与 patch 命名规则

每个新增功能是一个 `zmod` crate，**一一对应**一个 `patches` 下的补丁；二者用同一个 `<feature>` 短名（kebab-case，如 `auth-proxy`、`session-trace`）绑定。

**crate 命名**：

- 目录：`zmod/<feature>/`
- 包名（`Cargo.toml` 的 `[package] name`）：`codez-<feature>`，统一加 `codez-` 前缀，避免与 `codex-rs` 上游 crate（多为 `codex-*`）冲突。
- crate 内 lib/bin target 名用下划线形式 `codez_<feature>`（Cargo 自动转换，保持显式更清晰）。

**patch 命名**：

- 文件：`patches/<feature>.patch`，文件名与对应 zmod crate 的 `<feature>` 完全一致。
- 内容：仅包含为接入该 zmod 功能而对 `codex-rs` 做的最小侵入式改动（注册到 workspace `members`、在上游代码里插入调用点等）。
- 一个 `<feature>` 只对应一个 patch；若改动跨多个上游文件，仍合并进同一个 `<feature>.patch`，不要按文件拆分。

**对应关系示例**：

```text
zmod/auth-proxy/            ->  patches/auth-proxy.patch
  Cargo.toml name = codez-auth-proxy
zmod/session-trace/        ->  patches/session-trace.patch
  Cargo.toml name = codez-session-trace
```

**编译顺序**：先把 `patches/*.patch` 全部打到 `codex-rs` 上（patch 至少要把各 `codez-<feature>` 加入 workspace `members`），再对 `codex-rs` + `zmod` 一起 `cargo build`。

## zmod 运行时配置与开关

所有 zmod 功能都受配置文件 `~/.codex/config-zmod.toml` 控制，可以**单独开关**，互不影响。该文件与 codex 自身的 `~/.codex/config.toml` 并列，但只承载 zmod 的配置，不混入上游配置。

约定：

- 每个 zmod 功能在文件里有一张以 `<feature>` 为名的 table，至少包含 `enabled` 开关；功能的私有配置项也放在这张 table 下。
- `<feature>` 与 zmod crate / patch 的命名严格一致（见上节）。
- 文件或某个 table 缺失时，对应功能默认**关闭**（fail-safe）；功能代码读不到配置时不得报错，按未启用处理。

```toml
# ~/.codex/config-zmod.toml
[auth-proxy]
enabled = true
endpoint = "http://127.0.0.1:8080"

[session-trace]
enabled = false
```

## 同步与 Git 工作流

远程约定（详见 `scripts/git/README.md`）：

- `origin` = `git@github.com:dfbb/codez.git`，codez **唯一**的 push 目标。
- `upstream-codex` = `https://github.com/openai/codex.git`，**只读**，push URL 被固定为 `DISABLED`。
- 这不是 GitHub Fork，不会向 `openai/codex` 产生 PR。codez 主线只保存 `codex-rs` 的 squash 同步提交，不展开上游完整历史。

常用脚本（位于 `scripts/git/`，bash/zsh 兼容）：

```bash
scripts/git/01-init-remotes.zsh            # 初始化 / 修正 remote
scripts/git/02-check-remotes.zsh           # 检查 remote 安全性
scripts/git/03-import-codex-rs.zsh main main   # 首次导入 codex-rs
scripts/git/04-sync-codex-rs.zsh main main     # 后续同步上游 codex-rs（可能产生冲突）
scripts/git/05-reset-codex-rs-from-upstream.zsh main main --yes  # 危险：用上游强制重置
scripts/git/06-push-origin-slow-network.zsh main   # 慢网络下带进度推送到 origin
scripts/git/test-shell-compat.sh           # 对所有 .zsh 跑 bash -n / zsh -n 兼容性检查
```

### 同步 codex-rs 的规则（`04-sync-codex-rs.zsh`）

`scripts/git/04-sync-codex-rs.zsh [CODEZ_BRANCH=main] [CODEX_BRANCH=main]` 负责把上游最新 `codex-rs` 合并进来，其硬性约束和步骤：

1. **工作区必须干净**：脚本开头 `require_clean_worktree` 会拒绝有未提交改动（含已暂存）的工作区。跑同步前先 commit 或 stash。
2. 强制把 `upstream-codex` 的 push URL 设为 `DISABLED`（只读保护）。
3. `git switch` 到 `CODEZ_BRANCH`，fetch `origin` 与 `upstream-codex`。
4. 从 `upstream-codex/<CODEX_BRANCH>` 用 `git subtree split --prefix=codex-rs` 切出临时分支 `sync/codex-rs-latest`。
5. 用 `git subtree merge --prefix=codex-rs --squash` 合并，提交信息固定为 `Sync openai/codex codex-rs into codez`。首次没有 subtree 元数据时，会用 `merge --squash -Xsubtree=codex-rs` 引导并补写 `git-subtree-dir` / `git-subtree-split` 元数据。
6. 删除临时分支。脚本**不自动 push**，方便先解决冲突、跑检查。

冲突处理（只会发生在 `codex-rs/` 内，因为 codez 自己的改动都在 `zmod`/`patches`，理论上不与上游冲突）：

```bash
git status
# 编辑 codex-rs/ 下的冲突文件
git add codex-rs
git commit
```

同步后必做：重新核对 `patches/*.patch` 是否仍能干净地打到新版 `codex-rs` 上；上游若改动了 patch 命中的代码，需要更新对应 `<feature>.patch`。确认无误后再推送：

```bash
scripts/git/06-push-origin-slow-network.zsh main   # 慢网络推荐
# 或
git push --progress origin main
```

### 提交 codez 自己改动的规则

- codez 自己的改动（`scripts/` `docs/` `zmod/` `patches/`）在 feature 分支上开发，**不要**和「同步 codex-rs」的 squash 提交混在一条提交里。
- 一个 zmod 功能的 crate 与其 `patches/<feature>.patch` 尽量在同一组提交里一起改，保持 crate ↔ patch 同步。
- **绝不**直接修改 `codex-rs/` 源码来实现 codez 功能（用 `patches/` 表达），否则下次 `04-sync-codex-rs.zsh` 必然冲突。
- 所有提交只推到 `origin`（`dfbb/codez`），永远不向 `upstream-codex` push。

```bash
git switch -c b/my-change
# 改 scripts / docs / zmod / patches
git add .
git commit -m "Add codez change"
git push -u origin b/my-change
```

## 构建与测试（codex-rs）

`codex-rs` 是标准 Cargo workspace，工具链固定为 Rust `1.95.0`（见 `codex-rs/rust-toolchain.toml`，含 `clippy`、`rustfmt`、`rust-src`）。所有命令在 `codex-rs/` 目录下执行：

```bash
cargo build                      # 构建整个 workspace
cargo build -p codex-cli         # 只构建某个 crate（主二进制名为 codex，crate 名 codex-cli）
cargo nextest run                # 跑全部测试（仓库用 nextest，配置见 .config/nextest.toml）
cargo nextest run -p codex-core  # 只测单个 crate
cargo nextest run -E 'test(测试名)'   # 用 nextest 过滤表达式跑单个测试
cargo clippy --all-targets       # lint（clippy 规则见 codex-rs/clippy.toml）
cargo fmt                        # 格式化（rustfmt.toml）
```

注意事项：

- **TUI 颜色规则**：`clippy.toml` 禁用 `Color::Rgb` / `Color::Indexed` 及 `Stylize::white/black/yellow`，统一用 ANSI 颜色以适配不同终端主题。改 `tui/` 代码时遵守，配色参考 `tui/styles.md`。
- 测试中允许 `unwrap`/`expect`（`allow-unwrap-in-tests`），非测试代码避免。
- 子模块若有更细的约定，会有就近的 `AGENTS.md`（例如 `codex-rs/tui/src/bottom_pane/AGENTS.md`）。

## 文档

- `codex-rs/docs/`：上游文档（`protocol_v1.md`、`codex_mcp_interface.md`、`bazel.md`）。
- `scripts/git/README.md`：同步脚本的完整说明与设计原则。
