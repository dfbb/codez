# Task 07 — anthropic SSE→ResponseEvent

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints,尤其 §4.5。结构与 [Task 05](2026-06-20-llm-switch-05-chat-sse.md) 对称。

**Goal:** 实现 anthropic 入站状态机:Anthropic Messages SSE 事件序列 → `Vec<ResponseEvent>`。覆盖 `content_block_delta`/`text_delta` 累计、`tool_use` block + `input_json_delta` 聚合(**对象 stringify 回 arguments 字符串**)、§4.5 合成 assistant message、`message_start`/`message_delta`/`message_stop` → `Completed`(usage 累计、`stop_reason` → `end_turn`)。

**覆盖 spec:** §4.3(响应)、§4.5、§4.8。

**Files:**
- Create: `zmod/llm-switch/src/connector/anthropic_sse.rs`
- Modify: `zmod/llm-switch/src/connector/anthropic.rs`(`pub(crate) mod anthropic_sse;`)
- Test: `zmod/llm-switch/tests/anthropic_sse_test.rs`

**Interfaces:**
- Produces:`pub(crate) struct AnthropicSseState`、`push_event(&mut self, evt: &serde_json::Value) -> Result<Vec<ResponseEvent>, ConnError>`、`finish(&mut self) -> Vec<ResponseEvent>`;testing 转发 `translate_anthropic_sse_for_test(events, done)`。

---

- [ ] **Step 0: anthropic SSE 事件形态对照**

Anthropic Messages streaming 事件类型(以官方/llm-rosetta fixture 为准,执行前在 `../3rd/proxy/llm-rosetta` 找一段真实序列核对):
- `message_start`:`{"type":"message_start","message":{"id":"msg_..","usage":{"input_tokens":N,..}}}`
- `content_block_start`:`{"type":"content_block_start","index":i,"content_block":{"type":"text"|"tool_use","id":..,"name":..}}`
- `content_block_delta`:`{"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":".."}}` 或 `{"type":"input_json_delta","partial_json":".."}`
- `content_block_stop`:`{"type":"content_block_stop","index":i}`
- `message_delta`:`{"type":"message_delta","delta":{"stop_reason":"end_turn"|"tool_use"|"max_tokens"},"usage":{"output_tokens":N}}`
- `message_stop`:`{"type":"message_stop"}`
- `error`:`{"type":"error","error":{...}}`

- [ ] **Step 1: 写失败测试**

创建 `zmod/llm-switch/tests/anthropic_sse_test.rs`:

```rust
use codez_llm_switch::testing::translate_anthropic_sse_for_test as run;
use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use serde_json::json;

#[test]
fn text_stream_synthesizes_message_and_completed() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":3}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();
    let deltas: Vec<&String> = out.iter().filter_map(|e| match e { ResponseEvent::OutputTextDelta(s)=>Some(s),_=>None }).collect();
    assert_eq!(deltas, vec!["Hel","lo"]);
    let synth = out.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::Message{role,content,..}) if role=="assistant"=>Some(content),_=>None
    }).expect("synth assistant message");
    assert!(matches!(&synth[0], ContentItem::OutputText{text} if text=="Hello"));
    let (rid, usage, end) = out.iter().find_map(|e| match e {
        ResponseEvent::Completed{response_id,token_usage,end_turn}=>Some((response_id,token_usage,end_turn)),_=>None
    }).expect("Completed");
    assert_eq!(rid, "msg_1");
    assert_eq!(*end, Some(true));
    let u = usage.as_ref().unwrap();
    assert_eq!(u.input_tokens, 3);
    assert_eq!(u.output_tokens, 2);
}

#[test]
fn tool_use_aggregates_partial_json_to_arguments_string() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_2","usage":{"input_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"ci"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ty\":\"SF\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();
    let fc = out.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall{name,arguments,call_id,..})=>Some((name,arguments,call_id)),_=>None
    }).expect("FunctionCall");
    assert_eq!(fc.0, "get_weather");
    assert_eq!(fc.1, "{\"city\":\"SF\"}"); // partial_json 聚合 → arguments 字符串(§4.3)
    assert_eq!(fc.2, "toolu_1");            // tool_use.id → call_id(§4.8)
    let end = out.iter().find_map(|e| match e { ResponseEvent::Completed{end_turn,..}=>Some(*end_turn),_=>None }).unwrap();
    assert_eq!(end, Some(false)); // tool_use → end_turn=false
}

#[test]
fn error_event_fails() {
    let events = vec![json!({"type":"error","error":{"type":"overloaded_error","message":"x"}})];
    assert!(run(&events, false).is_err());
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd zmod/llm-switch && cargo test --test anthropic_sse_test`
Expected: 编译失败。

- [ ] **Step 3: 实现 `anthropic_sse.rs`**

```rust
use serde_json::Value;
use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use crate::connector::ConnError;

#[derive(Default)]
pub(crate) struct AnthropicSseState {
    text: String,
    response_id: Option<String>,
    stop_reason: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    // block index → tool_use 聚合
    blocks: std::collections::BTreeMap<i64, BlockAcc>,
    synth_counter: u64,
}

#[derive(Default)]
struct BlockAcc { is_tool_use: bool, call_id: Option<String>, name: Option<String>, partial_json: String }

impl AnthropicSseState {
    pub(crate) fn push_event(&mut self, evt: &Value) -> Result<Vec<ResponseEvent>, ConnError> {
        let mut out = Vec::new();
        match evt.get("type").and_then(|v| v.as_str()) {
            Some("error") => {
                let msg = evt.get("error").map(|e| e.to_string()).unwrap_or_default();
                return Err(ConnError::Http(codex_api::ApiError::Stream(msg)));
            }
            Some("message_start") => {
                if let Some(m) = evt.get("message") {
                    if let Some(id) = m.get("id").and_then(|v| v.as_str()) { self.response_id.get_or_insert_with(|| id.to_string()); }
                    if let Some(it) = m.get("usage").and_then(|u| u.get("input_tokens")).and_then(|v| v.as_i64()) { self.input_tokens = it; }
                }
            }
            Some("content_block_start") => {
                let idx = evt.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                let cb = evt.get("content_block");
                let acc = self.blocks.entry(idx).or_default();
                if cb.and_then(|c| c.get("type")).and_then(|v| v.as_str()) == Some("tool_use") {
                    acc.is_tool_use = true;
                    acc.call_id = cb.and_then(|c| c.get("id")).and_then(|v| v.as_str()).map(String::from);
                    acc.name = cb.and_then(|c| c.get("name")).and_then(|v| v.as_str()).map(String::from);
                }
            }
            Some("content_block_delta") => {
                let idx = evt.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                let delta = evt.get("delta");
                match delta.and_then(|d| d.get("type")).and_then(|v| v.as_str()) {
                    Some("text_delta") => {
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(|v| v.as_str()) {
                            self.text.push_str(t);
                            out.push(ResponseEvent::OutputTextDelta(t.to_string()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(p) = delta.and_then(|d| d.get("partial_json")).and_then(|v| v.as_str()) {
                            self.blocks.entry(idx).or_default().partial_json.push_str(p);
                        }
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(sr) = evt.get("delta").and_then(|d| d.get("stop_reason")).and_then(|v| v.as_str()) {
                    self.stop_reason = Some(sr.to_string());
                }
                if let Some(ot) = evt.get("usage").and_then(|u| u.get("output_tokens")).and_then(|v| v.as_i64()) {
                    self.output_tokens = ot;
                }
            }
            _ => {} // content_block_stop / message_stop / ping 等无需即时事件
        }
        Ok(out)
    }

    pub(crate) fn finish(&mut self) -> Vec<ResponseEvent> {
        let mut out = Vec::new();
        for (_idx, acc) in std::mem::take(&mut self.blocks) {
            if acc.is_tool_use {
                let call_id = acc.call_id.unwrap_or_else(|| self.synth_id("call"));
                out.push(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                    id: None, name: acc.name.unwrap_or_default(), namespace: None,
                    arguments: acc.partial_json, call_id, metadata: None,
                }));
            }
        }
        if !self.text.is_empty() {
            out.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: None, role: "assistant".into(),
                content: vec![ContentItem::OutputText { text: std::mem::take(&mut self.text) }],
                phase: None, metadata: None,
            }));
        }
        let response_id = self.response_id.take().unwrap_or_else(|| self.synth_id("resp"));
        out.push(ResponseEvent::Completed {
            response_id,
            token_usage: Some(TokenUsage {
                input_tokens: self.input_tokens, cached_input_tokens: 0,
                output_tokens: self.output_tokens, reasoning_output_tokens: 0,
                total_tokens: self.input_tokens + self.output_tokens,
            }),
            end_turn: match self.stop_reason.as_deref() {
                Some("end_turn") => Some(true),
                Some("tool_use") => Some(false),
                _ => None, // max_tokens / 未知
            },
        });
        out
    }

    fn synth_id(&mut self, kind: &str) -> String { self.synth_counter += 1; format!("llmswitch-{kind}-{}", self.synth_counter) }
}
```

> 注:tool_use 的 `partial_json` 已是对象的 JSON 字符串,直接作为 codex `FunctionCall.arguments`(字符串)——与 §4.3"对象 → stringify 回 arguments 字符串"一致;此处上游本就给字符串增量,聚合即得。若某 block 收到的 partial_json 为空(无参数工具),`arguments` 置 `"{}"`(在 finish 里 `if acc.partial_json.is_empty() { acc.partial_json = "{}".into() }`)。

- [ ] **Step 4: 挂模块 + testing 转发**

`connector/anthropic.rs` 加 `pub(crate) mod anthropic_sse;`。`lib.rs` testing 加 `translate_anthropic_sse_for_test(events, done)`(逐个 `push_event` 后按 done `finish`)。

- [ ] **Step 5: 运行测试确认通过**

Run: `cd zmod/llm-switch && cargo test --test anthropic_sse_test`
Expected: 3 个测试 PASS。

- [ ] **Step 6: 黄金 SSE fixture**

从 llm-rosetta 的 anthropic streaming fixture 取一段真实事件序列,落 `tests/fixtures/anthropic_sse_*.jsonl`,逐行喂 `push_event`,断言事件序列。注明基准来源。

- [ ] **Step 7: 提交**

```bash
git add zmod/llm-switch/src/connector/anthropic_sse.rs zmod/llm-switch/src/connector/anthropic.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/anthropic_sse_test.rs zmod/llm-switch/tests/fixtures
git commit -m "feat(llm-switch): anthropic SSE->ResponseEvent (text/tool_use aggregation, synth message, Completed)"
```
