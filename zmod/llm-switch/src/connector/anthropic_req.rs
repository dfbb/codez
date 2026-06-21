//! anthropic outbound request construction (Task 06)
//!
//! Translates a codex `ResponsesApiRequest` into Anthropic Messages request JSON.
//!
//! # Key differences from chat_req
//!
//! - `system` goes to a top-level field (not a role=system message in messages)
//! - message role is only user / assistant
//! - `FunctionCall` → assistant content[tool_use], arguments string parsed into an object
//! - `FunctionCallOutput` → user content[tool_result], `is_error` native field
//! - `max_tokens` is required, defaulted via default_max_tokens (fallback 4096)
//! - `parallel_tool_calls==false` → `tool_choice.disable_parallel_tool_use=true`
//! - no tool-message flattening/reordering (content blocks grouped by turn; consecutive same-role blocks merged)
//! - tool definitions: `{name, description, input_schema}` format (no top-level type field);
//!   `web_search` tool → Anthropic native `web_search_20250305` server tool
//!
//! # Types reused from Step 0 (pinned by Task 04)
//!
//! ## ContentItem
//! - `InputText { text: String }`
//! - `InputImage { image_url: String, detail: Option<ImageDetail> }` → image content block (vision)
//! - `OutputText { text: String }`
//!
//! ## FunctionCallOutputContentItem
//! - `InputText { text: String }`
//! - `InputImage { .. }` → image content block (vision)
//! - `EncryptedContent { .. }` → hard fail
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

use std::collections::HashSet;

use serde_json::{json, Value};

use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody, FunctionCallOutputContentItem,
    FunctionCallOutputPayload, ResponseItem,
};

use crate::connector::{ConnError, EgressCtx};

/// max_tokens fallback value (required by the Anthropic Messages API)
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Build the Anthropic Messages request JSON.
///
/// Covers spec §4.3, §4.0/§4.0b, §4.6, §4.8, §4.9, §4.10 (orphan repair, no reordering), §4.11, §7.1.
pub(crate) fn build_anthropic_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // ── Tool definition grading (§4.0b) ──────────────────────────────────────────
    let tools = map_tools(&req.tools)?;

    // ── messages construction ──────────────────────────────────────────────────
    // Anthropic requires consecutive same-role content blocks to be merged into one message.
    // Accumulate with an ordered list of (role, Vec<content block>), opening a new entry on a new role.
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();

    // call_id tracking: set of seen calls (O(1) lookup), list of calls awaiting results (in appearance order, for deterministic injection order)
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut calls_needing_result: Vec<String> = Vec::new();

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                // system already goes to top level; role mapping is only user/assistant
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
                                "AgentMessage 含加密内容（EncryptedContent），v1 anthropic connector 无法读取".into(),
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
                // namespaced function calls are not supported in v1 (§4.0)
                if namespace.is_some() {
                    return Err(ConnError::HardFail(format!(
                        "命名空间函数调用 '{name}' 在 v1 anthropic connector 中不支持"
                    )));
                }
                // parse the arguments string into a JSON object (§4.6)
                let input: Value = serde_json::from_str(arguments).map_err(|e| {
                    ConnError::HardFail(format!("FunctionCall arguments 非法 JSON: {e}"))
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
                    // orphan result → drop (§4.10)
                    tracing::warn!(
                        call_id = %call_id,
                        "丢弃孤儿 tool result（无对应 FunctionCall）"
                    );
                    continue;
                }
                calls_needing_result.retain(|id| id != call_id);
                let block = tool_result_block(call_id, output)?;
                push_block("user", block, &mut turns);
            }

            // ── dropped on egress (§4.0 / §4.4) ────────────────────────────────
            ResponseItem::Reasoning { .. } => {
                // Reasoning history items are dropped on egress, local history untouched
            }
            ResponseItem::CompactionTrigger { .. } => {
                // CompactionTrigger dropped on egress
            }

            // ── v1 hard-fail variants (§4.0) ─────────────────────────────────
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
                    "{} 在 v1 anthropic connector 中不支持",
                    crate::connector::variant_name(item)
                )));
            }
        }
    }

    // ── Orphan call repair: inject placeholder tool_result (§4.10) ────────────────────
    // Note: Anthropic has no reordering step; orphan tool_results are injected at the end of the user message.
    for call_id in &calls_needing_result {
        tracing::warn!(
            call_id = %call_id,
            "注入占位 tool_result（孤儿 FunctionCall 无对应 FunctionCallOutput）"
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

    // ── Convert turns into messages ──────────────────────────────────────────
    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, blocks)| json!({"role": role, "content": blocks}))
        .collect();

    // ── max_tokens (required, §4.6) ──────────────────────────────────────
    let max_tokens = ctx.default_max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

    // ── Assemble top-level body ──────────────────────────────────────────────────
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

    // ── tools + tool_choice (§4.11) ──────────────────────────────────
    if let Some(tools_arr) = tools {
        body["tools"] = Value::Array(tools_arr);

        // Build tool_choice, merging in disable_parallel_tool_use
        let mut tc = map_tool_choice(&req.tool_choice)?;

        if !req.parallel_tool_calls {
            // append disable_parallel_tool_use to the tool_choice object (§4.11)
            let obj = tc.get_or_insert_with(|| json!({"type": "auto"}));
            obj["disable_parallel_tool_use"] = json!(true);
        }

        if let Some(tc) = tc {
            body["tool_choice"] = tc;
        }
    }

    // ── §7.1 field downgrade ─────────────────────────────────────────────────
    apply_field_downgrade(&mut body, req);

    Ok(body)
}

// ── push_block ────────────────────────────────────────────────────────────────

/// Append a content block to the turns list.
/// If the last entry already has the same role, append to its blocks; otherwise open a new entry.
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

/// Convert a `ContentItem` into an Anthropic content block.
/// Image → Anthropic image content block (vision, §4.9).
fn content_item_to_block(c: &ContentItem) -> Result<Value, ConnError> {
    match c {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
            Ok(json!({"type": "text", "text": text}))
        }
        ContentItem::InputImage { image_url, .. } => image_block(image_url),
    }
}

/// Convert codex's `image_url` (data URL or http(s) URL) into an Anthropic image
/// content block. Anthropic has no field for `detail`, so it's dropped (resolution handled server-side).
/// - `data:<media_type>;base64,<data>` → `source.type = base64`
/// - `http(s)://...` → `source.type = url`
/// - other forms → hard fail (cannot be expressed as an Anthropic image source)
fn image_block(image_url: &str) -> Result<Value, ConnError> {
    if let Some(rest) = image_url.strip_prefix("data:") {
        // data:<media_type>;base64,<data>
        let (media_type, data) = rest.split_once(";base64,").ok_or_else(|| {
            ConnError::HardFail(
                "图片 data URL 非 base64 编码，anthropic connector 无法翻译".into(),
            )
        })?;
        Ok(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            }
        }))
    } else if image_url.starts_with("http://") || image_url.starts_with("https://") {
        Ok(json!({
            "type": "image",
            "source": {
                "type": "url",
                "url": image_url,
            }
        }))
    } else {
        Err(ConnError::HardFail(format!(
            "图片 image_url 形态无法翻译为 Anthropic image source: {}",
            &image_url[..image_url.len().min(32)]
        )))
    }
}

// ── tool_result_block ─────────────────────────────────────────────────────────

/// Convert a `FunctionCallOutput` into an Anthropic tool_result content block.
/// - `success == Some(false)` → `is_error: true` (Anthropic native field, §4.6)
/// - ContentItems containing images → translated into image content blocks; plain text still uses the string form
fn tool_result_block(
    call_id: &str,
    output: &FunctionCallOutputPayload,
) -> Result<Value, ConnError> {
    let content = match &output.body {
        FunctionCallOutputBody::Text(t) => Value::String(t.clone()),
        FunctionCallOutputBody::ContentItems(items) => tool_result_items_to_content(items)?,
    };

    let mut block = json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": content,
    });

    if output.success == Some(false) {
        block["is_error"] = json!(true);
    }

    Ok(block)
}

// ── tool_result_items_to_content ────────────────────────────────────────────────

/// Convert a list of `FunctionCallOutputContentItem` into Anthropic tool_result content.
/// - all text → merged into a single string (preserves the original v1 behavior)
/// - containing images → returns an array of content blocks (text / image blocks, vision)
/// - encrypted content → hard fail (§4.6)
fn tool_result_items_to_content(
    items: &[FunctionCallOutputContentItem],
) -> Result<Value, ConnError> {
    let has_image = items
        .iter()
        .any(|i| matches!(i, FunctionCallOutputContentItem::InputImage { .. }));

    if !has_image {
        let mut text = String::new();
        for item in items {
            match item {
                FunctionCallOutputContentItem::InputText { text: t } => text.push_str(t),
                FunctionCallOutputContentItem::EncryptedContent { .. } => {
                    return Err(ConnError::HardFail(
                        "工具结果含加密内容（EncryptedContent），anthropic connector 不支持".into(),
                    ));
                }
                FunctionCallOutputContentItem::InputImage { .. } => unreachable!(),
            }
        }
        return Ok(Value::String(text));
    }

    let mut blocks = Vec::new();
    for item in items {
        match item {
            FunctionCallOutputContentItem::InputText { text: t } => {
                blocks.push(json!({"type": "text", "text": t}));
            }
            FunctionCallOutputContentItem::InputImage { image_url, .. } => {
                blocks.push(image_block(image_url)?);
            }
            FunctionCallOutputContentItem::EncryptedContent { .. } => {
                return Err(ConnError::HardFail(
                    "工具结果含加密内容（EncryptedContent），anthropic connector 不支持".into(),
                ));
            }
        }
    }
    Ok(Value::Array(blocks))
}

// ── map_tools ─────────────────────────────────────────────────────────────────

/// Convert the tool definition list from Responses format into Anthropic Messages format.
///
/// Anthropic tool format: `{name, description, input_schema}` — no top-level `type` field.
/// standard `function` → Anthropic function tool; `web_search` → Anthropic native
/// `web_search_20250305` server tool (codex-side capability gating allows it for anthropic providers).
/// other types → hard fail (§4.0b).
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
                // description is only written when non-null
                if description != Value::Null {
                    tool["description"] = description;
                }
                out.push(tool);
            }
            // codex's web_search hosted tool → Anthropic native server tool.
            // The Responses-side params external_web_access / filters / user_location etc.
            // have no Anthropic counterpart and are dropped; only the type and name are declared, running on Anthropic defaults.
            Some("web_search") => {
                out.push(json!({
                    "type": "web_search_20250305",
                    "name": "web_search",
                }));
            }
            other => {
                return Err(ConnError::HardFail(format!(
                    "工具定义类型 {:?} 在 anthropic connector 中不支持（仅支持 function / web_search）",
                    other
                )));
            }
        }
    }
    Ok(Some(out))
}

// ── map_tool_choice ───────────────────────────────────────────────────────────

/// Map codex's `tool_choice` string into an Anthropic tool_choice object (§4.11).
///
/// - `"auto"` → `{"type":"auto"}`
/// - `"none"` → `{"type":"none"}`  (Anthropic Messages now supports none, forbidding any tool call)
/// - `"required"` → `{"type":"any"}`
/// - JSON string `{"type":"function","name":"..."}` → `{"type":"tool","name":"..."}`
/// - other → `ConnError::HardFail`
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
                "tool_choice '{other}' 无法在 Anthropic Messages 协议中表达"
            )))
        }
    }
}

// ── apply_field_downgrade ─────────────────────────────────────────────────────

/// §7.1 field downgrade: downgrade or drop Responses-API-specific fields.
///
/// - `reasoning` → `thinking` (mapped by effort; Anthropic extended thinking)
/// - `text.format` containing json_schema → downgraded to an appended system instruction (anthropic has no native response_format)
/// - `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` → silently dropped
fn apply_field_downgrade(body: &mut Value, req: &codex_api::ResponsesApiRequest) {
    // reasoning → thinking (Anthropic Extended Thinking, §7.1)
    if let Some(reasoning) = &req.reasoning {
        if reasoning.effort.is_some() {
            // Approximate mapping: any effort enables thinking (v1 does not distinguish budget by low/medium/high,
            // the effort-level semantics are lost in v1 — anthropic budget is a token count, not a tier).
            // Anthropic thinking format: {"type":"enabled","budget_tokens":N}
            // Cap 8000: per Anthropic Extended Thinking docs recommendation, a conservative value to avoid exceeding max_tokens
            let budget = body["max_tokens"]
                .as_u64()
                .map(|m| (m / 2).clamp(1024, 8000))
                .unwrap_or(4000);
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        } else if reasoning.summary.is_some() || reasoning.context.is_some() {
            tracing::warn!("reasoning.summary/context 在 v1 anthropic connector 中不支持，已丢弃");
        } else {
            // effort/summary/context all None: reasoning object exists but has no mappable fields, drop and warn
            tracing::warn!("reasoning 对象存在但 effort/summary/context 均为 None，已丢弃（v1 anthropic connector 无法映射）");
        }
    }

    // text.format → downgrade to an appended system instruction (Anthropic has no native response_format)
    // Unified strategy: for all structured-output formats (json_schema/json_object etc.) append a system instruction, never silently drop
    if let Some(text_controls) = &req.text {
        if let Some(format) = &text_controls.format {
            if let Ok(fmt_val) = serde_json::to_value(format) {
                let hint = if fmt_val.get("type").and_then(|t| t.as_str()) == Some("json_schema") {
                    // json_schema: append the schema description
                    format!(
                        "\n\nYou must respond with valid JSON matching this schema: {}",
                        serde_json::to_string(&fmt_val).unwrap_or_default()
                    )
                } else {
                    // json_object and other structured-output formats: append a generic JSON hint
                    "\n\nRespond with valid JSON.".to_string()
                };
                // append to system (if present) or set a new system
                let existing_system = body
                    .get("system")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                body["system"] = json!(format!("{existing_system}{hint}"));
                tracing::warn!(
                    format_type = ?fmt_val.get("type"),
                    "text.format 在 v1 anthropic connector 中降级为系统指令追加"
                );
            }
        }
    }

    // store/include/prompt_cache_key/service_tier/client_metadata silently dropped (§7.1 safe-to-ignore tier)
}
