//! chat 出站请求构造（Task 04）
//!
//! 把 codex `ResponsesApiRequest` 翻译成 OpenAI Chat Completions 请求 JSON。
//!
//! # Step 0 核实的真实类型变体
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
//! ## ResponseItem（16 变体）
//! - 出站丢弃：`Reasoning`、`CompactionTrigger`
//! - 硬失败：`LocalShellCall`、`ToolSearchCall`、`CustomToolCall`、`CustomToolCallOutput`、
//!   `ToolSearchOutput`、`WebSearchCall`、`ImageGenerationCall`、`Compaction`、
//!   `ContextCompaction`、`Other`
//! - 普通映射：`Message`、`FunctionCall`（无 namespace）、`FunctionCallOutput`、`AgentMessage`

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody,
    FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseItem,
};

use crate::connector::{ConnError, EgressCtx};

/// 构造 Chat Completions 请求 JSON。
///
/// 覆盖 spec §4.2、§4.0/§4.0b、§4.6、§4.8、§4.9、§4.10、§4.11、§7.1。
pub(crate) fn build_chat_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // ── 工具定义分级（§4.0b）──────────────────────────────────────────
    let tools = map_tools(&req.tools)?;

    // ── messages 构造 ──────────────────────────────────────────────────
    let mut messages: Vec<Value> = Vec::new();

    // instructions → system
    if !req.instructions.is_empty() {
        messages.push(json!({"role": "system", "content": req.instructions}));
    }

    // call_id 追踪：已见调用集合、待补结果集合
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
                // 命名空间函数调用 v1 不支持（§4.0）
                if namespace.is_some() {
                    return Err(ConnError::HardFail(format!(
                        "命名空间函数调用 '{name}' 在 v1 chat connector 中不支持"
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
                    // 孤儿结果 → 删除（§4.10）
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

            // ── 出站丢弃（§4.0 / §4.4）────────────────────────────────
            ResponseItem::Reasoning { .. } => {
                // Reasoning 历史项出站丢弃，不动本地历史
            }
            ResponseItem::CompactionTrigger { .. } => {
                // CompactionTrigger 出站丢弃
            }

            // ── v1 硬失败变体（§4.0）─────────────────────────────────
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

    // ── 孤儿调用修复：注入占位结果（§4.10）────────────────────────────
    // 注意：孤儿 call_id 无固定顺序，注入到末尾；重排步骤会把它们归位
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

    // ── tool 消息重排（§4.10，复刻 _reorder_tool_messages）────────────
    let messages = reorder_tool_messages(messages);

    // ── 组装顶层 body ─────────────────────────────────────────────────
    let mut body = json!({
        "model": ctx.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    if let Some(tools_arr) = tools {
        body["tools"] = Value::Array(tools_arr);
        // tool_choice / parallel_tool_calls 仅在有 tools 时写入；否则 strip（§4.10）。
        // 上游对「设了 parallel_tool_calls 但无 tools」会返回 400，故二者同处理。
        if let Some(tc) = map_tool_choice(&req.tool_choice)? {
            body["tool_choice"] = tc;
        }
        body["parallel_tool_calls"] = json!(req.parallel_tool_calls);
    }

    // ── §7.1 字段降级 ─────────────────────────────────────────────────
    apply_field_downgrade(&mut body, req);

    Ok(body)
}

// ── map_message ───────────────────────────────────────────────────────────────

/// 把 `Message` 变体的 content 列表转成 Chat 消息。
/// 图片内容 → 硬失败（§4.9，v1 无能力标志）。
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
    // role 归一到 Chat Completions 认得的集合：codex 会发 `developer`（开发者指令，
    // 新版 OpenAI 约定），但 DeepSeek 等只认 system/user/assistant/tool —— 语义上
    // developer 指令属系统级，归到 `system`。其余原样透传。
    let role = match role {
        "developer" => "system",
        other => other,
    };
    Ok(json!({"role": role, "content": text}))
}

// ── map_agent_message ─────────────────────────────────────────────────────────

/// 把 `AgentMessage` 变体转成 assistant 消息。
/// EncryptedContent → 硬失败（§4.0）。
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

/// 把 `FunctionCallOutput` 转成 Chat tool 消息。
/// - `success == Some(false)` → 前缀 `[tool error]`（Chat 无 is_error 字段）。
/// - ContentItems 含图片/加密 → 硬失败（§4.6）。
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

/// 把 `FunctionCallOutputContentItem` 列表转成纯文本。
/// 遇到图片或加密内容 → 硬失败（§4.6）。
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

/// 把工具定义列表从 Responses 格式转成 Chat Completions 格式。
/// v1 只支持标准 `function` 类型；其他类型 → 硬失败（§4.0b）。
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

/// 把 codex 的 `tool_choice` 字符串映射成 Chat Completions 的对应值（§4.11）。
fn map_tool_choice(tc: &str) -> Result<Option<Value>, ConnError> {
    match tc {
        "auto" => Ok(Some(json!("auto"))),
        "none" => Ok(Some(json!("none"))),
        "required" => Ok(Some(json!("required"))),
        "" => Ok(None),
        // 若 codex 用 JSON 字符串表达强制指定某函数，解析并转换格式
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

/// 重排 tool 消息，使每条 tool 消息紧跟产生它的 assistant tool_calls（§4.10）。
///
/// 复刻 llm-rosetta `_reorder_tool_messages` 算法：
/// 1. 先收集所有 tool 消息，按 `tool_call_id` 建索引。
/// 2. 遍历非 tool 消息；遇到带 `tool_calls` 的 assistant 消息后，
///    按 `tool_calls[].id` 顺序紧跟插入对应的 tool 消息。
/// 3. 未被插入的孤儿 tool 消息追加末尾（附 warn）。
fn reorder_tool_messages(messages: Vec<Value>) -> Vec<Value> {
    // 按 tool_call_id 收集 tool 消息（可能一个 id 多条，保留顺序）
    let mut tool_by_id: HashMap<String, Vec<Value>> = HashMap::new();
    let mut non_tool: Vec<Value> = Vec::new();

    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
            if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                tool_by_id.entry(id.to_string()).or_default().push(msg);
            } else {
                // 没有 tool_call_id 的 tool 消息，当普通消息处理
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

    // 未被匹配的 tool 消息追加末尾
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

/// §7.1 字段降级：把 Responses API 特有字段降级或丢弃。
///
/// - `reasoning.effort` → `reasoning_effort`（可安全传递）
/// - `reasoning` 其他字段 → warn 后丢弃
/// - `text.format` 含 json_schema → `response_format`
/// - `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` → 静默丢弃
fn apply_field_downgrade(body: &mut Value, req: &codex_api::ResponsesApiRequest) {
    // reasoning.effort → reasoning_effort
    if let Some(reasoning) = &req.reasoning {
        if let Some(effort) = &reasoning.effort {
            // ReasoningEffortConfig 实现了 serde，序列化为字符串（low/medium/high）
            if let Ok(effort_val) = serde_json::to_value(effort) {
                body["reasoning_effort"] = effort_val;
            }
        } else if reasoning.summary.is_some() || reasoning.context.is_some() {
            tracing::warn!("reasoning.summary/context 在 v1 chat connector 中不支持，已丢弃");
        }
    }

    // text.format → response_format（仅 json_schema）
    // TextFormat 序列化为 {"type":"json_schema","strict":bool,"schema":{...},"name":"..."}
    // Chat API 期望 {"type":"json_schema","json_schema":{"name":"...","schema":{...},"strict":true}}
    if let Some(text_controls) = &req.text {
        if let Some(format) = &text_controls.format {
            if let Ok(fmt_val) = serde_json::to_value(format) {
                if fmt_val.get("type").and_then(|t| t.as_str()) == Some("json_schema") {
                    // 从 TextFormat 字段重组 Chat API 所需形状
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
        // verbosity 不映射
    }

    // store/include/prompt_cache_key/service_tier/client_metadata 静默丢弃（§7.1 可安全忽略级）
}
