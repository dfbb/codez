//! chat outbound request construction (Task 04)
//!
//! Translates a codex `ResponsesApiRequest` into OpenAI Chat Completions request JSON.
//!
//! # Real type variants verified in Step 0
//!
//! ## ContentItem
//! - `InputText { text: String }`
//! - `InputImage { image_url: String, detail: Option<ImageDetail> }`
//! - `OutputText { text: String }`
//!
//! ## FunctionCallOutputContentItem
//! - `InputText { text: String }`
//! - `InputImage { image_url: String, detail: Option<ImageDetail> }`
//! - `EncryptedContent { encrypted_content: String }`
//!
//! ## FunctionCallOutputBody
//! - `Text(String)`
//! - `ContentItems(Vec<FunctionCallOutputContentItem>)`
//!
//! ## ResponseItem (16 variants)
//! - dropped on egress: `Reasoning`, `CompactionTrigger`
//! - hard fail: `LocalShellCall`, `ToolSearchCall`, `CustomToolCall`, `CustomToolCallOutput`,
//!   `ToolSearchOutput`, `WebSearchCall`, `ImageGenerationCall`, `Compaction`,
//!   `ContextCompaction`, `Other`
//! - normal mapping: `Message`, `FunctionCall` (no namespace), `FunctionCallOutput`, `AgentMessage`

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody,
    FunctionCallOutputContentItem, FunctionCallOutputPayload, ReasoningItemContent, ResponseItem,
};

use crate::connector::{ConnError, EgressCtx};

/// Build the Chat Completions request JSON.
///
/// Covers spec §4.2, §4.0/§4.0b, §4.6, §4.8, §4.9, §4.10, §4.11, §7.1.
pub(crate) fn build_chat_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // ── Tool definition grading (§4.0b) ──────────────────────────────────────────
    let tools = map_tools(&req.tools)?;

    // ── messages construction ──────────────────────────────────────────────────
    let mut messages: Vec<Value> = Vec::new();

    // instructions → system
    if !req.instructions.is_empty() {
        messages.push(json!({"role": "system", "content": req.instructions}));
    }

    // call_id tracking: set of seen calls, set of calls awaiting results
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut calls_needing_result: HashSet<String> = HashSet::new();
    // thinking models (DeepSeek etc.) require that each assistant message carrying tool_calls is sent back
    // with the `reasoning_content` that produced it, otherwise the upstream returns 400. In codex history the
    // Reasoning item sits right before its FunctionCall; cache the most recent Reasoning text and attach it to the next assistant tool_calls.
    let mut pending_reasoning: Option<String> = None;

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                pending_reasoning = None;
                messages.push(map_message(role, content)?);
            }

            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                // namespaced function calls are not supported in v1 (§4.0)
                if namespace.is_some() {
                    return Err(ConnError::HardFail(format!(
                        "命名空间函数调用 '{name}' 在 v1 chat connector 中不支持"
                    )));
                }
                seen_calls.insert(call_id.clone());
                calls_needing_result.insert(call_id.clone());
                // reasoning_content: take the cached Reasoning text; placeholder when missing (thinking models
                // hard-require non-empty, replicating cc-switch's "tool call" fallback).
                let reasoning = pending_reasoning
                    .take()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "tool call".to_string());
                messages.push(json!({
                    "role": "assistant",
                    "reasoning_content": reasoning,
                    "tool_calls": [{
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }]
                }));
            }

            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                if !seen_calls.contains(call_id) {
                    // orphan result → delete (§4.10)
                    tracing::warn!(
                        call_id = %call_id,
                        "丢弃孤儿 tool result（无对应 FunctionCall）"
                    );
                    continue;
                }
                calls_needing_result.remove(call_id);
                messages.push(map_function_call_output(call_id, output)?);
            }

            ResponseItem::AgentMessage { content, .. } => {
                messages.push(map_agent_message(content)?);
            }

            // ── dropped on egress (§4.0 / §4.4) ────────────────────────────────
            ResponseItem::Reasoning { content, .. } => {
                // No longer simply dropped: cache the reasoning text and attach it to the
                // immediately following assistant tool_calls message (thinking-model resend requirement).
                // It produces no standalone message of its own, nor is it written into codex's local history (only the request copy is modified).
                if let Some(text) = reasoning_items_to_text(content.as_deref()) {
                    pending_reasoning = Some(text);
                }
            }
            ResponseItem::CompactionTrigger { .. } => {
                // CompactionTrigger dropped on egress
            }

            // ── v1 hard-fail variants (§4.0) ─────────────────────────────────
            ResponseItem::LocalShellCall { .. } => {
                return Err(ConnError::HardFail(
                    "LocalShellCall 在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::ToolSearchCall { .. } => {
                return Err(ConnError::HardFail(
                    "ToolSearchCall 在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::ToolSearchOutput { .. } => {
                return Err(ConnError::HardFail(
                    "ToolSearchOutput 在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::WebSearchCall { .. } => {
                return Err(ConnError::HardFail(
                    "WebSearchCall 在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::ImageGenerationCall { .. } => {
                return Err(ConnError::HardFail(
                    "ImageGenerationCall 在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::CustomToolCall { .. } => {
                return Err(ConnError::HardFail(
                    "CustomToolCall 在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::CustomToolCallOutput { .. } => {
                return Err(ConnError::HardFail(
                    "CustomToolCallOutput 在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::Compaction { .. } => {
                return Err(ConnError::HardFail(
                    "Compaction（加密内容）在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::ContextCompaction { .. } => {
                return Err(ConnError::HardFail(
                    "ContextCompaction（加密内容）在 v1 chat connector 中不支持".into(),
                ));
            }
            ResponseItem::Other => {
                return Err(ConnError::HardFail(
                    "未知 ResponseItem 变体（Other）在 v1 chat connector 中不支持".into(),
                ));
            }
        }
    }

    // ── Orphan call repair: inject placeholder results (§4.10) ────────────────────────────
    // Note: orphan call_ids have no fixed order, injected at the end; the reordering step puts them back in place
    for call_id in &calls_needing_result {
        tracing::warn!(
            call_id = %call_id,
            "注入占位 tool result（孤儿 FunctionCall 无对应 FunctionCallOutput）"
        );
        messages.push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": "[No output available yet]"
        }));
    }

    // ── tool message reordering (§4.10, replicates _reorder_tool_messages) ────────────
    let messages = reorder_tool_messages(messages);

    // ── Assemble top-level body ─────────────────────────────────────────────────
    let mut body = json!({
        "model": ctx.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    if let Some(tools_arr) = tools {
        body["tools"] = Value::Array(tools_arr);
        // tool_choice / parallel_tool_calls are only written when there are tools; otherwise stripped (§4.10).
        // The upstream returns 400 for "parallel_tool_calls set but no tools", so the two are handled together.
        if let Some(tc) = map_tool_choice(&req.tool_choice)? {
            body["tool_choice"] = tc;
        }
        body["parallel_tool_calls"] = json!(req.parallel_tool_calls);
    }

    // ── §7.1 field downgrade ─────────────────────────────────────────────────
    apply_field_downgrade(&mut body, req);

    Ok(body)
}

// ── map_message ───────────────────────────────────────────────────────────────

/// Concatenate a Reasoning item's content blocks into plain text (both `ReasoningText` / `Text` variants take text).
/// No content or all empty → None.
fn reasoning_items_to_text(content: Option<&[ReasoningItemContent]>) -> Option<String> {
    let items = content?;
    let mut text = String::new();
    for c in items {
        match c {
            ReasoningItemContent::ReasoningText { text: t }
            | ReasoningItemContent::Text { text: t } => text.push_str(t),
        }
    }
    if text.trim().is_empty() { None } else { Some(text) }
}

/// Convert a `Message` variant's content list into a Chat message.
/// Image content → hard fail (§4.9, no capability flag in v1).
fn map_message(role: &str, content: &[ContentItem]) -> Result<Value, ConnError> {
    let mut text = String::new();
    for c in content {
        match c {
            ContentItem::InputText { text: t } | ContentItem::OutputText { text: t } => {
                text.push_str(t);
            }
            ContentItem::InputImage { .. } => {
                return Err(ConnError::HardFail(
                    "图片输入在 v1 chat connector 中不支持（无能力判定标志）".into(),
                ));
            }
        }
    }
    // Normalize role to the set Chat Completions recognizes: codex emits `developer` (developer instruction,
    // the newer OpenAI convention), but DeepSeek and others only recognize system/user/assistant/tool —
    // semantically a developer instruction is system-level, so map it to `system`. The rest pass through unchanged.
    let role = match role {
        "developer" => "system",
        other => other,
    };
    Ok(json!({"role": role, "content": text}))
}

// ── map_agent_message ─────────────────────────────────────────────────────────

/// Convert an `AgentMessage` variant into an assistant message.
/// EncryptedContent → hard fail (§4.0).
fn map_agent_message(content: &[AgentMessageInputContent]) -> Result<Value, ConnError> {
    let mut text = String::new();
    for c in content {
        match c {
            AgentMessageInputContent::InputText { text: t } => {
                text.push_str(t);
            }
            AgentMessageInputContent::EncryptedContent { .. } => {
                return Err(ConnError::HardFail(
                    "AgentMessage 含加密内容（EncryptedContent），v1 chat connector 无法读取".into(),
                ));
            }
        }
    }
    Ok(json!({"role": "assistant", "content": text}))
}

// ── map_function_call_output ──────────────────────────────────────────────────

/// Convert a `FunctionCallOutput` into a Chat tool message.
/// - `success == Some(false)` → prefix `[tool error]` (Chat has no is_error field).
/// - ContentItems containing images/encrypted → hard fail (§4.6).
fn map_function_call_output(
    call_id: &str,
    output: &FunctionCallOutputPayload,
) -> Result<Value, ConnError> {
    let mut text = match &output.body {
        FunctionCallOutputBody::Text(t) => t.clone(),
        FunctionCallOutputBody::ContentItems(items) => content_items_to_text(items)?,
    };

    if output.success == Some(false) {
        tracing::warn!(
            call_id = %call_id,
            "工具结果标记为失败；添加 [tool error] 前缀（Chat 协议无 is_error 字段）"
        );
        text = format!("[tool error] {text}");
    }

    Ok(json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": text
    }))
}

// ── content_items_to_text ─────────────────────────────────────────────────────

/// Convert a list of `FunctionCallOutputContentItem` into plain text.
/// On encountering an image or encrypted content → hard fail (§4.6).
fn content_items_to_text(items: &[FunctionCallOutputContentItem]) -> Result<String, ConnError> {
    let mut text = String::new();
    for item in items {
        match item {
            FunctionCallOutputContentItem::InputText { text: t } => {
                text.push_str(t);
            }
            FunctionCallOutputContentItem::InputImage { .. } => {
                return Err(ConnError::HardFail(
                    "工具结果含图片内容（InputImage），v1 chat connector 不支持".into(),
                ));
            }
            FunctionCallOutputContentItem::EncryptedContent { .. } => {
                return Err(ConnError::HardFail(
                    "工具结果含加密内容（EncryptedContent），v1 chat connector 不支持".into(),
                ));
            }
        }
    }
    Ok(text)
}

// ── map_tools ─────────────────────────────────────────────────────────────────

/// Convert the tool definition list from Responses format into Chat Completions format.
/// v1 only supports the standard `function` type; other types → hard fail (§4.0b).
fn map_tools(tools: &[Value]) -> Result<Option<Vec<Value>>, ConnError> {
    if tools.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for t in tools {
        match t.get("type").and_then(|v| v.as_str()) {
            Some("function") => {
                let name = t.get("name").cloned().unwrap_or(Value::Null);
                let description = t.get("description").cloned().unwrap_or(Value::Null);
                let parameters = t
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                out.push(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": parameters
                    }
                }));
            }
            other => {
                return Err(ConnError::HardFail(format!(
                    "工具定义类型 {:?} 在 v1 chat connector 中不支持（仅支持标准 function）",
                    other
                )));
            }
        }
    }
    Ok(Some(out))
}

// ── map_tool_choice ───────────────────────────────────────────────────────────

/// Map codex's `tool_choice` string into the corresponding Chat Completions value (§4.11).
fn map_tool_choice(tc: &str) -> Result<Option<Value>, ConnError> {
    match tc {
        "auto" => Ok(Some(json!("auto"))),
        "none" => Ok(Some(json!("none"))),
        "required" => Ok(Some(json!("required"))),
        "" => Ok(None),
        // if codex uses a JSON string to force a specific function, parse and convert the format
        other => {
            if let Ok(v) = serde_json::from_str::<Value>(other) {
                if v.get("type").and_then(|x| x.as_str()) == Some("function") {
                    if let Some(name) = v.get("name") {
                        return Ok(Some(json!({
                            "type": "function",
                            "function": {"name": name}
                        })));
                    }
                }
            }
            Err(ConnError::HardFail(format!(
                "tool_choice '{other}' 无法在 Chat Completions 协议中表达"
            )))
        }
    }
}

// ── reorder_tool_messages ─────────────────────────────────────────────────────

/// Reorder tool messages so each tool message immediately follows the assistant tool_calls that produced it (§4.10).
///
/// Replicates the llm-rosetta `_reorder_tool_messages` algorithm:
/// 1. First collect all tool messages, indexing by `tool_call_id`.
/// 2. Iterate over non-tool messages; after an assistant message with `tool_calls`,
///    insert the corresponding tool messages immediately after, in `tool_calls[].id` order.
/// 3. Orphan tool messages that were not inserted are appended at the end (with a warn).
fn reorder_tool_messages(messages: Vec<Value>) -> Vec<Value> {
    // collect tool messages by tool_call_id (one id may have several, order preserved)
    let mut tool_by_id: HashMap<String, Vec<Value>> = HashMap::new();
    let mut non_tool: Vec<Value> = Vec::new();

    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
            if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                tool_by_id.entry(id.to_string()).or_default().push(msg);
            } else {
                // a tool message with no tool_call_id is treated as a normal message
                non_tool.push(msg);
            }
        } else {
            non_tool.push(msg);
        }
    }

    let mut result: Vec<Value> = Vec::new();
    for msg in non_tool {
        let is_assistant_with_calls = msg.get("role").and_then(|r| r.as_str()) == Some("assistant")
            && msg.get("tool_calls").and_then(|tc| tc.as_array()).is_some();

        result.push(msg.clone());

        if is_assistant_with_calls {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) {
                for tc in tool_calls {
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if let Some(tool_msgs) = tool_by_id.remove(id) {
                            result.extend(tool_msgs);
                        }
                    }
                }
            }
        }
    }

    // unmatched tool messages are appended at the end
    for (id, remaining) in tool_by_id {
        tracing::warn!(
            tool_call_id = %id,
            count = remaining.len(),
            "tool 消息无对应 assistant tool_calls，追加末尾"
        );
        result.extend(remaining);
    }

    result
}

// ── apply_field_downgrade ─────────────────────────────────────────────────────

/// §7.1 field downgrade: downgrade or drop Responses-API-specific fields.
///
/// - `reasoning.effort` → `reasoning_effort` (safe to pass through)
/// - other `reasoning` fields → dropped after a warn
/// - `text.format` containing json_schema → `response_format`
/// - `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` → silently dropped
fn apply_field_downgrade(body: &mut Value, req: &codex_api::ResponsesApiRequest) {
    // reasoning.effort → reasoning_effort
    if let Some(reasoning) = &req.reasoning {
        if let Some(effort) = &reasoning.effort {
            // ReasoningEffortConfig implements serde, serialized as a string (low/medium/high)
            if let Ok(effort_val) = serde_json::to_value(effort) {
                body["reasoning_effort"] = effort_val;
            }
        } else if reasoning.summary.is_some() || reasoning.context.is_some() {
            tracing::warn!("reasoning.summary/context 在 v1 chat connector 中不支持，已丢弃");
        }
    }

    // text.format → response_format (json_schema only)
    // TextFormat serializes to {"type":"json_schema","strict":bool,"schema":{...},"name":"..."}
    // The Chat API expects {"type":"json_schema","json_schema":{"name":"...","schema":{...},"strict":true}}
    if let Some(text_controls) = &req.text {
        if let Some(format) = &text_controls.format {
            if let Ok(fmt_val) = serde_json::to_value(format) {
                if fmt_val.get("type").and_then(|t| t.as_str()) == Some("json_schema") {
                    // reassemble the shape the Chat API needs from the TextFormat fields
                    let mut json_schema = serde_json::Map::new();
                    if let Some(name) = fmt_val.get("name") {
                        json_schema.insert("name".into(), name.clone());
                    }
                    if let Some(schema) = fmt_val.get("schema") {
                        json_schema.insert("schema".into(), schema.clone());
                    }
                    if let Some(strict) = fmt_val.get("strict") {
                        json_schema.insert("strict".into(), strict.clone());
                    }
                    body["response_format"] = json!({
                        "type": "json_schema",
                        "json_schema": Value::Object(json_schema)
                    });
                } else {
                    tracing::warn!(
                        format = ?fmt_val,
                        "text.format 在 v1 chat connector 中无法映射，已丢弃"
                    );
                }
            }
        }
        // verbosity is not mapped
    }

    // store/include/prompt_cache_key/service_tier/client_metadata silently dropped (§7.1 safe-to-ignore tier)
}
