# Task 06 — anthropic 出站请求构造

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints。结构与 [Task 04](2026-06-20-llm-switch-04-chat-request.md) 对称,但目标是 Anthropic Messages,差异点见下。

**Goal:** 实现 `connector/anthropic_req.rs`:codex `ResponsesApiRequest` → Anthropic Messages 请求 JSON。差异于 chat:`system` 走顶层;消息 role 仅 user/assistant;`FunctionCall` → assistant `content[tool_use]`(**arguments 字符串 parse 成对象**);`FunctionCallOutput` → user `content[tool_result]`(`is_error` 原生);**`max_tokens` 必填**(default_max_tokens 兜底 4096);`parallel_tool_calls==false` → `tool_choice.disable_parallel_tool_use=true`;**无** tool 消息扁平重排(content block 按回合归组)。

**覆盖 spec:** §4.3、§4.0/§4.0b、§4.6、§4.8、§4.9、§4.10(孤儿修复,无重排)、§4.11、§7.1。

**Files:**
- Create: `zmod/llm-switch/src/connector/anthropic_req.rs`
- Modify: `zmod/llm-switch/src/connector/anthropic.rs`(`mod anthropic_req;`)
- Test: `zmod/llm-switch/tests/anthropic_request_test.rs`

**Interfaces:**
- Produces:`pub(crate) fn build_anthropic_request(req, ctx) -> Result<serde_json::Value, ConnError>`;testing 转发 `build_anthropic_request_for_test`。

---

- [ ] **Step 0: 沿用 Task 04 Step 0 已钉死的类型**(ContentItem / FunctionCallOutputBody / FunctionCallOutputContentItem / ResponseItem 全变体)。

- [ ] **Step 1: 写失败测试**

创建 `zmod/llm-switch/tests/anthropic_request_test.rs`:

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
    assert_eq!(block["input"]["city"], "SF"); // arguments 字符串 → 对象
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
    // tool_result 在某个 user 消息的 content block 里
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

// 硬失败 / 丢弃断言与 Task 04 同款:namespaced call、custom tool、image、encrypted、
// LocalShellCall/ToolSearch*/WebSearch/ImageGeneration/Custom*/Compaction/ContextCompaction/Other → is_err();
// Reasoning / CompactionTrigger → 不报错且不进 messages。逐个补齐。

fn find_block<'a>(v: &'a serde_json::Value, ty: &str) -> Option<&'a serde_json::Value> {
    v["messages"].as_array()?.iter()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|b| b["type"] == ty)
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd zmod/llm-switch && cargo test --test anthropic_request_test`
Expected: 编译失败。

- [ ] **Step 3: 实现 `anthropic_req.rs`**

要点:Anthropic 把 tool_use(assistant)与 tool_result(user)都作为 content block;需要把相邻同 role 的 block 合并进同一条消息(简化:逐 item 产出消息,连续同 role 合并)。

```rust
use serde_json::{json, Value};
use codex_protocol::models::{AgentMessageInputContent, ContentItem, FunctionCallOutputBody, ResponseItem};
use crate::connector::{ConnError, EgressCtx};

const DEFAULT_MAX_TOKENS: u32 = 4096;

pub(crate) fn build_anthropic_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // (role, Vec<content block>) 的有序列;连续同 role 合并 block
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
                let r = if role == "assistant" { "assistant" } else { "user" }; // system 已走顶层
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

    // 孤儿调用 → 注入占位 tool_result(§4.10)
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

    let tools = map_tools(&req.tools)?; // 与 chat 同款分级,但输出 {name,description,input_schema}
    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
        let mut tc = map_tool_choice(&req.tool_choice)?; // auto/any/tool;不可表达强制 → 硬失败
        if !req.parallel_tool_calls {
            let obj = tc.get_or_insert_with(|| json!({"type":"auto"}));
            obj["disable_parallel_tool_use"] = json!(true); // §4.11
        }
        if let Some(tc) = tc { body["tool_choice"] = tc; }
    }
    apply_field_downgrade(&mut body, req); // §7.1:reasoning→thinking、text.format→降级/warn,见下
    Ok(body)
}
```

helper:
- `content_item_to_block(&ContentItem)`:`InputText{text}`/`OutputText{text}` → `{"type":"text","text":...}`;`InputImage` → `ConnError::HardFail`(§4.9)。
- `tool_result_block(call_id, payload)`:body 文本 → `{"type":"tool_result","tool_use_id":call_id,"content":<text>}`;`ContentItems` 图片/加密 → 硬失败;`success==Some(false)` → 加 `"is_error": true`(§4.6)。
- `map_tools`:与 chat 同分级(只放行 `function`),输出 `{"name","description","input_schema": parameters}`。
- `map_tool_choice(&str)`:`"auto"`→`{"type":"auto"}`;`"none"`→`{"type":"none"}`;`"required"`→`{"type":"any"}`;`{"type":"function","name":...}`(JSON-in-string)→`{"type":"tool","name":...}`;其它不可表达 → `ConnError::HardFail`(§4.11)。
- `apply_field_downgrade`(§7.1):① `req.reasoning` 有 → `body["thinking"]={"type":"enabled",...}`(按 effort 近似),否则若 `Some` warn 丢弃;② `req.text` 含结构化输出 schema → anthropic 无原生 response_format,降级为系统指令追加或仅 warn(实现者择一并注明),不得静默丢失输出约束;`store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` 不复制(静默丢)。
- `variant_name`:复用 chat_req 的(抽到 `connector/mod.rs` 公共 `pub(crate) fn variant_name(&ResponseItem) -> &'static str`,chat/anthropic 共用)。

- [ ] **Step 4: 挂模块 + testing 转发**

`connector/anthropic.rs` 加 `mod anthropic_req; pub(crate) use anthropic_req::build_anthropic_request;`。`lib.rs` testing 模块加 `build_anthropic_request_for_test`、`dummy_ctx_anthropic(model, default_max_tokens)`(`auth=XApiKey`、`anthropic_version=Some("2023-06-01")`)。

- [ ] **Step 5: 运行测试确认通过**

Run: `cd zmod/llm-switch && cargo test --test anthropic_request_test`
Expected: 全 PASS。

- [ ] **Step 6: 黄金 fixture(基准来源,§8 必须明确)**

anthropic 无 Rust 基准 → 用 `../3rd/proxy/llm-rosetta` 的 Python anthropic converter(`tests/converters/anthropic`)生成期望输出,固化成 `tests/fixtures/anthropic_req_*.expected.json`;或声明**自建 fixture**(人工核对 Anthropic Messages 官方格式)。在测试文件头注释里写明你采用哪一种,**不得**笼统写"对应 converter"。

- [ ] **Step 7: 提交**

```bash
git add zmod/llm-switch/src/connector/anthropic_req.rs zmod/llm-switch/src/connector/anthropic.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/anthropic_request_test.rs zmod/llm-switch/tests/fixtures
git commit -m "feat(llm-switch): anthropic outbound request translation (tool_use/result, max_tokens, disable_parallel)"
```
