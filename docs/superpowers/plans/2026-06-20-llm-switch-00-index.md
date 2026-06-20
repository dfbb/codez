# llm-switch 实现计划 — 总索引

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development(推荐)或 superpowers:executing-plans 逐任务执行。每个任务是一个独立 plan 文件,步骤用 `- [ ]` 复选框追踪。

**Goal:** 实现 codez 第一个 zmod `codez-llm-switch`,在 codex 进程内接管 LLM API 层,把 codex 恒发的 Responses 协议翻译成 Anthropic Messages / Chat Completions 发往真上游,并把上游 SSE 翻回 `ResponseEvent`,使 codex 能接入 deepseek/claude 等非 OpenAI 模型。

**Architecture:** 进程内两层管线挂在 `core/src/client.rs` 的 `stream_responses_api`:① `TransformPlugin[]` 变换(v1 直通,将来挂压缩)→ ② `Connector` 出口翻译 + HTTP/SSE。连接器吃 codex 原生类型(`codex-api` / `codex-protocol`),返回与 `ApiResponsesClient::stream_request` 同型的 `codex_api::ResponseStream`,交给 core 既有 `map_response_stream` 包装。对 codex-rs 的侵入用 `patches/llm-switch.patch` 表达,不直接改源码。

**Tech Stack:** Rust 1.95.0、tokio、reqwest(SSE)、serde / serde_json、`codex-api` / `codex-protocol`(path 依赖)。

**设计依据:** `docs/superpowers/specs/2026-06-20-llm-switch-design.md`(已定稿,commit `eee639773`)。本计划每个任务标注其覆盖的 spec 小节。

## Global Constraints

逐条照抄自 spec,每个任务的要求隐含包含本节:

- **crate 命名**:包名 `codez-llm-switch`,目录 `zmod/llm-switch/`,lib target `codez_llm_switch`(spec §1)。
- **不声明自己的 `[workspace]`**:否则被当 path 依赖编译时触发 nested-workspace 报错(spec §6.1)。
- **反向 path 依赖**:`codex-api = { path = "../../codex-rs/codex-api" }`、`codex-protocol = { path = "../../codex-rs/protocol" }`,**不**用 `workspace = true`(spec §6.1)。
- **不进 workspace members**:由 patch 在 `codex-rs/core/Cargo.toml` 加 `codez-llm-switch = { path = "../../zmod/llm-switch" }`(spec §6.1)。
- **路由键 = `model_provider_id`**:不得用 `name` 或 `base_url`(spec §2.1)。
- **返回 `codex_api::ResponseStream`**:其字段 `rx_event` / `upstream_request_id` 是 `pub`,用 `mpsc::channel` 构造;与 `ApiResponsesClient::stream_request` 同型(spec §2.2,已由源码核实)。
- **错误/spawn 边界**:`run` 同步完成 HTTP+状态码校验+SSE 建立;非 2xx 直接 `return Err(ApiError)`;只有 2xx 才 `spawn` 读取任务(spec §4.7)。
- **第三方 401/403 映射成普通 `ApiError`**(非 `TransportError::Http{status==UNAUTHORIZED}`),避免触发 OpenAI 专属 recovery(spec §4.7)。
- **v1 工具能力仅标准 `function`**:一切 provider/native/custom/freeform 工具项及未知变体 → 硬失败返回 `ApiError`,绝不静默丢、绝不强译成函数(spec §4.0 / §4.0b)。
- **图片 v1 一律硬失败**:无能力判定字段,不猜测(spec §4.6 / §4.9)。
- **加密内容**(`EncryptedContent` / `Compaction` / `ContextCompaction`)→ 硬失败;`Reasoning` 历史项 → 出站丢弃;`CompactionTrigger` → 出站丢弃(spec §4.0 / §4.4)。
- **连接器只构造请求副本**,绝不改 codex 本地历史(spec §4.0 / §4.4 / §4.10)。
- **密钥**:连接器自取原始 key(`key_env` / testkey 的 `auth_key`),不依赖 codex `add_auth_headers`(只能产 Bearer);正式 config-zmod 出现 `auth_key` → 解析期直接报配置错误拒绝启动(spec §5.3)。
- **fail-safe**:config 文件或 `[llm-switch]` 缺失 → 整体关闭(spec §5.2)。
- **安全**:`tests/testkey.toml` 含真 key,已被 `.gitignore` 第 30 行覆盖,不得提交(spec §9)。
- **Rust 风格**:非测试代码避免 `unwrap`/`expect`;TUI 颜色规则不涉及本 crate。

## 实现层钉死的真实类型(避免按记忆猜)

- `ResponsesApiRequest`:`model: String`、`instructions: String`、`input: Vec<ResponseItem>`、`tools: Vec<serde_json::Value>`、`tool_choice: String`、`parallel_tool_calls: bool`、`reasoning: Option<Reasoning>`、`store/stream: bool`、`include: Vec<String>`、`service_tier/prompt_cache_key: Option<String>`、`text: Option<TextControls>`、`client_metadata: Option<HashMap<String,String>>`(`codex-api/src/common.rs:182`)。
- `ResponseEvent`:`OutputTextDelta(String)`、`OutputItemDone(ResponseItem)`、`Completed{response_id:String, token_usage:Option<TokenUsage>, end_turn:Option<bool>}`、`ToolCallInputDelta{item_id,call_id:Option<String>,delta}` 等(`codex-api/src/common.rs:73`)。
- `ResponseStream { pub rx_event: mpsc::Receiver<Result<ResponseEvent, ApiError>>, pub upstream_request_id: Option<String> }`(`codex-api/src/common.rs:305`)。
- `ResponseItem`(16 变体)、`ContentItem { InputText{text} | InputImage{image_url,detail} | OutputText{text} }`、`AgentMessageInputContent { InputText{text} | EncryptedContent{encrypted_content} }`、`FunctionCall{ id, name, namespace:Option<String>, arguments:String, call_id:String, .. }`、`FunctionCallOutputPayload{ body:FunctionCallOutputBody, success:Option<bool> }`、`FunctionCallOutputBody { Text(String) | ContentItems(Vec<FunctionCallOutputContentItem>) }`(`protocol/src/models.rs`)。
- `TokenUsage{ input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens, total_tokens: i64 }`(`protocol/src/protocol.rs:2000`)。
- `ApiError`(`codex-api/src/error.rs:8`)、`TransportError::Http{status:StatusCode, url, headers, body}`(`codex-client/src/error.rs:6`)。
- `SharedAuthProvider = Arc<dyn AuthProvider>`,`AuthProvider::add_auth_headers(&self, &mut HeaderMap)`(`codex-api/src/auth.rs`)。
- core 接入点:`ApiResponsesClient::new(transport, api_provider, api_auth).with_telemetry(...).stream_request(request, options)`(`core/src/client.rs:1324`);`map_response_stream(api_stream, session_telemetry, inference_trace_attempt)`(`client.rs:1758`)。

## 开发期构建与测试(架构决策,2026-06-20 定;详见 CLAUDE.md 情况 B「开发期测试」)

`zmod/llm-switch` 反向依赖 codex-api/codex-protocol(其依赖全 `{ workspace = true }`,版本由 codex-rs workspace 钉死)。两条 cargo 硬约束已实测确认:① 它作为「非 member 的 path 依赖」时**不能用 `[dev-dependencies]`、不能跑 `tests/*.rs` 集成测试**;② cargo **拒绝 codex-rs 之外的 member**。而 Task 08 需 `wiremock`(dev-dep)、多个任务用 `tests/*.rs` 黄金测试。

**决策:开发期(Task 01–08)用软链把本 crate 接进 codex-rs workspace 成为真 member**,从而完整支持 dev-deps + 集成测试,且共享 codex-rs 的 `Cargo.lock`/`target`(无漂移、复用已编译 codex-api 树)。落地:

- **软链 + members(dev-only 脚手架)**:
  ```bash
  ln -s ../zmod/llm-switch codex-rs/llm-switch    # 已建;cargo 视其为根下 member
  # codex-rs/Cargo.toml 的 members 末尾加一行: "llm-switch",
  ```
  软链 `codex-rs/llm-switch` 写进根 `.gitignore`(`/codex-rs/llm-switch`);members 那行与构建生成的 `codex-rs/Cargo.lock` 改动保持 uncommitted dirty。**这些都不进 `patches/llm-switch.patch`、不提交进 codex-rs 子树。**
- **测试命令统一**:各任务 brief 里 `cd zmod/llm-switch && cargo test --test X` 一律读作 **`cd codex-rs && cargo test -p codez-llm-switch`**(或 `--test X`)。集成测试与 dev-deps 因 member 身份而完整可用。
- **crate 自身**:`zmod/llm-switch/Cargo.toml` 的 codex-api/codex-protocol 为**激活** path 依赖(不注释);其余版本对齐 workspace;不声明自己的 `[workspace]`;`[dev-dependencies]` 正常声明(member 下可用);不提交自己的 `Cargo.lock`。
- **生产接入不变**:Task 09 patch 仍是 core path 依赖 + client.rs 调用点(情况 B);软链/members 仅为 dev 测试,与 patch 无关。`git reset --hard` 会撤 members 行(软链被 ignore 而留存),按上面两条命令重建。

## 任务依赖图

```
01 crate-skeleton-config ─┬─> 02 http-auth ──────────┐
                          ├─> 03 pipeline-connector ──┼─> 04 chat-request ─> 05 chat-sse ─┐
                          │                           ├─> 06 anthr-request ─> 07 anthr-sse ┼─> 08 run-sse-reader ─> 09 patch-core ─> 10 live-tests
                          └───────────────────────────┘                                   ┘
```

执行顺序建议:01 → 02 → 03 →(04→05 与 06→07 可并行)→ 08 → 09 → 10。

## 任务清单

1. [Task 01 — crate 骨架与配置](2026-06-20-llm-switch-01-crate-skeleton-config.md)
2. [Task 02 — http.rs 出口与鉴权](2026-06-20-llm-switch-02-http-auth.md)
3. [Task 03 — pipeline 与 connector trait/工厂](2026-06-20-llm-switch-03-pipeline-connector-trait.md)
4. [Task 04 — chat 出站请求构造](2026-06-20-llm-switch-04-chat-request.md)
5. [Task 05 — chat SSE→ResponseEvent](2026-06-20-llm-switch-05-chat-sse.md)
6. [Task 06 — anthropic 出站请求构造](2026-06-20-llm-switch-06-anthropic-request.md)
7. [Task 07 — anthropic SSE→ResponseEvent](2026-06-20-llm-switch-07-anthropic-sse.md)
8. [Task 08 — run() 接线与 SSE reader](2026-06-20-llm-switch-08-run-sse-reader.md)
9. [Task 09 — patch 接入 codex-rs core](2026-06-20-llm-switch-09-patch-core.md)
10. [Task 10 — testkey 门控实跑测试](2026-06-20-llm-switch-10-live-tests.md)

## 成功判据(全计划完成后,对照 spec §8)

1. codex 按 §5.1 + §5.2 配好后,deepseek(chat)与 claude(anthropic)**都**能跑通对话(仅启用标准 `function` 工具)。
2. 三连接器(含 responses 走原生分支)离线黄金测试全绿,覆盖 §7.1 降级/硬失败、§4.0/§4.0b 变体硬失败断言。
3. 硬不变量满足:出站丢 `Reasoning` 但本地历史不变;`call_id` 配对正确;标"硬失败"变体确实返回 `ApiError`。
4. core 触点仅 §6 所列(Cargo 依赖 + `ModelClient::new` 形参 + `stream_responses_api` 一处改写);原生路径保留 `.with_telemetry(...)`,接管路径不接 codex-api 请求/SSE 遥测(已记为已知缺口)。
5. `CLAUDE.md` 的 zmod 构建约定已是情况 A/B 两分(已于 `7a12f5291` 落地)。
