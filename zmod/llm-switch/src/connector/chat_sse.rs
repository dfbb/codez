//! chat SSE→ResponseEvent state machine (Task 05)
//!
//! `ChatSseState` is a purely functional accumulator: feed it one parsed JSON chunk at a time (`push_chunk`),
//! and on receiving `[DONE]`, call `finish()` to emit the synthesized assistant message completion item + `Completed`.
//! No I/O, no async, making offline unit testing easy.

use std::collections::BTreeMap;

use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ReasoningItemContent, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use serde_json::Value;

use crate::connector::ConnError;

// ─── Internal accumulator ──────────────────────────────────────────────────────────

/// Streaming aggregation state for a single tool_call.
#[derive(Default)]
struct ToolAcc {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

// ─── Main state machine ─────────────────────────────────────────────────────────────

/// Chat Completions SSE chunk accumulator.
///
/// Usage:
/// ```ignore
/// let mut st = ChatSseState::default();
/// for chunk in sse_chunks { events.extend(st.push_chunk(&chunk)?); }
/// events.extend(st.finish());   // call when [DONE] is received
/// ```
#[derive(Default)]
pub(crate) struct ChatSseState {
    /// accumulated text (used to synthesize the assistant message).
    text: String,
    /// accumulated reasoning_content (the thinking stream of thinking models, also given back to codex to store on resend).
    reasoning: String,
    /// this response's ID (taken from the first chunk's `id` field).
    response_id: Option<String>,
    /// the last `finish_reason`.
    finish_reason: Option<String>,
    /// the `usage` field (usually in the final chunk).
    usage: Option<Value>,
    /// tool_calls aggregated by `index`. BTreeMap guarantees ordered output.
    tool_calls: BTreeMap<i64, ToolAcc>,
    /// incrementing counter for synthesized IDs (deterministic, reproducible in tests).
    synth_counter: u64,
}

impl ChatSseState {
    /// Feed one parsed JSON chunk, returning the `ResponseEvent`s it produces.
    ///
    /// An error is only triggered when the chunk itself carries an `error` field (HardFail semantics).
    /// Missing fields / type mismatches are silently skipped (robust-handling principle).
    pub(crate) fn push_chunk(&mut self, chunk: &Value) -> Result<Vec<ResponseEvent>, ConnError> {
        let mut out = Vec::new();

        // chunk carries an error field → stream error
        if let Some(err) = chunk.get("error") {
            return Err(ConnError::HardFail(format!("upstream error in SSE chunk: {err}")));
        }

        // take the response ID (only record the first non-empty one)
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                self.response_id.get_or_insert_with(|| id.to_string());
            }
        }

        // usage (usually appears in the last data chunk or at the end of the stream)
        if let Some(u) = chunk.get("usage") {
            if !u.is_null() {
                self.usage = Some(u.clone());
            }
        }

        // take choices[0]
        let Some(choice) = chunk.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) else {
            return Ok(out);
        };

        // finish_reason
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            if !fr.is_empty() {
                self.finish_reason = Some(fr.to_string());
            }
        }

        let delta = choice.get("delta");

        // text delta
        if let Some(content) = delta
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
        {
            if !content.is_empty() {
                self.text.push_str(content);
                // OutputTextDelta is for display only, not accumulated (accumulation happens in self.text)
                out.push(ResponseEvent::OutputTextDelta(content.to_string()));
            }
        }

        // reasoning_content delta (the thinking stream of thinking models like DeepSeek).
        // Accumulated and synthesized into a Reasoning item in finish() to hand back to codex, so it can display
        // and resend on the next round (satisfying the upstream constraint that "the assistant message for tool_calls must carry reasoning_content").
        if let Some(rc) = delta
            .and_then(|d| d.get("reasoning_content"))
            .and_then(Value::as_str)
        {
            if !rc.is_empty() {
                self.reasoning.push_str(rc);
                out.push(ResponseEvent::ReasoningContentDelta {
                    delta: rc.to_string(),
                    content_index: 0,
                });
            }
        }

        // tool_calls delta (aggregated by index)
        if let Some(tcs) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for tc in tcs {
                let idx = tc.get("index").and_then(Value::as_i64).unwrap_or(0);
                let acc = self.tool_calls.entry(idx).or_default();

                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        acc.call_id = Some(id.to_string());
                    }
                }
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func.get("name").and_then(Value::as_str) {
                        if !name.is_empty() {
                            acc.name = Some(name.to_string());
                        }
                    }
                    if let Some(args) = func.get("arguments").and_then(Value::as_str) {
                        acc.arguments.push_str(args);
                    }
                }
            }
        }

        Ok(out)
    }

    /// Called when `[DONE]` is received.
    ///
    /// Order (§4.5):
    /// 1. each `FunctionCall` completion item (in index order)
    /// 2. synthesized assistant `Message` completion item (if there is text)
    /// 3. `Completed`
    pub(crate) fn finish(&mut self) -> Vec<ResponseEvent> {
        let mut out = Vec::new();

        // 0. Reasoning item (if there is a thinking stream): placed before FunctionCall / Message,
        //    so that in codex history the Reasoning sits right before its tool_call, and the next outbound round resends it correctly.
        if !self.reasoning.is_empty() {
            out.push(ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: Some(vec![ReasoningItemContent::ReasoningText {
                    text: std::mem::take(&mut self.reasoning),
                }]),
                encrypted_content: None,
                metadata: None,
            }));
        }

        // 1. FunctionCall completion items
        for (_idx, acc) in std::mem::take(&mut self.tool_calls) {
            // synthesize when call_id is missing (§4.8 call_id backfill)
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

        // 2. synthesized assistant message completion item (§4.5)
        // only synthesize an assistant Message completion item when there is accumulated text; a pure tool-call response only emits FunctionCall items + Completed
        // (no text means nothing to synthesize; whether an empty Message is needed is confirmed against codex's consumer side when Task 08 wires it up).
        if !self.text.is_empty() {
            out.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: std::mem::take(&mut self.text),
                }],
                phase: None,
                metadata: None,
            }));
        }

        // 3. Completed
        let response_id = self
            .response_id
            .take()
            .unwrap_or_else(|| self.synth_id("resp"));
        out.push(ResponseEvent::Completed {
            response_id,
            token_usage: self.usage.as_ref().map(map_usage),
            end_turn: map_end_turn(self.finish_reason.as_deref()),
        });

        out
    }

    /// Generate a deterministic synthesized ID (avoids random dependencies, reproducible in tests).
    fn synth_id(&mut self, kind: &str) -> String {
        self.synth_counter += 1;
        format!("llmswitch-{kind}-{}", self.synth_counter)
    }
}

// ─── Helper functions ─────────────────────────────────────────────────────────────────

/// Map `finish_reason` to `end_turn` (§4.5).
/// - `"stop"` → `Some(true)` (model stopped on its own)
/// - `"tool_calls"` → `Some(false)` (still needs a tool call)
/// - `"length"`/unknown reason → `None` (truncation/unknown, no determination of turn end)
fn map_end_turn(fr: Option<&str>) -> Option<bool> {
    match fr {
        Some("stop") => Some(true),
        Some("tool_calls") => Some(false),
        _ => None, // length/unknown
    }
}

/// Map the Chat Completions `usage` JSON to `TokenUsage`.
/// Default to 0 when fields are missing (robust handling).
fn map_usage(u: &Value) -> TokenUsage {
    let g = |k: &str| u.get(k).and_then(Value::as_i64).unwrap_or(0);
    TokenUsage {
        input_tokens: g("prompt_tokens"),
        cached_input_tokens: u
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        output_tokens: g("completion_tokens"),
        reasoning_output_tokens: 0,
        total_tokens: g("total_tokens"),
    }
}

// ─── SseTranslator impl ──────────────────────────────────────────────────────

impl crate::connector::SseTranslator for ChatSseState {
    fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, crate::connector::ConnError> {
        self.push_chunk(data)
    }
    fn finish(&mut self) -> Vec<codex_api::ResponseEvent> {
        self.finish()
    }
}

// ─── Convenience functions (for the testing module and direct calls) ────────────────────────────────────

/// Run an entire SSE chunk sequence to completion, returning all events.
/// When `done = true`, automatically calls `finish()` (equivalent to receiving `[DONE]`).
pub(crate) fn translate_chat_sse(
    chunks: &[Value],
    done: bool,
) -> Result<Vec<ResponseEvent>, ConnError> {
    let mut st = ChatSseState::default();
    let mut out = Vec::new();
    for c in chunks {
        out.extend(st.push_chunk(c)?);
    }
    if done {
        out.extend(st.finish());
    }
    Ok(out)
}
