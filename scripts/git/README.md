# codez Git 同步脚本

这些脚本用于让 `codez/codex-rs` 跟 `https://github.com/openai/codex` 仓库里的 `codex-rs` 目录保持同步。

目标结构：

```text
openai/codex/codex-rs  ->  codez/codex-rs

codez/scripts          ->  codez 自己的脚本
codez/docs             ->  codez 自己的文档
codez/zmod             ->  codez 自己新增的 Rust crate
```

远程仓库约定：

```text
origin          = git@github.com:dfbb/codez.git，可正常 push
upstream-codex  = https://github.com/openai/codex.git，只 fetch，push URL 固定为 DISABLED
```

这不是 GitHub Fork，不会给 `openai/codex` 自动产生 Pull Request。所有 codez 的提交都推到 `origin`。

默认同步策略是快照同步。脚本用 `git archive upstream-codex/main:codex-rs` 取出 `openai/codex` 里的 `codex-rs` 目录内容，然后作为普通 codez 提交写入 `codez/codex-rs`。这样不会把 `openai/codex` 的原始 Git 历史一起提交到 `dfbb/codez`。

如果你之前已经用旧的 subtree 历史导入方式创建过提交，并且 push 时看到类似 `Enumerating objects: 121763`，建议丢弃那条本地导入历史后重新用本 README 的快照脚本导入。否则旧提交仍然会要求 GitHub 接收大量 upstream 历史对象。

## 0. 前置条件

进入 codez 本地目录：

```bash
cd /Users/dfbb/Sites/skycode/codez
```

给脚本加执行权限：

```bash
chmod +x scripts/git/*.zsh
```

这些脚本使用 bash 兼容语法，保留 `.zsh` 后缀只是为了沿用现有文件名。可以直接执行，也可以显式用 bash 或 zsh 执行：

```bash
scripts/git/01-init-remotes.zsh
bash scripts/git/01-init-remotes.zsh
zsh scripts/git/01-init-remotes.zsh
```

如果这个目录还不是 Git 仓库，先执行初始化脚本。如果已经是从 `dfbb/codez` clone 下来的仓库，也可以执行，它会修正 remote。

## 1. 初始化 remote

脚本：

```bash
scripts/git/01-init-remotes.zsh
```

作用：

- 如果当前目录不是 Git 仓库，执行 `git init`
- 设置 `origin` 为 `git@github.com:dfbb/codez.git`
- 设置 `upstream-codex` 为 `https://github.com/openai/codex.git`
- 禁止向 `upstream-codex` push
- 如果仓库还没有任何提交，把初始分支设为 `main`，并创建一个包含 `scripts/git` 的初始提交

执行：

```bash
scripts/git/01-init-remotes.zsh
```

如果你想显式指定仓库地址：

```bash
scripts/git/01-init-remotes.zsh \
  git@github.com:dfbb/codez.git \
  git@github.com:openai/codex.git
```

## 2. 检查 remote 是否安全

脚本：

```bash
scripts/git/02-check-remotes.zsh
```

作用：

- 显示 `origin` 和 `upstream-codex`
- 检查 `origin` 是否指向 `dfbb/codez`
- 检查 `upstream-codex` 是否指向 `openai/codex`
- 如果 `upstream-codex` 的 push URL 不是 `DISABLED`，自动修正

执行：

```bash
scripts/git/02-check-remotes.zsh
```

## 3. 首次导入 openai/codex 的 codex-rs

脚本：

```bash
scripts/git/03-import-codex-rs.zsh
```

作用：

- 从 `upstream-codex/main:codex-rs` 导出目录快照
- 把快照导入到当前仓库的 `codex-rs/`
- 不把 `openai/codex` 的原始历史写进 codez 主线
- 只修改本地 codez 仓库，不会 push 到 GitHub

执行：

```bash
scripts/git/03-import-codex-rs.zsh main main
```

参数含义：

```text
第 1 个 main = codez 的目标分支
第 2 个 main = openai/codex 的来源分支
```

导入后检查结果：

```bash
git status
git log --oneline --decorate -5
```

确认无误后推送到 codez：

```bash
scripts/git/06-push-origin-slow-network.zsh main
```

## 4. 后续同步 codex-rs

脚本：

```bash
scripts/git/04-sync-codex-rs.zsh
```

作用：

- 拉取 `openai/codex`
- 重新导出 `openai/codex/codex-rs` 快照
- 用最新快照替换 `codez/codex-rs`
- 如果内容有变化，创建一个 codez 自己的同步提交
- 不自动 push，方便先运行检查

执行：

```bash
scripts/git/04-sync-codex-rs.zsh main main
```

这个脚本采用目录快照替换，不做三方合并。如果你在 `codez/codex-rs` 里维护了本地修改，同步前应先提交或迁移到 `zmod`，否则同步会以 upstream 快照覆盖 `codex-rs`。

```bash
git status
```

确认无误后推送到 codez：

```bash
scripts/git/06-push-origin-slow-network.zsh main
```

## 5. 慢网络推送到 codez

脚本：

```bash
scripts/git/06-push-origin-slow-network.zsh
```

作用：

- 只推送当前分支或指定分支到 `origin`
- 使用 `--progress` 显示上传进度
- 通过 `GIT_SSH_COMMAND` 开启 SSH keepalive，减少慢网络下 GitHub 连接长时间无响应后断开的概率

执行：

```bash
scripts/git/06-push-origin-slow-network.zsh main
```

这个脚本不能修复所有网络问题，但配合默认的快照同步，初次 push 的对象数量会明显少于保留完整 codex 历史的方案。

## 6. 危险操作：用 upstream 强制重置 codez/codex-rs

脚本：

```bash
scripts/git/05-reset-codex-rs-from-upstream.zsh
```

作用：

- 删除当前 `codez/codex-rs`
- 用 `openai/codex/codex-rs` 的最新内容重新填充
- 提交一个本地 commit
- 不带入 `openai/codex` 原始 Git 历史
- 不会自动 push

只有在你明确要放弃 `codez/codex-rs` 当前本地改动时使用。

执行：

```bash
scripts/git/05-reset-codex-rs-from-upstream.zsh main main --yes
```

确认无误后再推送：

```bash
git push origin main
```

## 推荐日常流程

首次创建：

```bash
cd /Users/dfbb/Sites/skycode/codez
chmod +x scripts/git/*.zsh
scripts/git/01-init-remotes.zsh
scripts/git/02-check-remotes.zsh
scripts/git/03-import-codex-rs.zsh main main
scripts/git/06-push-origin-slow-network.zsh main
```

平时开发 codez：

```bash
git switch -c b/my-change
# 修改 scripts/docs/zmod 或 codez 自己的代码
git add .
git commit -m "Add codez change"
git push -u origin b/my-change
```

定期同步 openai/codex 的 `codex-rs`：

```bash
git switch main
scripts/git/02-check-remotes.zsh
scripts/git/04-sync-codex-rs.zsh main main
# 解决冲突并运行检查
scripts/git/06-push-origin-slow-network.zsh main
```

## 设计原则

- `origin` 是 codez 唯一推送目标。
- `upstream-codex` 只读，只用于同步 `openai/codex`。
- `codez/codex-rs` 只承载来自 `openai/codex/codex-rs` 的同步代码。
- 默认只保存 `codex-rs` 快照同步提交，不保存 `openai/codex` 原始 Git 历史。
- `codez/scripts`、`codez/docs`、`codez/zmod` 放 codez 自己的内容。
- 不使用 GitHub Fork 按钮。
- 不向 `openai/codex` 创建 Pull Request。

## 脚本兼容性检查

执行：

```bash
scripts/git/test-shell-compat.sh
```

这个检查会对所有 `.zsh` 脚本分别运行 `bash -n` 和 `zsh -n`，并检查是否误用了 zsh 专用 shebang 或 zsh 专用参数展开。
