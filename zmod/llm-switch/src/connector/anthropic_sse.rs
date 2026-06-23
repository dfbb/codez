//! Anthropic Messages SSE→ResponseEvent state machine (Task 07)
//!
//! `AnthropicSseState` is a pure functional accumulator: each time you feed a parsed JSON event (`push_event`),
//! and when `message_stop` is received, call `finish()` to emit a synthesized assistant message completion item + `Completed`.
//! No I/O, no async, convenient for offline unit testing.

use std::collections::BTreeMap;

use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use serde_json::Value;

use crate::connector::ConnError;

// ─── Internal accumulator ──────────────────────────────────────────────────────────────

/// Streaming aggregation state for a single content block.
#[derive(Default)]
struct BlockAcc {
    /// true = tool_use block; false = text block.
    is_tool_use: bool,
    /// tool_use block's id (→ call_id).
    call_id: Option<String>,
    /// tool_use block's name.
    name: Option<String>,
    /// Aggregated input_json_delta fragments.
    partial_json: String,
}

// ─── Main state machine ─────────────────────────────────────────────────────────────────

/// Anthropic Messages SSE event accumulator.
///
/// Usage:
/// ```ignore
/// let mut st = AnthropicSseState::default();
/// for evt in sse_events { events.extend(st.push_event(&evt)?); }
/// events.extend(st.finish());   // call when message_stop is received
/// ```
#[derive(Default)]
pub(crate) struct AnthropicSseState {
    /// Accumulated text (for synthesizing assistant message).
    text: String,
    /// Current response ID (from message_start.message.id).
    response_id: Option<String>,
    /// stop_reason (from message_delta.delta.stop_reason).
    stop_reason: Option<String>,
    /// message_start.message.usage.input_tokens.
    input_tokens: i64,
    /// message_start.message.usage.cache_read_input_tokens.
    cached_input_tokens: i64,
    /// message_start.message.usage.cache_creation_input_tokens.
    cache_creation_input_tokens: i64,
    /// message_delta.usage.output_tokens.
    output_tokens: i64,
    /// Content blocks aggregated by index. BTreeMap ensures ordered output.
    blocks: BTreeMap<i64, BlockAcc>,
    /// Incremental counter for synthesized IDs (deterministic, tests reproducible).
    synth_counter: u64,
}

impl AnthropicSseState {
    /// Feed a parsed Anthropic SSE event JSON, return the list of `ResponseEvent`s produced by this event.
    ///
    /// Errors only trigger when event type is `error`.
    /// Missing fields/type mismatches are silently skipped (robustness principle).
    pub(crate) fn push_event(&mut self, evt: &Value) -> Result<Vec<ResponseEvent>, ConnError> {
        let mut out = Vec::new();

        match evt.get("type").and_then(Value::as_str) {
            Some("error") => {
                let msg = evt.get("error").map(|e| e.to_string()).unwrap_or_default();
                return Err(ConnError::HardFail(format!(
                    "anthropic upstream error: {msg}"
                )));
            }

            Some("message_start") => {
                if let Some(message) = evt.get("message") {
                    if let Some(id) = message.get("id").and_then(Value::as_str) {
                        if !id.is_empty() {
                            self.response_id.get_or_insert_with(|| id.to_string());
                        }
                    }
                    if let Some(usage) = message.get("usage") {
                        if let Some(it) = usage.get("input_tokens").and_then(Value::as_i64) {
                            self.input_tokens = it;
                        }
                        if let Some(ct) =
                            usage.get("cache_read_input_tokens").and_then(Value::as_i64)
                        {
                            self.cached_input_tokens = ct;
                        }
                        if let Some(cc) = usage
                            .get("cache_creation_input_tokens")
                            .and_then(Value::as_i64)
                        {
                            self.cache_creation_input_tokens = cc;
                        }
                    }
                }
            }

            Some("content_block_start") => {
                let idx = evt.get("index").and_then(Value::as_i64).unwrap_or(0);
                let cb = evt.get("content_block");
                let acc = self.blocks.entry(idx).or_default();
                if cb.and_then(|c| c.get("type")).and_then(Value::as_str) == Some("tool_use") {
                    acc.is_tool_use = true;
                    acc.call_id = cb
                        .and_then(|c| c.get("id"))
                        .and_then(Value::as_str)
                        .map(String::from);
                    acc.name = cb
                        .and_then(|c| c.get("name"))
                        .and_then(Value::as_str)
                        .map(String::from);
                }
            }

            Some("content_block_delta") => {
                let idx = evt.get("index").and_then(Value::as_i64).unwrap_or(0);
                let delta = evt.get("delta");
                match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(Value::as_str) {
                            self.text.push_str(t);
                            out.push(ResponseEvent::OutputTextDelta(t.to_string()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(p) = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(Value::as_str)
                        {
                            self.blocks.entry(idx).or_default().partial_json.push_str(p);
                        }
                    }
                    _ => {}
                }
            }

            Some("message_delta") => {
                if let Some(sr) = evt
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(sr.to_string());
                }
                if let Some(ot) = evt
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_i64)
                {
                    self.output_tokens = ot;
                }
            }

            // content_block_stop / message_stop / ping → no immediate events needed
            _ => {}
        }

        Ok(out)
    }

    /// Call after receiving `message_stop`.
    ///
    /// Order (§4.5):
    /// 1. Each `FunctionCall` completion item (by block index order)
    /// 2. Synthesized assistant `Message` completion item (if text exists)
    /// 3. `Completed`
    pub(crate) fn finish(&mut self) -> Vec<ResponseEvent> {
        let mut out = Vec::new();

        // 1. FunctionCall completion items
        for (_idx, mut acc) in std::mem::take(&mut self.blocks) {
            if acc.is_tool_use {
                // If partial_json is empty for parameterless tools, fill in "{}"
                if acc.partial_json.is_empty() {
                    acc.partial_json = "{}".into();
                }
                let call_id = acc.call_id.unwrap_or_else(|| self.synth_id("call"));
                out.push(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                    id: None,
                    name: acc.name.unwrap_or_default(),
                    namespace: None,
                    arguments: acc.partial_json,
                    call_id,
                    internal_chat_message_metadata_passthrough: None,
                }));
            }
        }

        // 2. Synthesized assistant message completion item (§4.5)
        // Pure tool-call responses only emit FunctionCall items + Completed, skip if no text
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
            token_usage: Some(TokenUsage {
                input_tokens: self.input_tokens,
                cached_input_tokens: self.cached_input_tokens,
                output_tokens: self.output_tokens,
                reasoning_output_tokens: 0,
                total_tokens: self.cached_input_tokens
                    + self.cache_creation_input_tokens
                    + self.input_tokens
                    + self.output_tokens,
            }),
            end_turn: map_end_turn(self.stop_reason.as_deref()),
        });

        out
    }

    /// Generate deterministic synthesized IDs (avoid random dependency, tests reproducible).
    fn synth_id(&mut self, kind: &str) -> String {
        self.synth_counter += 1;
        format!("llmswitch-{kind}-{}", self.synth_counter)
    }
}

// ─── Helper functions ─────────────────────────────────────────────────────────────────

/// Map Anthropic `stop_reason` to `end_turn`.
/// - `"end_turn"` → `Some(true)` (model stops proactively)
/// - `"tool_use"` → `Some(false)` (tool calls needed)
/// - `"max_tokens"` → `None` (token truncation, semantics unclear)
/// - Other unknown reasons → `None`
fn map_end_turn(stop_reason: Option<&str>) -> Option<bool> {
    match stop_reason {
        Some("end_turn") => Some(true),
        Some("tool_use") => Some(false),
        _ => None,
    }
}

// ─── SseTranslator impl ──────────────────────────────────────────────────────

impl crate::connector::SseTranslator for AnthropicSseState {
    fn push(
        &mut self,
        data: &serde_json::Value,
    ) -> Result<Vec<codex_api::ResponseEvent>, crate::connector::ConnError> {
        self.push_event(data)
    }
    fn finish(&mut self) -> Vec<codex_api::ResponseEvent> {
        self.finish()
    }
}

// ─── Convenience functions (for testing module and direct calls) ────────────────────────────────────

/// Run through the entire Anthropic SSE event sequence, return all events.
/// When `done = true`, automatically call `finish()` (equivalent to receiving `message_stop`).
pub(crate) fn translate_anthropic_sse(
    events: &[Value],
    done: bool,
) -> Result<Vec<ResponseEvent>, ConnError> {
    let mut st = AnthropicSseState::default();
    let mut out = Vec::new();
    for evt in events {
        out.extend(st.push_event(evt)?);
    }
    if done {
        out.extend(st.finish());
    }
    Ok(out)
}
