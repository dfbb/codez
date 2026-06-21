# Task 06 — anthropic Outbound Request Construction

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the [master index](2026-06-20-llm-switch-00-index.md) Global Constraints. The structure mirrors [Task 04](2026-06-20-llm-switch-04-chat-request.md), but the target is Anthropic Messages; see the differences below.

**Goal:** Implement `connector/anthropic_req.rs`: codex `ResponsesApiRequest` → Anthropic Messages request JSON. Differences from chat: `system` goes at the top level; message roles are limited to user/assistant; `FunctionCall` → assistant `content[tool_use]` (**arguments string parsed into an object**); `FunctionCallOutput` → user `content[tool_result]` (`is_error` native); **`max_tokens` is required** (default_max_tokens falls back to 4096); `parallel_tool_calls==false` → `tool_choice.disable_parallel_tool_use=true`; **no** tool-message flattening/reordering (content blocks are grouped by turn).

**Spec coverage:** §4.3, §4.0/§4.0b, §4.6, §4.8, §4.9, §4.10 (orphan repair, no reordering), §4.11, §7.1.

**Files:**
- Create: `zmod/llm-switch/src/connector/anthropic_req.rs`
- Modify: `zmod/llm-switch/src/connector/anthropic.rs` (`mod anthropic_req;`)
- Test: `zmod/llm-switch/tests/anthropic_request_test.rs`

**Interfaces:**
- Produces: `pub(crate) fn build_anthropic_request(req, ctx) -> Result<serde_json::Value, ConnError>`; testing forwards via `build_anthropic_request_for_test`.

---

- [ ] **Step 0: Reuse the types already pinned down in Task 04 Step 0** (ContentItem / FunctionCallOutputBody / FunctionCallOutputContentItem / all ResponseItem variants).

- [ ] **Step 1: Write failing tests**

Create `zmod/llm-switch/tests/anthropic_request_test.rs`:

```rust
use codez_llm_switch::testing::{build_anthropic_request_for_test as build, dummy_ctx_anthropic, sample_request};
use codex_protocol::models::{ContentItem, FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem};
use serde_json::json;

fn ctx() -> codez_llm_switch::EgressCtx { dummy_ctx_anthropic("claude-opus-4-8", Some(8192)) }
fn base_req() -> codex_api::ResponsesApiRequest { let mut r = sample_request(); r.model = "claude-opus-4-8".into(); r }

#[test]
fn system_goes_top_level_and_messages_have_no_system_role() {
    let mut req = base_req();
    req.instructions = "be brief".into();
    req.input = vec![ResponseItem::Message { id: None, role: "user".into(),
        content: vec![ContentItem::InputText { text: "hi".into() }], phase: None, metadata: None }];
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["system"], "be brief");
    let msgs = v["messages"].as_array().unwrap();
    assert!(msgs.iter().all(|m| m["role"] != "system"));
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn max_tokens_required_uses_default() {
    let v = build(&base_req(), &ctx()).unwrap();
    assert_eq!(v["max_tokens"], 8192);
}

#[test]
fn max_tokens_falls_back_to_4096_when_no_config() {
    let req = base_req();
    let ctx_no_default = codez_llm_switch::testing::dummy_ctx_anthropic("claude-opus-4-8", None);
    let v = build(&req, &ctx_no_default).unwrap();
    assert_eq!(v["max_tokens"], 4096);
}

#[test]
fn function_call_becomes_tool_use_with_parsed_object() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCall { id: None, name: "get_weather".into(), namespace: None,
        arguments: "{\"city\":\"SF\"}".into(), call_id: "call_1".into(), metadata: None }];
    let v = build(&req, &ctx()).unwrap();
    let asst = v["messages"].as_array().unwrap().iter().find(|m| m["role"] == "assistant").unwrap();
    let block = &asst["content"][0];
    assert_eq!(block["type"], "tool_use");
    assert_eq!(block["id"], "call_1");
    assert_eq!(block["name"], "get_weather");
    assert_eq!(block["input"]["city"], "SF"); // arguments string → object
}

#[test]
fn tool_output_maps_is_error() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall { id: None, name: "f".into(), namespace: None, arguments: "{}".into(), call_id: "c".into(), metadata: None },
        ResponseItem::FunctionCallOutput { id: None, call_id: "c".into(),
            output: FunctionCallOutputPayload { body: FunctionCallOutputBody::Text("boom".into()), success: Some(false) }, metadata: None },
    ];
    let v = build(&req, &ctx()).unwrap();
    // tool_result lives in a content block of some user message
    let tr = find_block(&v, "tool_result").expect("tool_result present");
    assert_eq!(tr["tool_use_id"], "c");
    assert_eq!(tr["is_error"], true);
}

#[test]
fn disable_parallel_tool_use_when_false() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.parallel_tool_calls = false;
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tool_choice"]["disable_parallel_tool_use"], true);
}

#[test]
fn tools_map_to_input_schema() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"function","name":"f","description":"d","parameters":{"type":"object"}})];
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tools"][0]["name"], "f");
    assert_eq!(v["tools"][0]["input_schema"]["type"], "object");
}

// Hard-fail / drop assertions are the same as in Task 04: namespaced call, custom tool, image, encrypted,
// LocalShellCall/ToolSearch*/WebSearch/ImageGeneration/Custom*/Compaction/ContextCompaction/Other → is_err();
// Reasoning / CompactionTrigger → no error and not added to messages. Fill these in one by one.

fn find_block<'a>(v: &'a serde_json::Value, ty: &str) -> Option<&'a serde_json::Value> {
    v["messages"].as_array()?.iter()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|b| b["type"] == ty)
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cd zmod/llm-switch && cargo test --test anthropic_request_test`
Expected: compilation failure.

- [ ] **Step 3: Implement `anthropic_req.rs`**

Key points: Anthropic represents both tool_use (assistant) and tool_result (user) as content blocks; you need to merge adjacent same-role blocks into a single message (simplification: emit messages item by item, merging consecutive same-role items).

```rust
use serde_json::{json, Value};
use codex_protocol::models::{AgentMessageInputContent, ContentItem, FunctionCallOutputBody, ResponseItem};
use crate::connector::{ConnError, EgressCtx};

const DEFAULT_MAX_TOKENS: u32 = 4096;

pub(crate) fn build_anthropic_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // An ordered list of (role, Vec<content block>); merge blocks for consecutive same roles
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();
    let mut push_block = |role: &str, block: Value, turns: &mut Vec<(String, Vec<Value>)>| {
        if let Some(last) = turns.last_mut() {
            if last.0 == role { last.1.push(block); return; }
        }
        turns.push((role.to_string(), vec![block]));
    };

    let mut seen_calls: std::collections::HashSet<String> = Default::default();
    let mut calls_needing_result: std::collections::HashSet<String> = Default::default();

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let r = if role == "assistant" { "assistant" } else { "user" }; // system already at top level
                for c in content {
                    push_block(r, content_item_to_block(c)?, &mut turns);
                }
            }
            ResponseItem::AgentMessage { content, .. } => {
                for c in content {
                    match c {
                        AgentMessageInputContent::InputText { text } =>
                            push_block("assistant", json!({"type":"text","text":text}), &mut turns),
                        AgentMessageInputContent::EncryptedContent { .. } =>
                            return Err(ConnError::HardFail("encrypted agent message unsupported".into())),
                    }
                }
            }
            ResponseItem::FunctionCall { name, namespace, arguments, call_id, .. } => {
                if namespace.is_some() {
                    return Err(ConnError::HardFail(format!("namespaced function call '{name}' unsupported in v1")));
                }
                let input: Value = serde_json::from_str(arguments)
                    .map_err(|e| ConnError::HardFail(format!("tool arguments not valid JSON object: {e}")))?;
                seen_calls.insert(call_id.clone());
                calls_needing_result.insert(call_id.clone());
                push_block("assistant", json!({"type":"tool_use","id":call_id,"name":name,"input":input}), &mut turns);
            }
            ResponseItem::FunctionCallOutput { call_id, output, .. } => {
                if !seen_calls.contains(call_id) { tracing::warn!("dropping orphan tool result {call_id}"); continue; }
                calls_needing_result.remove(call_id);
                push_block("user", tool_result_block(call_id, output)?, &mut turns);
            }
            ResponseItem::Reasoning { .. } | ResponseItem::CompactionTrigger { .. } => {}
            other => return Err(ConnError::HardFail(format!(
                "ResponseItem variant unsupported in v1 anthropic connector: {}", variant_name(other)))),
        }
    }

    // Orphan calls → inject placeholder tool_result (§4.10)
    for call_id in calls_needing_result {
        tracing::warn!("injecting placeholder tool_result for orphan call {call_id}");
        push_block("user", json!({"type":"tool_result","tool_use_id":call_id,
            "content":"[No output available yet]"}), &mut turns);
    }

    let messages: Vec<Value> = turns.into_iter()
        .map(|(role, blocks)| json!({"role":role,"content":blocks})).collect();

    let max_tokens = ctx.default_max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let mut body = json!({
        "model": ctx.model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
    });
    if !req.instructions.is_empty() { body["system"] = json!(req.instructions); }

    let tools = map_tools(&req.tools)?; // same tiering as chat, but outputs {name,description,input_schema}
    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
        let mut tc = map_tool_choice(&req.tool_choice)?; // auto/any/tool; cannot express forced → hard fail
        if !req.parallel_tool_calls {
            let obj = tc.get_or_insert_with(|| json!({"type":"auto"}));
            obj["disable_parallel_tool_use"] = json!(true); // §4.11
        }
        if let Some(tc) = tc { body["tool_choice"] = tc; }
    }
    apply_field_downgrade(&mut body, req); // §7.1: reasoning→thinking, text.format→downgrade/warn, see below
    Ok(body)
}
```

helpers:
- `content_item_to_block(&ContentItem)`: `InputText{text}`/`OutputText{text}` → `{"type":"text","text":...}`; `InputImage` → `ConnError::HardFail` (§4.9).
- `tool_result_block(call_id, payload)`: body text → `{"type":"tool_result","tool_use_id":call_id,"content":<text>}`; `ContentItems` image/encrypted → hard fail; `success==Some(false)` → add `"is_error": true` (§4.6).
- `map_tools`: same tiering as chat (only `function` allowed through), outputs `{"name","description","input_schema": parameters}`.
- `map_tool_choice(&str)`: `"auto"`→`{"type":"auto"}`; `"none"`→`{"type":"none"}`; `"required"`→`{"type":"any"}`; `{"type":"function","name":...}` (JSON-in-string)→`{"type":"tool","name":...}`; anything else that cannot be expressed → `ConnError::HardFail` (§4.11).
- `apply_field_downgrade` (§7.1): ① if `req.reasoning` is present → `body["thinking"]={"type":"enabled",...}` (approximated by effort), otherwise warn and drop if `Some`; ② if `req.text` contains a structured-output schema → anthropic has no native response_format, so downgrade by appending it to the system instructions or just warn (the implementer picks one and notes it), and must not silently drop the output constraint; `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` are not copied (silently dropped).
- `variant_name`: reuse the one from chat_req (extracted into a shared `pub(crate) fn variant_name(&ResponseItem) -> &'static str` in `connector/mod.rs`, shared by chat/anthropic).

- [ ] **Step 4: Wire up the module + testing forwarding**

In `connector/anthropic.rs` add `mod anthropic_req; pub(crate) use anthropic_req::build_anthropic_request;`. In the `lib.rs` testing module add `build_anthropic_request_for_test` and `dummy_ctx_anthropic(model, default_max_tokens)` (`auth=XApiKey`, `anthropic_version=Some("2023-06-01")`).

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cd zmod/llm-switch && cargo test --test anthropic_request_test`
Expected: all PASS.

- [ ] **Step 6: Golden fixture (baseline source, §8 must be explicit)**

anthropic has no Rust baseline → use the Python anthropic converter in `../3rd/proxy/llm-rosetta` (`tests/converters/anthropic`) to generate the expected output and freeze it into `tests/fixtures/anthropic_req_*.expected.json`; or declare a **self-authored fixture** (manually verified against the official Anthropic Messages format). State which one you used in a comment at the top of the test file; **do not** vaguely write "corresponds to the converter".

- [ ] **Step 7: Commit**

```bash
git add zmod/llm-switch/src/connector/anthropic_req.rs zmod/llm-switch/src/connector/anthropic.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/anthropic_request_test.rs zmod/llm-switch/tests/fixtures
git commit -m "feat(llm-switch): anthropic outbound request translation (tool_use/result, max_tokens, disable_parallel)"
```
