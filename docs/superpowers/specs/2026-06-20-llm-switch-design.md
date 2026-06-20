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
    None => {                                                          // 原生路径:遥测链保留
        let client = ApiResponsesClient::new(transport, api_provider, api_auth)
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));   // ★ 不可丢
        client.stream_request(request, options).await
    }
    Some(rt) =>                                                        // 接管路径(owned 入参,见下)
        codez_llm_switch::run(rt, request, api_provider, api_auth, transport, options).await,
  };
// 两臂互斥,api_provider/api_auth/transport move 进各自分支均合法,无需 clone/借用
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

### 2.4 遥测边界(修正:接管路径不接 codex-api 层请求/SSE 遥测)

原生路径在 `ApiResponsesClient::new(...).with_telemetry(Some(request_telemetry), Some(sse_telemetry))` 上挂了**请求级 + SSE 级**遥测(`client.rs:1324`)。这两个 telemetry 是绑定 `ApiResponsesClient`(Responses 端点)的,接管路径用**自己的** HTTP/SSE 客户端,**无法**复用。明确边界:

- **保留**(两路径都生效,在 `stream_responses_api` 外层,与连接器无关):`inference_trace_attempt`(`record_started`/`record_failed`/`record_cancelled`)、`map_response_stream` 的 `LastResponse` / cancellation / stream telemetry。
- **v1 不接入**(接管路径已知缺口,**不谎称"遥测不动"**):codex-api 层的 `request_telemetry` / `sse_telemetry`。连接器可在 crate 内自记最小指标(状态码、首字节延迟),但不复刻 codex-api 的这套 telemetry。
- 原生路径(responses 直通走原生 client、或未命中路由)**完整保留** `.with_telemetry(...)`。

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
      mod.rs                 # Connector trait + 工厂(仅 chat / anthropic);responses 不在此(走原生分支,§4.1)
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

`EgressCtx` 带 base_url、鉴权、reqwest transport、目标 model、config-zmod 覆盖项。

**`run` 的错误/spawn 边界(修正,见 §4.7)**:`run` 必须**同步**完成 HTTP 请求 + 状态码校验 + SSE 响应建立;**非 2xx(建连/鉴权失败)直接 `return Err(ApiError)`**(落到外层 `match stream_result`,走 `map_api_error` + `record_failed`);**只有拿到 2xx SSE 后才 `spawn` 读取任务**,task 内的流中错误才 `tx.send(Err(ApiError…))`(走 `map_response_events` 的流错误处理)。

字段映射的正确性基准见 §8(chat 用 rust-llm-proxy;anthropic 用 llm-rosetta Python converter 或自建 fixture),**不要**笼统按"对应 converter"找。

### 4.0 `ResponseItem` 变体处置策略(修正:覆盖全集,杜绝静默丢弃)

`ResponseItem` 实际有 16 个变体(`protocol/src/models.rs:919`)。连接器对每个变体必须有**明确**处置:`翻译`(译成目标协议)/ `出站丢弃`(不发上游,但**不动** codex 本地历史——连接器只构造请求副本,从不改 codex 存的 item)/ `硬失败`(返回 `ApiError`,绝不静默吞掉,否则破坏工具链/上下文)。

| ResponseItem 变体 | chat | anthropic | 说明 |
|---|---|---|---|
| `Message` | 翻译 | 翻译 | 文本/多模态 content;图片输入见 §7 字段分级 |
| `AgentMessage` | 纯 `InputText` 降级为 assistant 文本;含 `EncryptedContent` → **硬失败** | 同 | `AgentMessageInputContent` 有 `InputText`/`EncryptedContent` 两变体(`protocol/src/models.rs`)。后者非 Responses 上游读不了,硬失败而非静默丢(丢了改变模型可见的助手历史) |
| `FunctionCall` | 翻译(`tool_calls`);**`namespace.is_some()` → 硬失败** | 翻译(`tool_use`,arguments 字符串→对象);**`namespace.is_some()` → 硬失败** | 标准函数工具调用,**v1 唯一支持的工具调用形态**;`FunctionCall.namespace: Option<String>` 是 codex 命名空间工具,目标协议无可逆表达,v1 硬失败(与 §4.0b namespace 工具定义硬失败一致) |
| `FunctionCallOutput` | 见 §4.6(含 `success` 状态) | 见 §4.6 | payload = `body`(文本或 `ContentItems`,含 `InputImage`/`EncryptedContent`)+ `success: Option<bool>`,不能只映射 body |
| `CustomToolCall` / `CustomToolCallOutput` | **硬失败** | **硬失败** | freeform/custom 工具序列化为 `type:"custom"`、无标准 JSON schema 参数,Chat/Anthropic function tool 未必能表达;**v1 不支持,硬失败**(见 §4.0b) |
| `LocalShellCall` | **硬失败** | **硬失败** | provider/native 工具历史项,**不等同**普通 function call;译成函数会让模型继续引用目标 provider 并不存在的工具。v1 硬失败 |
| `ToolSearchCall` / `ToolSearchOutput` | **硬失败** | **硬失败** | 同上,provider/native 工具,v1 硬失败 |
| `WebSearchCall` | **硬失败** | **硬失败** | 同上,v1 硬失败 |
| `ImageGenerationCall` | **硬失败** | **硬失败** | 同上,v1 硬失败 |
| `Reasoning` | 出站丢弃 | 出站丢弃 | 见 §4.4(encrypted_content 不可发往非 Responses 上游) |
| `Compaction` / `ContextCompaction` | **硬失败** | **硬失败** | 二者都带 `encrypted_content`(`Compaction.encrypted_content: String`、`ContextCompaction.encrypted_content: Option<String>`,已核对),承载模型可见的压缩历史。非 Responses 上游读不了,**静默丢会改变模型可见历史**,故硬失败(等价于:v1 不支持含这些项的历史)|
| `CompactionTrigger` | 出站丢弃 | 出站丢弃 | 仅 `metadata`、无 `encrypted_content`/正文(已核对),纯触发标记,可安全丢弃 |
| `Other` | **硬失败** | **硬失败** | 未知变体(`#[serde(other)]`),无法安全翻译 |

> 原则:**v1 工具能力只支持标准 `FunctionCall`/`FunctionCallOutput`**;一切 provider/native/custom/freeform 工具项(及未知变体)一律**硬失败**返回 `ApiError`,绝不"warning 丢弃"、也绝不强行译成函数调用(那会让模型引用上游不存在的工具)。仅 codex 纯内部记账项可出站丢弃,且实现期须核实。

### 4.1 responses(直通 = 不进 zmod 路由)

修正(消除与 §2.4 遥测的矛盾):**v1 没有 zmod 内的 responses 连接器**。`connector = "responses"`(或 provider 未在 config-zmod 列出)→ `route()` 返回 **`None`** → 直接走 `stream_responses_api` 的**原生分支**(`ApiResponsesClient` + 完整 `.with_telemetry(...)`)。这样 responses 上游既零协议翻译、又**完整保留** codex-api 请求/SSE 遥测。

> 取舍:本来想"让 ① 变换层也作用于原生 Responses 上游",但那要求 responses 也进 zmod、从而丢掉 codex-api 遥测(§2.4)。v1 选择**遥测优先**:responses 不进 zmod。将来要在 responses 直通上做压缩时,再决定是把 telemetry 传进 zmod、还是在原生分支前插一个只改 `ResponsesApiRequest` 的 hook(不接管流)。

### 4.0b `tools` 定义级分级(修正:不可表达工具在请求构造期就拦截)

`ResponsesApiRequest.tools` 是 `Vec<serde_json::Value>`(`codex-api/src/common.rs:188`),可能含 `function` / `namespace` / `tool_search` / `image_generation` / `web_search` / `custom`(freeform)。目标协议只支持其中一部分,**必须在请求构造期按工具定义的 `type` 分级**(与 §4.0 的历史项处置对应,但更靠前——避免一开始就把不可表达的工具定义发错):

| 工具定义 `type` | chat | anthropic | 处置 |
|---|---|---|---|
| `function`(标准 JSON schema) | `tools[{type:"function", function:{name,description,parameters}}]` | `tools[{name,description,input_schema}]` | **v1 唯一支持** |
| `custom` / freeform | **硬失败** | **硬失败** | 无标准 schema,目标 function tool 表达不了(§4.0 CustomToolCall 同源) |
| `namespace` | **硬失败** | **硬失败** | codex 专有命名空间工具,v1 不支持 |
| `tool_search` / `web_search` / `image_generation` 等 hosted/native 工具 | **硬失败** | **硬失败** | provider 侧内置工具,目标无等价定义 |

> 即:**v1 只放行标准 `function` 工具定义**;请求里出现任何其它工具定义 → 连接器在构造出口请求前返回 `ApiError`(配 §4.0 历史项硬失败,二者一致)。这样既不会发出不可表达的工具定义,也不会留下"工具定义没发、但历史里有该工具调用"的不一致。

### 4.0a endpoint 路径拼接规则(修正:消除版本段歧义)

`egress_url = base_url.trim_end_matches('/') + path`,其中 `path` **由连接器提供默认值、可被 config-zmod 的 `path` 覆盖**:

| connector | 默认 `path` |
|---|---|
| chat | `/chat/completions` |
| anthropic | `/v1/messages` |
| responses | 不进 zmod(`route()` 返回 None,走原生分支,§4.1) |

约定:**`base_url` 只写到 API 根、不含上面这段 `path`**;版本前缀(如 deepseek 的 `/v1`)算 `base_url` 的一部分。于是:

- deepseek `base_url = https://api.deepseek.com/v1` + 默认 `/chat/completions` → `https://api.deepseek.com/v1/chat/completions` ✓
- anthropic 官方 `base_url = https://api.anthropic.com` + 默认 `/v1/messages` → `https://api.anthropic.com/v1/messages` ✓
- 网关型(testkey)`base_url = https://node-hk.sssaiapi.com/api` + 默认 `/v1/messages` → `https://node-hk.sssaiapi.com/api/v1/messages`(若网关路径不同,用 `path` 覆盖)✓

### 4.2 chat(deepseek / OpenAI 兼容)

出口 `POST {egress_url}`(默认 `{base_url}/chat/completions`,见 §4.0a),Bearer 鉴权。

- **请求**:`instructions` → `messages[0]` system;`input[Message]` → `messages`;`input[FunctionCall]` → assistant `tool_calls`(`arguments` 已是 JSON 字符串,直传);`input[FunctionCallOutput]` → `role:"tool"`;`tools` → `tools[{type:"function"}]`;`reasoning`/`store`/`include`/`prompt_cache_key` 等按 §7 分级处置;加 `stream:true` + `stream_options.include_usage`。
- **响应 SSE → ResponseEvent**:`delta.content` → `OutputTextDelta`(仅流式展示)**并累计文本**;`delta.tool_calls[].function.arguments` 按 index 聚合 → `OutputItemDone(FunctionCall)`;**完成前按 §4.5 合成 assistant message 的 `OutputItemDone`**;再发 `Completed{…}`(§4.5,`response_id` 用 chunk `id` 或合成,`end_turn` 由 `finish_reason` 映射);顶层 error → 失败。`data:[DONE]` 收尾。

### 4.3 anthropic

出口 `POST {egress_url}`(默认 `{base_url}/v1/messages`,见 §4.0a),头 `x-api-key` + `anthropic-version`(鉴权整形在 `http.rs`)。

- **请求**:`instructions` → 顶层 `system`;`input[Message]` → `messages`(role 仅 user/assistant);`input[FunctionCall]` → assistant `content[{type:"tool_use", id, name, input}]`(**`arguments` 字符串 → parse 成对象**);`input[FunctionCallOutput]` → user `content[{type:"tool_result", tool_use_id, content}]`;`tools` → `tools[{name, description, input_schema}]`;**`max_tokens` 必填** —— 缺省由 config-zmod 的 `default_max_tokens` 填充(兜底常量 4096)。
- **响应 SSE → ResponseEvent**:`content_block_delta`/`text_delta` → `OutputTextDelta`(仅展示)**并累计文本**;`tool_use` block + `input_json_delta` 聚合(**对象 → stringify 回 `arguments` 字符串**) → `OutputItemDone(FunctionCall)`;**完成前按 §4.5 合成 assistant message 的 `OutputItemDone`**;`message_delta`(usage) + `message_stop` → `Completed{…}`(§4.5,`response_id` 用 `message_start.message.id` 或合成,`end_turn` 由 `stop_reason` 映射);`error` → 失败。
- **硬约束**:tool_use ↔ tool_result 配对完整——**不完整时按 §4.10 修复**(注入合成结果 / 删孤儿结果 / strip 空 tools 的 tool_choice),不硬失败;Reasoning 出站处置见 §4.4。

### 4.5 响应完成契约(修正:必须合成 assistant message 完成项 + 补全 Completed)

`core/src/client.rs` 的 `map_response_events` **只在 `OutputItemDone(item)` 时**把 `item` 收进 `LastResponse.items_added`(`client.rs:1822`),`OutputTextDelta` 只负责流式展示、**不进历史**。Chat/Anthropic 没有 Responses 原生的 `output_item.done(message)`,**连接器必须自己累计文本并在 `Completed` 之前合成一条 assistant message 的完成项**,否则下一轮历史缺失 assistant 回复:

```
OutputItemDone(ResponseItem::Message {
    role: "assistant",
    content: vec![ContentItem::OutputText { text: <累计的全部文本> }],
    id / ... 按 protocol 填,
})
```

发送顺序:`OutputTextDelta…`(展示)→ 各 `OutputItemDone(FunctionCall)`(若有工具调用)→ **合成的 `OutputItemDone(Message assistant)`**(若有文本)→ `Completed`。

**`Completed` 三字段必须填全**(`codex-api/src/common.rs:88` `Completed { response_id, token_usage, end_turn }`):

| 字段 | chat | anthropic |
|---|---|---|
| `response_id` | chunk 顶层 `id`,缺则合成(如 `llmswitch-<uuid>`) | `message_start.message.id`,缺则合成 |
| `token_usage` | 末 chunk `usage`(`include_usage`)→ `TokenUsage` | `message_start` + `message_delta` 的 `usage` 累计 → `TokenUsage` |
| `end_turn` | `finish_reason=="stop"` → `Some(true)`;`"tool_calls"` → `Some(false)`;`"length"`/未知 → `None` | `stop_reason=="end_turn"` → `Some(true)`;`"tool_use"` → `Some(false)`;`"max_tokens"`/未知 → `None` |

### 4.6 `FunctionCallOutput` 内容分级(修正:覆盖结构化/多模态输出)

`FunctionCallOutputPayload` 的 body 可是纯文本,也可是 `ContentItems(Vec<...>)`,其中可含 `InputImage`、`EncryptedContent`(`protocol/src/models.rs`)。连接器按内容分级,**不得**把图片当"输入图片"规则硬塞、也不得静默发错加密内容:

| 输出内容 / 字段 | chat | anthropic |
|---|---|---|
| `body` 文本 / `ContentItems` 纯文本 | `role:"tool"` 的 `content` 文本 | `tool_result.content` 文本 |
| `body` `InputImage` | **v1 硬失败** | **v1 硬失败** |
| `body` `EncryptedContent` | **硬失败** | **硬失败** |
| `success: Option<bool>`(工具成败状态) | 无等价字段:`success == Some(false)` 时在 `content` 文本**前置**简短失败标记(如 `[tool error] ` 前缀)+ warn;`Some(true)`/`None` 不加 | 映射到 `tool_result.is_error = (success == Some(false))`(anthropic 原生支持);`None` → 不设 |

> `success` 处理理由:Chat 的 `role:"tool"` 没有成败字段,**静默丢失败状态会误导模型**,故前置文本标记 + warn;Anthropic 有原生 `is_error`,直接映射。

> **图片 v1 一律硬失败(修正)**:config / `ModelProviderInfo` 都没有 `supports_images`/视觉能力字段(已核对),连接器无从判定 DeepSeek/Claude 当前模型是否支持图片。因此 **v1 把一切图片(输入图片、工具图片输出)都硬失败**,不做能力猜测。将来要支持时,先在 config-zmod 增 `supports_images = true` 之类显式能力字段再放行。
> 与 §4.4 一致:加密内容只在 responses 直通透传;chat/anthropic 出口遇到 `EncryptedContent`(无论在 message、AgentMessage 还是 tool output 里)一律硬失败。

### 4.4 reasoning 的两类对象(修正:消除与 §7.1 的冲突)

**必须区分两个同名但不同的东西**,二者处理规则不同,不冲突:

1. **历史里的 `ResponseItem::Reasoning`**(`input[]` 中的加密推理**输出项**,含 `encrypted_content`):OpenAI 专有,非 Responses 上游读不了,"透传"无落点。处置——
   - **chat / anthropic 出站**:**不写入**发往上游的请求体(出站丢弃)。连接器只构造请求**副本**,codex 本地会话历史(原始 `ResponseItem` 列表)**不受影响**,后续轮次仍完整保留 reasoning。
   - **responses 直通**:`encrypted_content` 原样透传(走原生 client)。
2. **请求级 `ResponsesApiRequest.reasoning: Option<Reasoning>`**(reasoning **配置**:effort / summary,**非加密、非历史**,`codex-api/src/common.rs:191`):这才是 §7.1"降级转换"的对象——anthropic → `thinking`、chat → `reasoning_effort`,目标不支持则丢弃 + warn。

> 一句话:**加密的 reasoning 输出项(历史)出站丢弃;reasoning 配置(请求字段)按 §7.1 降级**。§4.4 与 §7.1 分别管这两者,无矛盾。

### 4.7 `run` 的错误与 spawn 边界(修正:建连错误必须同步返回)

为了让外层 `match stream_result` 能正确分流错误,`run` 内部分两阶段:

1. **同步阶段**(在 `run` 返回前完成):构造出口请求 → 发 HTTP → **校验状态码** → 建立 SSE 响应读取器。任何此阶段的失败(建连失败、DNS、非 2xx 状态、鉴权 4xx)**直接 `return Err(ApiError)`**,落到外层 `match`(`map_api_error` + `inference_trace_attempt.record_failed`)。
2. **异步阶段**(`spawn`):**仅当**第 1 阶段成功拿到 2xx SSE 才 `spawn` 读取 task;task 内的流中错误(SSE 中断、坏帧)走 `tx.send(Err(ApiError…))` → `map_response_events` 流错误处理。

**401 注意**:外层那条 `TransportError::Http{status==UNAUTHORIZED}` 臂会触发 **OpenAI 专属**的 `handle_unauthorized` recovery,对第三方上游无意义。为免无谓 recovery 循环,连接器对第三方的 **401/403 等鉴权失败映射成普通 `ApiError`(非 `UNAUTHORIZED` transport 变体)**,使其落到通用失败臂直接上报。

### 4.8 工具调用 id 映射(修正:配对字段写全)

`ResponseItem::FunctionCall.call_id: String`(`protocol/src/models.rs`,稳定 id)是配对锚点。出站构造请求历史时:

| 方向 | chat | anthropic |
|---|---|---|
| FunctionCall(assistant 调用) | `messages[assistant].tool_calls[].id = call_id`,`function.name/arguments` 同填 | `content[{type:"tool_use", id: call_id, name, input}]` |
| FunctionCallOutput(工具结果) | `messages[tool].tool_call_id = call_id` | `content[{type:"tool_result", tool_use_id: call_id, content}]` |

入站(上游 SSE → `OutputItemDone(FunctionCall)`)反向:chat `tool_calls[].id` / anthropic `tool_use.id` → 回填 `call_id`,保证下一轮历史里 call ↔ output 仍按 `call_id` 配对。**全程用同一个 `call_id` 串起 assistant 调用与 tool 结果**,不另造 id(除非上游完全不给 id,则连接器合成并在 call/result 间保持一致)。

### 4.9 `Message.content` 逐项映射(修正:ContentItem 三变体)

`ContentItem` 有 `InputText`/`InputImage`/`OutputText`(`protocol/src/models.rs`)。user/assistant 历史里都可能出现。映射:

| ContentItem | chat | anthropic |
|---|---|---|
| `InputText` | message `content` 文本(user/system) | message `content[{type:"text"}]` |
| `OutputText` | assistant message `content` 文本 | assistant `content[{type:"text"}]` |
| `InputImage` | **v1 硬失败**(§4.6 同理,无能力判定) | **v1 硬失败** |

> v1 文本(InputText/OutputText)正常映射,**图片硬失败**;role 由所在 `Message.role` 决定(anthropic 仅 user/assistant,system 走顶层 `system`)。

### 4.10 工具配对 / 配置完整性(修正:复刻 llm-rosetta,不硬失败)

codex 上下文压缩会破坏请求结构(孤儿 tool_call/result、有 `tool_choice` 但无 tools)——llm-rosetta 的 `fix_orphaned_tool_calls` / `strip_orphaned_tool_config`(`converters/anthropic/`、`converters/base/tools.py`)正是为此而生。**这正是本项目场景,应复刻其修复行为,不硬失败**(硬失败会让压缩后的正常会话直接挂掉):

- **孤儿 tool_call / tool_use**(有调用、无对应结果)→ **注入合成占位结果**(content 如 `[No output available yet]`),不硬失败。
- **孤儿 tool_result / tool_output**(有结果、无前序调用)→ **删除该孤儿结果**,不硬失败。
- **有 `tool_choice`/`tool_config` 但 `tools` 为空**(压缩删光工具定义)→ **strip 掉 `tool_choice`/`tool_config`** + warn(否则上游报 "tool_choice is set but no tools are provided")。
- **chat 专属:tool 消息重排**(复刻 llm-rosetta `_reorder_tool_messages`,`converters/openai_chat/message_ops.py`)。Chat Completions 要求 `role:"tool"` 消息**紧跟**产生对应 `tool_calls` 的 `role:"assistant"` 消息;codex 在 Responses 格式里把 `function_call_output` 与其它 item 交错排布(见 openai/codex PR #7038),转成 chat 扁平消息序列后 tool 消息会与其 assistant 分离 → 上游 400。**仅 id 配对(§4.8)不够,必须重排**:出站到 chat 前,把 tool 消息按各自 `tool_call_id` 归组,遍历非 tool 消息,在每条带 `tool_calls` 的 assistant 后**按 `tool_calls` 顺序**插回匹配的 tool 消息;未匹配上的 tool 消息**追加到末尾**(不静默丢弃)+ warn。重排发生即记一条 warning。
  - **anthropic 不需要**:其 `tool_result` 是 user 回合内的 content block、按回合归组(§4.3),不是扁平消息序列,无此扁平排序问题。
- 以上各项都只作用于发往上游的请求**副本**,不动 codex 本地历史;各记 warning。

> 注:这与 §4.0 的"不支持变体硬失败"不冲突——§4.0 处理的是**v1 不支持的项类型**(native/custom 工具),§4.10 处理的是**支持的标准 function 工具因压缩产生的结构破损**,后者修复、前者拒绝。

### 4.11 `tool_choice` / `parallel_tool_calls` 映射(修正:统一规则)

`ResponsesApiRequest` 顶层的工具控制字段(非工具定义本身):

- **`parallel_tool_calls`**(修正:anthropic 有对应,不能丢):
  - chat → 透传 `parallel_tool_calls`。
  - anthropic → 映射到 `tool_choice.disable_parallel_tool_use`(llm-rosetta `anthropic/tool_ops.py`):codex `parallel_tool_calls == false` → `disable_parallel_tool_use = true`;`true`/未设 → 不设(anthropic 默认并行)。
- **`tool_choice`**(修正:统一为"会改变工具链语义的强制选择不可降级 → 硬失败"):
  - `auto` / `none` → 映射到目标对应档(chat `"auto"`/`"none"`;anthropic `{type:"auto"}` / `{type:"none"}`)。
  - **强制调用特定工具 / 强制必调(`required` / 指定函数)**:这会改变工具链语义。目标能等价表达则映射(chat `{type:"function", function:{name}}` / `"required"`;anthropic `{type:"tool", name}` / `{type:"any"}`);**目标无法等价表达该强制语义时 → 硬失败**(不降级、不 warn 放行——降级会让模型行为偏离 codex 预期)。
  - 即:**可表达就映射,不可表达的强制档一律硬失败**(取代此前"warn 放行"与"硬失败"并存的矛盾写法)。

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
key_env   = "DEEPSEEK_API_KEY"             # 连接器自己读原始 key(见 §5.3)
# path    = "/chat/completions"            # 可选,覆盖默认出口路径(§4.0a)

[llm-switch.providers.claude]
connector         = "anthropic"
base_url          = "https://api.anthropic.com"
auth              = "x-api-key"
key_env           = "ANTHROPIC_API_KEY"
anthropic_version = "2023-06-01"
default_max_tokens = 8192
```

- 命中表名则用对应连接器,未命中 → 原生 Responses 路径。
- 文件或 `[llm-switch]` 缺失 → 整体关闭(fail-safe,符合 codez 的 zmod 约定)。
- **`model`(可选)**:覆盖/映射发往真上游的模型名。运行时接管 codex 时缺省用 codex 请求里的 `model`;独立运行 / 实跑测试时由此字段指定(如 testkey 里的 `deepseek-v4-pro`、`claude-opus-4-8`)。

### 5.3 密钥来源与优先级(修正:连接器自取原始 key,不依赖 codex auth 整形)

**关键约束**:codex 的 `api_auth` 是 `SharedAuthProvider = Arc<dyn AuthProvider>`,只暴露 `add_auth_headers()`(写 `Authorization: Bearer <token>`),**不暴露原始 key**(`codex-api/src/auth.rs:68`、`model-provider/src/bearer_auth_provider.rs`)。而 Anthropic 需要 `x-api-key` 头——**无法**从 codex 的 auth 重整形。所以连接器必须**自己拿到原始 key**。

密钥来源,按优先级:

1. **`key_env`**(config-zmod 每个 provider 的字段)→ 连接器 `std::env::var(key_env)` 直接读原始 key。**运行时接管的主路径**(与 codex `config.toml` 的 `env_key` 可指向同一个环境变量,但由连接器独立读取,不经 codex auth)。
2. **`auth_key` 内联** → **仅允许出现在 gitignored 的 `zmod/llm-switch/tests/testkey.toml`**,供离线/实跑测试与独立运行。**确定策略(非"告警或拒绝"二义)**:正式 `~/.codex/config-zmod.toml` 中**一旦出现 `auth_key`,`config.rs` 解析时直接返回配置错误、拒绝启动**(明文密钥不得落 codex 主配置);`auth_key` 字段只在从 `tests/testkey.toml` 加载的代码路径里被接受。
3. **(仅 `auth = "bearer"` 且未配 `key_env`/`auth_key` 时的退路)** 复用 codex 的 `api_auth.add_auth_headers()` 写 `Authorization: Bearer`——因为 bearer 形态与 codex 一致,可直接借力。x-api-key 形态**无**此退路,必须有 `key_env`/`auth_key`。

`http.rs` 用拿到的原始 key 按 `auth` 整形:`bearer` → `Authorization: Bearer <key>`;`x-api-key` → `x-api-key: <key>` + `anthropic-version`。`anthropic` 连接器启动时校验 key 可得,否则 `ApiError`(配置缺失早失败)。

## 6. patch(对 codex-rs 的全部改动)

修正:这**不是**"3 处清单式追加 + 早返回",而是一次真实的接入(因为要按 id 路由 + 接进既有 stream_result 流程)。`patches/llm-switch.patch` 触点:

### 6.1 构建集成(修正:不当 workspace member)

`zmod/llm-switch` 在 codex-rs workspace 根(`codex-rs/`)**之外**,且要反向依赖 `codex-api`/`codex-protocol`(它们是 codex-rs 的 member)。把 `../zmod/llm-switch` 塞进 codex-rs 的 `[workspace] members` 不合适(跨根、且要同步 `[workspace.dependencies]`)。改为:

- `zmod/llm-switch/Cargo.toml` 是**独立包**(**不声明自己的 `[workspace]`**,否则被当 path 依赖编译时会触发"nested workspace"报错),用显式 path 依赖反指:`codex-api = { path = "../../codex-rs/codex-api" }`、`codex-protocol = { path = "../../codex-rs/protocol" }`(版本随 codex-rs 走,不用 `workspace = true`)。
- patch 在 **`codex-rs/core/Cargo.toml`** 加一条 path 依赖:`codez-llm-switch = { path = "../../zmod/llm-switch" }`。它作为 core 的普通 path 依赖被一起编译,**不进** workspace member 列表;无依赖环(llm-switch 只依赖 api/protocol,不依赖 core)。
- 独立 `cargo test`:在 `zmod/llm-switch/` 直接跑,path 依赖会定位到 codex-rs 的 crate,正常解析。

### 6.4 构建约定文档更新(交付物,非可选)

情况 B 与原 `CLAUDE.md`"patch 把 `codez-<feature>` 加入 workspace members"的约定冲突。这是**仓库构建约定的变更**,必须作为本功能的交付物之一:更新 `CLAUDE.md` 的"zmod crate 与 patch 命名规则 / 构建集成",明确分两种情况(A:独立 crate 进 members;B:反向依赖 codex-rs crate 的用外部 path 依赖)。否则实现会违反仓库现有约定。
> (已在 commit `7a12f5291` 落地;此处登记为正式交付物,纳入成功判据。)

### 6.2 路由键透传(修正:ModelClient 需要 id)

- **`core/src/client.rs`** `ModelClient::new` 增形参 `model_provider_id: String`,存进 `ModelClientState.model_provider_id`;其唯一调用处从 `Config.model_provider_id`(`config/mod.rs:631`)传入。

### 6.3 发送边界接入(修正:接进 stream_result,非早返回)

- **`core/src/client.rs`** `stream_responses_api`:把现有的"构造 `ApiResponsesClient` + `client.stream_request(...)`"那段,改成按 `codez_llm_switch::route(&self.client.state.model_provider_id)` 二选一(见 §2 代码)。**owned 入参**:`run` 签名为 `run(rt: Route, request: ResponsesApiRequest, api_provider: codex_api::Provider, api_auth: SharedAuthProvider, transport: ReqwestTransport, options: …) -> Result<codex_api::ResponseStream, ApiError>`——与原生臂把 `api_provider`/`api_auth`/`transport` move 进 `ApiResponsesClient::new` 对称;两臂互斥,move 合法,**不保留借用**。**下游 `match stream_result { … }`(unauthorized recovery / `map_api_error` / `record_failed` / `map_response_stream`)完全不动**,二者共用。

翻译/网络逻辑全在 `codez-llm-switch` crate;core 触点是 Cargo 依赖 + `ModelClient::new` 形参 + `stream_responses_api` 里一处赋值改写。下游错误/`inference_trace`/`map_response_stream` 逻辑不动;但**接管路径不接 codex-api 层请求/SSE 遥测**(§2.4,已知缺口),原生路径完整保留 `.with_telemetry(...)`。同步 codex-rs 时冲突面小但**非零**(`ModelClient::new` 签名属较稳定的接口)。

## 7. 错误处理与字段分级

- 连接器翻译/网络失败 → 映射成 codex 既有 `ApiError`(`tx.send(Err(..))` 或 `run` 直接返回 `Err`),codex 按原生错误流程处理(重试/报错)。

### 7.1 请求字段分级(修正:不一律 warning 丢弃)

按"丢弃后是否改变模型可见语义"分三级,各连接器据此处置(`ResponseItem` 变体见 §4.0):

| 级别 | 字段(示例) | 处置 |
|---|---|---|
| **可安全忽略**(纯传输/缓存元数据,不影响模型输出) | `store`、`include`、`prompt_cache_key`、`service_tier`、`client_metadata` | 静默丢弃 |
| **降级转换**(目标有近似表达,尽力映射,丢真实语义时记 warning) | **请求级 `reasoning` 配置**(`ResponsesApiRequest.reasoning`,即 effort/summary,**非**历史里的加密 `Reasoning` item——后者按 §4.4 出站丢弃)→ anthropic→`thinking` / chat→`reasoning_effort`,无则丢+warn;`text.format` 结构化输出 schema(chat→`response_format` json_schema;anthropic 无→降级为指令或 warn) | 尽力映射 + 必要时 warn |
| **专项规则**(见对应小节,不在此泛化) | `parallel_tool_calls`、`tool_choice` → §4.11;孤儿 tool_call/result、空 tools 的 tool_choice → §4.10 | 见 §4.10 / §4.11 |
| **必须硬失败**(静默丢会破坏模型可见语义/工具链,且无法降级) | 图片/多模态输入(§4.9/§4.6,v1 一律);承载工具调用/结果的不支持 `ResponseItem` 变体(§4.0 标"硬失败"者);目标无法等价表达的强制 `tool_choice`(§4.11) | 返回 `ApiError`,不发请求 |

> 实现期:每个连接器对上表逐项落实,黄金测试覆盖"降级"与"硬失败"两类断言。

### 7.2 鉴权整形

`http.rs`:`auth = "bearer"` → `Authorization: Bearer <key>`;`auth = "x-api-key"` → `x-api-key: <key>` + `anthropic-version`。**原始 key 由连接器自取**(`key_env` / testkey 的 `auth_key`),不依赖 codex 的 `add_auth_headers`(它只能产出 Bearer)。来源与优先级见 §5.3。

## 8. 测试与成功判据

- **离线黄金测试**(主力,不需 key)。断言 `ResponsesApiRequest → 目标请求 JSON` 语义等价(忽略字段序、可选省略策略);静态 SSE chunk 序列驱动连接器,断言产出的 `ResponseEvent` 序列正确(含 tool_call 聚合、usage、收尾)。**各连接器的基准来源必须明确**(修正:`rust-llm-proxy` 只实现了 OpenAiChat,Anthropic 无 Rust 基准):
  - **chat**:可用 `../3rd/proxy/rust-llm-proxy` 的 OpenAiChat converter/fixtures 作基准。
  - **anthropic**:用 **`../3rd/proxy/llm-rosetta` 的 Python anthropic converter**(`tests/converters/anthropic`)生成期望输出,固化成本仓 fixture;或声明**自建 fixture**(由人工核对 Anthropic Messages 官方格式)。二者择一,**不得**笼统写"对应 converter"。
  - 覆盖 §7.1 降级 / 硬失败、§4.0 变体硬失败、§4.0b 工具定义硬失败的断言。
- **集成/实跑测试**(门控):读 `zmod/llm-switch/tests/testkey.toml`(schema 即 `[llm-switch.providers.<id>]` + `auth_key` + `model`),真实打 deepseek(chat)/claude(anthropic)端点,验证端到端连通。用 `#[ignore]` 或环境变量门控;`testkey.toml` 缺失时自动跳过,CI 无 key 也能全绿,本地 `cargo test -- --ignored` 跑真链路。
- **不变量**:chat/anthropic 出站丢弃 `Reasoning` 但 codex 本地历史不变(§4.4);responses 直通时 `encrypted_content` 不变;tool_call ↔ output 的 `call_id` 关联正确;§4.0 标"硬失败"的变体确实返回 `ApiError` 而非静默丢。
- **独立可测**:crate 能脱离 codex 独立 `cargo test`(path 依赖 codex-api / codex-protocol)。

成功判据:

1. codex 按 §5.1 配好 `[model_providers.deepseek]` **和** `[model_providers.claude]` + §5.2 config-zmod 路由后,**两者都**能跑通对话。**工具调用验收口径(消除与 v1 只支持 function 的歧义)**:实跑场景**只启用标准 `function` 工具**——即只验证 `FunctionCall`/`FunctionCallOutput` 往返;测试/配置须**关闭** codex 默认可能暴露的 namespace/custom/tool_search/web_search/image_generation 等工具(否则按 §4.0/§4.0b 会硬失败,无法验收)。这些被关的工具的"遇到即硬失败"行为由离线黄金测试(判据 2)覆盖,不在实跑判据内。
2. 三个连接器的离线黄金测试全绿(语义等价),且覆盖 §7.1 降级/硬失败两类断言。
3. 上述硬不变量满足。
4. core 触点仅 §6 所列(Cargo 依赖 + `ModelClient::new` 形参 + `stream_responses_api` 一处赋值改写),不改既有错误/`inference_trace`/流映射逻辑;原生路径保留 `.with_telemetry(...)`,接管路径不接 codex-api 请求/SSE 遥测(§2.4,已记为已知缺口)。
5. `CLAUDE.md` 的 zmod 构建约定已更新为情况 A/B 两分(§6.4),与本功能的情况 B 集成方式一致。

## 9. 安全注记

`zmod/llm-switch/tests/testkey.toml` 含真实 API key,已被 `.gitignore`(第 30 行 `testkey.toml` 全局匹配)排除,不得提交到 GitHub。新增任何含密钥的测试夹具,须同样确保被 gitignore 覆盖。
