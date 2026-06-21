# Task 05 — chat SSE→ResponseEvent

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the [master index](2026-06-20-llm-switch-00-index.md) Global Constraints, especially §4.5 on synthesizing the assistant completion item.

**Goal:** Implement the **inbound direction** of chat: translate the sequence of Chat Completions SSE chunks into a `Vec<ResponseEvent>` (a pure functional state machine, convenient for offline testing). Cover displaying and accumulating text deltas, aggregating tool_calls by index, synthesizing the assistant message completion item (§4.5), filling in the three fields of `Completed`, and wrapping up on `[DONE]`. The actual HTTP/spawn wiring happens in Task 08.

**Spec coverage:** §4.2 (responses), §4.5 (synthesized completion item + Completed), §4.8 (call_id backfill).

**Files:**
- Create: `zmod/llm-switch/src/connector/chat_sse.rs`
- Modify: `zmod/llm-switch/src/connector/chat.rs` (`mod chat_sse;`)
- Test: `zmod/llm-switch/tests/chat_sse_test.rs`

**Interfaces:**
- Consumes: `ResponseEvent`, `ResponseItem`, `ContentItem`, `TokenUsage`, `ConnError`.
- Produces (Task 08 depends on these):
  - `pub(crate) struct ChatSseState { ... }` (accumulated text, tool_calls aggregator, response_id, usage)
  - `pub(crate) fn ChatSseState::push_chunk(&mut self, chunk: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>` (feed one parsed JSON chunk, return the events produced by that chunk)
  - `pub(crate) fn ChatSseState::finish(&mut self) -> Vec<codex_api::ResponseEvent>` (called on `[DONE]`: emit the synthesized assistant message + Completed)
  - For ease of testing: `pub(crate) fn translate_chat_sse(chunks: &[serde_json::Value], done: bool) -> Result<Vec<codex_api::ResponseEvent>, ConnError>` (run the whole sequence to completion)

---

- [ ] **Step 0: Nail down the ResponseEvent / ResponseItem construction shapes**

Run:
```bash
grep -n "pub enum ResponseEvent" -A 45 codex-rs/codex-api/src/common.rs
grep -n "OutputItemDone\|OutputTextDelta\|Completed" codex-rs/codex-api/src/common.rs
```
Confirm: `OutputTextDelta(String)`, `OutputItemDone(ResponseItem)`, `Completed{response_id:String, token_usage:Option<TokenUsage>, end_turn:Option<bool>}`. `TokenUsage` has 5 i64 fields (`input_tokens/cached_input_tokens/output_tokens/reasoning_output_tokens/total_tokens`).

- [ ] **Step 1: Write a failing test**

Create `zmod/llm-switch/tests/chat_sse_test.rs`:

```rust
use codez_llm_switch::testing::translate_chat_sse_for_test as run;
use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use serde_json::json;

fn text_chunk(id: &str, delta: &str) -> serde_json::Value {
    json!({"id": id, "choices":[{"index":0,"delta":{"content": delta},"finish_reason": null}]})
}

#[test]
fn accumulates_text_and_synthesizes_assistant_message() {
    let chunks = vec![
        text_chunk("resp-1", "Hello"),
        text_chunk("resp-1", " world"),
        json!({"id":"resp-1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
               "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}),
    ];
    let events = run(&chunks, true).unwrap();
    // At least two OutputTextDelta events (for display)
    let deltas: Vec<&String> = events.iter().filter_map(|e| match e {
        ResponseEvent::OutputTextDelta(s) => Some(s), _ => None }).collect();
    assert_eq!(deltas, vec!["Hello", " world"]);
    // Synthesized assistant message completion item (§4.5)
    let synth = events.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. }) if role == "assistant" => Some(content),
        _ => None }).expect("synth assistant message present");
    assert!(matches!(&synth[0], ContentItem::OutputText { text } if text == "Hello world"));
    // The three Completed fields
    let completed = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { response_id, token_usage, end_turn } => Some((response_id, token_usage, end_turn)),
        _ => None }).expect("Completed present");
    assert_eq!(completed.0, "resp-1");
    assert_eq!(*completed.2, Some(true)); // finish_reason=stop → end_turn=true
    assert_eq!(completed.1.as_ref().unwrap().output_tokens, 2);
}

#[test]
fn aggregates_tool_call_arguments_by_index() {
    let chunks = vec![
        json!({"id":"r","choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"ci"}}]},"finish_reason":null}]}),
        json!({"id":"r","choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"ty\":\"SF\"}"}}]},"finish_reason":null}]}),
        json!({"id":"r","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
               "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
    ];
    let events = run(&chunks, true).unwrap();
    let fc = events.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name, arguments, call_id, .. }) => Some((name, arguments, call_id)),
        _ => None }).expect("FunctionCall present");
    assert_eq!(fc.0, "get_weather");
    assert_eq!(fc.1, "{\"city\":\"SF\"}"); // arguments fully aggregated by index
    assert_eq!(fc.2, "call_1");            // call_id backfill (§4.8)
    // end_turn=false for tool_calls
    let end_turn = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { end_turn, .. } => Some(*end_turn), _ => None }).unwrap();
    assert_eq!(end_turn, Some(false));
}

#[test]
fn missing_id_synthesizes_response_id() {
    let chunks = vec![json!({"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":"stop"}]})];
    let events = run(&chunks, true).unwrap();
    let rid = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { response_id, .. } => Some(response_id.clone()), _ => None }).unwrap();
    assert!(rid.starts_with("llmswitch-"), "synth id when upstream omits id: {rid}");
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cd zmod/llm-switch && cargo test --test chat_sse_test`
Expected: compile failure.

- [ ] **Step 3: Implement `chat_sse.rs`**

```rust
use serde_json::Value;
use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use crate::connector::ConnError;

#[derive(Default)]
pub(crate) struct ChatSseState {
    text: String,
    response_id: Option<String>,
    finish_reason: Option<String>,
    usage: Option<Value>,
    // index → (call_id, name, accumulated arguments)
    tool_calls: std::collections::BTreeMap<i64, ToolAcc>,
    synth_counter: u64,
}

#[derive(Default)]
struct ToolAcc { call_id: Option<String>, name: Option<String>, arguments: String }

impl ChatSseState {
    pub(crate) fn push_chunk(&mut self, chunk: &Value) -> Result<Vec<ResponseEvent>, ConnError> {
        let mut out = Vec::new();
        if let Some(err) = chunk.get("error") {
            return Err(ConnError::Http(codex_api::ApiError::Stream(err.to_string())));
        }
        if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
            self.response_id.get_or_insert_with(|| id.to_string());
        }
        if let Some(u) = chunk.get("usage") { if !u.is_null() { self.usage = Some(u.clone()); } }
        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else { return Ok(out); };
        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(fr.to_string());
        }
        let delta = choice.get("delta");
        if let Some(content) = delta.and_then(|d| d.get("content")).and_then(|v| v.as_str()) {
            if !content.is_empty() {
                self.text.push_str(content);
                out.push(ResponseEvent::OutputTextDelta(content.to_string())); // display only
            }
        }
        if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                let acc = self.tool_calls.entry(idx).or_default();
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) { acc.call_id = Some(id.to_string()); }
                if let Some(n) = tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) {
                    acc.name = Some(n.to_string());
                }
                if let Some(a) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|v| v.as_str()) {
                    acc.arguments.push_str(a);
                }
            }
        }
        Ok(out)
    }

    /// On [DONE]: first emit each FunctionCall completion item, then the synthesized assistant message, and finally Completed (§4.5 order).
    pub(crate) fn finish(&mut self) -> Vec<ResponseEvent> {
        let mut out = Vec::new();
        for (_idx, acc) in std::mem::take(&mut self.tool_calls) {
            let call_id = acc.call_id.unwrap_or_else(|| self.synth_id("call"));
            out.push(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                id: None,
                name: acc.name.unwrap_or_default(),
                namespace: None,
                arguments: acc.arguments,
                call_id,
                metadata: None,
            }));
        }
        if !self.text.is_empty() {
            out.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText { text: std::mem::take(&mut self.text) }],
                phase: None,
                metadata: None,
            }));
        }
        let response_id = self.response_id.take().unwrap_or_else(|| self.synth_id("resp"));
        out.push(ResponseEvent::Completed {
            response_id,
            token_usage: self.usage.as_ref().map(map_usage),
            end_turn: map_end_turn(self.finish_reason.as_deref()),
        });
        out
    }

    fn synth_id(&mut self, kind: &str) -> String {
        self.synth_counter += 1;
        format!("llmswitch-{kind}-{}", self.synth_counter)
    }
}

fn map_end_turn(fr: Option<&str>) -> Option<bool> {
    match fr {
        Some("stop") => Some(true),
        Some("tool_calls") => Some(false),
        _ => None, // length / unknown
    }
}

fn map_usage(u: &Value) -> TokenUsage {
    let g = |k: &str| u.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    TokenUsage {
        input_tokens: g("prompt_tokens"),
        cached_input_tokens: 0,
        output_tokens: g("completion_tokens"),
        reasoning_output_tokens: 0,
        total_tokens: g("total_tokens"),
    }
}
```

> `synth_id` uses an incrementing counter rather than random/time (the crate has no random source, so this keeps tests reproducible); it only needs to be consistent between call/synth within a single stream. Spec §4.5 says "if missing, synthesize `llmswitch-<uuid>`"; here a deterministic counter satisfies "synthesize when missing, and be stable".

- [ ] **Step 4: Test-forwarding function**

Add to the `testing` module in `lib.rs`:

```rust
pub fn translate_chat_sse_for_test(chunks: &[serde_json::Value], done: bool) -> Result<Vec<codex_api::ResponseEvent>, crate::ConnError> {
    let mut st = crate::connector::chat::chat_sse::ChatSseState::default();
    let mut out = Vec::new();
    for c in chunks { out.extend(st.push_chunk(c)?); }
    if done { out.extend(st.finish()); }
    Ok(out)
}
```

(Correspondingly, change `mod chat_sse;` to `pub(crate) mod chat_sse;` in `connector/chat.rs` so that testing can access it.)

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cd zmod/llm-switch && cargo test --test chat_sse_test`
Expected: 3 tests PASS.

- [ ] **Step 6: Golden SSE fixture**

Take one real chunk sequence from the OpenAiChat streaming tests in `../3rd/proxy/rust-llm-proxy`, drop it into `tests/fixtures/chat_sse_*.jsonl` (one chunk per line), and in the test parse it line by line, feed `push_chunk`, and assert the event sequence.

- [ ] **Step 7: Commit**

```bash
git add zmod/llm-switch/src/connector/chat_sse.rs zmod/llm-switch/src/connector/chat.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/chat_sse_test.rs zmod/llm-switch/tests/fixtures
git commit -m "feat(llm-switch): chat SSE->ResponseEvent (text accum, tool aggregation, synth message, Completed)"
```
