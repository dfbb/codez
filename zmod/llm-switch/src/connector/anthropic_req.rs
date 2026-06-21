//! anthropic 出站请求构造（Task 06）
//!
//! 把 codex `ResponsesApiRequest` 翻译成 Anthropic Messages 请求 JSON。
//!
//! # 与 chat_req 的关键差异
//!
//! - `system` 走顶层字段（不是 messages 里的 role=system）
//! - 消息 role 仅 user / assistant
//! - `FunctionCall` → assistant content[tool_use]，arguments 字符串 parse 成对象
//! - `FunctionCallOutput` → user content[tool_result]，`is_error` 原生字段
//! - `max_tokens` 必填，用 default_max_tokens 兜底（缺省 4096）
//! - `parallel_tool_calls==false` → `tool_choice.disable_parallel_tool_use=true`
//! - 无 tool 消息扁平重排（content block 按回合归组；连续同 role block 合并）
//! - 工具定义：`{name, description, input_schema}` 格式（无顶层 type 字段）
//!
//! # Step 0 复用的类型（Task 04 钉死）
//!
//! ## ContentItem
//! - `InputText { text: String }`
//! - `InputImage { image_url: String, detail: Option<ImageDetail> }` → 硬失败
//! - `OutputText { text: String }`
//!
//! ## FunctionCallOutputContentItem
//! - `InputText { text: String }`
//! - `InputImage { .. }` → 硬失败
//! - `EncryptedContent { .. }` → 硬失败
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

use std::collections::HashSet;

use serde_json::{json, Value};

use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody, FunctionCallOutputContentItem,
    FunctionCallOutputPayload, ResponseItem,
};

use crate::connector::{ConnError, EgressCtx};

/// max_tokens 兜底值（Anthropic Messages API 必填）
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// 构造 Anthropic Messages 请求 JSON。
///
/// 覆盖 spec §4.3、§4.0/§4.0b、§4.6、§4.8、§4.9、§4.10（孤儿修复，无重排）、§4.11、§7.1。
pub(crate) fn build_anthropic_request(
    req: &codex_api::ResponsesApiRequest,
    ctx: &EgressCtx,
) -> Result<Value, ConnError> {
    // ── 工具定义分级（§4.0b）──────────────────────────────────────────
    let tools = map_tools(&req.tools)?;

    // ── messages 构造 ──────────────────────────────────────────────────
    // Anthropic 要求连续同 role 的 content block 合并进同一条消息。
    // 用 (role, Vec<content block>) 有序列来积累，新 role 时新开一条。
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();

    // call_id 追踪：已见调用集合（O(1) 查找）、待补结果列表（按出现顺序，保证注入顺序确定）
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut calls_needing_result: Vec<String> = Vec::new();

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                // system 已走顶层；role 映射仅 user/assistant
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
                // 命名空间函数调用 v1 不支持（§4.0）
                if namespace.is_some() {
                    return Err(ConnError::HardFail(format!(
                        "命名空间函数调用 '{name}' 在 v1 anthropic connector 中不支持"
                    )));
                }
                // arguments 字符串 parse 成 JSON 对象（§4.6）
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
                    // 孤儿结果 → 丢弃（§4.10）
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

            // ── 出站丢弃（§4.0 / §4.4）────────────────────────────────
            ResponseItem::Reasoning { .. } => {
                // Reasoning 历史项出站丢弃，不动本地历史
            }
            ResponseItem::CompactionTrigger { .. } => {
                // CompactionTrigger 出站丢弃
            }

            // ── v1 硬失败变体（§4.0）─────────────────────────────────
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

    // ── 孤儿调用修复：注入占位 tool_result（§4.10）────────────────────
    // 注意：Anthropic 无重排步骤，孤儿 tool_result 注入到 user 消息末尾。
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

    // ── 把 turns 转成 messages ──────────────────────────────────────────
    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, blocks)| json!({"role": role, "content": blocks}))
        .collect();

    // ── max_tokens（必填，§4.6）──────────────────────────────────────
    let max_tokens = ctx.default_max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

    // ── 组装顶层 body ──────────────────────────────────────────────────
    let mut body = json!({
        "model": ctx.model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
    });

    // instructions → 顶层 system（§4.3）
    if !req.instructions.is_empty() {
        body["system"] = json!(req.instructions);
    }

    // ── 工具 + tool_choice（§4.11）──────────────────────────────────
    if let Some(tools_arr) = tools {
        body["tools"] = Value::Array(tools_arr);

        // 构造 tool_choice，合并 disable_parallel_tool_use
        let mut tc = map_tool_choice(&req.tool_choice)?;

        if !req.parallel_tool_calls {
            // disable_parallel_tool_use 追加到 tool_choice 对象（§4.11）
            let obj = tc.get_or_insert_with(|| json!({"type": "auto"}));
            obj["disable_parallel_tool_use"] = json!(true);
        }

        if let Some(tc) = tc {
            body["tool_choice"] = tc;
        }
    }

    // ── §7.1 字段降级 ─────────────────────────────────────────────────
    apply_field_downgrade(&mut body, req);

    Ok(body)
}

// ── push_block ────────────────────────────────────────────────────────────────

/// 把 content block 追加到 turns 列。
/// 若末尾已有相同 role 的条目，追加到其 blocks；否则新开一条。
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

/// 把 `ContentItem` 转成 Anthropic content block。
/// 图片 → 硬失败（§4.9，v1 无能力判定标志）。
fn content_item_to_block(c: &ContentItem) -> Result<Value, ConnError> {
    match c {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
            Ok(json!({"type": "text", "text": text}))
        }
        ContentItem::InputImage { .. } => Err(ConnError::HardFail(
            "图片输入在 v1 anthropic connector 中不支持（无能力判定标志）".into(),
        )),
    }
}

// ── tool_result_block ─────────────────────────────────────────────────────────

/// 把 `FunctionCallOutput` 转成 Anthropic tool_result content block。
/// - `success == Some(false)` → `is_error: true`（Anthropic 原生字段，§4.6）
/// - ContentItems 含图片/加密 → 硬失败
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

/// 把 `FunctionCallOutputContentItem` 列表转成纯文本。
/// 遇到图片或加密内容 → 硬失败（§4.6）。
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
                    "工具结果含图片内容（InputImage），v1 anthropic connector 不支持".into(),
                ));
            }
            FunctionCallOutputContentItem::EncryptedContent { .. } => {
                return Err(ConnError::HardFail(
                    "工具结果含加密内容（EncryptedContent），v1 anthropic connector 不支持".into(),
                ));
            }
        }
    }
    Ok(text)
}

// ── map_tools ─────────────────────────────────────────────────────────────────

/// 把工具定义列表从 Responses 格式转成 Anthropic Messages 格式。
///
/// Anthropic 工具格式：`{name, description, input_schema}` — 无顶层 `type` 字段。
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
                let input_schema = t
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                let mut tool = json!({
                    "name": name,
                    "input_schema": input_schema,
                });
                // description 只在非 null 时写入
                if description != Value::Null {
                    tool["description"] = description;
                }
                out.push(tool);
            }
            other => {
                return Err(ConnError::HardFail(format!(
                    "工具定义类型 {:?} 在 v1 anthropic connector 中不支持（仅支持标准 function）",
                    other
                )));
            }
        }
    }
    Ok(Some(out))
}

// ── map_tool_choice ───────────────────────────────────────────────────────────

/// 把 codex 的 `tool_choice` 字符串映射成 Anthropic tool_choice 对象（§4.11）。
///
/// - `"auto"` → `{"type":"auto"}`
/// - `"none"` → `{"type":"none"}`  （Anthropic Messages 现支持 none，禁止调用任何工具）
/// - `"required"` → `{"type":"any"}`
/// - JSON 字符串 `{"type":"function","name":"..."}` → `{"type":"tool","name":"..."}`
/// - 其他 → `ConnError::HardFail`
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

/// §7.1 字段降级：把 Responses API 特有字段降级或丢弃。
///
/// - `reasoning` → `thinking`（按 effort 映射；Anthropic extended thinking）
/// - `text.format` 含 json_schema → 降级为追加系统指令（anthropic 无原生 response_format）
/// - `store`/`include`/`prompt_cache_key`/`service_tier`/`client_metadata` → 静默丢弃
fn apply_field_downgrade(body: &mut Value, req: &codex_api::ResponsesApiRequest) {
    // reasoning → thinking（Anthropic Extended Thinking，§7.1）
    if let Some(reasoning) = &req.reasoning {
        if reasoning.effort.is_some() {
            // 近似映射：有 effort 即开启 thinking（v1 不按 low/medium/high 区分 budget，
            // effort 级别语义在 v1 丢失——anthropic budget 是 token 数而非档位）。
            // Anthropic thinking 格式：{"type":"enabled","budget_tokens":N}
            // 上限 8000：参考 Anthropic Extended Thinking 文档建议，保守值避免超出 max_tokens
            let budget = body["max_tokens"]
                .as_u64()
                .map(|m| (m / 2).clamp(1024, 8000))
                .unwrap_or(4000);
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        } else if reasoning.summary.is_some() || reasoning.context.is_some() {
            tracing::warn!("reasoning.summary/context 在 v1 anthropic connector 中不支持，已丢弃");
        } else {
            // effort/summary/context 均为 None，reasoning 对象存在但无可映射字段，丢弃并 warn
            tracing::warn!("reasoning 对象存在但 effort/summary/context 均为 None，已丢弃（v1 anthropic connector 无法映射）");
        }
    }

    // text.format → 降级为系统指令追加（Anthropic 无原生 response_format）
    // 统一策略：对所有结构化输出 format（json_schema/json_object 等）追加系统指令，不静默丢弃
    if let Some(text_controls) = &req.text {
        if let Some(format) = &text_controls.format {
            if let Ok(fmt_val) = serde_json::to_value(format) {
                let hint = if fmt_val.get("type").and_then(|t| t.as_str()) == Some("json_schema") {
                    // json_schema：追加 schema 说明
                    format!(
                        "\n\nYou must respond with valid JSON matching this schema: {}",
                        serde_json::to_string(&fmt_val).unwrap_or_default()
                    )
                } else {
                    // json_object 等其他结构化输出 format：追加通用 JSON 提示
                    "\n\nRespond with valid JSON.".to_string()
                };
                // 追加到 system（若存在）或设置新 system
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

    // store/include/prompt_cache_key/service_tier/client_metadata 静默丢弃（§7.1 可安全忽略级）
}
