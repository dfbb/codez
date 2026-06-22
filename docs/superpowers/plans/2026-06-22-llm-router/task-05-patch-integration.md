# Task 5: patches/llm-switch.patch — codex 侧集成

> 隶属 [llm-router 实现计划](README.md)。先读 README 的 Global Constraints。**本 task 不改 `codex-rs/` 源码进 git,改动只表达在 patch 文件里。**

**Files:**
- Modify: `patches/llm-switch.patch`(在现有 patch 基础上扩展)
- 临时改(打 patch 后、不提交进 codex-rs 子树):`codex-rs/core/src/client.rs`、`codex-rs/core/src/compact.rs`、`codex-rs/memories/write/src/runtime.rs`、各测试调用点

**Interfaces:**
- Consumes(Task 4 产物,签名逐字依赖):
  - `codez_llm_switch::route(provider_id: &str, source: Option<&SessionSource>, request: &ResponsesApiRequest) -> Option<Route>`
  - `codez_llm_switch::should_bypass_websocket(provider_id: &str, source: Option<&SessionSource>) -> bool`
- Produces:更新后的 `patches/llm-switch.patch`,能 `git apply --check` 干净应用到当前 `codex-rs`,打上后 `cargo build -p codex-core` 通过。

---

## 背景与约束(给零上下文工程师)

`patches/llm-switch.patch` 是 codez「不直接改 codex-rs 源码」约定的产物(见 CLAUDE.md)。**现状:该 patch 尚未应用到工作树**(已 `grep` 核验 `core/Cargo.toml` 无 `codez-llm-switch`、`client.rs` 无 `codez_llm_switch`)。

现有 patch 已做的事(读 `patches/llm-switch.patch` 确认):
- `core/Cargo.toml` 加 `codez-llm-switch` path 依赖。
- `ModelClientState` 加 `model_provider_id: String` 字段;`ModelClient::new` 加同名形参;`session.rs:1019` 调用点补传 `config.model_provider_id.clone()`。
- `client.rs` 的 `stream_responses_api` 把 `ApiResponsesClient` 那段改成 `match codez_llm_switch::route(&self.client.state.model_provider_id) { None => 原生, Some(rt) => run() }`。
- 若干测试调用点补 `model_provider_id` 实参。

**本 task 要在 patch 里增量加 4 件事**(spec §5):
1. `route()` 调用点改 3 参签名:`route(&effective_source_provider_id?, Some(&effective_source), &request)`。
2. `ModelClientSession` 加 `source_override: Option<SessionSource>` 字段 + 公开 setter;`route()`/`should_bypass_websocket()` 读 `effective_source`(override 优先,否则 `state.session_source`)。
3. compact 与 memory phase1 调用点设 override(compact→`SubAgent(Compact)`,memory phase1→`Internal(MemoryConsolidation)`)。
4. `stream()` 顶部 WS 绕过:`responses_websocket_enabled() && !should_bypass_websocket(...)`。
5. `memories/write/src/runtime.rs:229` 的 `ModelClient::new()` 补 `model_provider_id` 实参(编译必需,现有 patch 未覆盖)。

> **`effective_source` 设计**:`ModelClientState` 已存 `session_source`(基础 source)。`ModelClientSession` 新增 `source_override: Option<SessionSource>`。在 `stream_responses_api` 与 `stream` 内,`let effective_source = self.source_override.as_ref().unwrap_or(&self.client.state.session_source);`。compact/memory-phase1 直连复用 client、其 `state.session_source` 是主线程值,靠 override 纠正;review/memory-phase2 起独立 session、`state.session_source` 已正确,不设 override。

---

- [ ] **Step 1: 应用现有 patch 到工作树(可逆,便于增量编辑)**

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git apply --check patches/llm-switch.patch && git apply patches/llm-switch.patch && echo "applied"
```

Expected: 打印 `applied`。此时 `codex-rs` 工作树带上 v1 集成改动(uncommitted)。

确认 crate 能编译进 core(v1 集成基线):

```bash
cd codex-rs && cargo build -p codex-core 2>&1 | tail -15
```

Expected: 编译通过(route 仍是旧 1 参签名,与 Task 4 不一致——下一步修)。

> 注:Task 4 已把 crate 的 `route` 改成 3 参。所以这一步 `cargo build -p codex-core` 实际会因 `route(&...)` 参数不匹配而**失败**;这是预期的——它正是 Step 2 要改的调用点。若 Step 1 build 失败并指向 `client.rs` 的 `route(` 调用,直接进 Step 2。

- [ ] **Step 2: client.rs — 加 source_override 字段与 setter**

编辑 `codex-rs/core/src/client.rs`:

(a) `ModelClientSession` 结构(`client.rs:238`)加字段:

```rust
pub struct ModelClientSession {
    client: ModelClient,
    websocket_session: WebsocketSession,
    turn_state: Arc<OnceLock<String>>,
    /// zmod/llm-switch purpose routing:覆盖本 session 请求的 source(compact/memory-phase1 用)。
    source_override: Option<codex_protocol::protocol::SessionSource>,
}
```

(b) `new_session()`(`client.rs:429`)初始化该字段:

```rust
    pub fn new_session(&self) -> ModelClientSession {
        ModelClientSession {
            client: self.clone(),
            websocket_session: self.take_cached_websocket_session(),
            turn_state: Arc::new(OnceLock::new()),
            source_override: None,
        }
    }
```

(c) 在 `impl ModelClientSession`(`client.rs:990` 一带)加公开 setter(builder 风格,便于 `new_session().with_source_override(...)`):

```rust
    /// zmod/llm-switch:为本 session 的请求打 source 标记(purpose 路由用)。
    pub fn with_source_override(
        mut self,
        source: codex_protocol::protocol::SessionSource,
    ) -> Self {
        self.source_override = Some(source);
        self
    }
```

- [ ] **Step 3: client.rs — route() 调用点改 3 参签名**

在 `stream_responses_api`(`client.rs:1263`),把现有(打了 v1 patch 后的)`match codez_llm_switch::route(&self.client.state.model_provider_id) {` 一段,改为先算 `effective_source` 再传 3 参。定位 `inference_trace_attempt.record_started(&request);`(`client.rs:1313`)之后的 `let client = ApiResponsesClient::new(...)` / `route(...)` 区域,替换为:

```rust
            let effective_source = self
                .source_override
                .as_ref()
                .unwrap_or(&self.client.state.session_source);
            let stream_result = match codez_llm_switch::route(
                &self.client.state.model_provider_id,
                Some(effective_source),
                &request,
            ) {
                None => {
                    let client = ApiResponsesClient::new(
                        transport,
                        client_setup.api_provider,
                        client_setup.api_auth,
                    )
                    .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
                    client.stream_request(request, options).await
                }
                Some(rt) => codez_llm_switch::run(rt, request, client_setup.api_auth).await,
            };
```

> 若现有 patch 中该段写法与上面略有出入(变量名 `transport`/`options`/`client_setup` 等),以工作树实际为准,只改 `route(...)` 的参数为 3 个并在其前插入 `effective_source` 绑定;`None`/`Some` 两臂逻辑保持 v1 不变。

- [ ] **Step 4: client.rs — stream() 顶部 WS 绕过**

在 `ModelClientSession::stream()`(`client.rs:1619`)内,`responses_websocket_enabled()` 判断(`client.rs:1633` 的 `if self.client.responses_websocket_enabled() {`)改为同时检查绕过:

```rust
                let effective_source = self
                    .source_override
                    .as_ref()
                    .unwrap_or(&self.client.state.session_source);
                let bypass_ws = codez_llm_switch::should_bypass_websocket(
                    &self.client.state.model_provider_id,
                    Some(effective_source),
                );
                if self.client.responses_websocket_enabled() && !bypass_ws {
```

> `effective_source` 在 `stream()` 这个作用域内重新绑定(与 Step 3 的是不同函数)。purpose 命中时 `bypass_ws==true` → 跳过整个 WS 分支 → 落到 `stream_responses_api()`(spec §4.2)。

- [ ] **Step 5: 验证 client.rs 改动可编译**

```bash
cd codex-rs && cargo build -p codex-core 2>&1 | tail -20
```

Expected: 编译通过(crate 3 参 route 与调用点一致;新字段已初始化)。若报 `session_source` 私有不可访问,确认 `ModelClientState.session_source` 在同 crate `client.rs` 内可见(同模块,私有字段同 crate 可访问——通过 `self.client.state.session_source`,`state` 是 `Arc<ModelClientState>`,字段在本文件定义,合法)。

- [ ] **Step 6: compact.rs — phase 设 override**

编辑 `codex-rs/core/src/compact.rs`,定位 `let mut client_session = sess.services.model_client.new_session();`(`compact.rs:217`),改为链式设 override:

```rust
    let mut client_session = sess
        .services
        .model_client
        .new_session()
        .with_source_override(codex_protocol::protocol::SessionSource::SubAgent(
            codex_protocol::protocol::SubAgentSource::Compact,
        ));
```

- [ ] **Step 7: runtime.rs — memory phase1 补 ModelClient::new 实参 + 设 override**

编辑 `codex-rs/memories/write/src/runtime.rs`:

(a) `ModelClient::new(...)`(`runtime.rs:229`)末尾补 `model_provider_id` 实参。该调用现有最后一个实参是 `/*attestation_provider*/ None,`(`runtime.rs:238`),在其后加:

```rust
            /*model_provider_id*/ config.model_provider_id.clone(),
```

(b) `let mut client_session = model_client.new_session();`(`runtime.rs:241`)改为设 override:

```rust
        let mut client_session = model_client
            .new_session()
            .with_source_override(codex_protocol::protocol::SessionSource::Internal(
                codex_protocol::protocol::InternalSessionSource::MemoryConsolidation,
            ));
```

> `config` 在该函数作用域内(`stream_stage_one_prompt` 的 `config: &Config` 参数),`config.model_provider_id` 是 `String`,`.clone()` 合法。确认 `codex-config`/`Config` 在 runtime.rs 已 use(它已用 `config.model_provider` 等字段,故 `model_provider_id` 同结构可直接取)。

- [ ] **Step 8: 验证整体编译(含 memory crate)**

```bash
cd codex-rs && cargo build -p codex-core -p codex-memories-write 2>&1 | tail -20
```

Expected: 两个 crate 都编译通过(memory write 包名已核实为 `codex-memories-write`)。

- [ ] **Step 9: 确认无遗漏的 ModelClient::new 测试调用点**

现有 patch 已补了 core 内已知测试调用点。memory crate 内仅 `runtime.rs:229` 一处 `ModelClient::new(`(已在 Step 7 处理),**无 memory 测试调用点**。复查 core 是否有现有 patch 未覆盖的新增调用点:

```bash
cd codex-rs && grep -rn "ModelClient::new(" --include="*.rs" core/ memories/
```

Expected:命中点 = `session.rs` 主调用 + `runtime.rs:229` + 现有 patch 已列的若干 core 测试文件。若出现清单外的新命中点,在其 `attestation_provider` 实参后补 `/*model_provider_id*/ String::new(),`。然后:

```bash
cd codex-rs && cargo test -p codex-core -p codex-memories-write --no-run 2>&1 | tail -20
```

Expected: 测试编译通过(`--no-run` 只编译不跑,省时)。

- [ ] **Step 10: 重新生成 patch 文件**

把工作树对 codex-rs 的全部改动重新导出为 patch。**只包含 codez 集成相关文件**,排除 dev 脚手架(`Cargo.toml` 的 members 行、`Cargo.lock`):

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git diff -- \
  codex-rs/core/Cargo.toml \
  codex-rs/core/src/client.rs \
  codex-rs/core/src/client_tests.rs \
  codex-rs/core/src/compact.rs \
  codex-rs/core/src/session/session.rs \
  codex-rs/core/src/session/tests.rs \
  codex-rs/core/tests/responses_headers.rs \
  codex-rs/core/tests/suite/client.rs \
  codex-rs/core/tests/suite/client_websockets.rs \
  codex-rs/memories/write/src/runtime.rs \
  > patches/llm-switch.patch
```

> 注:上面文件清单 = 现有 patch 覆盖的 core 文件 + 本 task 新增的 `compact.rs`、`memories/write/src/runtime.rs`。生成后用 `grep -n '^diff --git' patches/llm-switch.patch` 核对没有混入 `Cargo.lock` 或 `codex-rs/Cargo.toml`(workspace 根,members 那行是 dev-only)。若有 memory crate 的测试文件被改,也加进清单。

- [ ] **Step 11: 还原工作树,验证 patch 干净可应用**

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git checkout -- codex-rs/core codex-rs/memories   # 撤销 patch 改动(保留 Cargo.toml members 脚手架 dirty)
git apply --check patches/llm-switch.patch && echo "patch applies cleanly"
```

Expected: 打印 `patch applies cleanly`。

> 若 `git checkout` 把 `codex-rs/Cargo.toml` 的 members 脚手架行也撤了(因 Cargo.toml 不在清单、但被 checkout 路径覆盖),按 CLAUDE.md 重建:在 members 末尾加回 `"llm-switch",`(软链仍在,因 .gitignore 留存)。

- [ ] **Step 12: 端到端验证 — 打 patch 后构建并跑 crate 测试**

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git apply patches/llm-switch.patch
cd codex-rs && cargo build -p codex-core -p codex-memories-write 2>&1 | tail -10
cargo test -p codez-llm-switch 2>&1 | tail -15
```

Expected: 构建通过;crate 全部测试 PASS。

- [ ] **Step 13: 提交 patch(仅 patch 文件;codex-rs 工作树改动不提交)**

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git checkout -- codex-rs/core codex-rs/memories   # 再次还原,确保不提交 codex-rs 改动
git add patches/llm-switch.patch
git commit -m "feat(llm-switch): patch 接入 purpose 路由(route 3 参 + source override + WS 绕过 + memory 调用点)"
```

> 最终状态:`patches/llm-switch.patch` 已更新并提交;`codex-rs/` 子树保持未改(干净);dev 脚手架(软链 + Cargo.toml members 行 + Cargo.lock)保持 uncommitted dirty,符合 CLAUDE.md 纪律。
