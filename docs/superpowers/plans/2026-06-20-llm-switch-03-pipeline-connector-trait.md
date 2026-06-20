# Task 03 — pipeline 与 connector trait/工厂

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints。

**Goal:** 搭出两层管线的骨架与公共契约:① `pipeline.rs` 的 `TransformPlugin` trait + 有序执行(v1 直通,仅注册点);`transform/mod.rs`(v1 空);② `connector/mod.rs` 的 `Connector` trait、`EgressCtx`、`ConnError`、按 `config::Connector` 选连接器的工厂。chat/anthropic 的具体实现是 Task 04–07,本任务给出空壳 + 工厂分派测试。

**覆盖 spec:** §2(两层管线)、§3(模块布局)、§4(Connector 契约 / EgressCtx)。

**Files:**
- Create: `zmod/llm-switch/src/pipeline.rs`
- Create: `zmod/llm-switch/src/transform/mod.rs`
- Create: `zmod/llm-switch/src/connector/mod.rs`
- Create: `zmod/llm-switch/src/connector/chat.rs`(空壳)
- Create: `zmod/llm-switch/src/connector/anthropic.rs`(空壳)
- Modify: `zmod/llm-switch/src/lib.rs`(挂模块)
- Test: `zmod/llm-switch/tests/pipeline_test.rs`

**Interfaces:**
- Consumes(Task 01/02):`config::Connector`、`ProviderCfg`。
- Produces(Task 04–08 依赖):
  - `pub trait TransformPlugin: Send + Sync { fn transform(&self, req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>; }`
  - `pub fn run_transforms(plugins: &[Box<dyn TransformPlugin>], req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>`
  - `pub fn default_plugins() -> Vec<Box<dyn TransformPlugin>>`(v1 返回空 vec)
  - `pub struct EgressCtx { pub base_url: String, pub model: String, pub auth: AuthKind, pub key: Option<String>, pub anthropic_version: Option<String>, pub path_override: Option<String>, pub default_max_tokens: Option<u32>, pub http: reqwest::Client }`
  - `pub trait Connector: Send + Sync { async fn run(&self, req: codex_api::ResponsesApiRequest, ctx: &EgressCtx) -> Result<codex_api::ResponseStream, codex_api::ApiError>; }`(用 `async-trait` 或 `impl Future`,见 Step 4 说明)
  - `pub fn make_connector(kind: config::Connector) -> Box<dyn Connector>`
  - `pub enum ConnError { HardFail(String), Http(codex_api::ApiError) }` + `impl From<ConnError> for codex_api::ApiError`
- 说明:`EgressCtx` 由 Task 08 的 `run()` 从 `Route` + `resolve_key` 组装。

---

- [ ] **Step 0: 确认 ApiError / ResponsesApiRequest 路径**

Run: `grep -rn "pub use\|pub struct ResponsesApiRequest\|pub enum ApiError" codex-rs/codex-api/src/lib.rs codex-rs/codex-api/src/common.rs codex-rs/codex-api/src/error.rs`
确认 `codex_api::ResponsesApiRequest`、`codex_api::ResponseStream`、`codex_api::ApiError` 都从 crate 根重导出;若不是,记录正确路径(如 `codex_api::common::ResponsesApiRequest`)并在后续代码统一用之。

- [ ] **Step 1: 写失败测试(工厂分派 + transform 直通)**

创建 `zmod/llm-switch/tests/pipeline_test.rs`:

```rust
use codez_llm_switch::{default_plugins, make_connector, run_transforms};
use codez_llm_switch::Connector as ConnectorKind; // config::Connector 重导出

#[test]
fn v1_transforms_are_noop_passthrough() {
    // 构造一个最小 ResponsesApiRequest;字段以源码为准(见下方 helper 注释)。
    let mut req = sample_request();
    let before = serde_json::to_value(&req).unwrap();
    let plugins = default_plugins();
    assert!(plugins.is_empty(), "v1 has no transforms");
    run_transforms(&plugins, &mut req).expect("noop ok");
    let after = serde_json::to_value(&req).unwrap();
    assert_eq!(before, after, "v1 transform must not mutate the request");
}

#[test]
fn factory_returns_distinct_connectors() {
    // 仅验证工厂能为两种 kind 各造出一个 Connector(类型擦除后无法直接比较,
    // 用一个标识方法或 std::any 占位;此处验证不 panic 即可)。
    let _chat = make_connector(ConnectorKind::Chat);
    let _anthropic = make_connector(ConnectorKind::Anthropic);
}

// 构造样本请求:字段名/类型以 codex-api/src/common.rs:182 ResponsesApiRequest 为准。
fn sample_request() -> codex_api::ResponsesApiRequest {
    codex_api::ResponsesApiRequest {
        model: "test".into(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        include: vec![],
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}
```

> 注:`sample_request()` 字段必须与源码 `ResponsesApiRequest` 完全一致。执行前 `grep -n "pub struct ResponsesApiRequest" -A 25 codex-rs/codex-api/src/common.rs` 对齐(若上游新增字段,补上)。这是后续所有连接器测试共用的样本,Task 04+ 会扩展它。

- [ ] **Step 2: 运行确认失败**

Run: `cd zmod/llm-switch && cargo test --test pipeline_test`
Expected: 编译失败。

- [ ] **Step 3: 实现 `pipeline.rs` 与 `transform/mod.rs`**

`zmod/llm-switch/src/pipeline.rs`:

```rust
use crate::connector::ConnError;

/// ① 层变换插件:作用于 codex 原生 ResponsesApiRequest,协议无关。
/// v1 无实现;将来 compressor 落在这里。
pub trait TransformPlugin: Send + Sync {
    fn transform(&self, req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>;
}

/// 有序执行所有插件;任一失败即中止。
pub fn run_transforms(
    plugins: &[Box<dyn TransformPlugin>],
    req: &mut codex_api::ResponsesApiRequest,
) -> Result<(), ConnError> {
    for p in plugins {
        p.transform(req)?;
    }
    Ok(())
}

/// v1 默认插件集:空。
pub fn default_plugins() -> Vec<Box<dyn TransformPlugin>> {
    crate::transform::plugins()
}
```

`zmod/llm-switch/src/transform/mod.rs`:

```rust
use crate::pipeline::TransformPlugin;

/// v1:无变换插件。将来 compressor 在此注册。
pub fn plugins() -> Vec<Box<dyn TransformPlugin>> {
    Vec::new()
}
```

- [ ] **Step 4: 实现 `connector/mod.rs`(trait + EgressCtx + 工厂 + 错误)**

异步 trait 方案:codex-rs 已依赖 `async-trait`(执行前 `grep async-trait codex-rs/Cargo.toml` 确认版本);在 `Cargo.toml` 加 `async-trait = "0.1"` 并用之,签名最简洁。

`zmod/llm-switch/src/connector/mod.rs`:

```rust
mod chat;
mod anthropic;

use async_trait::async_trait;
use thiserror::Error;

use crate::config::{AuthKind, Connector as ConnectorKind};

/// 连接器内部错误。HardFail = v1 不支持/结构无法表达(§4.0 等),映射成 ApiError::InvalidRequest;
/// Http = 已是 codex ApiError(建连/状态码/流错误)。
#[derive(Debug, Error)]
pub enum ConnError {
    #[error("llm-switch unsupported: {0}")]
    HardFail(String),
    #[error(transparent)]
    Http(#[from] codex_api::ApiError),
}

impl From<ConnError> for codex_api::ApiError {
    fn from(e: ConnError) -> Self {
        match e {
            ConnError::Http(api) => api,
            ConnError::HardFail(msg) => codex_api::ApiError::InvalidRequest { message: msg },
        }
    }
}

/// 出口上下文:由 Task 08 run() 从 Route + resolve_key 组装。
pub struct EgressCtx {
    pub base_url: String,
    pub model: String,
    pub auth: AuthKind,
    pub key: Option<String>,
    pub anthropic_version: Option<String>,
    pub path_override: Option<String>,
    pub default_max_tokens: Option<u32>,
    pub http: reqwest::Client,
}

#[async_trait]
pub trait Connector: Send + Sync {
    /// 同步完成 HTTP+状态码+SSE 建立后才 spawn(§4.7);返回与 stream_request 同型的流。
    async fn run(
        &self,
        req: codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError>;
}

pub fn make_connector(kind: ConnectorKind) -> Box<dyn Connector> {
    match kind {
        ConnectorKind::Chat => Box::new(chat::ChatConnector),
        ConnectorKind::Anthropic => Box::new(anthropic::AnthropicConnector),
    }
}
```

- [ ] **Step 5: chat/anthropic 空壳**

`zmod/llm-switch/src/connector/chat.rs`:

```rust
use async_trait::async_trait;
use super::{Connector, EgressCtx};

pub struct ChatConnector;

#[async_trait]
impl Connector for ChatConnector {
    async fn run(
        &self,
        _req: codex_api::ResponsesApiRequest,
        _ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        // 实体逻辑见 Task 04(请求)+ Task 05(SSE)+ Task 08(接线)。
        Err(codex_api::ApiError::InvalidRequest {
            message: "chat connector not implemented yet".into(),
        })
    }
}
```

`zmod/llm-switch/src/connector/anthropic.rs`:同结构,`pub struct AnthropicConnector;`,消息 `"anthropic connector not implemented yet"`。

- [ ] **Step 6: `lib.rs` 挂模块并重导出**

`lib.rs` 顶部模块声明区补:

```rust
mod pipeline;
mod transform;
mod connector;

pub use pipeline::{default_plugins, run_transforms, TransformPlugin};
pub use connector::{make_connector, ConnError, Connector as ConnectorTrait, EgressCtx};
```

> 命名冲突注意:`config::Connector`(枚举)与 `connector::Connector`(trait)同名。对外:`pub use config::Connector;`(枚举,Task 01 已导出),trait 重命名为 `ConnectorTrait`。`make_connector` 形参是枚举。测试里 `use codez_llm_switch::Connector as ConnectorKind` 取枚举。

在 `Cargo.toml` `[dependencies]` 增 `async-trait = "0.1"`。

- [ ] **Step 7: 运行测试确认通过**

Run: `cd zmod/llm-switch && cargo test --test pipeline_test`
Expected: 2 个测试 PASS(transform 直通、工厂分派)。

- [ ] **Step 8: 提交**

```bash
git add zmod/llm-switch/src/pipeline.rs zmod/llm-switch/src/transform zmod/llm-switch/src/connector zmod/llm-switch/src/lib.rs zmod/llm-switch/Cargo.toml zmod/llm-switch/tests/pipeline_test.rs
git commit -m "feat(llm-switch): pipeline transform trait + connector trait/factory skeleton"
```
