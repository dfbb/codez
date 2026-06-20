//! Anthropic Messages SSE→ResponseEvent 状态机（Task 07）
//!
//! `AnthropicSseState` 是纯函数式累加器：每次喂一个已解析的 JSON 事件（`push_event`），
//! 收到 `message_stop` 后调用 `finish()` 发合成 assistant message 完成项 + `Completed`。
//! 无 I/O、无 async，便于离线单元测试。

use std::collections::BTreeMap;

use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use serde_json::Value;

use crate::connector::ConnError;

// ─── 内部聚合器 ──────────────────────────────────────────────────────────────

/// 单个 content block 的流式聚合状态。
#[derive(Default)]
struct BlockAcc {
    /// true = tool_use block；false = text block。
    is_tool_use: bool,
    /// tool_use block 的 id（→ call_id）。
    call_id: Option<String>,
    /// tool_use block 的 name。
    name: Option<String>,
    /// 聚合的 input_json_delta 片段。
    partial_json: String,
}

// ─── 主状态机 ─────────────────────────────────────────────────────────────────

/// Anthropic Messages SSE 事件累加器。
///
/// 使用方式：
/// ```ignore
/// let mut st = AnthropicSseState::default();
/// for evt in sse_events { events.extend(st.push_event(&evt)?); }
/// events.extend(st.finish());   // 收到 message_stop 时调用
/// ```
#[derive(Default)]
pub(crate) struct AnthropicSseState {
    /// 累计文本（用于合成 assistant message）。
    text: String,
    /// 本次响应 ID（取 message_start.message.id）。
    response_id: Option<String>,
    /// stop_reason（取 message_delta.delta.stop_reason）。
    stop_reason: Option<String>,
    /// message_start.message.usage.input_tokens。
    input_tokens: i64,
    /// message_delta.usage.output_tokens。
    output_tokens: i64,
    /// content block 按 index 聚合。BTreeMap 保证有序输出。
    blocks: BTreeMap<i64, BlockAcc>,
    /// 合成 ID 用的递增计数器（确定性，测试可复现）。
    synth_counter: u64,
}

impl AnthropicSseState {
    /// 喂一个已解析的 Anthropic SSE 事件 JSON，返回该事件产生的 `ResponseEvent` 列表。
    ///
    /// 错误只在事件类型为 `error` 时触发。
    /// 字段缺失/类型不符均静默跳过（稳健处理原则）。
    pub(crate) fn push_event(&mut self, evt: &Value) -> Result<Vec<ResponseEvent>, ConnError> {
        let mut out = Vec::new();

        match evt.get("type").and_then(Value::as_str) {
            Some("error") => {
                let msg = evt
                    .get("error")
                    .map(|e| e.to_string())
                    .unwrap_or_default();
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
                    if let Some(it) = message
                        .get("usage")
                        .and_then(|u| u.get("input_tokens"))
                        .and_then(Value::as_i64)
                    {
                        self.input_tokens = it;
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
                        if let Some(t) =
                            delta.and_then(|d| d.get("text")).and_then(Value::as_str)
                        {
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

            // content_block_stop / message_stop / ping → 无需即时事件
            _ => {}
        }

        Ok(out)
    }

    /// 收到 `message_stop` 后调用。
    ///
    /// 顺序（§4.5）：
    /// 1. 各 `FunctionCall` 完成项（按 block index 顺序）
    /// 2. 合成 assistant `Message` 完成项（若有文本）
    /// 3. `Completed`
    pub(crate) fn finish(&mut self) -> Vec<ResponseEvent> {
        let mut out = Vec::new();

        // 1. FunctionCall 完成项
        for (_idx, mut acc) in std::mem::take(&mut self.blocks) {
            if acc.is_tool_use {
                // 无参数工具的 partial_json 为空时，补 "{}"
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
                    metadata: None,
                }));
            }
        }

        // 2. 合成 assistant message 完成项（§4.5）
        // 纯 tool-call 响应只发 FunctionCall 项 + Completed，无文本则跳过
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
            token_usage: Some(TokenUsage {
                input_tokens: self.input_tokens,
                cached_input_tokens: 0,
                output_tokens: self.output_tokens,
                reasoning_output_tokens: 0,
                total_tokens: self.input_tokens + self.output_tokens,
            }),
            end_turn: map_end_turn(self.stop_reason.as_deref()),
        });

        out
    }

    /// 生成确定性合成 ID（避免随机依赖，测试可复现）。
    fn synth_id(&mut self, kind: &str) -> String {
        self.synth_counter += 1;
        format!("llmswitch-{kind}-{}", self.synth_counter)
    }
}

// ─── 辅助函数 ─────────────────────────────────────────────────────────────────

/// 把 Anthropic `stop_reason` 映射到 `end_turn`。
/// - `"end_turn"` → `Some(true)`（模型主动停止）
/// - `"tool_use"` → `Some(false)`（还需要工具调用）
/// - `"max_tokens"` → `None`（token 截断，语义不明确）
/// - 其他未知 reason → `None`
fn map_end_turn(stop_reason: Option<&str>) -> Option<bool> {
    match stop_reason {
        Some("end_turn") => Some(true),
        Some("tool_use") => Some(false),
        _ => None,
    }
}

// ─── 便利函数（供 testing 模块和直接调用） ────────────────────────────────────

/// 把整段 Anthropic SSE 事件序列跑完，返回所有事件。
/// `done = true` 时自动调用 `finish()`（相当于收到 `message_stop`）。
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
