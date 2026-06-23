//! Chat SSE → ResponseEvent state machine (Task 05)
//!
//! `ChatSseState` is a pure functional accumulator: each time a parsed JSON chunk is fed (`push_chunk`),
//! when `[DONE]` is received, call `finish()` to emit a synthesized assistant message completion item + `Completed`.
//! No I/O, no async, convenient for offline unit testing.

use std::collections::BTreeMap;

use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use serde_json::Value;

use crate::connector::ConnError;

// ─── Internal aggregator ──────────────────────────────────────────────────────────────

/// Streaming aggregation state for a single tool_call.
#[derive(Default)]
struct ToolAcc {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

// ─── Main state machine ─────────────────────────────────────────────────────────────────

/// Chat Completions SSE chunk accumulator.
///
/// Usage:
/// ```ignore
/// let mut st = ChatSseState::default();
/// for chunk in sse_chunks { events.extend(st.push_chunk(&chunk)?); }
/// events.extend(st.finish());   // Call when [DONE] is received
/// ```
#[derive(Default)]
pub(crate) struct ChatSseState {
    /// Accumulated text (for synthesizing assistant message).
    text: String,
    /// Response ID for this response (taken from `id` field of first chunk).
    response_id: Option<String>,
    /// Last `finish_reason`.
    finish_reason: Option<String>,
    /// `usage` field (usually in final chunk).
    usage: Option<Value>,
    /// tool_calls aggregated by `index`. BTreeMap ensures ordered output.
    tool_calls: BTreeMap<i64, ToolAcc>,
    /// Incrementing counter for synthesized IDs (deterministic, tests are reproducible).
    synth_counter: u64,
}

impl ChatSseState {
    /// Feed a parsed JSON chunk, return list of `ResponseEvent`s produced by this chunk.
    ///
    /// Errors only trigger when the chunk carries an `error` field (HardFail semantics).
    /// Missing fields/type mismatches are silently skipped (robust handling principle).
    pub(crate) fn push_chunk(&mut self, chunk: &Value) -> Result<Vec<ResponseEvent>, ConnError> {
        let mut out = Vec::new();

        // Chunk carries error field → stream error
        if let Some(err) = chunk.get("error") {
            return Err(ConnError::HardFail(format!("upstream error in SSE chunk: {err}")));
        }

        // Get response ID (only record first non-empty one)
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                self.response_id.get_or_insert_with(|| id.to_string());
            }
        }

        // usage (usually appears in last data chunk or at end of stream)
        if let Some(u) = chunk.get("usage") {
            if !u.is_null() {
                self.usage = Some(u.clone());
            }
        }

        // Get choices[0]
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

        // Text delta
        if let Some(content) = delta
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
        {
            if !content.is_empty() {
                self.text.push_str(content);
                // OutputTextDelta is only for display, not accumulated (accumulated in self.text)
                out.push(ResponseEvent::OutputTextDelta(content.to_string()));
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

    /// Call when `[DONE]` is received.
    ///
    /// Order (§4.5):
    /// 1. `FunctionCall` completion items for each (in index order)
    /// 2. Synthesized assistant `Message` completion item (if text exists)
    /// 3. `Completed`
    pub(crate) fn finish(&mut self) -> Vec<ResponseEvent> {
        let mut out = Vec::new();

        // 1. FunctionCall completion items
        for (_idx, acc) in std::mem::take(&mut self.tool_calls) {
            // Synthesize call_id when missing (§4.8 call_id backfill)
            let call_id = acc.call_id.unwrap_or_else(|| self.synth_id("call"));
            out.push(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                id: None,
                name: acc.name.unwrap_or_default(),
                namespace: None,
                arguments: acc.arguments,
                call_id,
                internal_chat_message_metadata_passthrough: None,
            }));
        }

        // 2. Synthesize assistant message completion item (§4.5)
        // Only synthesize assistant Message completion item when accumulated text exists; pure tool-call responses only emit FunctionCall items + Completed
        // (no text means no content to synthesize; whether empty Message is needed will be confirmed in Task 08 integration against codex consumer).
        if !self.text.is_empty() {
            out.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: std::mem::take(&mut self.text),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
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

    /// Generate deterministic synthesized ID (avoid random dependency, tests are reproducible).
    fn synth_id(&mut self, kind: &str) -> String {
        self.synth_counter += 1;
        format!("llmswitch-{kind}-{}", self.synth_counter)
    }
}

// ─── Helper functions ─────────────────────────────────────────────────────────────────

/// Map `finish_reason` to `end_turn`.
/// - `"stop"` → `Some(true)` (model stopped voluntarily)
/// - `"tool_calls"` → `Some(false)` (tool calls still needed)
/// - `"length"` → `Some(false)` (token truncation = not a natural end)
/// - other unknown reason → `None`
fn map_end_turn(fr: Option<&str>) -> Option<bool> {
    match fr {
        Some("stop") => Some(true),
        Some("tool_calls") => Some(false),
        Some("length") => Some(false), // token truncation, not a natural end
        _ => None,
    }
}

/// Map Chat Completions `usage` JSON to `TokenUsage`.
/// Missing fields default to 0 (robust handling).
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

// ─── Convenience functions (for testing module and direct calls) ────────────────────────────────────

/// Run through entire SSE chunk sequence, return all events.
/// When `done = true`, automatically call `finish()` (equivalent to receiving `[DONE]`).
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
