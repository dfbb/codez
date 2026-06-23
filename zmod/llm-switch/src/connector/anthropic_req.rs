//! Anthropic outbound request construction (Task 06)
//!
//! Translate codex `ResponsesApiRequest` into Anthropic Messages request JSON.
//!
//! # Key differences from chat_req
//!
//! - `system` goes to top-level field (not role=system in messages)
//! - Message roles are only user / assistant
//! - `FunctionCall` → assistant content[tool_use], arguments string parsed into object
//! - `FunctionCallOutput` → user content[tool_result], `is_error` native field
//! - `max_tokens` required, fallback to default_max_tokens (default 4096)
//! - `parallel_tool_calls==false` → `tool_choice.disable_parallel_tool_use=true`
//! - Messages without tools flattened and reordered (content blocks grouped by turn; consecutive same-role blocks merged)
//! - Tool definitions: `{name, description, input_schema}` format (no top-level type field)
//!
//! # Step 0 reused types (Task 04 pinned)
//!
//! ## ContentItem
//! - `InputText { text: String }`
//! - `InputImage { image_url: String, detail: Option<ImageDetail> }` → hard fail
//! - `OutputText { text: String }`
//!
//! ## FunctionCallOutputContentItem
//! - `InputText { text: String }`
//! - `InputImage { .. }` → hard fail
//! - `EncryptedContent { .. }` → hard fail
//!
//! ## FunctionCallOutputBody
//! - `Text(String)`
//! - `ContentItems(Vec<FunctionCallOutputContentItem>)`
//!
//! ## ResponseItem (16 variants)
//! - Outbound drop: `Reasoning`, `CompactionTrigger`
//! - Hard fail: `LocalShellCall`, `ToolSearchCall`, `CustomToolCall`, `CustomToolCallOutput`,
//!   `ToolSearchOutput`, `WebSearchCall`, `ImageGenerationCall`, `Compaction`,
//!   `ContextCompaction`, `Other`
//! - Normal mapping: `Message`, `FunctionCall` (no namespace), `FunctionCallOutput`, `AgentMessage`

use std::collections::HashSet;

use serde_json::{json, Value};

use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody, FunctionCallOutputContentItem,
    FunctionCallOutputPayload, ResponseItem,
};

use crate::connector::{ConnError, EgressCtx};

/// max_tokens fallback value (required by Anthropic Messages API)
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Build Anthropic Messages request JSON.
///
/// Covers spec §4.3, §4.0/§4.0b, §4.6, §4.8, §4.9, §4.10 (orphan fix, no reordering), §4.11, §7.1.
pub(crate) fn build_anthropic_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // ── Tool definition prioritization (§4.0b) ────────────────────────────
    let tools = map_tools(&req.tools)?;

    // ── messages construction ──────────────────────────────────────────────
    // Anthropic requires merging consecutive same-role content blocks into one message.
    // Use ordered (role, Vec<content block>) list to accumulate; open new entry on role change.
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();

    // call_id tracking: set of seen calls (O(1) lookup), list of pending results (ordered, ensures deterministic injection)
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut calls_needing_result: Vec<String> = Vec::new();

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                // system already handled at top level; role mapping is user/assistant only
                let r = if role == "assistant" { "assistant" } else { "user" };
                for c in content {
                    let block = content_item_to_block(c)?;
                    push_block(r, block, &mut turns);
                }
            }

            ResponseItem::AgentMessage { content, .. } => {
                for c in content {
                    match c {
                        AgentMessageInputContent::InputText { text } => {
                            push_block(
                                "assistant",
                                json!({"type": "text", "text": text}),
                                &mut turns,
                            );
                        }
                        AgentMessageInputContent::EncryptedContent { .. } => {
                            return Err(ConnError::HardFail(
                                "AgentMessage contains encrypted content (EncryptedContent), v1 anthropic connector cannot read it".into(),
                            ));
                        }
                    }
                }
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
                        "namespaced function call '{name}' not supported in v1 anthropic connector"
                    )));
                }
                // Parse arguments string into JSON object (§4.6)
                let input: Value = serde_json::from_str(arguments).map_err(|e| {
                    ConnError::HardFail(format!("FunctionCall arguments invalid JSON: {e}"))
                })?;
                seen_calls.insert(call_id.clone());
                calls_needing_result.push(call_id.clone());
                push_block(
                    "assistant",
                    json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": input
                    }),
                    &mut turns,
                );
            }

            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                if !seen_calls.contains(call_id) {
                    // Orphan result → drop (§4.10)
                    tracing::warn!(
                        call_id = %call_id,
                        "dropping orphan tool result (no corresponding FunctionCall)"
                    );
                    continue;
                }
                calls_needing_result.retain(|id| id != call_id);
                let block = tool_result_block(call_id, output)?;
                push_block("user", block, &mut turns);
            }

            // ── Outbound drops (§4.0 / §4.4) ──────────────────────────────
            ResponseItem::Reasoning { .. } => {
                // Reasoning history items dropped on outbound, local history untouched
            }
            ResponseItem::CompactionTrigger { .. } => {
                // CompactionTrigger dropped on outbound
            }

            // ── v1 hard fail variants (§4.0) ──────────────────────────────
            ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => {
                return Err(ConnError::HardFail(format!(
                    "{} not supported in v1 anthropic connector",
                    crate::connector::variant_name(item)
                )));
            }
        }
    }

    // ── Orphan call fix: inject placeholder tool_result (§4.10) ────────────
    // Note: Anthropic has no reordering step; orphan tool_result injected at end of user message.
    for call_id in &calls_needing_result {
        tracing::warn!(
            call_id = %call_id,
            "injecting placeholder tool_result (orphan FunctionCall has no corresponding FunctionCallOutput)"
        );
        push_block(
            "user",
            json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": "[No output available yet]"
            }),
            &mut turns,
        );
    }

    // ── Convert turns to messages ──────────────────────────────────────────
    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, blocks)| json!({"role": role, "content": blocks}))
        .collect();

    // ── max_tokens (required, §4.6) ────────────────────────────────────────
    let max_tokens = ctx.default_max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

    // ── Assemble top-level body ───────────────────────────────────────────
    let mut body = json!({
        "model": ctx.model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
    });

    // instructions → top-level system (§4.3)
    if !req.instructions.is_empty() {
        body["system"] = json!(req.instructions);
    }

    // ── Tools + tool_choice (§4.11) ────────────────────────────────────────
    if let Some(tools_arr) = tools {
        body["tools"] = Value::Array(tools_arr);

        // Build tool_choice, merge disable_parallel_tool_use
        let mut tc = map_tool_choice(&req.tool_choice)?;

        if !req.parallel_tool_calls {
            // Append disable_parallel_tool_use to tool_choice object (§4.11)
            let obj = tc.get_or_insert_with(|| json!({"type": "auto"}));
            obj["disable_parallel_tool_use"] = json!(true);
        }

        if let Some(tc) = tc {
            body["tool_choice"] = tc;
        }
    }

    // ── §7.1 Field downgrade ──────────────────────────────────────────────
    apply_field_downgrade(&mut body, req);

    // Opt-in Anthropic automatic prompt caching (top-level breakpoint). Off by default;
    // only emitted for providers that set `prompt_cache = true`, since unsupported
    // endpoints (Bedrock/Vertex/third-party gateways) may 400 on an unknown field.
    if ctx.prompt_cache {
        body["cache_control"] = json!({"type": "ephemeral"});
    }

    Ok(body)
}

// ── push_block ────────────────────────────────────────────────────────────────

/// Append content block to turns list.
/// If the last entry has the same role, append to its blocks; otherwise open new entry.
fn push_block(role: &str, block: Value, turns: &mut Vec<(String, Vec<Value>)>) {
    if let Some(last) = turns.last_mut() {
        if last.0 == role {
            last.1.push(block);
            return;
        }
    }
    turns.push((role.to_string(), vec![block]));
}

// ── content_item_to_block ──────────────────────────────────────────────────────

/// Convert `ContentItem` to Anthropic content block.
/// Images → hard fail (§4.9, v1 lacks capability flags).
fn content_item_to_block(c: &ContentItem) -> Result<Value, ConnError> {
    match c {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
            Ok(json!({"type": "text", "text": text}))
        }
        ContentItem::InputImage { .. } => Err(ConnError::HardFail(
            "image input not supported in v1 anthropic connector (no capability flags)".into(),
        )),
    }
}

// ── tool_result_block ─────────────────────────────────────────────────────────

/// Convert `FunctionCallOutput` to Anthropic tool_result content block.
/// - `success == Some(false)` → `is_error: true` (Anthropic native field, §4.6)
/// - ContentItems with images/encrypted → hard fail
fn tool_result_block(
    call_id: &str,
    output: &FunctionCallOutputPayload,
) -> Result<Value, ConnError> {
    let content_text = match &output.body {
        FunctionCallOutputBody::Text(t) => t.clone(),
        FunctionCallOutputBody::ContentItems(items) => {
            tool_result_items_to_text(items)?
        }
    };

    let mut block = json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": content_text,
    });

    if output.success == Some(false) {
        block["is_error"] = json!(true);
    }

    Ok(block)
}

// ── tool_result_items_to_text ──────────────────────────────────────────────────

/// Convert `FunctionCallOutputContentItem` list to plain text.
/// Images or encrypted content → hard fail (§4.6).
fn tool_result_items_to_text(
    items: &[FunctionCallOutputContentItem],
) -> Result<String, ConnError> {
    let mut text = String::new();
    for item in items {
        match item {
            FunctionCallOutputContentItem::InputText { text: t } => {
                text.push_str(t);
            }
            FunctionCallOutputContentItem::InputImage { .. } => {
                return Err(ConnError::HardFail(
                    "tool result contains image content (InputImage), v1 anthropic connector not supported".into(),
                ));
            }
            FunctionCallOutputContentItem::EncryptedContent { .. } => {
                return Err(ConnError::HardFail(
                    "tool result contains encrypted content (EncryptedContent), v1 anthropic connector not supported".into(),
                ));
            }
        }
    }
    Ok(text)
}

// ── map_tools ─────────────────────────────────────────────────────────────────

/// Convert tool definition list from Responses format to Anthropic Messages format.
///
/// Anthropic tool format: `{name, description, input_schema}` — no top-level `type` field.
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
                let input_schema = t
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                let mut tool = json!({
                    "name": name,
                    "input_schema": input_schema,
                });
                // description only written if non-null
                if description != Value::Null {
                    tool["description"] = description;
                }
                out.push(tool);
            }
            other => {
                return Err(ConnError::HardFail(format!(
                    "tool definition type {:?} not supported in v1 anthropic connector (only standard function)",
                    other
                )));
            }
        }
    }
    Ok(Some(out))
}

// ── map_tool_choice ───────────────────────────────────────────────────────────

/// Map codex `tool_choice` string to Anthropic tool_choice object (§4.11).
///
/// - `"auto"` → `{"type":"auto"}`
/// - `"none"` → `{"type":"none"}` (Anthropic doesn't support none, but structure is expressible)
/// - `"required"` → `{"type":"any"}`
/// - JSON string `{"type":"function","name":"..."}` → `{"type":"tool","name":"..."}`
/// - Other → `ConnError::HardFail`
fn map_tool_choice(tc: &str) -> Result<Option<Value>, ConnError> {
    match tc {
        "auto" => Ok(Some(json!({"type": "auto"}))),
        "none" => Ok(Some(json!({"type": "none"}))),
        "required" => Ok(Some(json!({"type": "any"}))),
        "" => Ok(None),
        other => {
            if let Ok(v) = serde_json::from_str::<Value>(other) {
                if v.get("type").and_then(|x| x.as_str()) == Some("function") {
                    if let Some(name) = v.get("name") {
                        return Ok(Some(json!({
                            "type": "tool",
                            "name": name
                        })));
                    }
                }
            }
            Err(ConnError::HardFail(format!(
                "tool_choice '{other}' cannot be expressed in Anthropic Messages protocol"
            )))
        }
    }
}

// ── apply_field_downgrade ─────────────────────────────────────────────────────

/// §7.1 Field downgrade: downgrade or drop fields unique to Responses API.
///
/// - `reasoning` → `thinking` (mapped by effort; Anthropic extended thinking)
/// - `text.format` with json_schema → downgrade to appended system prompt (anthropic has no native response_format)
/// - `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` → silently drop
fn apply_field_downgrade(body: &mut Value, req: &codex_api::ResponsesApiRequest) {
    // reasoning → thinking (Anthropic Extended Thinking, §7.1)
    if let Some(reasoning) = &req.reasoning {
        if reasoning.effort.is_some() {
            // Map by effort: enable thinking when effort is present
            // Anthropic thinking format: {"type":"enabled","budget_tokens":N}
            // Cap at 8000: conservative value from Anthropic Extended Thinking docs, avoid exceeding max_tokens
            let budget = body["max_tokens"]
                .as_u64()
                .map(|m| (m / 2).max(1024).min(8000))
                .unwrap_or(4000);
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        } else if reasoning.summary.is_some() || reasoning.context.is_some() {
            tracing::warn!("reasoning.summary/context not supported in v1 anthropic connector, dropped");
        } else {
            // All of effort/summary/context are None, reasoning object exists but no mappable fields, drop with warn
            tracing::warn!("reasoning object exists but effort/summary/context all None, dropped (v1 anthropic connector cannot map)");
        }
    }

    // text.format → downgrade to appended system prompt (Anthropic has no native response_format)
    // Unified strategy: append system prompt for all structured output formats (json_schema/json_object etc), not silently drop
    if let Some(text_controls) = &req.text {
        if let Some(format) = &text_controls.format {
            if let Ok(fmt_val) = serde_json::to_value(format) {
                let hint = if fmt_val.get("type").and_then(|t| t.as_str()) == Some("json_schema") {
                    // json_schema: append schema explanation
                    format!(
                        "\n\nYou must respond with valid JSON matching this schema: {}",
                        serde_json::to_string(&fmt_val).unwrap_or_default()
                    )
                } else {
                    // json_object and other structured output formats: append generic JSON hint
                    "\n\nRespond with valid JSON.".to_string()
                };
                // Append to system (if exists) or set new system
                let existing_system = body
                    .get("system")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                body["system"] = json!(format!("{existing_system}{hint}"));
                tracing::warn!(
                    format_type = ?fmt_val.get("type"),
                    "text.format downgraded to system prompt append in v1 anthropic connector"
                );
            }
        }
    }

    // store/include/prompt_cache_key/service_tier/client_metadata silently drop (§7.1 safely ignorable level)
}
