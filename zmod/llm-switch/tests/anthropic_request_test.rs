/// anthropic 出站请求构造测试（Task 06）
///
/// # Fixture 来源声明（§8 必须明确）
/// 本文件采用**自建 fixture**方案：人工按照 Anthropic Messages API 官方文档格式核对，
/// 固化成 `tests/fixtures/anthropic_req_*.expected.json`，不依赖第三方 converter 工具。
/// 理由：本仓库当前无 `../3rd/proxy/llm-rosetta` Python 依赖，且官方格式已有完整文档支撑。
///
/// # 复用 Task 04 钉死的类型（Step 0）
/// - ContentItem: InputText{text} | InputImage{image_url,detail} | OutputText{text}
/// - FunctionCallOutputContentItem: InputText{text} | InputImage{image_url,detail} | EncryptedContent{encrypted_content}
/// - FunctionCallOutputBody: Text(String) | ContentItems(Vec<FunctionCallOutputContentItem>)
/// - FunctionCallOutputPayload: { body: FunctionCallOutputBody, success: Option<bool> }
/// - ResponseItem 变体（16个）：Message/AgentMessage/Reasoning/LocalShellCall/FunctionCall/
///   ToolSearchCall/FunctionCallOutput/CustomToolCall/CustomToolCallOutput/ToolSearchOutput/
///   WebSearchCall/ImageGenerationCall/Compaction/CompactionTrigger/ContextCompaction/Other
use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody, FunctionCallOutputContentItem,
    FunctionCallOutputPayload, ResponseItem,
};
use serde_json::json;

use codez_llm_switch::testing::{build_anthropic_request_for_test as build, dummy_ctx_anthropic};

fn ctx() -> codez_llm_switch::EgressCtx {
    dummy_ctx_anthropic("claude-opus-4-8", Some(8192))
}

fn base_req() -> codex_api::ResponsesApiRequest {
    let mut r = codez_llm_switch::testing::sample_request();
    r.model = "claude-opus-4-8".into();
    r
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn find_block<'a>(v: &'a serde_json::Value, ty: &str) -> Option<&'a serde_json::Value> {
    v["messages"]
        .as_array()?
        .iter()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|b| b["type"] == ty)
}

// ============================================================
// §4.3 system 走顶层
// ============================================================

#[test]
fn system_goes_top_level_and_messages_have_no_system_role() {
    let mut req = base_req();
    req.instructions = "be brief".into();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputText { text: "hi".into() }],
        phase: None,
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["system"], "be brief");
    let msgs = v["messages"].as_array().unwrap();
    assert!(
        msgs.iter().all(|m| m["role"] != "system"),
        "messages 中不应有 role=system"
    );
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn empty_instructions_no_system_field() {
    let mut req = base_req();
    req.instructions = "".into();
    req.input = vec![];
    let v = build(&req, &ctx()).unwrap();
    assert!(
        v.get("system").is_none() || v["system"] == serde_json::Value::Null,
        "空 instructions 不应产生 system 字段"
    );
}

// ============================================================
// §4.6 max_tokens 必填（兜底）
// ============================================================

#[test]
fn max_tokens_required_uses_default() {
    let v = build(&base_req(), &ctx()).unwrap();
    assert_eq!(v["max_tokens"], 8192);
}

#[test]
fn max_tokens_falls_back_to_4096_when_no_config() {
    let req = base_req();
    let ctx_no_default = dummy_ctx_anthropic("claude-opus-4-8", None);
    let v = build(&req, &ctx_no_default).unwrap();
    assert_eq!(v["max_tokens"], 4096);
}

// ============================================================
// §4.3 消息 role 仅 user/assistant（FunctionCall → assistant content[tool_use]）
// ============================================================

#[test]
fn function_call_becomes_tool_use_with_parsed_object() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCall {
        id: None,
        name: "get_weather".into(),
        namespace: None,
        arguments: "{\"city\":\"SF\"}".into(),
        call_id: "call_1".into(),
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let asst = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant 消息必须存在");
    let block = &asst["content"][0];
    assert_eq!(block["type"], "tool_use");
    assert_eq!(block["id"], "call_1");
    assert_eq!(block["name"], "get_weather");
    // arguments 字符串解析成对象（§4.6）
    assert_eq!(block["input"]["city"], "SF");
}

#[test]
fn function_call_invalid_json_arguments_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCall {
        id: None,
        name: "f".into(),
        namespace: None,
        arguments: "not-valid-json".into(),
        call_id: "c".into(),
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "arguments 非法 JSON 应硬失败"
    );
}

// ============================================================
// §4.6 tool_result：is_error 原生字段
// ============================================================

#[test]
fn tool_output_maps_is_error() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "c".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("boom".into()),
                success: Some(false),
            },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let tr = find_block(&v, "tool_result").expect("tool_result present");
    assert_eq!(tr["tool_use_id"], "c");
    assert_eq!(tr["is_error"], true);
    // 内容应为错误文本
    assert!(tr["content"].is_string(), "tool_result content 应为字符串");
}

#[test]
fn tool_output_success_no_is_error_field() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "c".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("ok".into()),
                success: Some(true),
            },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let tr = find_block(&v, "tool_result").expect("tool_result present");
    // 成功时不应有 is_error 字段（或为 false）
    let is_error = tr.get("is_error");
    assert!(
        is_error.is_none() || is_error == Some(&serde_json::Value::Bool(false)),
        "success 时 is_error 不应为 true"
    );
}

#[test]
fn tool_result_image_content_hard_fails() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "c".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::ContentItems(vec![
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "data:...".into(),
                        detail: None,
                    },
                ]),
                success: None,
            },
            metadata: None,
        },
    ];
    assert!(
        build(&req, &ctx()).is_err(),
        "tool_result 含图片应硬失败（§4.9）"
    );
}

#[test]
fn tool_result_encrypted_content_hard_fails() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "c".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::ContentItems(vec![
                    FunctionCallOutputContentItem::EncryptedContent {
                        encrypted_content: "enc".into(),
                    },
                ]),
                success: None,
            },
            metadata: None,
        },
    ];
    assert!(
        build(&req, &ctx()).is_err(),
        "tool_result 含加密内容应硬失败"
    );
}

// ============================================================
// §4.11 tool_choice / disable_parallel_tool_use
// ============================================================

#[test]
fn disable_parallel_tool_use_when_false() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.parallel_tool_calls = false;
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tool_choice"]["disable_parallel_tool_use"], true);
}

#[test]
fn parallel_true_no_disable_parallel_field() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.parallel_tool_calls = true;
    let v = build(&req, &ctx()).unwrap();
    // disable_parallel_tool_use 不应为 true（可能没有该字段）
    let disable = v
        .get("tool_choice")
        .and_then(|tc| tc.get("disable_parallel_tool_use"));
    assert!(
        disable.is_none() || disable == Some(&serde_json::Value::Bool(false)),
        "parallel_tool_calls=true 时不应设置 disable_parallel_tool_use=true"
    );
}

#[test]
fn tools_map_to_input_schema() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"function","name":"f","description":"d","parameters":{"type":"object"}})];
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tools"][0]["name"], "f");
    assert_eq!(v["tools"][0]["description"], "d");
    assert_eq!(v["tools"][0]["input_schema"]["type"], "object");
    // 不应有 type 字段（Anthropic tools 格式）
    assert_eq!(v["tools"][0].get("type"), None);
}

#[test]
fn non_function_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"custom","name":"freeform"})];
    assert!(
        build(&req, &ctx()).is_err(),
        "非 function 工具类型应硬失败（§4.0b）"
    );
}

// ============================================================
// §4.10 孤儿修复（无重排——仅注入占位）
// ============================================================

#[test]
fn orphan_tool_call_gets_placeholder_tool_result() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCall {
        id: None,
        name: "f".into(),
        namespace: None,
        arguments: "{}".into(),
        call_id: "orphan".into(),
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let tr = find_block(&v, "tool_result");
    assert!(
        tr.is_some(),
        "孤儿 FunctionCall 必须获得占位 tool_result block"
    );
    assert_eq!(tr.unwrap()["tool_use_id"], "orphan");
}

#[test]
fn orphan_tool_result_is_dropped() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "ghost".into(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("x".into()),
            success: None,
        },
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let tr = find_block(&v, "tool_result");
    assert!(tr.is_none(), "无对应 call 的孤儿 result 应被丢弃");
}

// ============================================================
// 同 role 消息 content block 合并
// ============================================================

#[test]
fn consecutive_same_role_blocks_merged() {
    // 两个 FunctionCall 应合并进同一条 assistant 消息
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f1".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c1".into(),
            metadata: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            name: "f2".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c2".into(),
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    let asst_msgs: Vec<_> = msgs.iter().filter(|m| m["role"] == "assistant").collect();
    // 两个 tool_use block 应在同一条 assistant 消息里
    assert_eq!(asst_msgs.len(), 1, "连续同 role 消息应合并为一条");
    assert_eq!(asst_msgs[0]["content"].as_array().unwrap().len(), 2);
}

// ============================================================
// 出站丢弃变体（§4.0 / §4.4）
// ============================================================

#[test]
fn reasoning_item_is_discarded_silently() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::Reasoning {
            id: None,
            summary: vec![],
            content: None,
            encrypted_content: None,
            metadata: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: "hi".into() }],
            phase: None,
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn compaction_trigger_is_discarded_silently() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::CompactionTrigger { metadata: None },
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: "hi".into() }],
            phase: None,
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

// ============================================================
// 硬失败变体（§4.0）
// ============================================================

#[test]
fn namespaced_function_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCall {
        id: None,
        name: "f".into(),
        namespace: Some("mcp".into()),
        arguments: "{}".into(),
        call_id: "c".into(),
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "命名空间函数调用应硬失败"
    );
}

#[test]
fn input_image_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputImage {
            image_url: "data:...".into(),
            detail: None,
        }],
        phase: None,
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "图片输入应硬失败（§4.9）"
    );
}

#[test]
fn local_shell_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::LocalShellCall {
        id: None,
        call_id: None,
        status: codex_protocol::models::LocalShellStatus::Completed,
        action: codex_protocol::models::LocalShellAction::Exec(
            codex_protocol::models::LocalShellExecAction {
                command: vec![],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            },
        ),
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "LocalShellCall 应硬失败");
}

#[test]
fn tool_search_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ToolSearchCall {
        id: None,
        call_id: None,
        status: None,
        execution: "{}".into(),
        arguments: json!({}),
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "ToolSearchCall 应硬失败");
}

#[test]
fn web_search_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::WebSearchCall {
        id: None,
        status: None,
        action: None,
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "WebSearchCall 应硬失败");
}

#[test]
fn image_generation_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ImageGenerationCall {
        id: None,
        status: "completed".into(),
        revised_prompt: None,
        result: "base64data".into(),
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "ImageGenerationCall 应硬失败"
    );
}

#[test]
fn custom_tool_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: "c".into(),
        name: "custom".into(),
        input: "{}".into(),
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "CustomToolCall 应硬失败");
}

#[test]
fn custom_tool_call_output_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "c".into(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("x".into()),
            success: None,
        },
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "CustomToolCallOutput 应硬失败"
    );
}

#[test]
fn tool_search_output_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ToolSearchOutput {
        id: None,
        call_id: None,
        status: "done".into(),
        execution: "{}".into(),
        tools: vec![],
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "ToolSearchOutput 应硬失败");
}

#[test]
fn compaction_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Compaction {
        id: None,
        encrypted_content: "enc".into(),
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "Compaction 应硬失败（加密内容）"
    );
}

#[test]
fn context_compaction_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ContextCompaction {
        id: None,
        encrypted_content: Some("enc".into()),
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "ContextCompaction 应硬失败（加密内容）"
    );
}

#[test]
fn agent_message_encrypted_content_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::AgentMessage {
        id: None,
        author: "agent".into(),
        recipient: "user".into(),
        content: vec![AgentMessageInputContent::EncryptedContent {
            encrypted_content: "enc".into(),
        }],
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "AgentMessage 含 EncryptedContent 应硬失败"
    );
}

#[test]
fn other_variant_hard_fails() {
    let unknown_item: ResponseItem =
        serde_json::from_str(r#"{"type":"unknown_future_variant"}"#).unwrap();
    let mut req = base_req();
    req.input = vec![unknown_item];
    assert!(build(&req, &ctx()).is_err(), "Other 未知变体应硬失败");
}

// ============================================================
// §7.1 字段降级
// ============================================================

#[test]
fn stream_is_always_true() {
    let v = build(&base_req(), &ctx()).unwrap();
    assert_eq!(v["stream"], true);
}

#[test]
fn model_field_comes_from_ctx() {
    let v = build(&base_req(), &ctx()).unwrap();
    assert_eq!(v["model"], "claude-opus-4-8");
}

// ============================================================
// §8 黄金 fixture（自建，人工核对 Anthropic Messages API 格式）
// ============================================================

#[test]
fn golden_system_user_tool_roundtrip() {
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/anthropic_req_system_user_tool_roundtrip.expected.json"
    ))
    .expect("fixture JSON 解析失败");

    let mut req = codez_llm_switch::testing::sample_request();
    req.model = "claude-opus-4-8".into();
    req.instructions = "You are a helpful assistant.".into();
    req.tools = vec![json!({
        "type": "function",
        "name": "get_weather",
        "description": "Get current weather for a city",
        "parameters": {
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        }
    })];
    req.tool_choice = "auto".into();
    req.parallel_tool_calls = true;
    req.input = vec![
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: "What is the weather in SF?".into(),
            }],
            phase: None,
            metadata: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            name: "get_weather".into(),
            namespace: None,
            arguments: "{\"city\":\"SF\"}".into(),
            call_id: "call_weather_1".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "call_weather_1".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Sunny, 72°F".into()),
                success: Some(true),
            },
            metadata: None,
        },
        ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::OutputText {
                text: "It's sunny and 72°F in SF.".into(),
            }],
            phase: None,
            metadata: None,
        },
    ];

    let ctx = dummy_ctx_anthropic("claude-opus-4-8", Some(8192));
    let actual = build(&req, &ctx).unwrap();

    assert_eq!(
        actual, expected,
        "实际输出与黄金 fixture 不符\n实际:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}

#[test]
fn multiple_orphan_calls_injected_in_order() {
    // 验证多孤儿 FunctionCall 按出现顺序注入占位 tool_result
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f1".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "orphan_a".into(),
            metadata: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            name: "f2".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "orphan_b".into(),
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    // 取所有 tool_result block 的 tool_use_id 顺序
    let ids: Vec<&str> = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .filter(|b| b["type"] == "tool_result")
        .map(|b| b["tool_use_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["orphan_a", "orphan_b"], "多孤儿应按出现顺序注入");
}
