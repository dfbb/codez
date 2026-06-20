# Task 04 — chat 出站请求构造

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints,尤其"v1 仅 function 工具""图片硬失败""加密硬失败"。

**Goal:** 实现 `connector/chat.rs` 的**出站方向**:把 codex `ResponsesApiRequest` 翻成 Chat Completions 请求 JSON。覆盖 instructions→system、Message→messages、FunctionCall→tool_calls、FunctionCallOutput→tool(含 success 前缀)、tools 分级、tool_choice/parallel、孤儿修复 + tool 消息重排、字段分级、硬失败变体。SSE 回程是 Task 05。

**覆盖 spec:** §4.2(chat 请求)、§4.0/§4.0b(变体/工具定义硬失败)、§4.6(FunctionCallOutput)、§4.8(call_id)、§4.9(ContentItem)、§4.10(孤儿修复 + 重排)、§4.11(tool_choice/parallel)、§7.1(字段分级)。

**Files:**
- Create: `zmod/llm-switch/src/connector/chat_req.rs`(请求构造,从 chat.rs 分出以保持文件聚焦)
- Modify: `zmod/llm-switch/src/connector/chat.rs`(`mod chat_req;`)
- Create: `zmod/llm-switch/tests/chat_request_test.rs`
- Create: `zmod/llm-switch/tests/fixtures/`(放黄金 JSON,见 Step 7)

**Interfaces:**
- Consumes:`config`、`ConnError`(Task 03)、`EgressCtx`、`ResponsesApiRequest`/`ResponseItem`/`ContentItem`/`FunctionCallOutputPayload`。
- Produces(Task 05/08 依赖):
  - `pub(crate) fn build_chat_request(req: &codex_api::ResponsesApiRequest, ctx: &EgressCtx) -> Result<serde_json::Value, ConnError>`
  - 内部:`map_tools`、`map_tool_choice`、`map_messages`(含孤儿修复 + 重排)、`map_function_call_output`。

---

- [ ] **Step 0: 钉死外部类型定义(避免凭记忆)**

Run 并阅读输出(把这些类型抄进实现时对照):
```bash
grep -n "pub enum ContentItem" -A 18 codex-rs/protocol/src/models.rs
grep -n "pub struct FunctionCallOutputPayload" -A 6 codex-rs/protocol/src/models.rs
grep -n "pub enum FunctionCallOutputBody" -A 6 codex-rs/protocol/src/models.rs
grep -n "FunctionCallOutputContentItem" -A 20 codex-rs/protocol/src/models.rs
grep -n "pub enum ResponseItem" -A 120 codex-rs/protocol/src/models.rs
```
确认:`ContentItem::{InputText{text},InputImage{image_url,detail},OutputText{text}}`;`FunctionCallOutputBody::{Text(String),ContentItems(Vec<FunctionCallOutputContentItem>)}`;`FunctionCallOutputContentItem` 的变体(预期含 text / image / 可能 encrypted —— 以实际为准决定 §4.6 分级)。把 `FunctionCallOutputContentItem` 的真实变体记进实现注释。

- [ ] **Step 1: 写失败测试(逐场景断言)**

创建 `zmod/llm-switch/tests/chat_request_test.rs`。用 helper 复用 Task 03 的 `sample_request()`(抽到 `tests/common/mod.rs` 或各测试内重复定义——本计划各测试自带 helper 以便乱序执行):

```rust
use codez_llm_switch::testing::build_chat_request_for_test as build; // 见 Step 6 暴露方式
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
    // tool result 紧跟 assistant(§4.10 重排)
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
    // 有调用、无结果(压缩破坏)→ 注入合成占位结果,不硬失败(§4.10)
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
    // 有 tool_choice 但 tools 为空 → strip(§4.10)
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

// ---- 硬失败断言 ----
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

> 还需为以下变体各加一个 `*_hard_fails` 测试(同上结构):`LocalShellCall`、`ToolSearchCall`、`WebSearchCall`、`ImageGenerationCall`、`CustomToolCall`、`Compaction`、`ContextCompaction`、`AgentMessage` 含 `EncryptedContent`、`Other`。每个构造对应 `ResponseItem` 放进 `input`,断言 `build(...).is_err()`。`CompactionTrigger` 与 `Reasoning` 反向:断言**不**报错且**不**出现在 messages(出站丢弃)。

- [ ] **Step 2: 运行确认失败**

Run: `cd zmod/llm-switch && cargo test --test chat_request_test`
Expected: 编译失败(`build_chat_request_for_test` 未定义)。

- [ ] **Step 3: 实现 `chat_req.rs` 主流程**

创建 `zmod/llm-switch/src/connector/chat_req.rs`。核心:遍历 `input` 收集 messages,按变体处置;然后修复孤儿、重排、组装顶层。

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
    // call_id → 是否见过对应调用(用于孤儿结果判定)
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
                    continue; // 孤儿结果 → 删除(§4.10)
                }
                calls_needing_result.remove(call_id);
                messages.push(map_function_call_output(call_id, output)?);
            }
            ResponseItem::Reasoning { .. } | ResponseItem::CompactionTrigger { .. } => {
                // 出站丢弃(§4.0 / §4.4),不动本地历史
            }
            ResponseItem::AgentMessage { content, .. } => {
                messages.push(map_agent_message(content)?);
            }
            // ---- v1 硬失败变体(§4.0)----
            other => return Err(ConnError::HardFail(format!(
                "ResponseItem variant unsupported in v1 chat connector: {}",
                variant_name(other)
            ))),
        }
    }

    // 孤儿调用 → 注入占位结果(§4.10)
    for call_id in calls_needing_result {
        tracing::warn!("injecting placeholder result for orphan tool call call_id={call_id}");
        messages.push(json!({"role":"tool","tool_call_id":call_id,"content":"[No output available yet]"}));
    }

    // tool 消息重排:紧跟产生它的 assistant tool_calls(§4.10 _reorder_tool_messages)
    let messages = reorder_tool_messages(messages);

    // 工具定义分级(§4.0b)
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
        // tool_choice 仅在有 tools 时映射;否则 strip(§4.10)
        if let Some(tc) = map_tool_choice(&req.tool_choice)? {
            body["tool_choice"] = tc;
        }
    }
    // §7.1 降级:reasoning 配置 → reasoning_effort;text.format → response_format;store/include/prompt_cache_key 静默丢。
    apply_field_downgrade(&mut body, req);
    Ok(body)
}
```

> `variant_name(&ResponseItem) -> &'static str`:对每个变体返回名字串,供错误信息。`LocalShellCall`/`ToolSearchCall`/`ToolSearchOutput`/`WebSearchCall`/`ImageGenerationCall`/`CustomToolCall`/`CustomToolCallOutput`/`Compaction`/`ContextCompaction`/`Other` 都落到 `other =>` 臂硬失败。

- [ ] **Step 4: 实现各 helper**

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
        FunctionCallOutputBody::ContentItems(items) => content_items_to_text(items)?, // 图片/加密 → 硬失败
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

/// tool_choice 是 String(codex 原生)。auto/none 映射;required/具体函数:能表达就映射,否则硬失败(§4.11)。
fn map_tool_choice(tc: &str) -> Result<Option<Value>, ConnError> {
    match tc {
        "auto" => Ok(Some(json!("auto"))),
        "none" => Ok(Some(json!("none"))),
        "required" => Ok(Some(json!("required"))),
        "" => Ok(None),
        // 若 codex 用 JSON-in-string 表达强制具体函数,解析之;不可表达 → 硬失败。
        other => {
            if let Ok(v) = serde_json::from_str::<Value>(other) {
                // 形如 {"type":"function","name":"f"} → chat {"type":"function","function":{"name":"f"}}
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

`content_items_to_text(items)`:遍历 `FunctionCallOutputContentItem`(Step 0 已记其变体);纯文本拼接返回,遇 image/encrypted → `ConnError::HardFail`。`reorder_tool_messages(Vec<Value>) -> Vec<Value>`:复刻 spec §4.10 算法——分出 tool/非 tool;按 `tool_call_id` 归组;遍历非 tool,在带 `tool_calls` 的 assistant 后按 `tool_calls[].id` 顺序插回;未匹配 tool 追加末尾 + warn。`apply_field_downgrade`(§7.1 降级层,合并两件事):① `req.reasoning` 有 effort 则写 `body["reasoning_effort"]`,否则若 `req.reasoning.is_some()` warn 丢弃;② `req.text`(`Option<TextControls>`)若含结构化输出 schema(json_schema)→ 写 `body["response_format"] = {"type":"json_schema","json_schema": <schema>}`,无法映射的部分 warn。`store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` 不复制(静默丢,§7.1 可安全忽略级)。

> `TextControls` 的真实形状执行前核对:`grep -n "pub struct TextControls" -A 12 codex-rs/codex-api/src/common.rs`,据此取出 json_schema 字段;若 v1 暂不接结构化输出,至少保证"有 schema 但未映射时 warn",不得静默改变模型可见的输出约束。

- [ ] **Step 5: 把 chat_req 挂进 chat.rs**

`connector/chat.rs` 顶部加 `mod chat_req; pub(crate) use chat_req::build_chat_request;`。

- [ ] **Step 6: 暴露测试入口**

测试需调内部函数。在 `lib.rs` 加一个 `#[doc(hidden)] pub mod testing`,内含:`pub fn sample_request()`(Task 03 的样本)、`pub fn dummy_ctx(model: &str) -> EgressCtx`(`reqwest::Client::new()`、base_url 任意、key Some("x"))、`pub fn build_chat_request_for_test(req, ctx)` 转发 `connector::chat::build_chat_request`。这样测试不暴露内部 API 到正式公共面。

- [ ] **Step 7: 黄金 fixture(对照 rust-llm-proxy)**

基准来源:`../3rd/proxy/rust-llm-proxy` 的 OpenAiChat converter。挑 2 个代表样本(一段含 system+user+一次 tool 往返、一段含多工具)落成 `tests/fixtures/chat_req_*.expected.json`,在测试里 `assert_eq!(build(...), expected)`(忽略字段序:用 `serde_json::Value` 比较即顺序无关)。

- [ ] **Step 8: 运行测试确认通过**

Run: `cd zmod/llm-switch && cargo test --test chat_request_test`
Expected: 全部 PASS(含所有硬失败断言)。

- [ ] **Step 9: 提交**

```bash
git add zmod/llm-switch/src/connector/chat_req.rs zmod/llm-switch/src/connector/chat.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/chat_request_test.rs zmod/llm-switch/tests/fixtures
git commit -m "feat(llm-switch): chat outbound request translation (tools, pairing, reorder, hard-fails)"
```
