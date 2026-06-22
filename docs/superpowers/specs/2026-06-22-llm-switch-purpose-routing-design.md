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
| 范围外 | agent 委派 / delegate_task 工具 / 主会话运行中切模型 / 按激活 skills 路由 / title-summary 独立模型 |

### 1.2 为什么不按 skills 路由

codex 的 skills(`core/src/skills.rs`、`core/src/context/available_skills_instructions.rs`)是**纯上下文注入**:把 skill 内容渲染成文本指令塞进模型上下文,完全不参与模型/session 选择,与模型选择正交。让「加载某 skill → 换模型」要新引入 skill→model 耦合,本质偏向 oh-my-opencode 的重量级路线,与「自动内部路由」不是一类,故排除。若以后想要,更合适的形态是 skill 元数据声明 `preferred_model`,而非路由维度。

### 1.3 title/summary 的落空说明

调研未在 codex 代码中找到**独立的标题/摘要生成 LLM 调用点**——compact 自身内联完成上下文摘要,没有单独的 title-gen 请求。故 title/summary **第一期不在范围**。第一期实际可路由的内部用途为 compact、review、memory 三个。

## 2. 架构与数据流

```
codex 内部调用点 ──带 SessionSource──> ModelClient.stream_request
                          │
                          ▼
        codez_llm_switch::route(provider_id, Some(&session_source))
                          │  在 llm-switch 内:purpose = purpose_from_source(source)
                          ▼  查 [llm-switch.purpose] 映射表
              compact -> "deepseek-cheap"
              review  -> "claude-sonnet"
              memory  -> "deepseek-cheap"
                          │ 命中 -> 取该 provider 的后端配置
                          ▼ 既有 connector(chat / anthropic)转发到对应后端
```

要点:

1. **purpose 解析在 llm-switch 内**(传入 `SessionSource`,llm-switch 映射成内部 `Purpose` 枚举),codex 不引入新概念。
2. **两级路由 + 兜底**:先看 purpose 有无专属映射;没有则回退到原有「按 provider_id 路由」;再没有则回退主模型原生路径。
3. **patch 改动局限**:`route()` 调用点多传一个 source 参数(已可读);compact 处额外打 `Compact` 标记;review/memory 走独立 source 零额外改动。

### 2.1 一个关键约束:compact 拿不到 purpose(已确认接受较大 patch)

两类内部调用机制不同:

- **review / thread-spawn / memory(若起独立 session)**:起**独立子 agent**(独立 Config + 独立 ModelClient),其 `session_source` 本身就是 `SubAgent(Review)` / `Internal(MemoryConsolidation)` 等。✅ purpose 能直接从 `state.session_source` 读到。
- **compact**:**不起子 agent**,复用主 session 的 client(`core/src/compact.rs` 的 `sess.services.model_client.new_session()`)。该主 client 的 `session_source` 是创建主会话时的值(`Cli`/`VSCode`),**不是 `Compact`**。❌ 若不处理,`route()` 看到的是主 source,识别不出压缩任务。

**结论**:compact 要能路由,patch 必须让那次请求可识别为 `Compact`。已确认接受这块稍大的 patch(见 §4)。

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
- 指向不存在的 provider id 时,**该用途映射忽略**(降级到原生),并 `tracing::warn` 一次,不报错。
- `purpose` 表缺失 → 所有用途都走原生路径(fail-safe),等于该特性未启用。
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
route(provider_id: &str, source: Option<&SessionSource>) -> Option<Route>:
  1. 若 enabled == false                    -> None(原生路径)
  2. purpose = source.and_then(purpose_from_source)
  3. 若 purpose 命中且 [purpose] 表里配了该用途:
       target_id = purpose_map[purpose]
       若 providers[target_id] 存在        -> Some(Route{target_id, cfg})   // 用途路由生效
       否则 tracing::warn 一次,继续往下
  4. 回退:若 providers[provider_id] 存在    -> Some(Route{provider_id, cfg}) // 原有按 provider 路由
  5. 否则                                    -> None(原生主模型路径)
```

耦合方案 A:`route()` 直接收 `Option<&SessionSource>` 并在 llm-switch 内 match。llm-switch 已反向依赖 `codex_protocol`(`SessionSource` 所在 crate),不新增依赖边;match 逻辑收在 llm-switch 内更内聚。

## 5. patch 改动清单(对 codex-rs 的最小侵入)

`patches/llm-switch.patch` 在现有基础上增量改动:

1. **`route()` 调用点改签名**(`core/src/client.rs`,llm-switch 集成点):
   `codez_llm_switch::route(&provider_id)` → `codez_llm_switch::route(&provider_id, Some(&self.client.state.session_source))`。`session_source` 已是 `ModelClientState` 字段(`client.rs` 中 `ModelClientState` 定义),直接可读;review / memory **零额外改动**即生效。

2. **compact 打标记**(`core/src/compact.rs` 一带):compact 复用主 client,其 session_source 是主 source,需让这一次请求携带 `Compact`。最小做法:给 `ModelClientSession`(或其请求路径)增加一个 per-session 的 `source_override: Option<SessionSource>`,compact 处 `new_session()` 后设为 `SubAgent(Compact)`;`route()` 调用点优先读 override、否则读 `state.session_source`。改动局限 compact 一处 + client.rs 一个字段。

3. **memory 确认**:确认 memory 调用走独立 source(`Internal(MemoryConsolidation)` 或 `SubAgent(MemoryConsolidation)`)。若确为独立 client,则零额外改动;`purpose_from_source` 同时认这两个变体。

> 注:第 2 项 `source_override` 的精确落点(加在 `ModelClientSession` 字段还是沿调用链传参)留到 writing-plans 阶段对着代码定;本 spec 只锁定约束「compact 那次请求必须可识别为 `Compact`」。

## 6. 错误处理与测试

错误处理(全程 fail-safe,遵守 zmod「读不到配置不报错」约定):

- `[purpose]` 表缺失 / 某用途未配 → 该用途返回 None 走原生,不报错。
- purpose 指向不存在的 provider id → `tracing::warn` 一次,降级原生路径。
- 已启用但后端请求失败 → 沿用 llm-switch 现有错误转换(返回 `ApiError`),不吞错。

测试(用 CLAUDE.md 约定的软链 + member 方式跑 `cd codex-rs && cargo test -p codez-llm-switch`):

- `purpose_from_source`:各 `SubAgentSource` / `InternalSessionSource` 变体 → 正确 `Purpose`;主 source(Cli/VSCode/Exec)→ None。
- 两级 route:用途命中优先;用途未配回退 provider;provider 也无 → None;指向不存在 provider 的 warn + 降级。
- 配置解析:`[purpose]` 表存在 / 缺失 / 部分配置三种。
- 端到端(wiremock):带 `Compact` source 的请求被转发到 purpose 映射的后端;主 source 请求不被接管(走原生)。

