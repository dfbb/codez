//! chat SSE→ResponseEvent 状态机（Task 05）
//!
//! `ChatSseState` 是纯函数式累加器：每次喂一个已解析的 JSON chunk（`push_chunk`），
//! 收到 `[DONE]` 时调用 `finish()` 发合成 assistant message 完成项 + `Completed`。
//! 无 I/O、无 async，便于离线单元测试。

use std::collections::BTreeMap;

use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codex_protocol::protocol::TokenUsage;
use serde_json::Value;

use crate::connector::ConnError;

// ─── 内部聚合器 ──────────────────────────────────────────────────────────────

/// 单条 tool_call 的流式聚合状态。
#[derive(Default)]
struct ToolAcc {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

// ─── 主状态机 ─────────────────────────────────────────────────────────────────

/// Chat Completions SSE chunk 累加器。
///
/// 使用方式：
/// ```ignore
/// let mut st = ChatSseState::default();
/// for chunk in sse_chunks { events.extend(st.push_chunk(&chunk)?); }
/// events.extend(st.finish());   // 收到 [DONE] 时调用
/// ```
#[derive(Default)]
pub(crate) struct ChatSseState {
    /// 累计文本（用于合成 assistant message）。
    text: String,
    /// 本次响应 ID（取第一个 chunk 的 `id` 字段）。
    response_id: Option<String>,
    /// 最后一个 `finish_reason`。
    finish_reason: Option<String>,
    /// `usage` 字段（通常在末尾 chunk）。
    usage: Option<Value>,
    /// tool_calls 按 `index` 聚合。BTreeMap 保证有序输出。
    tool_calls: BTreeMap<i64, ToolAcc>,
    /// 合成 ID 用的递增计数器（确定性，测试可复现）。
    synth_counter: u64,
}

impl ChatSseState {
    /// 喂一个解析好的 JSON chunk，返回该 chunk 产生的 `ResponseEvent` 列表。
    ///
    /// 错误只在 chunk 本身携带 `error` 字段时触发（HardFail 语义）。
    /// 字段缺失/类型不符均静默跳过（稳健处理原则）。
    pub(crate) fn push_chunk(&mut self, chunk: &Value) -> Result<Vec<ResponseEvent>, ConnError> {
        let mut out = Vec::new();

        // chunk 携带 error 字段 → 流报错
        if let Some(err) = chunk.get("error") {
            return Err(ConnError::HardFail(format!("upstream error in SSE chunk: {err}")));
        }

        // 取响应 ID（只记第一个非空的）
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                self.response_id.get_or_insert_with(|| id.to_string());
            }
        }

        // usage（通常出现在最后一个数据 chunk 或流末尾）
        if let Some(u) = chunk.get("usage") {
            if !u.is_null() {
                self.usage = Some(u.clone());
            }
        }

        // 取 choices[0]
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

        // 文本 delta
        if let Some(content) = delta
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
        {
            if !content.is_empty() {
                self.text.push_str(content);
                // OutputTextDelta 仅用于展示，不累计（累计在 self.text）
                out.push(ResponseEvent::OutputTextDelta(content.to_string()));
            }
        }

        // tool_calls delta（按 index 聚合）
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

    /// 收到 `[DONE]` 时调用。
    ///
    /// 顺序（§4.5）：
    /// 1. 各 `FunctionCall` 完成项（按 index 顺序）
    /// 2. 合成 assistant `Message` 完成项（若有文本）
    /// 3. `Completed`
    pub(crate) fn finish(&mut self) -> Vec<ResponseEvent> {
        let mut out = Vec::new();

        // 1. FunctionCall 完成项
        for (_idx, acc) in std::mem::take(&mut self.tool_calls) {
            // call_id 缺失时合成（§4.8 call_id 回填）
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

        // 2. 合成 assistant message 完成项（§4.5）
        // 仅当有累计文本时才合成 assistant Message 完成项；纯 tool-call 响应只发 FunctionCall 项 + Completed
        // （无文本即无可合成内容；是否需要空 Message 由 Task 08 接线时对照 codex 消费端确认）。
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

    /// 生成确定性合成 ID（避免随机依赖，测试可复现）。
    fn synth_id(&mut self, kind: &str) -> String {
        self.synth_counter += 1;
        format!("llmswitch-{kind}-{}", self.synth_counter)
    }
}

// ─── 辅助函数 ─────────────────────────────────────────────────────────────────

/// 把 `finish_reason` 映射到 `end_turn`。
/// - `"stop"` → `Some(true)`（模型主动停止）
/// - `"tool_calls"` → `Some(false)`（还需要工具调用）
/// - `"length"` → `Some(false)`（token 截断 = 非自然结束）
/// - 其他未知 reason → `None`
fn map_end_turn(fr: Option<&str>) -> Option<bool> {
    match fr {
        Some("stop") => Some(true),
        Some("tool_calls") => Some(false),
        Some("length") => Some(false), // token 截断，非自然结束
        _ => None,
    }
}

/// 把 Chat Completions `usage` JSON 映射到 `TokenUsage`。
/// 字段缺失时默认 0（稳健处理）。
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

// ─── 便利函数（供 testing 模块和直接调用） ────────────────────────────────────

/// 把整段 SSE chunk 序列跑完，返回所有事件。
/// `done = true` 时自动调用 `finish()`（相当于收到 `[DONE]`）。
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
