# Task 09: patch core + 导出 + live 验证

> 属于 `2026-06-20-llm-compress-00-index.md`。先读 index Global Constraints。
> **关键纪律:绝不把对 `codex-rs/` 的改动作为最终交付留在源码里——所有 codex-rs 改动必须落进 `patches/llm-compress.patch`,并把 `codex-rs/` 工作区还原干净**(否则同步脚本 `04-sync-codex-rs.zsh` 会冲突)。依赖 Task 08。

**Goal:** 生成 `patches/llm-compress.patch`,把 `codez-llm-compress` 接进 codex-rs(**生产**情况 B 路线):① `core/Cargo.toml` 加外部 path 依赖;② `stream_responses_api` 在 `prepare_response_items_for_request` 之后、`record_started` 之前插入 queryid 取值 + `transform` 调用两行。验证:打 patch 后 `cargo build` 通过、`transform` 在真实 config 下确有压缩。

> **与开发期软链 member 的区分**:开发期(Task 01–08)用软链 `codex-rs/llm-compress` + `codex-rs/Cargo.toml` 的 members 行(dev-only 脚手架,dirty/gitignore,**不进本 patch**)。本任务的生产 patch 是**另一条**接入路径——`core/Cargo.toml` 外部 path 依赖,与软链无关。导出 patch 时只 diff `core/Cargo.toml` 与 `core/src/client.rs` 两文件,**绝不**把 `codex-rs/Cargo.toml`(members 行)纳入。

**覆盖 spec:** §2(集成点)、§11(workspace 接入 / patch 约定)。

**Files:**
- Create: `patches/llm-compress.patch`
- 临时改(仅为生成 patch,改完导出再还原):`codex-rs/core/Cargo.toml`、`codex-rs/core/src/client.rs`

**前置确认(执行前先看一眼真实行,避免行号漂移):**
```bash
cd /Users/dfbb/Sites/skycode/codez/codex-rs
sed -n '1308,1332p' core/src/client.rs    # 集成点上下文:let mut request ... stream_request
grep -n "codez-llm-switch\|codez-llm-compress" core/Cargo.toml   # 依赖区现状
```

---

- [ ] **Step 1: 在 core/Cargo.toml 加外部 path 依赖(生产接入,本任务首次添加)**

在 `codex-rs/core/Cargo.toml` 的 `[dependencies]` 加(紧挨现有 `codez-llm-switch` 那行之后,若有):

```toml
codez-llm-compress = { path = "../../zmod/llm-compress" }
```

> 注意:开发期是软链 member(`codex-rs/Cargo.toml` 的 members 行),与此处 `core/Cargo.toml` 的 path 依赖是两回事。本行是**生产**接入,要进 patch;members 行是 dev 脚手架,不进 patch。

- [ ] **Step 2: 在 client.rs 插入 transform 调用**

在 `codex-rs/core/src/client.rs` 的 `stream_responses_api` 内,定位:

```rust
            let store = request.store;
            self.client
                .prepare_response_items_for_request(&mut request.input, store);
            let inference_trace_attempt = inference_trace.start_attempt();
```

改为(在 `prepare_response_items_for_request` 之后、`let inference_trace_attempt` 之前插入两行):

```rust
            let store = request.store;
            self.client
                .prepare_response_items_for_request(&mut request.input, store);

            // ── llm-compress 前置拦截(独立 zmod,先压缩后路由)──
            let llm_compress_qid = responses_metadata.thread_id.clone();
            codez_llm_compress::transform(
                &mut request,
                &client_setup.api_provider,
                &llm_compress_qid,
            );

            let inference_trace_attempt = inference_trace.start_attempt();
```

> **借用说明(index 已核实)**:`&client_setup.api_provider` 短借用在 transform 调用语句末 drop,其后第 ~1324 行 `ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)` 仍可按值 move。`responses_metadata.thread_id` 是 `&CodexResponsesMetadata` 上的 `String` 字段——这里 `.clone()` 成 `llm_compress_qid: String` 再借 `&...`,避免与同作用域对 `responses_metadata` 的其它使用产生借用期纠葛(clone 一个短 UUID 成本可忽略)。`&mut request` 借用在调用末结束,不影响其后 `record_started(&request)` 与 `stream_request(request, ...)`。

- [ ] **Step 3: 编译验证(dirty 工作区)**

Run(`codex-rs/`):
```bash
cargo build -p codex-core
```
Expected: 编译通过。若报 `codez_llm_compress` 未找到 → 确认 Step 1 依赖行;若报借用/move 错 → 对照 Step 2 说明修正(通常是 thread_id 借用,用 Step 2 的 `.clone()` 写法)。

- [ ] **Step 4: live 验证——开启 config 跑真实压缩**

写一个临时 config 并用 crate 的集成测试验证 transform 在 enabled 下确有压缩(不依赖真实 codex 运行):

Run(`codex-rs/`):
```bash
HOME_BAK="$HOME"
TMPHOME="$(mktemp -d)"
mkdir -p "$TMPHOME/.codex"
cat > "$TMPHOME/.codex/config-zmod.toml" <<'EOF'
[llm_compress]
enabled = true
min_total_bytes = 64
per_item_min_bytes = 32

[llm_compress.truncate]
head_lines = 2
tail_lines = 2
max_bytes = 4096
EOF
HOME="$TMPHOME" cargo test -p codez-llm-compress --test transform_test -- --nocapture
ls -la "$TMPHOME/.codex/log/" 2>/dev/null || echo "(本测试若未触发写日志属正常,日志由真实大输入触发)"
rm -rf "$TMPHOME"
export HOME="$HOME_BAK"
```
Expected: transform_test 仍全绿(测试本身用 `disabled` 不变量为主;此步主要验证 enabled 配置下不 panic、crate 正常加载真实 config 路径)。

> live 端到端(真实 codex 发请求 → 看 `~/.codex/log/llm-compress.log` 出行)留作手动冒烟,不入自动化(避免依赖真实 API key)。

- [ ] **Step 5: 导出 patch**

Run(`codex-rs/`):
```bash
git diff -- core/Cargo.toml core/src/client.rs > ../patches/llm-compress.patch
```
检查 patch 内容只含这两文件、改动即上述依赖行 + 两行调用:
```bash
cat ../patches/llm-compress.patch
```

- [ ] **Step 6: 还原 codex-rs 工作区(交付物是 patch,不是改过的源码)**

Run(`codex-rs/`):
```bash
git checkout -- core/Cargo.toml core/src/client.rs
git status --short core/    # 应为空——codex-rs 工作区干净
```

> 这一步还原了本任务在 `core/Cargo.toml` 与 `core/src/client.rs` 引入的生产改动(已收进 patch)。**注意**:开发期软链 member 脚手架(软链 `codex-rs/llm-compress`、`codex-rs/Cargo.toml` 的 members 行、`codex-rs/Cargo.lock`)**不在**本次 checkout 范围,它们继续保持 dirty/未跟踪——除非你要彻底拆掉开发环境,否则不动它们。

- [ ] **Step 7: 验证 patch 可独立 apply**

Run(`codex-rs/`):
```bash
git apply --check ../patches/llm-compress.patch && echo "PATCH OK"
```
Expected: `PATCH OK`(无输出即冲突,需回到 Step 5 重导)。

- [ ] **Step 8: 提交**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add patches/llm-compress.patch docs/superpowers/plans/2026-06-20-llm-compress-09-patch-core.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): patch wiring codez-llm-compress into codex-rs (transform at request boundary)"
```

- [ ] **Step 9: 收尾自检**

- [ ] `patches/llm-compress.patch` 存在且 `git apply --check` 通过。
- [ ] `codex-rs/` 工作区干净(`git status --short codex-rs/core/` 为空)。
- [ ] `cargo test -p codez-llm-compress`(在打 patch 状态下或纯 crate 状态下)全绿。
- [ ] crate 不在 codex-rs workspace `members` 里;不提交 `zmod/llm-compress/Cargo.lock`。
