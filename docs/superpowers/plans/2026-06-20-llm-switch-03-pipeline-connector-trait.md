# Task 03 — pipeline and connector trait/factory

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the Global Constraints in the [master index](2026-06-20-llm-switch-00-index.md).

**Goal:** Lay out the skeleton and shared contracts for the two-stage pipeline: ① the `TransformPlugin` trait in `pipeline.rs` + ordered execution (v1 is pass-through, only the registration point); `transform/mod.rs` (empty in v1); ② the `Connector` trait, `EgressCtx`, `ConnError` in `connector/mod.rs`, plus a factory that selects a connector by `config::Connector`. The concrete chat/anthropic implementations come in Tasks 04–07; this task provides the empty shells + a factory-dispatch test.

**Spec coverage:** §2 (two-stage pipeline), §3 (module layout), §4 (Connector contract / EgressCtx).

**Files:**
- Create: `zmod/llm-switch/src/pipeline.rs`
- Create: `zmod/llm-switch/src/transform/mod.rs`
- Create: `zmod/llm-switch/src/connector/mod.rs`
- Create: `zmod/llm-switch/src/connector/chat.rs` (empty shell)
- Create: `zmod/llm-switch/src/connector/anthropic.rs` (empty shell)
- Modify: `zmod/llm-switch/src/lib.rs` (wire up modules)
- Test: `zmod/llm-switch/tests/pipeline_test.rs`

**Interfaces:**
- Consumes (Task 01/02): `config::Connector`, `ProviderCfg`.
- Produces (depended on by Tasks 04–08):
  - `pub trait TransformPlugin: Send + Sync { fn transform(&self, req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>; }`
  - `pub fn run_transforms(plugins: &[Box<dyn TransformPlugin>], req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>`
  - `pub fn default_plugins() -> Vec<Box<dyn TransformPlugin>>` (returns an empty vec in v1)
  - `pub struct EgressCtx { pub base_url: String, pub model: String, pub auth: AuthKind, pub key: Option<String>, pub anthropic_version: Option<String>, pub path_override: Option<String>, pub default_max_tokens: Option<u32>, pub http: reqwest::Client }`
  - `pub trait Connector: Send + Sync { async fn run(&self, req: codex_api::ResponsesApiRequest, ctx: &EgressCtx) -> Result<codex_api::ResponseStream, codex_api::ApiError>; }` (use `async-trait` or `impl Future`, see the Step 4 notes)
  - `pub fn make_connector(kind: config::Connector) -> Box<dyn Connector>`
  - `pub enum ConnError { HardFail(String), Http(codex_api::ApiError) }` + `impl From<ConnError> for codex_api::ApiError`
- Note: `EgressCtx` is assembled by Task 08's `run()` from `Route` + `resolve_key`.

---

- [ ] **Step 0: Confirm the ApiError / ResponsesApiRequest paths**

Run: `grep -rn "pub use\|pub struct ResponsesApiRequest\|pub enum ApiError" codex-rs/codex-api/src/lib.rs codex-rs/codex-api/src/common.rs codex-rs/codex-api/src/error.rs`
Confirm that `codex_api::ResponsesApiRequest`, `codex_api::ResponseStream`, and `codex_api::ApiError` are all re-exported from the crate root; if not, record the correct path (e.g. `codex_api::common::ResponsesApiRequest`) and use it consistently in the code that follows.

- [ ] **Step 1: Write a failing test (factory dispatch + transform pass-through)**

Create `zmod/llm-switch/tests/pipeline_test.rs`:

```rust
use codez_llm_switch::{default_plugins, make_connector, run_transforms};
use codez_llm_switch::Connector as ConnectorKind; // re-export of config::Connector

#[test]
fn v1_transforms_are_noop_passthrough() {
    // Build a minimal ResponsesApiRequest; fields follow the source (see the helper notes below).
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
    // Only verify that the factory can produce a Connector for each of the two kinds (after type
    // erasure they can't be compared directly; use an identity method or std::any as a placeholder;
    // here it's enough that it doesn't panic).
    let _chat = make_connector(ConnectorKind::Chat);
    let _anthropic = make_connector(ConnectorKind::Anthropic);
}

// Build a sample request: field names/types follow codex-api/src/common.rs:182 ResponsesApiRequest.
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

> Note: the fields of `sample_request()` must exactly match the source `ResponsesApiRequest`. Before running, align them with `grep -n "pub struct ResponsesApiRequest" -A 25 codex-rs/codex-api/src/common.rs` (and add any fields upstream may have introduced). This is the shared sample used by all later connector tests; Task 04+ will extend it.

- [ ] **Step 2: Run and confirm it fails**

Run: `cd zmod/llm-switch && cargo test --test pipeline_test`
Expected: compilation failure.

- [ ] **Step 3: Implement `pipeline.rs` and `transform/mod.rs`**

`zmod/llm-switch/src/pipeline.rs`:

```rust
use crate::connector::ConnError;

/// Stage ① transform plugins: operate on codex's native ResponsesApiRequest, protocol-agnostic.
/// No implementation in v1; the compressor will live here in the future.
pub trait TransformPlugin: Send + Sync {
    fn transform(&self, req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>;
}

/// Run all plugins in order; abort on the first failure.
pub fn run_transforms(
    plugins: &[Box<dyn TransformPlugin>],
    req: &mut codex_api::ResponsesApiRequest,
) -> Result<(), ConnError> {
    for p in plugins {
        p.transform(req)?;
    }
    Ok(())
}

/// v1 default plugin set: empty.
pub fn default_plugins() -> Vec<Box<dyn TransformPlugin>> {
    crate::transform::plugins()
}
```

`zmod/llm-switch/src/transform/mod.rs`:

```rust
use crate::pipeline::TransformPlugin;

/// v1: no transform plugins. The compressor will register here in the future.
pub fn plugins() -> Vec<Box<dyn TransformPlugin>> {
    Vec::new()
}
```

- [ ] **Step 4: Implement `connector/mod.rs` (trait + EgressCtx + factory + error)**

Async-trait approach: codex-rs already depends on `async-trait` (before running, confirm the version with `grep async-trait codex-rs/Cargo.toml`); add `async-trait = "0.1"` to `Cargo.toml` and use it for the cleanest signature.

`zmod/llm-switch/src/connector/mod.rs`:

```rust
mod chat;
mod anthropic;

use async_trait::async_trait;
use thiserror::Error;

use crate::config::{AuthKind, Connector as ConnectorKind};

/// Internal connector error. HardFail = unsupported in v1 / cannot be expressed structurally (§4.0 etc.),
/// mapped to ApiError::InvalidRequest;
/// Http = already a codex ApiError (connection setup / status code / stream error).
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

/// Egress context: assembled by Task 08's run() from Route + resolve_key.
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
    /// Spawn only after HTTP + status code + SSE setup complete synchronously (§4.7); returns a
    /// stream of the same shape as stream_request.
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

- [ ] **Step 5: chat/anthropic empty shells**

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
        // Concrete logic is in Task 04 (request) + Task 05 (SSE) + Task 08 (wiring).
        Err(codex_api::ApiError::InvalidRequest {
            message: "chat connector not implemented yet".into(),
        })
    }
}
```

`zmod/llm-switch/src/connector/anthropic.rs`: same structure, `pub struct AnthropicConnector;`, with message `"anthropic connector not implemented yet"`.

- [ ] **Step 6: Wire up modules and re-export in `lib.rs`**

Add to the module-declaration section at the top of `lib.rs`:

```rust
mod pipeline;
mod transform;
mod connector;

pub use pipeline::{default_plugins, run_transforms, TransformPlugin};
pub use connector::{make_connector, ConnError, Connector as ConnectorTrait, EgressCtx};
```

> Naming-collision note: `config::Connector` (enum) and `connector::Connector` (trait) share a name. Externally: `pub use config::Connector;` (the enum, already exported in Task 01), and the trait is renamed to `ConnectorTrait`. `make_connector`'s parameter is the enum. In tests, `use codez_llm_switch::Connector as ConnectorKind` picks the enum.

Add `async-trait = "0.1"` to `[dependencies]` in `Cargo.toml`.

- [ ] **Step 7: Run the tests and confirm they pass**

Run: `cd zmod/llm-switch && cargo test --test pipeline_test`
Expected: 2 tests PASS (transform pass-through, factory dispatch).

- [ ] **Step 8: Commit**

```bash
git add zmod/llm-switch/src/pipeline.rs zmod/llm-switch/src/transform zmod/llm-switch/src/connector zmod/llm-switch/src/lib.rs zmod/llm-switch/Cargo.toml zmod/llm-switch/tests/pipeline_test.rs
git commit -m "feat(llm-switch): pipeline transform trait + connector trait/factory skeleton"
```
