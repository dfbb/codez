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
core/src/client.rs  stream_responses_api()
  │ 已组装好的 ResponsesApiRequest (codex 原生)
  ▼
[patch 分支] codez_llm_switch::route(provider_id) 命中?
  ├─ 否 → 原生 ApiResponsesClient::stream_request(...)        (零改动路径)
  └─ 是 → codez_llm_switch::run(request, api_provider, api_auth, transport, options)
            ① TransformPlugin[] 变换  —— 作用于 codex 原生 Responses 类型
            ②          Connector 出口翻译 + HTTP/SSE  →  真上游
            返回 ResponseStream(同一个 mpsc<Result<ResponseEvent, ApiError>> 类型)
  ▼
codex 拿到标准 ResponseEvent 流(无感)
```

**分层依据**:压缩关心的是"模型实际看到的内容"——codex 已组装好的 `ResponsesApiRequest`(各 `ResponseItem` 的 `content` / `FunctionCallOutput.output`),与上游是 anthropic 还是 deepseek 无关,应在 Responses 语义空间里只压一次(管线 ①)。协议翻译是出口的事(管线 ②)。两件事分两层。

**集成点只有一处**:`stream_responses_api` 里 `client.stream_request(request, options)` 调用前加一个路由分支。`ResponseStream` 即 `mpsc::Receiver<Result<ResponseEvent, ApiError>>`,连接器起一个 task 读上游 SSE、翻译成 `ResponseEvent` 塞进 channel,codex 完全无感。

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
    async fn run(&self, req: ResponsesApiRequest, ctx: &EgressCtx) -> Result<ResponseStream>;
}
```

`EgressCtx` 带 base_url、鉴权、reqwest transport、目标 model、config-zmod 覆盖项。连接器内部起一个 task 读上游 SSE → 翻译 → `tx.send(ResponseEvent)`。

三者的字段映射逐条对照 `../3rd/proxy/llm-rosetta` 对应 converter(`tests/converters/anthropic`、`openai_chat`)作为正确性基准。

### 4.1 responses(直通)

不翻译,直接委托 codex 原生 `ApiResponsesClient`。存在意义:让管线 ① 变换层(将来的压缩)也能作用于原生 Responses 上游。零协议风险。

### 4.2 chat(deepseek / OpenAI 兼容)

出口 `POST {base_url}/chat/completions`,Bearer 鉴权。

- **请求**:`instructions` → `messages[0]` system;`input[Message]` → `messages`;`input[FunctionCall]` → assistant `tool_calls`(`arguments` 已是 JSON 字符串,直传);`input[FunctionCallOutput]` → `role:"tool"`;`tools` → `tools[{type:"function"}]`;丢弃 `reasoning`/`store`/`include`/`prompt_cache_key`;加 `stream:true` + `stream_options.include_usage`。
- **响应 SSE → ResponseEvent**:`delta.content` → `output_text.delta`;`delta.tool_calls[].function.arguments` 按 index 聚合 → `OutputItemDone(FunctionCall)`;`finish_reason` + 末 chunk usage → `Completed { token_usage }`;顶层 error → 失败。`data:[DONE]` 收尾。

### 4.3 anthropic

出口 `POST {base_url}/v1/messages`,头 `x-api-key` + `anthropic-version`(鉴权整形在 `http.rs`)。

- **请求**:`instructions` → 顶层 `system`;`input[Message]` → `messages`(role 仅 user/assistant);`input[FunctionCall]` → assistant `content[{type:"tool_use", id, name, input}]`(**`arguments` 字符串 → parse 成对象**);`input[FunctionCallOutput]` → user `content[{type:"tool_result", tool_use_id, content}]`;`tools` → `tools[{name, description, input_schema}]`;**`max_tokens` 必填** —— 缺省由 config-zmod 的 `default_max_tokens` 填充(兜底常量 4096)。
- **响应 SSE → ResponseEvent**:`content_block_delta`/`text_delta` → `output_text.delta`;`tool_use` block + `input_json_delta` 聚合(**对象 → stringify 回 `arguments` 字符串**) → `OutputItemDone(FunctionCall)`;`message_delta`(usage) + `message_stop` → `Completed`;`error` → 失败。
- **硬约束**:`Reasoning` 的 `encrypted_content` 透传不动;tool_use ↔ tool_result 配对完整。

## 5. 配置与路由(config-zmod)

`~/.codex/config-zmod.toml`:

```toml
[llm-switch]
enabled = true

# 路由键 = codex 的 model_provider id;codex config.toml 里照常配 provider
[llm-switch.providers.deepseek]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"   # 可选;缺省用 codex provider 的 base_url
auth      = "bearer"

[llm-switch.providers.claude]
connector = "anthropic"
base_url  = "https://api.anthropic.com"
auth      = "x-api-key"
anthropic_version = "2023-06-01"
default_max_tokens = 8192
```

- 路由键 = codex 的 `model_provider` id;命中则用对应连接器,未命中 → 原生 Responses 路径。
- codex 侧 `config.toml` 照常配 `[model_providers.deepseek]`(`wire_api="responses"`、`base_url` 指真上游),llm-switch 在发送前接管并改写协议/路径/鉴权。
- 文件或 `[llm-switch]` 缺失 → 整体关闭(fail-safe,符合 codez 的 zmod 约定)。
- **鉴权来源**:运行时接管 codex 时优先用 codex 的 `api_auth`;config 里的 `auth_key`(内联)作兜底,主要给测试 / 独立运行用。
- **`model`(可选)**:覆盖/映射发往真上游的模型名。运行时接管 codex 时缺省用 codex 请求里的 `model`;独立运行 / 实跑测试时由此字段指定(如 testkey 里的 `deepseek-v4-pro`、`claude-opus-4-8`)。

## 6. patch(对 codex-rs 的全部改动)

`patches/llm-switch.patch`,三处清单式追加:

1. **`codex-rs/Cargo.toml`**:workspace `members` 加 `"../zmod/llm-switch"`。
2. **`codex-rs/core/Cargo.toml`**:加依赖 `codez-llm-switch = { path = "../../zmod/llm-switch" }`(core 依赖它;它只依赖 codex-api / codex-protocol,无循环)。
3. **`codex-rs/core/src/client.rs`** `stream_responses_api`:在 `client.stream_request(request, options)` 前加路由分支 —— `codez_llm_switch::route(&provider_id)` 命中则 `return codez_llm_switch::run(request, api_provider, api_auth, transport, options).await`,否则原样。约 8–10 行。

热点函数体只多一个早返回分支,翻译逻辑全在 zmod crate;同步 codex-rs 时冲突面极小。

## 7. 错误处理

- 连接器翻译/网络失败 → 映射成 codex 既有 `ApiError` 塞进 `rx_event`,codex 按原生错误流程处理(重试/报错)。
- 非致命特性损失(目标 provider 不支持的字段)记 warning 并丢弃,不阻断请求。
- **鉴权整形**(`http.rs`):`auth = "bearer"` → `Authorization: Bearer <key>`;`auth = "x-api-key"` → `x-api-key: <key>` + `anthropic-version`。密钥优先取 codex 的 `api_auth`,缺则取 config 的 `auth_key`。

## 8. 测试与成功判据

- **离线黄金测试**(主力,不需 key):从 `../3rd/proxy/llm-rosetta` `tests/` 抽 fixture,断言 `ResponsesApiRequest → 目标请求 JSON` 语义等价(忽略字段序、可选省略策略);静态 SSE chunk 序列驱动连接器,断言产出的 `ResponseEvent` 序列正确(含 tool_call 聚合、usage、收尾)。
- **集成/实跑测试**(门控):读 `zmod/llm-switch/tests/testkey.toml`(schema 即 `[llm-switch.providers.<id>]` + `auth_key` + `model`),真实打 deepseek(chat)/claude(anthropic)端点,验证端到端连通。用 `#[ignore]` 或环境变量门控;`testkey.toml` 缺失时自动跳过,CI 无 key 也能全绿,本地 `cargo test -- --ignored` 跑真链路。
- **不变量**:`Reasoning.encrypted_content` 透传不变;tool_call ↔ output 的 `call_id` 关联正确。
- **独立可测**:crate 能脱离 codex(只依赖 codex-api / codex-protocol 类型 + serde)独立 `cargo test`。

成功判据:

1. codex 配好 `[model_providers.deepseek]` + config-zmod 路由后,真能与 deepseek / claude 跑通对话(含工具调用)。
2. 三个连接器的离线黄金测试全绿(语义等价)。
3. 上述硬不变量满足。
4. patch 仅三处清单式追加,不碰 codex 热点函数体。

## 9. 安全注记

`zmod/llm-switch/tests/testkey.toml` 含真实 API key,已被 `.gitignore`(第 30 行 `testkey.toml` 全局匹配)排除,不得提交到 GitHub。新增任何含密钥的测试夹具,须同样确保被 gitignore 覆盖。
