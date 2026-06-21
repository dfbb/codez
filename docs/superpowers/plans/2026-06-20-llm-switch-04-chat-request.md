# Task 04 — chat Outbound Request Construction

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the Global Constraints in the [master index](2026-06-20-llm-switch-00-index.md), especially "v1 function tools only", "images hard-fail", and "encryption hard-fails".

**Goal:** Implement the **outbound direction** of `connector/chat.rs`: translate the codex `ResponsesApiRequest` into a Chat Completions request JSON. Covers instructions→system, Message→messages, FunctionCall→tool_calls, FunctionCallOutput→tool (including the success prefix), tool tiering, tool_choice/parallel, orphan repair + tool-message reordering, field tiering, and hard-fail variants. The SSE return path is Task 05.

**Spec coverage:** §4.2 (chat request), §4.0/§4.0b (variant / tool-definition hard-fail), §4.6 (FunctionCallOutput), §4.8 (call_id), §4.9 (ContentItem), §4.10 (orphan repair + reorder), §4.11 (tool_choice/parallel), §7.1 (field tiering).

**Files:**
- Create: `zmod/llm-switch/src/connector/chat_req.rs` (request construction, split out of chat.rs to keep the file focused)
- Modify: `zmod/llm-switch/src/connector/chat.rs` (`mod chat_req;`)
- Create: `zmod/llm-switch/tests/chat_request_test.rs`
- Create: `zmod/llm-switch/tests/fixtures/` (holds golden JSON, see Step 7)

**Interfaces:**
- Consumes: `config`, `ConnError` (Task 03), `EgressCtx`, `ResponsesApiRequest`/`ResponseItem`/`ContentItem`/`FunctionCallOutputPayload`.
- Produces (Task 05/08 depend on these):
  - `pub(crate) fn build_chat_request(req: &codex_api::ResponsesApiRequest, ctx: &EgressCtx) -> Result<serde_json::Value, ConnError>`
  - Internal: `map_tools`, `map_tool_choice`, `map_messages` (including orphan repair + reorder), `map_function_call_output`.

---

- [ ] **Step 0: Pin down external type definitions (avoid relying on memory)**

Run and read the output (copy these types into the implementation for reference):
```bash
grep -n "pub enum ContentItem" -A 18 codex-rs/protocol/src/models.rs
grep -n "pub struct FunctionCallOutputPayload" -A 6 codex-rs/protocol/src/models.rs
grep -n "pub enum FunctionCallOutputBody" -A 6 codex-rs/protocol/src/models.rs
grep -n "FunctionCallOutputContentItem" -A 20 codex-rs/protocol/src/models.rs
grep -n "pub enum ResponseItem" -A 120 codex-rs/protocol/src/models.rs
```
Confirm: `ContentItem::{InputText{text},InputImage{image_url,detail},OutputText{text}}`; `FunctionCallOutputBody::{Text(String),ContentItems(Vec<FunctionCallOutputContentItem>)}`; the variants of `FunctionCallOutputContentItem` (expected to include text / image / possibly encrypted — let the actual definition decide the §4.6 tiering). Record the real `FunctionCallOutputContentItem` variants in an implementation comment.

- [ ] **Step 1: Write failing tests (assert each scenario)**

Create `zmod/llm-switch/tests/chat_request_test.rs`. Reuse Task 03's `sample_request()` via a helper (extracted into `tests/common/mod.rs` or duplicated within each test — this plan gives each test its own helper so they can run in any order):

```rust
use codez_llm_switch::testing::build_chat_request_for_test as build; // see Step 6 for the exposure mechanism
use codex_protocol::models::{ContentItem, FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem};
use serde_json::json;

fn ctx() -> codez_llm_switch::EgressCtx { codez_llm_switch::testing::dummy_ctx("deepseek-v4-pro") }

fn base_req() -> codex_api::ResponsesApiRequest {
    let mut r = codez_llm_switch::testing::sample_request();
    r.model = "deepseek-v4-pro".into();
    r
}

#[test]
fn instructions_become_system_message() {
    let mut req = base_req();
    req.instructions = "You are helpful".into();
    req.input = vec![ResponseItem::Message {
        id: None, role: "user".into(),
        content: vec![ContentItem::InputText { text: "hi".into() }],
        phase: None, metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "You are helpful");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"], "hi");
    assert_eq!(v["stream"], true);
    assert_eq!(v["stream_options"]["include_usage"], true);
    assert_eq!(v["model"], "deepseek-v4-pro");
}

#[test]
fn function_call_and_output_pair_by_call_id() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None, name: "get_weather".into(), namespace: None,
            arguments: "{\"city\":\"SF\"}".into(), call_id: "call_1".into(), metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None, call_id: "call_1".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("sunny".into()), success: Some(true),
            },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    // assistant tool_calls
    let asst = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(asst["tool_calls"][0]["id"], "call_1");
    assert_eq!(asst["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(asst["tool_calls"][0]["function"]["arguments"], "{\"city\":\"SF\"}");
    // tool result immediately follows the assistant (§4.10 reorder)
    let asst_idx = msgs.iter().position(|m| m["role"] == "assistant").unwrap();
    assert_eq!(msgs[asst_idx + 1]["role"], "tool");
    assert_eq!(msgs[asst_idx + 1]["tool_call_id"], "call_1");
    assert_eq!(msgs[asst_idx + 1]["content"], "sunny");
}

#[test]
fn tool_output_failure_prefixes_marker() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall { id: None, name: "f".into(), namespace: None, arguments: "{}".into(), call_id: "c".into(), metadata: None },
        ResponseItem::FunctionCallOutput {
            id: None, call_id: "c".into(),
            output: FunctionCallOutputPayload { body: FunctionCallOutputBody::Text("boom".into()), success: Some(false) },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let tool = v["messages"].as_array().unwrap().iter().find(|m| m["role"] == "tool").unwrap();
    assert!(tool["content"].as_str().unwrap().starts_with("[tool error]"));
}

#[test]
fn orphan_tool_call_gets_placeholder_result() {
    // call present, result missing (compaction damage) → inject a synthetic placeholder result, do not hard-fail (§4.10)
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCall {
        id: None, name: "f".into(), namespace: None, arguments: "{}".into(), call_id: "orphan".into(), metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let tool = v["messages"].as_array().unwrap().iter().find(|m| m["role"] == "tool");
    assert!(tool.is_some(), "orphan call must get a placeholder tool result");
    assert_eq!(tool.unwrap()["tool_call_id"], "orphan");
}

#[test]
fn orphan_tool_result_is_dropped() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCallOutput {
        id: None, call_id: "ghost".into(),
        output: FunctionCallOutputPayload { body: FunctionCallOutputBody::Text("x".into()), success: None },
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let has_tool = v["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool");
    assert!(!has_tool, "orphan result with no prior call must be dropped");
}

#[test]
fn tool_choice_none_when_no_tools() {
    // tool_choice present but tools empty → strip (§4.10)
    let mut req = base_req();
    req.tool_choice = "required".into();
    req.tools = vec![];
    let v = build(&req, &ctx()).unwrap();
    assert!(v.get("tool_choice").is_none(), "tool_choice stripped when no tools");
}

#[test]
fn function_tool_definition_maps() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"function","name":"f","description":"d","parameters":{"type":"object"}})];
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tools"][0]["type"], "function");
    assert_eq!(v["tools"][0]["function"]["name"], "f");
}

#[test]
fn parallel_tool_calls_passthrough() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.parallel_tool_calls = false;
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["parallel_tool_calls"], false);
}

// ---- hard-fail assertions ----
#[test]
fn custom_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"custom","name":"freeform"})];
    assert!(build(&req, &ctx()).is_err());
}

#[test]
fn namespaced_function_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCall {
        id: None, name: "f".into(), namespace: Some("mcp".into()),
        arguments: "{}".into(), call_id: "c".into(), metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err());
}

#[test]
fn input_image_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Message {
        id: None, role: "user".into(),
        content: vec![ContentItem::InputImage { image_url: "data:...".into(), detail: None }],
        phase: None, metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err());
}
```

> You also need to add one `*_hard_fails` test for each of the following variants (same structure as above): `LocalShellCall`, `ToolSearchCall`, `WebSearchCall`, `ImageGenerationCall`, `CustomToolCall`, `Compaction`, `ContextCompaction`, `AgentMessage` containing `EncryptedContent`, and `Other`. For each, construct the corresponding `ResponseItem`, put it into `input`, and assert `build(...).is_err()`. `CompactionTrigger` and `Reasoning` are the inverse: assert it does **not** error and does **not** appear in messages (dropped on the outbound path).

- [ ] **Step 2: Run to confirm failure**

Run: `cd zmod/llm-switch && cargo test --test chat_request_test`
Expected: compile failure (`build_chat_request_for_test` undefined).

- [ ] **Step 3: Implement the `chat_req.rs` main flow**

Create `zmod/llm-switch/src/connector/chat_req.rs`. Core: iterate over `input` to collect messages, handling each variant; then repair orphans, reorder, and assemble the top level.

```rust
use serde_json::{json, Value};
use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody, ResponseItem,
};
use crate::connector::{ConnError, EgressCtx};

pub(crate) fn build_chat_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    let mut messages: Vec<Value> = Vec::new();
    if !req.instructions.is_empty() {
        messages.push(json!({"role":"system","content": req.instructions}));
    }
    // call_id → whether the corresponding call has been seen (used to detect orphan results)
    let mut seen_calls: std::collections::HashSet<String> = Default::default();
    let mut calls_needing_result: std::collections::HashSet<String> = Default::default();

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                messages.push(map_message(role, content)?);
            }
            ResponseItem::FunctionCall { name, namespace, arguments, call_id, .. } => {
                if namespace.is_some() {
                    return Err(ConnError::HardFail(format!("namespaced function call '{name}' unsupported in v1")));
                }
                seen_calls.insert(call_id.clone());
                calls_needing_result.insert(call_id.clone());
                messages.push(json!({
                    "role":"assistant",
                    "tool_calls":[{
                        "id": call_id,
                        "type":"function",
                        "function": {"name": name, "arguments": arguments}
                    }]
                }));
            }
            ResponseItem::FunctionCallOutput { call_id, output, .. } => {
                if !seen_calls.contains(call_id) {
                    tracing::warn!("dropping orphan tool result call_id={call_id}");
                    continue; // orphan result → drop (§4.10)
                }
                calls_needing_result.remove(call_id);
                messages.push(map_function_call_output(call_id, output)?);
            }
            ResponseItem::Reasoning { .. } | ResponseItem::CompactionTrigger { .. } => {
                // dropped on the outbound path (§4.0 / §4.4), local history untouched
            }
            ResponseItem::AgentMessage { content, .. } => {
                messages.push(map_agent_message(content)?);
            }
            // ---- v1 hard-fail variants (§4.0) ----
            other => return Err(ConnError::HardFail(format!(
                "ResponseItem variant unsupported in v1 chat connector: {}",
                variant_name(other)
            ))),
        }
    }

    // orphan calls → inject placeholder results (§4.10)
    for call_id in calls_needing_result {
        tracing::warn!("injecting placeholder result for orphan tool call call_id={call_id}");
        messages.push(json!({"role":"tool","tool_call_id":call_id,"content":"[No output available yet]"}));
    }

    // tool-message reorder: each tool result immediately follows the assistant tool_calls that produced it (§4.10 _reorder_tool_messages)
    let messages = reorder_tool_messages(messages);

    // tool-definition tiering (§4.0b)
    let tools = map_tools(&req.tools)?;
    let mut body = json!({
        "model": ctx.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
        "parallel_tool_calls": req.parallel_tool_calls,
    });
    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
        // tool_choice is only mapped when tools are present; otherwise strip (§4.10)
        if let Some(tc) = map_tool_choice(&req.tool_choice)? {
            body["tool_choice"] = tc;
        }
    }
    // §7.1 downgrade: reasoning config → reasoning_effort; text.format → response_format; store/include/prompt_cache_key silently dropped.
    apply_field_downgrade(&mut body, req);
    Ok(body)
}
```

> `variant_name(&ResponseItem) -> &'static str`: returns the name string for each variant, for use in error messages. `LocalShellCall`/`ToolSearchCall`/`ToolSearchOutput`/`WebSearchCall`/`ImageGenerationCall`/`CustomToolCall`/`CustomToolCallOutput`/`Compaction`/`ContextCompaction`/`Other` all fall into the `other =>` arm and hard-fail.

- [ ] **Step 4: Implement each helper**

```rust
fn map_message(role: &str, content: &[ContentItem]) -> Result<Value, ConnError> {
    let mut text = String::new();
    for c in content {
        match c {
            ContentItem::InputText { text: t } | ContentItem::OutputText { text: t } => text.push_str(t),
            ContentItem::InputImage { .. } => {
                return Err(ConnError::HardFail("image input unsupported in v1 (no capability flag)".into()));
            }
        }
    }
    Ok(json!({"role": role, "content": text}))
}

fn map_agent_message(content: &[AgentMessageInputContent]) -> Result<Value, ConnError> {
    let mut text = String::new();
    for c in content {
        match c {
            AgentMessageInputContent::InputText { text: t } => text.push_str(t),
            AgentMessageInputContent::EncryptedContent { .. } => {
                return Err(ConnError::HardFail("encrypted agent message unreadable by non-Responses upstream".into()));
            }
        }
    }
    Ok(json!({"role":"assistant","content": text}))
}

fn map_function_call_output(
    call_id: &str,
    output: &codex_protocol::models::FunctionCallOutputPayload,
) -> Result<Value, ConnError> {
    let mut text = match &output.body {
        FunctionCallOutputBody::Text(t) => t.clone(),
        FunctionCallOutputBody::ContentItems(items) => content_items_to_text(items)?, // image/encrypted → hard-fail
    };
    if output.success == Some(false) {
        tracing::warn!("tool result marked failure; prefixing [tool error] (chat has no is_error)");
        text = format!("[tool error] {text}");
    }
    Ok(json!({"role":"tool","tool_call_id": call_id,"content": text}))
}

fn map_tools(tools: &[Value]) -> Result<Option<Vec<Value>>, ConnError> {
    if tools.is_empty() { return Ok(None); }
    let mut out = Vec::new();
    for t in tools {
        match t.get("type").and_then(|v| v.as_str()) {
            Some("function") => {
                let name = t.get("name").cloned().unwrap_or(Value::Null);
                let description = t.get("description").cloned().unwrap_or(Value::Null);
                let parameters = t.get("parameters").cloned().unwrap_or(json!({"type":"object"}));
                out.push(json!({"type":"function","function":{"name":name,"description":description,"parameters":parameters}}));
            }
            other => return Err(ConnError::HardFail(format!(
                "tool definition type {other:?} unsupported in v1 (only standard function)"
            ))),
        }
    }
    Ok(Some(out))
}

/// tool_choice is a String (codex native). auto/none are mapped; required/specific-function: map if expressible, otherwise hard-fail (§4.11).
fn map_tool_choice(tc: &str) -> Result<Option<Value>, ConnError> {
    match tc {
        "auto" => Ok(Some(json!("auto"))),
        "none" => Ok(Some(json!("none"))),
        "required" => Ok(Some(json!("required"))),
        "" => Ok(None),
        // if codex uses JSON-in-string to express forcing a specific function, parse it; if not expressible → hard-fail.
        other => {
            if let Ok(v) = serde_json::from_str::<Value>(other) {
                // form {"type":"function","name":"f"} → chat {"type":"function","function":{"name":"f"}}
                if v.get("type").and_then(|x| x.as_str()) == Some("function") {
                    if let Some(name) = v.get("name") {
                        return Ok(Some(json!({"type":"function","function":{"name":name}})));
                    }
                }
            }
            Err(ConnError::HardFail(format!("tool_choice '{other}' not expressible for chat")))
        }
    }
}
```

`content_items_to_text(items)`: iterate over `FunctionCallOutputContentItem` (variants already recorded in Step 0); concatenate plain text and return it, hard-failing on image/encrypted via `ConnError::HardFail`. `reorder_tool_messages(Vec<Value>) -> Vec<Value>`: replicate the spec §4.10 algorithm — separate tool / non-tool messages; group tool messages by `tool_call_id`; iterate over the non-tool messages and, after each assistant carrying `tool_calls`, reinsert the tool results in the order of `tool_calls[].id`; append any unmatched tool messages at the end + warn. `apply_field_downgrade` (the §7.1 downgrade layer, combining two things): ① if `req.reasoning` has an effort, write `body["reasoning_effort"]`; otherwise if `req.reasoning.is_some()` warn and drop it; ② if `req.text` (`Option<TextControls>`) contains a structured-output schema (json_schema) → write `body["response_format"] = {"type":"json_schema","json_schema": <schema>}`, warning on any part that cannot be mapped. `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` are not copied (silently dropped, §7.1 safely-ignorable tier).

> Verify the real shape of `TextControls` before implementing: `grep -n "pub struct TextControls" -A 12 codex-rs/codex-api/src/common.rs`, and extract the json_schema field accordingly; if v1 does not yet support structured output, at least ensure "warn when a schema is present but unmapped" — never silently alter the model-visible output constraints.

- [ ] **Step 5: Wire chat_req into chat.rs**

Add `mod chat_req; pub(crate) use chat_req::build_chat_request;` at the top of `connector/chat.rs`.

- [ ] **Step 6: Expose the test entry point**

The tests need to call internal functions. Add a `#[doc(hidden)] pub mod testing` in `lib.rs`, containing: `pub fn sample_request()` (the Task 03 sample), `pub fn dummy_ctx(model: &str) -> EgressCtx` (`reqwest::Client::new()`, arbitrary base_url, key Some("x")), and `pub fn build_chat_request_for_test(req, ctx)` that forwards to `connector::chat::build_chat_request`. This keeps the internal API out of the formal public surface for tests.

- [ ] **Step 7: Golden fixtures (against rust-llm-proxy)**

Reference baseline: the OpenAiChat converter in `../3rd/proxy/rust-llm-proxy`. Pick 2 representative samples (one with system+user+a single tool round-trip, one with multiple tools) and capture them as `tests/fixtures/chat_req_*.expected.json`, then `assert_eq!(build(...), expected)` in the tests (ignoring field order: comparing via `serde_json::Value` is order-independent).

- [ ] **Step 8: Run tests to confirm they pass**

Run: `cd zmod/llm-switch && cargo test --test chat_request_test`
Expected: all PASS (including every hard-fail assertion).

- [ ] **Step 9: Commit**

```bash
git add zmod/llm-switch/src/connector/chat_req.rs zmod/llm-switch/src/connector/chat.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/chat_request_test.rs zmod/llm-switch/tests/fixtures
git commit -m "feat(llm-switch): chat outbound request translation (tools, pairing, reorder, hard-fails)"
```
