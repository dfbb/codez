# Task 09 — patch 接入 codex-rs core

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints。**关键纪律:绝不直接改 `codex-rs/` 源码作为最终交付——所有对 codex-rs 的改动必须落进 `patches/llm-switch.patch`**(否则下次 `04-sync-codex-rs.zsh` 必冲突)。

**Goal:** 生成 `patches/llm-switch.patch`,把 `codez-llm-switch` 接进 codex-rs:① `core/Cargo.toml` 加 path 依赖;② `ModelClient::new` 增 `model_provider_id: String` 形参 + `ModelClientState` 增字段,调用处从 `Config.model_provider_id` 传入;③ `stream_responses_api` 把"构造 `ApiResponsesClient` + `stream_request`"改成按 `route()` 二选一,产出同一 `Result<codex_api::ResponseStream, ApiError>` 落进既有 `match stream_result`。验证:打 patch 后 `cargo build` + 既有测试通过。

**覆盖 spec:** §6(patch 全量)、§2.1/§2.3/§6.2/§6.3。

**Files:**
- Create: `patches/llm-switch.patch`
- 临时改(仅为生成 patch,改完导出再还原工作区):`codex-rs/core/Cargo.toml`、`codex-rs/core/src/client.rs`

**Interfaces:**
- Consumes(Task 08):`codez_llm_switch::route(&str) -> Option<Route>`、`codez_llm_switch::run(rt, request, api_auth) -> Result<codex_api::ResponseStream, ApiError>`、`codez_llm_switch::Route`。

---

- [ ] **Step 0: 读取真实接入点(逐字,务必先做)**

```bash
sed -n '160,230p' codex-rs/core/src/client.rs       # ModelClientState + ModelClient 结构
sed -n '370,412p' codex-rs/core/src/client.rs        # ModelClient::new 签名
sed -n '1270,1360p' codex-rs/core/src/client.rs      # stream_responses_api:构造 client + stream_request + match stream_result
grep -rn "ModelClient::new(" codex-rs/core/src        # 唯一调用处
grep -n "model_provider_id" codex-rs/core/src/config/mod.rs   # Config 字段(约 632 行)
```
记录:(a)`ModelClient::new` 全部实参顺序;(b)`stream_responses_api` 里如何拿到 `transport` / `client_setup.api_provider` / `client_setup.api_auth` / `request` / `options` / `request_telemetry` / `sse_telemetry`;(c)`match stream_result` 各臂逐字;(d)`ModelClient::new` 调用处能否拿到 `config.model_provider_id`。**后续步骤的代码以这里读到的真实文本为准**,下面给的是模板。

- [ ] **Step 1: `core/Cargo.toml` 加 path 依赖**

在 `codex-rs/core/Cargo.toml` 的 `[dependencies]` 加(情况 B,不进 workspace members):

```toml
codez-llm-switch = { path = "../../zmod/llm-switch" }
```

- [ ] **Step 2: `ModelClientState` 增字段 + `ModelClient::new` 增形参**

`ModelClientState` 结构体加:
```rust
    model_provider_id: String,
```
`ModelClient::new` 形参表末尾(或语义合适处)加 `model_provider_id: String`,并在构造 `ModelClientState { ... }` 处填入。其唯一调用处传 `config.model_provider_id.clone()`。

> 若 `ModelClient::new` 调用处无 `config` 在手,改为从已有的 `provider_info`/上层 `Config` 链路取 `model_provider_id`——以 Step 0 读到的真实上下文为准。**不得**用 `provider_info.name` 兜(§2.1)。

- [ ] **Step 3: `stream_responses_api` 改为路由二选一**

把 Step 0 读到的这段(模板):
```rust
let client = ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
    .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
let stream_result = client.stream_request(request, options).await;
```
改成:
```rust
let stream_result = match codez_llm_switch::route(&self.state.model_provider_id) {
    None => {
        // 原生路径:遥测链完整保留
        let client = ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
        client.stream_request(request, options).await
    }
    Some(rt) => {
        // 接管路径:连接器自带 HTTP/SSE;api_auth 仅作 bearer 退路。
        // transport / api_provider / request_telemetry / sse_telemetry 在本臂未用,作用域结束自动 drop。
        codez_llm_switch::run(rt, request, client_setup.api_auth).await
    }
};
```

> 注意:
> - `self.state.model_provider_id` 的真实访问路径以 Step 0 为准(可能是 `self.state.model_provider_id`,字段在 `Arc<ModelClientState>` 内)。
> - 两臂都产出 `Result<codex_api::ResponseStream, ApiError>`,**下游 `match stream_result { Ok(stream) => map_response_stream(...), Err(401) => handle_unauthorized..., Err(err) => map_api_error... }` 一字不动**。
> - `request` / `options` / `api_auth` 是 owned:接管臂 move `request` + `api_auth`,原生臂 move `request` + `options` + `api_provider` + `api_auth` + `transport`。二者互斥,move 合法。若编译器报 `api_auth` 在两臂都被 move 而它本身不是每臂独占——用 `client_setup.api_auth.clone()`(`SharedAuthProvider = Arc`,clone 廉价)在接管臂,保留原生臂 move;以编译结果为准。

- [ ] **Step 4: 打 patch 前先编译验证(在临时改动状态下)**

```bash
cd codex-rs && cargo build -p codex-core
```
Expected: 编译通过。若 `codez-llm-switch` 的 path 依赖把 codex-api/protocol 又编一遍导致版本冲突,核对 Task 01 的 path 是否精确指向 `codex-rs/codex-api`、`codex-rs/protocol`(同一份源,Cargo 会识别为同一 crate)。

- [ ] **Step 5: 导出 patch 并还原工作区**

```bash
cd codex-rs
git diff -- core/Cargo.toml core/src/client.rs > ../patches/llm-switch.patch
git checkout -- core/Cargo.toml core/src/client.rs    # 还原 codex-rs 源码,改动只留在 patch 里
```

> 关键:`codex-rs/` 工作区必须还原干净——交付物是 `patches/llm-switch.patch`,不是改过的 codex-rs 源码。`04-sync-codex-rs.zsh` 跑前要求 codex-rs 工作区干净。

- [ ] **Step 6: 验证 patch 可干净应用**

```bash
cd codex-rs && git apply --check ../patches/llm-switch.patch && echo "PATCH OK"
```
Expected: 输出 `PATCH OK`(`--check` 不实改)。

- [ ] **Step 7: 端到端编译验证(应用 patch 后整 workspace 编译 + 既有测试)**

```bash
cd codex-rs
git apply ../patches/llm-switch.patch
cargo build                              # 整 workspace + zmod 一起编译
cargo nextest run -p codex-core          # 既有 core 测试不回归
git checkout -- core/Cargo.toml core/src/client.rs   # 验证完还原
```
Expected: build 成功、core 测试通过。若有失败,修 `zmod/llm-switch` 或 patch,重导出。

- [ ] **Step 8: 提交**

```bash
git add patches/llm-switch.patch
git commit -m "feat(llm-switch): patch wiring codez-llm-switch into codex-rs core (route + ModelClient id + stream_responses_api)"
```

> `CLAUDE.md` 情况 A/B 两分约定已于 `7a12f5291` 落地(spec §6.4),本任务无需再改 CLAUDE.md;若 Step 0 发现约定与实际不符,在本任务一并更新 CLAUDE.md 并登记。
