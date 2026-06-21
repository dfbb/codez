# codez-llm-switch

codez 的第一个 zmod 功能 crate，在 **codex 进程内接管 LLM API 层**，让 codex 能接入 DeepSeek、Claude 等非 OpenAI 模型。

- 包名：`codez-llm-switch`　lib target：`codez_llm_switch`
- 对应补丁：构建集成 `patches/001-build.patch`（共享）+ 代码接入 `patches/002-llm-switch.patch`
- 设计文档：`docs/superpowers/specs/2026-06-20-llm-switch-design.md`

## 作用

codex 对 `base_url` **恒只说 OpenAI Responses 协议**（`WireApi` 枚举当前仅 `Responses`）。要接入 Anthropic / DeepSeek 这类上游，必须在 codex 与真上游之间做协议翻译。本 crate 挂在 codex client 的 HTTP 发送边界（`core/src/client.rs` 的 `stream_responses_api`），做两件事：

1. **出站**：把 codex 组装好的 `ResponsesApiRequest`（Responses 原生类型）翻译成目标协议请求体（Chat Completions 或 Anthropic Messages）。
2. **入站**：读上游 SSE，逐事件翻译回 codex 的 `ResponseEvent`，返回与 `ApiResponsesClient::stream_request` **同型**的 `codex_api::ResponseStream`，交回 core 既有的 `map_response_stream` 包装。

```
codex (Responses) ──run()──▶ ① 变换层(v1 直通) ──▶ ② Connector 出口翻译 + HTTP/SSE ──▶ 真上游
        ▲                                                                                  │
        └────────────────  ResponseEvent ◀── SSE 翻译 ◀── 上游 SSE  ◀─────────────────────┘
```

路由键是 codex 的 **`model_provider_id`**（不是 `name` 或 `base_url`）：命中 config-zmod 里配置的 provider 则接管，否则返回 `None`、走 codex 原生 Responses 分支（遥测链完整保留）。

## 支持的连接器

| connector | 目标协议 | 默认出口 path | 鉴权 |
| --- | --- | --- | --- |
| `chat` | Chat Completions（DeepSeek / OpenAI 兼容） | `/chat/completions` | `bearer` → `Authorization: Bearer <key>` |
| `anthropic` | Anthropic Messages | `/v1/messages` | `x-api-key` → `x-api-key: <key>` + `anthropic-version` |

`connector = "responses"` 或未在 config-zmod 列出的 provider → 不接管，走原生分支。

出口 URL = `base_url.trim_end_matches('/') + path`；`base_url` 只写到 API 根（版本前缀如 `/v1` 算 base_url 的一部分），`path` 可由配置覆盖。

## v1 能力边界

v1 **只支持标准 `function` 工具**与纯文本对话。下列情形连接器一律**硬失败**返回 `ApiError`（绝不静默丢、绝不强译成函数）：

- 非标准工具：`namespace` / `custom` / freeform / `tool_search` / `web_search` / `image_generation` 等工具定义或对应历史项。
- 图片输入 / 工具图片输出（无能力判定字段，不猜测）。
- 加密内容：`EncryptedContent` / `Compaction` / `ContextCompaction`。
- 目标协议无法等价表达的强制 `tool_choice`。

> **托管工具的源头降级**：codex 默认 `namespace_tools` / `web_search` / `image_generation` 能力均为 `true`，会把多智能体/协作、联网搜索、图片生成等工具打包进 Responses 请求（`{"type":"namespace"}` / `{"type":"web_search"}` 等），撞上面的硬失败。为此 `002-llm-switch.patch` 在 `core/src/tools/spec_plan.rs` 加了 `provider_capabilities()` 包装——当被接管的 provider 配 `captype = "chat"`（缺省）时按「无任何托管能力」处理（三项能力全 `false`），复用 codex 原生的能力门控从源头不产生这些托管工具。连接器里的硬失败保留作兜底安全网。因此实跑标准第三方 provider 时**无需**再手动关多智能体 / 搜索 / 图片等特性。
>
> 若某上游出口仍走 Responses 协议、能自行处理这些托管工具，给它配 `captype = "response"` 即可透传 codex 原生能力，不做屏蔽。

可安全降级或丢弃：

- 历史里的 `Reasoning` 项、`CompactionTrigger` → 出站丢弃（**只改请求副本，不动 codex 本地历史**）。
- 请求级 `reasoning` 配置 → chat `reasoning_effort` / anthropic `thinking`；`text.format` → chat `response_format` / anthropic 追加系统指令。
- `store` / `include` / `prompt_cache_key` / `service_tier` / `client_metadata` → 静默丢弃。
- 压缩造成的结构破损（孤儿 tool_call/result、空 tools 的 tool_choice、chat tool 消息错位）→ 自动修复（复刻 llm-rosetta），不硬失败。

## 配置与用法

所有 zmod 功能受 `~/.codex/config-zmod.toml` 控制，与 codex 自身的 `~/.codex/config.toml` 并列。每个 provider 需**两处**配置。

### 1. codex `~/.codex/config.toml`

照常配 provider，`wire_api` 必须是 `responses`（codex 对内只会说 Responses）：

```toml
[model_providers.deepseek]
name     = "DeepSeek"
base_url = "https://api.deepseek.com/v1"   # 接管语义下不参与路由
wire_api = "responses"
env_key  = "DEEPSEEK_API_KEY"

[model_providers.claude]
name     = "Claude"
base_url = "https://api.anthropic.com"
wire_api = "responses"
env_key  = "ANTHROPIC_API_KEY"
```

切换时设 `model_provider = "deepseek"`（或 `"claude"`）。

### 2. codez `~/.codex/config-zmod.toml`

表名 = codex 的 `model_provider_id`，决定路由与出口翻译：

```toml
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"   # 可选；缺省用 codex provider 的 base_url
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"              # 连接器自读原始 key
# path    = "/chat/completions"             # 可选，覆盖默认出口路径
# model   = "deepseek-v4-pro"               # 可选，覆盖发往上游的模型名

[llm-switch.providers.claude]
connector          = "anthropic"
base_url           = "https://api.anthropic.com"
auth               = "x-api-key"
key_env            = "ANTHROPIC_API_KEY"
anthropic_version  = "2023-06-01"
default_max_tokens = 8192                    # anthropic max_tokens 兜底（缺省常量 4096）
```

字段说明：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `connector` | 是 | `chat` / `anthropic` / `responses`（responses 不进路由表） |
| `captype` | 否 | `chat`（缺省）屏蔽托管工具能力；`response` 透传 codex 原生能力（见下） |
| `base_url` | 否 | 出口 API 根；缺省回退 codex provider 的 base_url |
| `auth` | 是 | `bearer` / `x-api-key` |
| `key_env` | 否* | 读环境变量取原始 key（运行时主路径） |
| `path` | 否 | 覆盖默认出口 path |
| `model` | 否 | 覆盖发往上游的模型名；缺省用 codex 请求里的 model |
| `anthropic_version` | 否 | x-api-key 形态下的版本头，缺省 `2023-06-01` |
| `default_max_tokens` | 否 | anthropic `max_tokens` 兜底，缺省 4096 |
| `context_window` | 否 | 覆盖 codex 对该模型的上下文窗口（token）。被接管的第三方模型多不在 codex 内置表、走 fallback（硬上限 272k）；配此值经 patch 在 `with_config_overrides` 连同 `max_context_window` 一起抬高，绕过 clamp。例：`1000000` |
| `model_catalog_json` | 否 | 该 provider 专属的模型目录 JSON 路径（支持 `~`）。用此 provider 时 codex 以该表作模型目录，使第三方模型进 `/model` 列表并带推理强度。例：`~/.codex/model-catalog-deepseek.json` |

> **`context_window` 的实施**：codex 对未知模型用 fallback 元数据（`max_context_window = 272_000`），其 `model_context_window` 顶层覆盖会被 clamp 到该上限。`002-llm-switch.patch` 给 `ModelsManagerConfig` 加了 `force_context_window`，由 `core` 的 `to_models_manager_config()` 从 `codez_llm_switch::context_window(provider_id)` 填充，在 `with_config_overrides` 里**同时**设 `context_window` 与 `max_context_window`（不 clamp），从而突破 272k。

> **`model_catalog_json` 的实施**：`/model` 列表由 `build_available_models` 从模型目录（catalog）映射，第三方 slug 不在 codex 内置表、走 fallback（`visibility=None`、`supported_reasoning_levels` 空），既不进列表也无推理强度可选。`002-llm-switch.patch` 在 `core` 加载 config 时，若 `codez_llm_switch::model_catalog_json(provider_id)` 返回路径，则用 `load_llm_switch_model_catalog` 读它并覆盖 `config.model_catalog`——后续 `StaticModelsManager` 即以该表作目录，模型带 `visibility=list` 与 `supported_reasoning_levels` 进 `/model`。catalog JSON 即 codex 的 `ModelsResponse`（`{"models":[{slug,display_name,visibility:"list",supported_reasoning_levels,context_window,...}]}`，必填字段见 `~/.codex/model-catalog-*.json` 示例）。

### fail-safe 与开关

- 文件缺失、`[llm-switch]` 段缺失、`enabled = false`、或 provider 未命中 → 整体**关闭**，走原生 Responses 分支。
- 配置解析出错 → 记 `warn` 并关闭，不让 codex 启动失败。

### 密钥来源（优先级）

1. `key_env` → `std::env::var(key_env)` 读原始 key（**运行时主路径**）。
2. `auth_key` 内联明文 → **仅允许出现在 gitignored 的 `tests/testkey.toml`**；正式 `config-zmod.toml` 一旦出现 `auth_key`，解析期直接报错拒绝启动。
3. 仅 `auth = "bearer"` 且未配 key 时的退路：复用 codex 的 `api_auth.add_auth_headers()` 写 `Authorization: Bearer`。`x-api-key` 形态**无**此退路，必须有 `key_env` / `auth_key`。

## 与 codex-rs 的集成（patch）

本 crate 反向依赖 `codex-api` / `codex-protocol`（path 依赖，见 `Cargo.toml`），属 CLAUDE.md 所述「情况 B」。生产接入分两个 patch：构建集成在共享的 `patches/001-build.patch`，代码接入点在 `patches/002-llm-switch.patch`。触点：

1. **`001-build.patch`** → `core/Cargo.toml`：加 `codez-llm-switch = { path = "../../zmod/llm-switch" }`（普通 path 依赖，不进 workspace members）。
2. **`002-llm-switch.patch`** → `core/src/client.rs`：`ModelClient::new` 增形参 `model_provider_id: String`，存进 `ModelClientState`（含 `memories/write` 等所有调用点同步改）。
3. **`002-llm-switch.patch`** → `core/src/client.rs` 的 `stream_responses_api`：按 `codez_llm_switch::route(...)` 二选一——`None` 走原生 `ApiResponsesClient`（保留 `.with_telemetry(...)`），`Some(rt)` 走 `codez_llm_switch::run(...)`，二者落进同一个 `match stream_result`。

> 已知缺口：接管路径不接 codex-api 层的请求/SSE 遥测（连接器用自己的 HTTP/SSE 客户端）；`inference_trace` 与 `map_response_stream` 的 LastResponse/cancellation 两路径都保留。

## 公开 API

```rust
// 路由判定（patch 在 stream_responses_api 里调用）
pub fn route(model_provider_id: &str) -> Option<Route>;
pub fn enabled() -> bool;

// 接管入口（patch 调用契约，签名固定）
pub async fn run(
    rt: Route,
    request: codex_api::ResponsesApiRequest,
    api_auth: codex_api::SharedAuthProvider,
) -> Result<codex_api::ResponseStream, codex_api::ApiError>;

// 配置
pub fn load_config_from_str(toml_text: &str, allow_inline_key: bool) -> Result<Config, ConfigError>;
pub fn load_testkey_config(path: &Path) -> Result<Config, ConfigError>;  // 仅测试，允许内联 auth_key
```

运行时从 `~/.codex/config-zmod.toml`（或 `$CODEX_HOME/config-zmod.toml`）读一次并进程级缓存。

## 模块布局

```
src/
  lib.rs            run()/route()/enabled() 入口；配置缓存
  config.rs         解析 config-zmod 的 [llm-switch] 段
  http.rs           出口 URL 拼接 + 鉴权头整形 + 密钥解析
  pipeline.rs       TransformPlugin trait + 有序执行（v1 直通）
  transform/        将来的压缩变换落点（v1 空）
  sse.rs            出口 HTTP 请求 + SSE 字节读取（跨 chunk 多字节安全）
  connector/
    mod.rs          Connector trait + EgressCtx + 工厂
    chat.rs         chat 连接器；chat_req.rs 请求构造；chat_sse.rs SSE 翻译
    anthropic.rs    anthropic 连接器；anthropic_req.rs 请求构造；anthropic_sse.rs SSE 翻译
```

## 构建与测试

本 crate 在 codex-rs workspace **之外**且反向依赖其 crate，受 cargo 两条硬约束：作为「非 member 的 path 依赖」时不能用 `[dev-dependencies]`、不能跑 `tests/*.rs`；而 cargo 又拒绝 codex-rs 之外的 member。

**开发期解法**（CLAUDE.md「情况 B 开发期测试」）：用软链把本 crate 临时接进 codex-rs workspace 成为真 member，从而支持 dev-deps（wiremock）与集成测试，共享 codex-rs 的 `Cargo.lock` / `target`。

```bash
# 在仓库根
ln -s ../zmod/llm-switch codex-rs/llm-switch          # 软链(已被 .gitignore 覆盖)
# 在 codex-rs/Cargo.toml 的 [workspace] members 末尾加一行: "llm-switch",

cd codex-rs
cargo test -p codez-llm-switch                         # 全部测试
cargo test -p codez-llm-switch --test chat_request_test # 单个集成测试
cargo clippy -p codez-llm-switch --all-targets         # lint
```

> 纪律：软链 `codex-rs/llm-switch`、`codex-rs/Cargo.toml` 的 members 那行、构建生成的 `codex-rs/Cargo.lock` 改动都是 **dev-only 脚手架**，保持 uncommitted dirty，**绝不**提交进 codex-rs 子树、**不进**任何 patch。`git reset --hard` 会撤掉 members 行（软链因被 ignore 而留存），按上面两步重建即可。

### 实跑测试（门控）

`tests/live_test.rs` 真打 DeepSeek / Claude 端点，默认 `#[ignore]`。需在 `tests/testkey.toml`（**gitignored，含真 key，不得提交**）配好 provider + `auth_key` + `model` 后：

```bash
cargo test -p codez-llm-switch -- --ignored
```

`testkey.toml` 缺失时自动跳过，CI 无 key 也全绿。离线黄金测试（请求构造 / SSE 翻译 / 配置 / 硬失败 / 降级断言）不需要 key。

