# zmod/llm-switch — 按用途编排多模型(purpose routing)设计文档

日期:2026-06-22
状态:已批准设计,待写实现计划

## 1. 目标与背景

灵感来自 `~/Sites/skycode/3rd/multi/oh-my-opencode` 的卖点 **"Mix and match models. Orchestrate them by purpose."** —— 按任务用途混搭不同模型。oh-my-opencode 的实现是**重量级 agent 委派**:主编排 agent(Atlas)调用 `delegate_task(category=...)` 工具,把子任务主动派给子 agent,每个 category(quick/ultrabrain/writing...)绑定一套「模型 + variant + prompt」。

codez 选择**轻量形态:自动内部路由**。主模型行为完全不变,不给它新增任何工具;只让 codex 那些**内部子任务**各自路由到为该用途配置的模型,再借现有 [`zmod/llm-switch`](2026-06-20-llm-switch-design.md) 把请求转发到对应后端。

本特性是 llm-switch 的增量,不新建 crate、不新建 patch,沿用 `codez-llm-switch` / `patches/llm-switch.patch`。

### 1.1 关键决策(已确认)

| 决策点 | 选择 |
|---|---|
| 编排形态 | **自动内部路由(轻量)**:主模型不变、不加工具,只路由内部子任务 |
| 路由层 | **llm-switch 统一路由**:`route()` 增加 source 入参,在 llm-switch 内解析 purpose 并查表 |
| 路由键 | 复用 codex 现成的 `SessionSource` / `SubAgentSource`,**codex 侧不引入新概念** |
| 第一期用途 | **compact、review、memory** 三个 |
| 耦合方案 | **方案 A**:`route()` 直接收 `Option<&SessionSource>`,在 llm-switch 内 match(llm-switch 已反向依赖 codex_protocol,不新增依赖边) |
| 回退策略 | 两级:用途映射优先 → 原有按 provider_id 路由 → 原生主模型(fail-safe) |
| namespace 工具冲突 | **purpose 路由专属 fail-safe**:purpose 命中但请求含 namespace 工具(llm-switch v1 不可表达)时,放弃用途路由、**回退 provider-id 路由**(沿两级链下落,多数情形最终到原生,功能无损)而非硬失败;**provider-id 路由保留 v1 硬失败契约不变**(见 §4.1) |
| WebSocket 绕过 | **purpose 命中须在 `stream()` 选传输前跳过 WebSocket**,强制走 HTTP 让 route() 接管,否则默认 WS-on 的主会话下特性静默失效(见 §4.2) |
| 范围外 | agent 委派 / delegate_task 工具 / 主会话运行中切模型 / 按激活 skills 路由 / title-summary 独立模型 |

### 1.2 为什么不按 skills 路由

codex 的 skills(`core/src/skills.rs`、`core/src/context/available_skills_instructions.rs`)是**纯上下文注入**:把 skill 内容渲染成文本指令塞进模型上下文,完全不参与模型/session 选择,与模型选择正交。让「加载某 skill → 换模型」要新引入 skill→model 耦合,本质偏向 oh-my-opencode 的重量级路线,与「自动内部路由」不是一类,故排除。若以后想要,更合适的形态是 skill 元数据声明 `preferred_model`,而非路由维度。

### 1.3 title/summary 的落空说明

调研未在 codex 代码中找到**独立的标题/摘要生成 LLM 调用点**——compact 自身内联完成上下文摘要,没有单独的 title-gen 请求。故 title/summary **第一期不在范围**。第一期实际可路由的内部用途为 compact、review、memory 三个。

## 2. 架构与数据流

```
codex 内部调用点 ──带 SessionSource──> ModelClientSession::stream()
                          │
                          ├─[传输选择]─ should_bypass_websocket(provider_id, source)?
                          │              命中 purpose → 跳过 WebSocket,强制走 HTTP(§4.2)
                          ▼
                  stream_responses_api()  ── 构造好 ResponsesApiRequest 后
                          │
                          ▼
        codez_llm_switch::route(provider_id, Some(&source), &request)
                          │  在 llm-switch 内:purpose = purpose_from_source(source)
                          ▼  查 [llm-switch.purpose] 映射表(+ §4.1 namespace 预检)
              compact -> "deepseek-cheap"
              review  -> "claude-sonnet"
              memory  -> "deepseek-cheap"
                          │ 命中 -> 取该 provider 的后端配置
                          ▼ 既有 connector(chat / anthropic)转发到对应后端
```

要点:

1. **purpose 解析在 llm-switch 内**(传入 `SessionSource`,llm-switch 映射成内部 `Purpose` 枚举),codex 不引入新概念。
2. **两级路由 + 兜底**:先看 purpose 有无专属映射;没有则回退到原有「按 provider_id 路由」;再没有则回退主模型原生路径。
3. **传输层绕过(§4.2)**:purpose 命中时须在 `stream()` 选传输前跳过 WebSocket、强制走 HTTP,否则 route() 被 WS 路径绕过、特性静默失效。
4. **patch 改动局限**:`route()` 调用点多传 source + request 参数;`stream()` 顶部加 WS 绕过判断;compact 与 memory phase 1 处各打 source override 标记;review、memory phase 2 走独立 source 零额外改动(细分见 §2.1、§5)。

### 2.1 关键约束:三类内部调用的 source 可见性各不同

第一期三个用途、实际五段 LLM 调用,按「`route()` 能否直接看出真实用途」分两类:

**A 类——起独立 session/thread,source 天生正确(零额外改动)**:

- **review**:`tasks/review.rs` 起独立子 agent,`session_source = SubAgent(Review)`。✅
- **memory phase 2(consolidation)**:`memories/write/src/runtime.rs:307` 的 `spawn_consolidation_agent` 起独立 thread,`session_source = Internal(MemoryConsolidation)`、`dynamic_tools: Vec::new()`。✅

**B 类——直连复用某 client、source 是主线程 source,必须打 override 标记**:

- **compact**:`core/src/compact.rs` 复用主 session 的 client(`sess.services.model_client.new_session()`),session_source 是主会话值(`Cli`/`VSCode`),**不是 `Compact`**。
- **memory phase 1(extraction)**:`memories/write/src/runtime.rs:229` 直连 `ModelClient::new()`,用 `config_snapshot.session_source`(=主线程 source,`runtime.rs:226`)构造,随后 `runtime.rs:253` 直接 `.stream()`。**不是 `Memory`**。

**结论**:B 类两段(compact、memory phase 1)若不处理,`route()` 看到主 source、识别不出真实用途。两者用同一个 source override 机制解决(见 §5 第 2 项)。已确认接受这块稍大的 patch。

## 3. 配置格式

在 `~/.codex/config-zmod.toml` 的 `[llm-switch]` 下新增一张 `purpose` 映射表,value 为已有 `providers` 里的 provider id:

```toml
[llm-switch]
enabled = true

# 后端 provider(沿用现有结构,不变)
[llm-switch.providers.deepseek-cheap]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"
model     = "deepseek-v3"

[llm-switch.providers.claude-sonnet]
connector = "anthropic"
base_url  = "https://api.anthropic.com"
auth      = "x-api-key"
key_env   = "ANTHROPIC_API_KEY"
model     = "claude-sonnet-4-5"

# 新增:用途 -> provider id 映射
[llm-switch.purpose]
compact = "deepseek-cheap"
review  = "claude-sonnet"
memory  = "deepseek-cheap"
```

规则:

- `purpose` 表的 key 是固定枚举名(`compact` / `review` / `memory`),value 必须是 `providers` 里已存在的 id。
- 指向不存在的 provider id 时,**该用途映射忽略**,`tracing::warn` 一次,**回退 provider-id 路由**(§4 第 3a→4 步),不报错。
- `purpose` 表缺失 / 某用途未配 → 该用途不命中,**回退 provider-id 路由**;若 provider-id 也未接管才走原生(等于该特性对这次请求未生效)。
- **复用现有 `providers` 配置**,不为每个用途重复写后端;多个用途可指向同一 provider(如 compact 与 memory 都用 `deepseek-cheap`)。

## 4. 路由逻辑(llm-switch 内部)

新增 `Purpose` 枚举与解析函数,`route()` 升级为两级查表。

```rust
// 新增:用途枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose { Compact, Review, Memory }

// 从 codex 的 SessionSource 解析出 Purpose(方案 A:直接 match codex enum)
//   SubAgent(Review)                 -> Some(Review)
//   SubAgent(Compact)                -> Some(Compact)
//   SubAgent(MemoryConsolidation)    -> Some(Memory)
//   Internal(MemoryConsolidation)    -> Some(Memory)
//   其余(Cli/VSCode/Exec/...)        -> None
pub fn purpose_from_source(source: &SessionSource) -> Option<Purpose>
```

`route()` 新签名与逻辑:

```text
route(provider_id: &str, source: Option<&SessionSource>, request: &ResponsesApiRequest) -> Option<Route>:
  1. 若 enabled == false                          -> None(跳过 llm-switch,原生路径)
  2. purpose = source.and_then(purpose_from_source)
  3. 【purpose 分支】若 purpose 命中且 [purpose] 表里配了该用途:
       target_id = purpose_map[purpose]
       a. 若 providers[target_id] 不存在          -> warn 一次,继续第 4 步      // 坏映射
       b. 若 request 含不可表达工具(见 §4.1 预检) -> warn 一次,继续第 4 步      // namespace fail-safe
       c. 否则                                     -> Some(Route{target_id, cfg}) // 用途路由生效
  4. 【provider-id 分支】若 providers[provider_id] 存在 -> Some(Route{provider_id, cfg}) // 原有 v1 路由,不做 namespace 降级
  5. 否则                                          -> None(原生主模型路径)
```

**两级回退语义(钉死,消除歧义)**:第 3 步任何不满足(purpose 未命中 / 用途未配 / 坏映射 / 含 namespace 工具)都**不直接跳到原生**,而是**继续第 4 步回退 provider-id 路由**;只有 provider-id 也未接管(第 5 步)才最终走原生。即文档全篇的「两级:purpose → provider_id → 原生」是唯一权威口径;后文凡提「降级 / 走原生」均指**沿此链下落**,不是越级跳到原生。

**为何预检在 `route()` 内(收 `&request`)而非调用点**:这样「该不该 namespace 降级」由 route() 内部「当前在 purpose 分支还是 provider-id 分支」决定——purpose 分支(第 3b 步)才降级,provider-id 分支(第 4 步)绝不降级、保留 v1 硬失败契约。区分依据是**分支**而非目标 provider id,故即使 `purpose_map[purpose] == provider_id`(目标与主 provider 相同)也不产生歧义:含 namespace 工具时第 3b 跳过 → 落到第 4 步以 provider-id 路由返回 → 连接器照 v1 硬失败(符合「用户已显式把主会话接管到该 provider」的既有契约)。`Route` 因此**无需**新增来源/目的标记。

耦合方案 A:`route()` 直接收 `Option<&SessionSource>` 并在 llm-switch 内 match。llm-switch 已反向依赖 `codex_protocol`(`SessionSource` 所在 crate),不新增依赖边;match 逻辑收在 llm-switch 内更内聚。

### 4.1 namespace 工具冲突:purpose 路由专属 fail-safe

**背景**:llm-switch v1 对带 `namespace` 的函数调用 / 工具定义是**硬失败**——`connector/chat_req.rs:75`、`connector/anthropic_req.rs:110` 在构造请求体时遇到 `namespace.is_some()` 直接返回 `ConnError::HardFail`(见 [v1 设计](2026-06-20-llm-switch-design.md) §4.0b 与项目记忆 `llm-switch-namespace-gate`)。这对**主会话(provider-id 路由)**是合理的「响亮失败」:用户显式把主会话接到某 provider,配错了 namespace_tools 就该报错提醒。

**问题**:三个用途的工具暴露面不同——

- **compact**:`compact.rs` 以 `Prompt { ..Default::default() }` 起请求,无工具 → 安全。
- **memory**:phase 1 抽取(`runtime.rs:229`)起请求不带动态工具,phase 2 合并(`runtime.rs:307`)以 `dynamic_tools: Vec::new()` 起 agent,两段均无工具 → 安全。
- **review**:`tasks/review.rs` 起**带完整工具集**的子 agent(只关掉 web_search/collab/multi-agent/csv,**保留标准工具 + 用户配置的 MCP 工具**)。用户一旦配了 MCP server,review 请求就携带 `mcp__...` 这类 namespace 工具 → 命中 purpose 路由后硬失败。

用户配 `[purpose] review = ...` 时只想换模型,**不会联想到要去关 MCP 工具**;让它硬失败违反 §6 的 fail-safe 原则。

**决策(选项 2 收敛版)**:purpose 路由命中、但请求含 llm-switch 不可表达的 namespace 工具时,**放弃用途路由、回退 provider-id 路由(§4 第 3b→4 步)**(`tracing::warn` 一次),不硬失败。多数情形下 provider-id 也不接管 → 最终落到原生 Responses API,而原生本就支持 namespace 工具,故**功能无损**,代价仅是这一次 review 拿不到 purpose 模型的省钱收益。**此降级只发生在 purpose 分支;provider-id 分支(第 4 步)保留 v1 硬失败契约不变**(含 `purpose_map[purpose]==provider_id` 的同 id 情形,理由见 §4 末段)。

**为什么现有 captype 抑制救不了这一例**:v1 已有 `suppress_hosted_tools(provider_id)` / captype 机制(`spec_plan.rs` 从源头按 `config.model_provider_id` 屏蔽 namespace/web_search/image 托管工具)。但该抑制按 **codex 侧的 `config.model_provider_id`** 决定,而 purpose 路由的目标后端是 **llm-switch 在 stream 时内部选的**,codex 配置里的 `model_provider_id` 仍是主 provider。典型用法(主会话走原生 GPT、`[purpose] review` 指向便宜 chat 后端)下,主 provider 未被抑制 → namespace 工具开着 → review 携带 MCP 工具 → 被 purpose 路由到 chat 后端 → 撞硬失败。故现有 captype 覆盖不到 purpose 路由,§4.1 的运行时预检 + 降级是必需的。

**落点约束**:namespace 硬失败有**两个独立来源**,预检必须**同时覆盖**,否则 review 首轮请求会漏检:

1. **工具定义**:在 `request.tools`(`codex-api/src/common.rs` 的 `ResponsesApiRequest.tools`),连接器 `map_tools(&req.tools)` 对非 `function` 类型工具定义硬失败(`chat_req.rs:323`、`anthropic_req.rs` 同理)。**review 子 agent 首轮请求的 `mcp__...` namespace 工具定义就在这里**,此时 `input` 里还没有任何 function call。
2. **函数调用**:在 `request.input` 的 `ResponseItem::FunctionCall { namespace, .. }`,连接器 `namespace.is_some()` 时硬失败(`chat_req.rs:75`、`anthropic_req.rs:110`),出现在后续轮次。

因此预检判定为:`fn request_has_namespace_tools(req: &ResponsesApiRequest) -> bool` —— **既扫 `req.tools`(有无非 `function` 类型的工具定义)、又扫 `req.input`(有无 `namespace.is_some()` 的 function call)**,任一命中即返回 true。该函数在 llm-switch 内实现,由 `route()` 在 purpose 分支(§4 第 3b 步)调用;命中则放弃用途路由、回退 provider-id。`route()` 收 `&request` 入参即可完成,无需调用点参与判断,也无需给 `Route` 加来源标记(理由见 §4 末段)。

### 4.2 传输层:purpose 命中前必须绕过 WebSocket

**问题**:`route()` 只在 HTTP Responses 路径(`stream_responses_api`)被调用,但 `ModelClientSession::stream()`(`core/src/client.rs:1619`)在 `responses_websocket_enabled()` 为真时**先走 WebSocket**(`stream_responses_websocket()`,`client.rs:1633-1654`),成功就 `return`,**不进 `stream_responses_api()`、route() 永不被调用**。

`responses_websocket_enabled()`(`client.rs:836`)= `provider.info().supports_websockets && !disable_websockets`。OpenAI provider 默认 `supports_websockets = true`(`model-provider-info/src/lib.rs:357`)。

**为何 v1 不受影响、purpose 受影响**:

- **v1 provider-id 路由**:用户**显式**把主会话接到接管 provider,可同时给该 provider 配 `supports_websockets = false`(v1 设计 §配置示例),`stream()` 直接落 HTTP → route() 生效。
- **purpose 路由**:典型场景是**主会话走原生 GPT(默认 WS 开)**,只把内部子任务转发到便宜后端。ModelClient 用的是**主 provider** 的 info,WS 开着 → compact/review/memory 请求先走 WS 成功返回 → 绕过 route() → **purpose 路由静默失效**。这对三个用途全成立(review/memory phase2 虽独立子 agent,但 Config 从主 Config 派生、provider 仍 OpenAI、WS 仍开)。

**为何不能靠关 WS 解决**:purpose 场景下要关的是**主 provider** 的 WS,直接关会伤害主会话本身;`disable_websockets` 那个 `AtomicBool` 是 per-client 永久禁用,且 compact/memory-phase1 复用主 client,同样误伤主会话。

**决策(已确认实现 stream() 绕过)**:在 `stream()` 选传输**之前**、基于 source 预判这趟是否会命中 purpose 路由,命中则**跳过 WS 分支、直接进 `stream_responses_api()`**,让 route() 接管。llm-switch 导出一个**只看 source 的轻量预判**:`fn should_bypass_websocket(provider_id: &str, source: Option<&SessionSource>) -> bool` —— 当 `enabled` 且 `purpose_from_source(source)` 命中且 `[purpose]` 表为该用途配了存在的 provider 时返回 true。`stream()` 顶部在 `responses_websocket_enabled()` 判断里(或之前)`&&` 上 `!should_bypass(...)`。

**已知边界(功能无损)**:`stream()` 顶部预判时 request 尚未构造,**拿不到 tools 做 §4.1 namespace 预检**,只能用 source 预判。故「purpose source + 有映射 + 含 namespace 工具」这一少见组合会:绕 WS 进 HTTP → route() 里 namespace 预检触发(§4.1)→ 降级回 provider-id → 若 provider-id 未接管则走原生 **HTTP**(本可走 WS,这趟损失 WS 优化)。HTTP Responses 完全可用,**功能无损**,仅损失该趟 WS。此组合罕见(主要是配了 MCP 的 review),不构成正确性问题。

## 5. patch 改动清单(对 codex-rs 的最小侵入)

`patches/llm-switch.patch` 在现有基础上增量改动。**全仓 `ModelClient::new()` 仅两个生产调用点**(`grep` 核准):`core/src/session/session.rs:1019`(主会话,现有 patch 已覆盖)与 `memories/write/src/runtime.rs:229`(memory phase 1,**现有 patch 未覆盖**)。`ModelClientState` 若新增字段,两处都必须同步改,否则编译失败。

1. **`route()` 调用点改签名**(`core/src/client.rs`,llm-switch 集成点):
   `codez_llm_switch::route(&provider_id)` → `codez_llm_switch::route(&provider_id, Some(&effective_source), &request)`(`effective_source` = source override 优先、否则 `state.session_source`,见第 2 项;`request` 即将提交的 `ResponsesApiRequest`,供 §4.1 预检)。review(独立子 agent,`SubAgent(Review)`)、memory phase 2(独立 thread,`Internal(MemoryConsolidation)`)**零额外改动**即生效。

2. **source override(compact + memory phase 1 共用机制)**:这两类调用都**直连复用某个 client、其 session_source 是主线程 source**,`route()` 看不出真实用途,必须打标记。统一做法:给 `ModelClientSession`(或其请求路径)增加一个 per-session 的 `source_override: Option<SessionSource>`,`route()` 调用点优先读 override、否则读 `state.session_source`。
   - **compact**(`core/src/compact.rs` 一带):`new_session()` 后设 override = `SubAgent(Compact)`。
   - **memory phase 1**(`memories/write/src/runtime.rs:241` 的 `new_session()` 一带):设 override = `Internal(MemoryConsolidation)`,使抽取请求被识别为 Memory(否则它用 `config_snapshot.session_source` = 主线程 source,`purpose_from_source` 识别不出)。

3. **`memories/write/src/runtime.rs:229` 的 `ModelClient::new()` 改签名**:随 `ModelClientState` 新增的 `model_provider_id` 字段(现有 llm-switch patch 引入),此调用点必须补传对应实参,否则 memory crate 编译失败 / phase 1 缺正确的 provider-id 回退值。此项**与功能无关、纯属编译必需**,但必须纳入 patch 清单。

4. **`stream()` 顶部 WebSocket 绕过**(`core/src/client.rs:1619` 的 `ModelClientSession::stream()`):在 `responses_websocket_enabled()` 判断处 `&&` 上 `!codez_llm_switch::should_bypass_websocket(&self.client.state.model_provider_id, Some(&effective_source))`,使 purpose 命中的请求跳过 WS 分支、直接进 `stream_responses_api()`(理由与边界见 §4.2)。`effective_source` 取法同第 1 项(override 优先)。改动局限 `stream()` 顶部一处条件。

> 注:第 2 项 `source_override` 的精确落点(加在 `ModelClientSession` 字段还是沿调用链传参)留到 writing-plans 阶段对着代码定;本 spec 锁定约束「compact 与 memory phase 1 那两次请求必须分别可识别为 `Compact` / `Memory`」。memory phase 2(consolidation agent)走独立 thread、`dynamic_tools: Vec::new()`(无工具,namespace 安全),无需 override。

## 6. 错误处理与测试

错误处理(全程 fail-safe,遵守 zmod「读不到配置不报错」约定;回退一律沿「purpose → provider-id → 原生」链下落,不越级):

- `[purpose]` 表缺失 / 某用途未配 → 该用途不命中,**回退 provider-id 路由**(provider-id 也无则原生),不报错。
- purpose 指向不存在的 provider id → `tracing::warn` 一次,**回退 provider-id 路由**(§4 第 3a 步)。
- **purpose 命中但请求含 namespace 工具(v1 不可表达,§4.1 预检 true)→ `tracing::warn` 一次,放弃用途路由、回退 provider-id 路由(§4 第 3b 步),不硬失败**。
- provider-id 路由(主会话,第 4 步)遇 namespace 工具 → **保留 v1 硬失败契约**(连接器返回 `ApiError`),不变。
- 已启用但后端请求失败 → 沿用 llm-switch 现有错误转换(返回 `ApiError`),不吞错。

测试(用 CLAUDE.md 约定的软链 + member 方式跑 `cd codex-rs && cargo test -p codez-llm-switch`):

- `purpose_from_source`:各 `SubAgentSource` / `InternalSessionSource` 变体 → 正确 `Purpose`(`SubAgent(Review)`→Review、`SubAgent(Compact)`→Compact、`SubAgent(MemoryConsolidation)` 与 `Internal(MemoryConsolidation)`→Memory);主 source(Cli/VSCode/Exec)→ None。
- 两级 route:用途命中优先;用途未配 / 坏映射 → 回退 provider-id;provider-id 也无 → None;坏映射时 warn + 回退(而非越级原生)。
- `request_has_namespace_tools` 预检:**`req.tools` 含非 `function` 类型工具定义 → true(覆盖 review 首轮)**;`req.input` 含 `namespace.is_some()` 的 FunctionCall → true;两者皆无 → false。
- `should_bypass_websocket`:enabled + purpose 命中 + 映射存在 → true;purpose 未命中 / 未配映射 / 坏映射 / disabled → false。
- **namespace fail-safe(§4.1):purpose 命中 + 预检 true → 放弃用途路由、回退 provider-id(provider-id 无则原生);purpose 命中 + 预检 false → 正常用途路由;provider-id 分支 + namespace 工具 → 仍硬失败(契约不变,含同 id 情形)。**
- 配置解析:`[purpose]` 表存在 / 缺失 / 部分配置三种。
- 端到端(wiremock):带 `Compact` / `Internal(MemoryConsolidation)` source(B 类 override 或 A 类独立 session)的请求被转发到 purpose 映射的后端;主 source 请求不被接管;带 `Review` source + 含 namespace 工具定义的请求不命中 purpose 后端(回退 provider-id;若无则原生)。

> source override 行为(compact / memory phase 1 那两次直连请求被正确标记为 `Compact` / `Memory`)依赖 codex 侧 patch,属集成行为,在 codez crate 单测里以「构造带 override 的 ModelClientSession → route() 返回 purpose 路由」近似覆盖;真实 override 接线的端到端验证在打 patch 后的 `codex-rs` workspace 内进行。

