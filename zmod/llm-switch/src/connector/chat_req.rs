//! Chat outbound request construction (Task 04)
//!
//! Translate codex `ResponsesApiRequest` into OpenAI Chat Completions request JSON.
//!
//! # Step 0 Verified actual type variants
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
//! - Outbound discard: `Reasoning`, `CompactionTrigger`
//! - Hard fail: `LocalShellCall`, `ToolSearchCall`, `CustomToolCall`, `CustomToolCallOutput`,
//!   `ToolSearchOutput`, `WebSearchCall`, `ImageGenerationCall`, `Compaction`,
//!   `ContextCompaction`, `Other`
//! - Normal mapping: `Message`, `FunctionCall` (no namespace), `FunctionCallOutput`, `AgentMessage`

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody,
    FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseItem,
};

use crate::connector::{ConnError, EgressCtx};

/// Build Chat Completions request JSON.
///
/// Covers spec §4.2, §4.0/§4.0b, §4.6, §4.8, §4.9, §4.10, §4.11, §7.1.
pub(crate) fn build_chat_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // ── Tool definition tiering (§4.0b) ────────────────────────────────
    let tools = map_tools(&req.tools)?;

    // ── messages construction ──────────────────────────────────────────
    let mut messages: Vec<Value> = Vec::new();

    // instructions → system
    if !req.instructions.is_empty() {
        messages.push(json!({"role": "system", "content": req.instructions}));
    }

    // call_id tracking: seen calls set, calls awaiting results set
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut calls_needing_result: HashSet<String> = HashSet::new();

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                messages.push(map_message(role, content)?);
            }

            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                // Namespaced function calls not supported in v1 (§4.0)
                if namespace.is_some() {
                    return Err(ConnError::HardFail(format!(
                        "namespaced function call '{name}' not supported in v1 chat connector"
                    )));
                }
                seen_calls.insert(call_id.clone());
                calls_needing_result.insert(call_id.clone());
                messages.push(json!({
                    "role": "assistant",
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
                    // Orphan result → discard (§4.10)
                    tracing::warn!(
                        call_id = %call_id,
                        "discarding orphan tool result (no corresponding FunctionCall)"
                    );
                    continue;
                }
                calls_needing_result.remove(call_id);
                messages.push(map_function_call_output(call_id, output)?);
            }

            ResponseItem::AgentMessage { content, .. } => {
                messages.push(map_agent_message(content)?);
            }

            // ── Outbound discard (§4.0 / §4.4) ────────────────────────
            ResponseItem::Reasoning { .. } => {
                // Reasoning history items discarded on egress, local history unchanged
            }
            ResponseItem::CompactionTrigger { .. } => {
                // CompactionTrigger discarded on egress
            }

            // ── v1 hard fail variants (§4.0) ──────────────────────────
            ResponseItem::LocalShellCall { .. } => {
                return Err(ConnError::HardFail(
                    "LocalShellCall not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::ToolSearchCall { .. } => {
                return Err(ConnError::HardFail(
                    "ToolSearchCall not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::ToolSearchOutput { .. } => {
                return Err(ConnError::HardFail(
                    "ToolSearchOutput not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::WebSearchCall { .. } => {
                return Err(ConnError::HardFail(
                    "WebSearchCall not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::ImageGenerationCall { .. } => {
                return Err(ConnError::HardFail(
                    "ImageGenerationCall not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::CustomToolCall { .. } => {
                return Err(ConnError::HardFail(
                    "CustomToolCall not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::CustomToolCallOutput { .. } => {
                return Err(ConnError::HardFail(
                    "CustomToolCallOutput not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::Compaction { .. } => {
                return Err(ConnError::HardFail(
                    "Compaction (encrypted content) not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::ContextCompaction { .. } => {
                return Err(ConnError::HardFail(
                    "ContextCompaction (encrypted content) not supported in v1 chat connector".into(),
                ));
            }
            ResponseItem::Other => {
                return Err(ConnError::HardFail(
                    "unknown ResponseItem variant (Other) not supported in v1 chat connector".into(),
                ));
            }
        }
    }

    // ── Orphan call remediation: inject placeholder results (§4.10) ────
    // Note: orphan call_ids have no fixed order; injection is at end; reordering step will normalize them
    for call_id in &calls_needing_result {
        tracing::warn!(
            call_id = %call_id,
            "injecting placeholder tool result (orphan FunctionCall has no corresponding FunctionCallOutput)"
        );
        messages.push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": "[No output available yet]"
        }));
    }

    // ── tool message reordering (§4.10, replicates _reorder_tool_messages) ────
    let messages = reorder_tool_messages(messages);

    // ── assemble top-level body ────────────────────────────────────────
    let mut body = json!({
        "model": ctx.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
        "parallel_tool_calls": req.parallel_tool_calls,
    });

    if let Some(tools_arr) = tools {
        body["tools"] = Value::Array(tools_arr);
        // tool_choice only written if tools present; otherwise stripped (§4.10)
        if let Some(tc) = map_tool_choice(&req.tool_choice)? {
            body["tool_choice"] = tc;
        }
    }

    // ── §7.1 field downgrade ──────────────────────────────────────────
    apply_field_downgrade(&mut body, req);

    Ok(body)
}

// ── map_message ───────────────────────────────────────────────────────────────

/// Convert `Message` variant's content list into Chat message.
/// Image content → hard fail (§4.9, v1 lacks capability flag).
fn map_message(role: &str, content: &[ContentItem]) -> Result<Value, ConnError> {
    let mut text = String::new();
    for c in content {
        match c {
            ContentItem::InputText { text: t } | ContentItem::OutputText { text: t } => {
                text.push_str(t);
            }
            ContentItem::InputImage { .. } => {
                return Err(ConnError::HardFail(
                    "image input not supported in v1 chat connector (no capability flag)".into(),
                ));
            }
        }
    }
    Ok(json!({"role": role, "content": text}))
}

// ── map_agent_message ─────────────────────────────────────────────────────────

/// Convert `AgentMessage` variant into assistant message.
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
                    "AgentMessage contains encrypted content (EncryptedContent), v1 chat connector cannot read it".into(),
                ));
            }
        }
    }
    Ok(json!({"role": "assistant", "content": text}))
}

// ── map_function_call_output ──────────────────────────────────────────────────

/// Convert `FunctionCallOutput` into Chat tool message.
/// - `success == Some(false)` → prefix with `[tool error]` (Chat has no is_error field).
/// - ContentItems with images/encrypted → hard fail (§4.6).
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
            "tool result marked as failure; adding [tool error] prefix (Chat protocol has no is_error field)"
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

/// Convert `FunctionCallOutputContentItem` list into plain text.
/// On encountering images or encrypted content → hard fail (§4.6).
fn content_items_to_text(items: &[FunctionCallOutputContentItem]) -> Result<String, ConnError> {
    let mut text = String::new();
    for item in items {
        match item {
            FunctionCallOutputContentItem::InputText { text: t } => {
                text.push_str(t);
            }
            FunctionCallOutputContentItem::InputImage { .. } => {
                return Err(ConnError::HardFail(
                    "tool result contains image content (InputImage), v1 chat connector does not support it".into(),
                ));
            }
            FunctionCallOutputContentItem::EncryptedContent { .. } => {
                return Err(ConnError::HardFail(
                    "tool result contains encrypted content (EncryptedContent), v1 chat connector does not support it".into(),
                ));
            }
        }
    }
    Ok(text)
}

// ── map_tools ─────────────────────────────────────────────────────────────────

/// Convert tool definition list from Responses format to Chat Completions format.
/// v1 only supports standard `function` type; other types → hard fail (§4.0b).
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
                    "tool definition type {:?} not supported in v1 chat connector (only standard function supported)",
                    other
                )));
            }
        }
    }
    Ok(Some(out))
}

// ── map_tool_choice ───────────────────────────────────────────────────────────

/// Map codex `tool_choice` string to corresponding Chat Completions value (§4.11).
fn map_tool_choice(tc: &str) -> Result<Option<Value>, ConnError> {
    match tc {
        "auto" => Ok(Some(json!("auto"))),
        "none" => Ok(Some(json!("none"))),
        "required" => Ok(Some(json!("required"))),
        "" => Ok(None),
        // If codex uses JSON string to express forced specification of a function, parse and convert format
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
                "tool_choice '{other}' cannot be expressed in Chat Completions protocol"
            )))
        }
    }
}

// ── reorder_tool_messages ─────────────────────────────────────────────────────

/// Reorder tool messages so each tool message immediately follows the assistant tool_calls that produced it (§4.10).
///
/// Replicates llm-rosetta `_reorder_tool_messages` algorithm:
/// 1. First collect all tool messages, index by `tool_call_id`.
/// 2. Iterate non-tool messages; after assistant message with `tool_calls`,
///    immediately insert corresponding tool messages in order of `tool_calls[].id`.
/// 3. Orphan tool messages not inserted are appended at end (with warn).
fn reorder_tool_messages(messages: Vec<Value>) -> Vec<Value> {
    // Collect tool messages by tool_call_id (can be multiple per id, preserving order)
    let mut tool_by_id: HashMap<String, Vec<Value>> = HashMap::new();
    let mut non_tool: Vec<Value> = Vec::new();

    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
            if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                tool_by_id.entry(id.to_string()).or_default().push(msg);
            } else {
                // tool message without tool_call_id, treat as normal message
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

    // Append unmatched tool messages at end
    for (id, remaining) in tool_by_id {
        tracing::warn!(
            tool_call_id = %id,
            count = remaining.len(),
            "tool message has no corresponding assistant tool_calls, appending at end"
        );
        result.extend(remaining);
    }

    result
}

// ── apply_field_downgrade ─────────────────────────────────────────────────────

/// §7.1 field downgrade: downgrade or discard fields specific to Responses API.
///
/// - `reasoning.effort` → `reasoning_effort` (safe to pass through)
/// - Other `reasoning` fields → warn then discard
/// - `text.format` containing json_schema → `response_format`
/// - `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` → silently discard
fn apply_field_downgrade(body: &mut Value, req: &codex_api::ResponsesApiRequest) {
    // reasoning.effort → reasoning_effort
    if let Some(reasoning) = &req.reasoning {
        if let Some(effort) = &reasoning.effort {
            // ReasoningEffortConfig implements serde, serializes as string (low/medium/high)
            if let Ok(effort_val) = serde_json::to_value(effort) {
                body["reasoning_effort"] = effort_val;
            }
        } else if reasoning.summary.is_some() || reasoning.context.is_some() {
            tracing::warn!("reasoning.summary/context not supported in v1 chat connector, discarded");
        }
    }

    // text.format → response_format (json_schema only)
    // TextFormat serializes as {"type":"json_schema","strict":bool,"schema":{...},"name":"..."}
    // Chat API expects {"type":"json_schema","json_schema":{"name":"...","schema":{...},"strict":true}}
    if let Some(text_controls) = &req.text {
        if let Some(format) = &text_controls.format {
            if let Ok(fmt_val) = serde_json::to_value(format) {
                if fmt_val.get("type").and_then(|t| t.as_str()) == Some("json_schema") {
                    // Restructure from TextFormat into Chat API required shape
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
                        "text.format cannot be mapped in v1 chat connector, discarded"
                    );
                }
            }
        }
        // verbosity not mapped
    }

    // store/include/prompt_cache_key/service_tier/client_metadata silently discarded (§7.1 safely ignorable)
}
