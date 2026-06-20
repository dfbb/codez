# zmod/llm-switch — 设计文档

日期:2026-06-20
状态:已批准设计,待写实现计划

## 1. 目标与背景

让 codex 能接入 Anthropic、DeepSeek 等非 OpenAI 模型。codex 对 `base_url` **恒只说 Responses 协议**(`WireApi` 枚举当前仅 `Responses`,`Chat` 已被永久移除),所以接入这些上游需要在 codex 与真上游之间做协议翻译。

本项目 = codez 的第一个 zmod,crate 名 `codez-llm-switch`,目录 `zmod/llm-switch`,对应补丁 `patches/llm-switch.patch`(遵循 codez 的 zmod ↔ patch 命名约定)。

### 1.1 关键决策(已确认)

| 决策点 | 选择 |
|---|---|
| 交付目标 | 端到端可用:codex 配好后真能跑通 anthropic/deepseek |
| 集成形态 | **直接接管 codex 的 LLM API 层(进程内),不起独立代理进程** |
| 连接层引擎 | **薄型化连接器**,操作 codex 原生类型,翻译规则参照 `../3rd/proxy/llm-rosetta` |
| v1 协议覆盖 | Anthropic Messages、Chat Completions(deepseek/OpenAI 兼容)、Responses 直通 |
| 未来扩展 | LLM query 压缩(类 rtk/headroom),作为变换插件挂在管线 ① 层 |
| 范围外(v1) | Google GenAI、WebSocket 传输、模型主动 MCP 回取 |

### 1.2 与 rust-llm-proxy / llm-rosetta 的关系

`../3rd/proxy/rust-llm-proxy` 是 llm-rosetta 的 Rust 移植,但只是**纯转换库**(且只实现了 OpenAiChat 转换器,无网络层)。本设计**不整库移植其通用 IR-hub**:因为压缩在管线 ① 层用 codex 原生类型完成、协议无关,连接层只需 `Responses→目标`(出站)+ `目标SSE→Responses`(入站)单向翻译,通用 N×N hub 用不上。薄连接器代码更少、最贴 codex 类型、与上游 rebase 最友好;翻译正确性靠对照 `../3rd/proxy/llm-rosetta` 的 converter + 黄金测试保证。

## 2. 架构与集成点

进程内两层管线,挂在 codex client 的 HTTP 发送边界(`core/src/client.rs` 的 `stream_responses_api`)。

```
core/src/client.rs  stream_responses_api()  —— loop 内,构造好 request 之后
  │ 已组装好的 ResponsesApiRequest (codex 原生)
  ▼
let stream_result: Result<codex_api::ResponseStream, ApiError> =
  match codez_llm_switch::route(&state.model_provider_id) {
    None      => client.stream_request(request, options).await,        // 原生路径
    Some(rt)  => codez_llm_switch::run(rt, request, &api_provider, &api_auth, transport, options).await,
  };
  ▼
// ↓ 复用 stream_responses_api 既有的同一段下游处理,不另起早返回:
match stream_result {
  Ok(stream)            => map_response_stream(stream, telemetry, trace_attempt),  // LastResponse / cancellation / telemetry
  Err(401 Unauthorized) => handle_unauthorized(...) 后 continue,                    // 原生 recovery 不变
  Err(err)              => map_api_error(err) + trace_attempt.record_failed(...),  // 原生失败记录不变
}
```

`codez_llm_switch::run` 内部:① `TransformPlugin[]` 变换(作用于 codex 原生 `ResponsesApiRequest`,v1 直通,将来挂 compressor)→ ② `Connector` 出口翻译 + HTTP/SSE → 起一个 task 读上游 SSE、翻译成 `ResponseEvent`、塞进 channel,**返回 `codex_api::ResponseStream`**。

**分层依据**:压缩关心的是"模型实际看到的内容"——codex 已组装好的 `ResponsesApiRequest`(各 `ResponseItem` 的 `content` / `FunctionCallOutput.output`),与上游是 anthropic 还是 deepseek 无关,应在 Responses 语义空间里只压一次(管线 ①)。协议翻译是出口的事(管线 ②)。两件事分两层。

### 2.1 路由键:`model_provider_id`(修正:不能用 name)

`ModelProviderInfo` 只有 `name`(显示名),**没有** `[model_providers.<id>]` 的 key;`ModelClient::new` 也只收 `ModelProviderInfo`、不收 id。配置 key 在 `Config.model_provider_id`(`core/src/config/mod.rs:631`)。

修正:patch 给 `ModelClient::new` 增一个 `model_provider_id: String` 形参(由调用处从 `Config.model_provider_id` 传入),存进 `ModelClientState.model_provider_id`,路由即用它。**不得用 `name` 或 `base_url` 兜**(name 是自由显示名、base_url 在接管语义下不必等于真上游,二者都不稳定/不唯一)。

### 2.2 流类型:返回 `codex_api::ResponseStream`,由 core 映射(修正:不构造 core 私有流)

`core/src/client_common.rs:103` 的 `ResponseStream` 字段是 `pub(crate)`(`rx_event` / `consumer_dropped`),zmod **无法**构造。

修正:`codez_llm_switch::run` 返回 `Result<codex_api::ResponseStream, ApiError>`——与 `ApiResponsesClient::stream_request` **完全同型**。core 侧继续用既有的 `map_response_stream(stream, …)`(`client.rs:1758`)把它包成 core `ResponseStream`,白拿 `LastResponse` 追踪、`consumer_dropped` cancellation、stream telemetry。`map_response_events<S>`(`client.rs:1779`)本就接受任意 `Stream<Item = Result<ResponseEvent, ApiError>>`,所以即便将来连接器想返回别的流也只需满足这个 bound。

### 2.3 集成不是早返回,而是接进既有 `stream_result` 流程(修正)

`stream_responses_api` 的 loop 含 unauthorized recovery、`map_api_error`、`inference_trace_attempt.record_failed`、`map_response_stream`。直接 `return` 会把这些全绕过。

修正:把"原生 `client.stream_request(...)`"与"`llm_switch::run(...)`"做成产出**同一 `Result<codex_api::ResponseStream, ApiError>`** 的二选一,二者结果落进 **同一个** `match stream_result { … }`。连接器把自身传输/翻译错误映射成 `ApiError`(`ApiError::Transport(TransportError::Http{status,…})` 等),既有 match 臂即可正确处理。注:OpenAI 专属的 401 recovery 对 anthropic/deepseek 不会触发(它们的错误走通用失败臂),这是可接受的——recovery 逻辑保留、只是不命中。

**连接器直接吃 codex 原生类型**:`codez-llm-switch` 依赖 `codex-api`(`ResponsesApiRequest`/`ResponseEvent`/`ResponseStream`)与 `codex-protocol`(`ResponseItem`)。这样天然"跟 codex 最新三方支持兼容"——codex 改了 Responses 类型,编译期即可感知。

## 3. crate 模块布局

```
zmod/llm-switch/
  Cargo.toml                 # name = codez-llm-switch;依赖 codex-api / codex-protocol / reqwest / tokio / serde
  src/
    lib.rs                   # run():管线入口;enabled()/route() 路由判定
    config.rs                # 读 ~/.codex/config-zmod.toml 的 [llm-switch] 段
    pipeline.rs              # TransformPlugin trait + 有序执行(v1 仅注册点)
    transform/
      mod.rs                 # 将来 compressor 落这里;v1 空
    connector/
      mod.rs                 # Connector trait + 工厂(按路由选)
      responses.rs           # 直通:委托 codex 原生 client + SSE 透传
      chat.rs                # Responses ⇄ Chat Completions(deepseek / OpenAI 兼容)
      anthropic.rs           # Responses ⇄ Anthropic Messages
    sse.rs                   # 上游 SSE 读取 + 逐事件喂给连接器的流式翻译
    http.rs                  # 出口 HTTP 客户端 + 鉴权头整形(Bearer / x-api-key)
  tests/
    testkey.toml             # 实跑用真 key(gitignore,不提交)
    fixtures/                # 取自 llm-rosetta 的样本 JSON
    chat_roundtrip.rs
    anthropic_roundtrip.rs

patches/llm-switch.patch     # 见 §6
```

## 4. 连接器翻译细节

公共契约:

```rust
trait Connector {
    // 返回与 ApiResponsesClient::stream_request 同型的流,交给 core 的 map_response_stream 包装
    async fn run(&self, req: ResponsesApiRequest, ctx: &EgressCtx)
        -> Result<codex_api::ResponseStream, ApiError>;
}
```

`EgressCtx` 带 base_url、鉴权、reqwest transport、目标 model、config-zmod 覆盖项。连接器内部起一个 task 读上游 SSE → 翻译 → `tx.send(Ok(ResponseEvent))`,失败时 `tx.send(Err(ApiError…))`。

三者的字段映射逐条对照 `../3rd/proxy/llm-rosetta` 对应 converter(`tests/converters/anthropic`、`openai_chat`)作为正确性基准。

### 4.0 `ResponseItem` 变体处置策略(修正:覆盖全集,杜绝静默丢弃)

`ResponseItem` 实际有 16 个变体(`protocol/src/models.rs:919`)。连接器对每个变体必须有**明确**处置:`翻译`(译成目标协议)/ `出站丢弃`(不发上游,但**不动** codex 本地历史——连接器只构造请求副本,从不改 codex 存的 item)/ `硬失败`(返回 `ApiError`,绝不静默吞掉,否则破坏工具链/上下文)。

| ResponseItem 变体 | chat | anthropic | 说明 |
|---|---|---|---|
| `Message` | 翻译 | 翻译 | 文本/多模态 content;图片输入见 §7 字段分级 |
| `AgentMessage` | 翻译为 assistant 文本 | 同 | codex 专有的助手消息,降级成普通 assistant message |
| `FunctionCall` | 翻译(`tool_calls`) | 翻译(`tool_use`,arguments 字符串→对象) | 工具调用 |
| `FunctionCallOutput` | 翻译(`role:"tool"`) | 翻译(`tool_result`) | 工具结果 |
| `CustomToolCall` / `CustomToolCallOutput` | 翻译为 function tool_call / 结果 | 翻译为 tool_use / tool_result | 自定义工具,按普通函数工具表达 |
| `LocalShellCall` | 翻译为 function tool_call | 同 | codex 本地 shell 工具,按函数工具表达;无法表达则**硬失败** |
| `ToolSearchCall` / `ToolSearchOutput` | 翻译为 function tool_call / 结果 | 同 | 同上 |
| `Reasoning` | 出站丢弃 | 出站丢弃 | 见 §4.4(encrypted_content 不可发往非 Responses 上游) |
| `WebSearchCall` | **硬失败** | **硬失败** | provider 侧内置工具,目标无等价物;v1 不支持,硬失败而非静默丢(丢了会破坏历史里的工具配对) |
| `ImageGenerationCall` | **硬失败** | **硬失败** | 同上,v1 不支持 |
| `Compaction` / `CompactionTrigger` / `ContextCompaction` | 出站丢弃 | 出站丢弃 | codex 内部上下文压缩记账项,非上游可懂内容。**实现期须逐一核实**确实不携带模型可见正文,若携带则改翻译 |
| `Other` | **硬失败** | **硬失败** | 未知变体(`#[serde(other)]`),无法安全翻译 |

> 原则:**凡承载工具调用/结果或模型可见正文的未知/不支持变体,一律硬失败**,绝不"warning 丢弃";仅 codex 纯内部记账项可出站丢弃,且实现期须核实。

### 4.1 responses(直通)

不翻译,直接委托 codex 原生 `ApiResponsesClient`。存在意义:让管线 ① 变换层(将来的压缩)也能作用于原生 Responses 上游。零协议风险。

### 4.0a endpoint 路径拼接规则(修正:消除版本段歧义)

`egress_url = base_url.trim_end_matches('/') + path`,其中 `path` **由连接器提供默认值、可被 config-zmod 的 `path` 覆盖**:

| connector | 默认 `path` |
|---|---|
| chat | `/chat/completions` |
| anthropic | `/v1/messages` |
| responses | `/responses`(直通,实际走原生 client) |

约定:**`base_url` 只写到 API 根、不含上面这段 `path`**;版本前缀(如 deepseek 的 `/v1`)算 `base_url` 的一部分。于是:

- deepseek `base_url = https://api.deepseek.com/v1` + 默认 `/chat/completions` → `https://api.deepseek.com/v1/chat/completions` ✓
- anthropic 官方 `base_url = https://api.anthropic.com` + 默认 `/v1/messages` → `https://api.anthropic.com/v1/messages` ✓
- 网关型(testkey)`base_url = https://node-hk.sssaiapi.com/api` + 默认 `/v1/messages` → `https://node-hk.sssaiapi.com/api/v1/messages`(若网关路径不同,用 `path` 覆盖)✓

### 4.2 chat(deepseek / OpenAI 兼容)

出口 `POST {egress_url}`(默认 `{base_url}/chat/completions`,见 §4.0a),Bearer 鉴权。

- **请求**:`instructions` → `messages[0]` system;`input[Message]` → `messages`;`input[FunctionCall]` → assistant `tool_calls`(`arguments` 已是 JSON 字符串,直传);`input[FunctionCallOutput]` → `role:"tool"`;`tools` → `tools[{type:"function"}]`;`reasoning`/`store`/`include`/`prompt_cache_key` 等按 §7 分级处置;加 `stream:true` + `stream_options.include_usage`。
- **响应 SSE → ResponseEvent**:`delta.content` → `output_text.delta`;`delta.tool_calls[].function.arguments` 按 index 聚合 → `OutputItemDone(FunctionCall)`;`finish_reason` + 末 chunk usage → `Completed { token_usage }`;顶层 error → 失败。`data:[DONE]` 收尾。

### 4.3 anthropic

出口 `POST {egress_url}`(默认 `{base_url}/v1/messages`,见 §4.0a),头 `x-api-key` + `anthropic-version`(鉴权整形在 `http.rs`)。

- **请求**:`instructions` → 顶层 `system`;`input[Message]` → `messages`(role 仅 user/assistant);`input[FunctionCall]` → assistant `content[{type:"tool_use", id, name, input}]`(**`arguments` 字符串 → parse 成对象**);`input[FunctionCallOutput]` → user `content[{type:"tool_result", tool_use_id, content}]`;`tools` → `tools[{name, description, input_schema}]`;**`max_tokens` 必填** —— 缺省由 config-zmod 的 `default_max_tokens` 填充(兜底常量 4096)。
- **响应 SSE → ResponseEvent**:`content_block_delta`/`text_delta` → `output_text.delta`;`tool_use` block + `input_json_delta` 聚合(**对象 → stringify 回 `arguments` 字符串**) → `OutputItemDone(FunctionCall)`;`message_delta`(usage) + `message_stop` → `Completed`;`error` → 失败。
- **硬约束**:tool_use ↔ tool_result 配对完整;Reasoning 出站处置见 §4.4。

### 4.4 Reasoning / encrypted_content 的出站处置(修正:"透传"无落点)

OpenAI Responses 的 `Reasoning`(含 `encrypted_content`)是 OpenAI 专有的加密推理项,**非 Responses 上游无法理解**,所以"透传不动"在 chat/anthropic 请求里没有协议落点。明确处置:

- **chat / anthropic 出站**:`Reasoning` item **不写入**发往上游的请求体(出站丢弃)。连接器只构造请求**副本**,codex 的本地会话历史(原始 `ResponseItem` 列表)**不受影响**,后续轮次仍完整保留 reasoning。
- **responses 直通**:`encrypted_content` 原样透传不变(走原生 client,本就如此)。
- 这样既不向不懂它的上游发无效字段,也不破坏 codex 本地对 reasoning 的保真。

## 5. 配置与路由(config-zmod)

路由键 = codex 的 `model_provider_id`(§2.1)。codex 侧 `config.toml` 照常配 provider,llm-switch 在发送前按同名 id 接管并改写协议/路径/鉴权。两个 provider 各需**两处**配置:

### 5.1 codex `~/.codex/config.toml`(两个 provider 都要)

```toml
# —— deepseek ——
[model_providers.deepseek]
name     = "DeepSeek"
base_url = "https://api.deepseek.com/v1"   # 接管语义下此值不参与路由,可留真上游
wire_api = "responses"                     # codex 对内只会说 Responses
env_key  = "DEEPSEEK_API_KEY"
supports_websockets = false

# —— claude ——
[model_providers.claude]
name     = "Claude"
base_url = "https://api.anthropic.com"
wire_api = "responses"
env_key  = "ANTHROPIC_API_KEY"
supports_websockets = false
```

切换时设 `model_provider = "deepseek"`(或 `"claude"`)。

### 5.2 codez `~/.codex/config-zmod.toml`(路由 + 出口翻译)

```toml
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]          # 表名 = codex 的 model_provider_id
connector = "chat"
base_url  = "https://api.deepseek.com/v1"  # 可选;缺省用 codex provider 的 base_url
auth      = "bearer"
# path    = "/chat/completions"            # 可选,覆盖默认出口路径(§4.0a)

[llm-switch.providers.claude]
connector         = "anthropic"
base_url          = "https://api.anthropic.com"
auth              = "x-api-key"
anthropic_version = "2023-06-01"
default_max_tokens = 8192
```

- 命中表名则用对应连接器,未命中 → 原生 Responses 路径。
- 文件或 `[llm-switch]` 缺失 → 整体关闭(fail-safe,符合 codez 的 zmod 约定)。
- **`model`(可选)**:覆盖/映射发往真上游的模型名。运行时接管 codex 时缺省用 codex 请求里的 `model`;独立运行 / 实跑测试时由此字段指定(如 testkey 里的 `deepseek-v4-pro`、`claude-opus-4-8`)。

### 5.3 密钥来源与优先级(修正:正式配置不放明文 key)

正式 `~/.codex/config-zmod.toml` **不支持** `auth_key` 明文字段(避免敏感信息落盘,符合 codez 密钥不入库原则)。密钥按以下优先级取:

1. **codex provider auth**(`api_auth`,来自 `config.toml` 的 `env_key` 指向的环境变量)—— 运行时接管的主路径。
2. **环境变量**(连接器按 provider 约定读,如 `ANTHROPIC_API_KEY` / `DEEPSEEK_API_KEY`)。
3. **`auth_key` 内联** —— **仅允许出现在 gitignored 的 `zmod/llm-switch/tests/testkey.toml`**,供离线/实跑测试与独立运行;正式 config 出现 `auth_key` 应告警(或拒绝)。

`http.rs` 拿到密钥后按 `auth` 整形:`bearer` → `Authorization: Bearer <key>`;`x-api-key` → `x-api-key: <key>` + `anthropic-version`。

## 6. patch(对 codex-rs 的全部改动)

修正:这**不是**"3 处清单式追加 + 早返回",而是一次真实的接入(因为要按 id 路由 + 接进既有 stream_result 流程)。`patches/llm-switch.patch` 触点:

### 6.1 构建集成(修正:不当 workspace member)

`zmod/llm-switch` 在 codex-rs workspace 根(`codex-rs/`)**之外**,且要反向依赖 `codex-api`/`codex-protocol`(它们是 codex-rs 的 member)。把 `../zmod/llm-switch` 塞进 codex-rs 的 `[workspace] members` 不合适(跨根、且要同步 `[workspace.dependencies]`)。改为:

- `zmod/llm-switch/Cargo.toml` 是**独立包**(**不声明自己的 `[workspace]`**,否则被当 path 依赖编译时会触发"nested workspace"报错),用显式 path 依赖反指:`codex-api = { path = "../../codex-rs/codex-api" }`、`codex-protocol = { path = "../../codex-rs/protocol" }`(版本随 codex-rs 走,不用 `workspace = true`)。
- patch 在 **`codex-rs/core/Cargo.toml`** 加一条 path 依赖:`codez-llm-switch = { path = "../../zmod/llm-switch" }`。它作为 core 的普通 path 依赖被一起编译,**不进** workspace member 列表;无依赖环(llm-switch 只依赖 api/protocol,不依赖 core)。
- 独立 `cargo test`:在 `zmod/llm-switch/` 直接跑,path 依赖会定位到 codex-rs 的 crate,正常解析。

> 注:这与现有 `CLAUDE.md`"patch 把 codez-<feature> 加入 workspace members"的约定不符。需要时一并更新 CLAUDE.md:**当 zmod crate 反向依赖 codex-rs crate 时,用 core 的外部 path 依赖,而非 workspace member**。

### 6.2 路由键透传(修正:ModelClient 需要 id)

- **`core/src/client.rs`** `ModelClient::new` 增形参 `model_provider_id: String`,存进 `ModelClientState.model_provider_id`;其唯一调用处从 `Config.model_provider_id`(`config/mod.rs:631`)传入。

### 6.3 发送边界接入(修正:接进 stream_result,非早返回)

- **`core/src/client.rs`** `stream_responses_api`:把 `let stream_result = client.stream_request(request, options).await;` 改成按 `codez_llm_switch::route(&self.client.state.model_provider_id)` 二选一——命中则 `codez_llm_switch::run(rt, request, &api_provider, &api_auth, transport, options).await`(返回 `Result<codex_api::ResponseStream, ApiError>`),否则原生。**下游 `match stream_result { … }`(unauthorized recovery / `map_api_error` / `record_failed` / `map_response_stream`)完全不动**,二者共用。

翻译/网络逻辑全在 `codez-llm-switch` crate;core 触点是 Cargo 依赖 + `ModelClient::new` 形参 + `stream_responses_api` 里一处赋值改写,均不动既有错误/遥测/流映射逻辑,同步 codex-rs 时冲突面小但**非零**(`ModelClient::new` 签名属较稳定的接口)。

## 7. 错误处理与字段分级

- 连接器翻译/网络失败 → 映射成 codex 既有 `ApiError`(`tx.send(Err(..))` 或 `run` 直接返回 `Err`),codex 按原生错误流程处理(重试/报错)。

### 7.1 请求字段分级(修正:不一律 warning 丢弃)

按"丢弃后是否改变模型可见语义"分三级,各连接器据此处置(`ResponseItem` 变体见 §4.0):

| 级别 | 字段(示例) | 处置 |
|---|---|---|
| **可安全忽略**(纯传输/缓存元数据,不影响模型输出) | `store`、`include`、`prompt_cache_key`、`service_tier`、`client_metadata` | 静默丢弃 |
| **降级转换**(目标有近似表达,尽力映射,丢真实语义时记 warning) | `reasoning`(anthropic→`thinking` / chat→`reasoning_effort`,无则丢+warn)、`parallel_tool_calls`(chat 透传 / anthropic 无直接对应→丢+warn)、`tool_choice`(映射到目标 tool_choice;无法表达的强制档→warn)、`text.format` 结构化输出 schema(chat→`response_format` json_schema;anthropic 无→降级为指令或 warn) | 尽力映射 + 必要时 warn |
| **必须硬失败**(静默丢会破坏模型可见语义/工具链,且无法降级) | 图片/多模态输入而目标模型无视觉能力;承载工具调用/结果的不支持 `ResponseItem` 变体(§4.0 标"硬失败"者);`tool_choice` 指定了目标完全无法表达的必调工具 | 返回 `ApiError`,不发请求 |

> 实现期:每个连接器对上表逐项落实,黄金测试覆盖"降级"与"硬失败"两类断言。

### 7.2 鉴权整形

`http.rs`:`auth = "bearer"` → `Authorization: Bearer <key>`;`auth = "x-api-key"` → `x-api-key: <key>` + `anthropic-version`。密钥来源与优先级见 §5.3。

## 8. 测试与成功判据

- **离线黄金测试**(主力,不需 key):从 `../3rd/proxy/llm-rosetta` `tests/` 抽 fixture,断言 `ResponsesApiRequest → 目标请求 JSON` 语义等价(忽略字段序、可选省略策略);静态 SSE chunk 序列驱动连接器,断言产出的 `ResponseEvent` 序列正确(含 tool_call 聚合、usage、收尾)。
- **集成/实跑测试**(门控):读 `zmod/llm-switch/tests/testkey.toml`(schema 即 `[llm-switch.providers.<id>]` + `auth_key` + `model`),真实打 deepseek(chat)/claude(anthropic)端点,验证端到端连通。用 `#[ignore]` 或环境变量门控;`testkey.toml` 缺失时自动跳过,CI 无 key 也能全绿,本地 `cargo test -- --ignored` 跑真链路。
- **不变量**:chat/anthropic 出站丢弃 `Reasoning` 但 codex 本地历史不变(§4.4);responses 直通时 `encrypted_content` 不变;tool_call ↔ output 的 `call_id` 关联正确;§4.0 标"硬失败"的变体确实返回 `ApiError` 而非静默丢。
- **独立可测**:crate 能脱离 codex 独立 `cargo test`(path 依赖 codex-api / codex-protocol)。

成功判据:

1. codex 按 §5.1 配好 `[model_providers.deepseek]` **和** `[model_providers.claude]` + §5.2 config-zmod 路由后,**两者都**能跑通对话(含工具调用)。
2. 三个连接器的离线黄金测试全绿(语义等价),且覆盖 §7.1 降级/硬失败两类断言。
3. 上述硬不变量满足。
4. core 触点仅 §6 所列三类(Cargo 依赖 + `ModelClient::new` 形参 + `stream_responses_api` 一处赋值改写),不改既有错误/遥测/流映射逻辑。

## 9. 安全注记

`zmod/llm-switch/tests/testkey.toml` 含真实 API key,已被 `.gitignore`(第 30 行 `testkey.toml` 全局匹配)排除,不得提交到 GitHub。新增任何含密钥的测试夹具,须同样确保被 gitignore 覆盖。
