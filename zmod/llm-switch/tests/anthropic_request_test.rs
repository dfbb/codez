/// anthropic outbound request construction tests (Task 06)
///
/// # Fixture provenance declaration (§8 must be explicit)
/// This file uses a **self-built fixture** approach: manually checked against the official Anthropic Messages API doc format,
/// frozen into `tests/fixtures/anthropic_req_*.expected.json`, with no dependency on a third-party converter tool.
/// Rationale: this repo currently has no `../3rd/proxy/llm-rosetta` Python dependency, and the official format is fully documented.
///
/// # Reuses the types pinned by Task 04 (Step 0)
/// - ContentItem: InputText{text} | InputImage{image_url,detail} | OutputText{text}
/// - FunctionCallOutputContentItem: InputText{text} | InputImage{image_url,detail} | EncryptedContent{encrypted_content}
/// - FunctionCallOutputBody: Text(String) | ContentItems(Vec<FunctionCallOutputContentItem>)
/// - FunctionCallOutputPayload: { body: FunctionCallOutputBody, success: Option<bool> }
/// - ResponseItem variants (16): Message/AgentMessage/Reasoning/LocalShellCall/FunctionCall/
///   ToolSearchCall/FunctionCallOutput/CustomToolCall/CustomToolCallOutput/ToolSearchOutput/
///   WebSearchCall/ImageGenerationCall/Compaction/CompactionTrigger/ContextCompaction/Other
use codex_protocol::config_types::Verbosity;
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

// ── Helper functions ──────────────────────────────────────────────────────────────

fn find_block<'a>(v: &'a serde_json::Value, ty: &str) -> Option<&'a serde_json::Value> {
    v["messages"]
        .as_array()?
        .iter()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|b| b["type"] == ty)
}

// ============================================================
// §4.3 system goes to top level
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
// §4.6 max_tokens required (fallback)
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
// §4.3 message role is only user/assistant (FunctionCall → assistant content[tool_use])
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
    // arguments string parsed into an object (§4.6)
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
// §4.6 tool_result: is_error native field
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
    // content should be the error text
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
    // on success there should be no is_error field (or it's false)
    let is_error = tr.get("is_error");
    assert!(
        is_error.is_none() || is_error == Some(&serde_json::Value::Bool(false)),
        "success 时 is_error 不应为 true"
    );
}

#[test]
fn tool_result_image_content_becomes_image_block() {
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
                    FunctionCallOutputContentItem::InputText {
                        text: "见图：".into(),
                    },
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "data:image/png;base64,iVBORw0KGgo=".into(),
                        detail: None,
                    },
                ]),
                success: None,
            },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).expect("含图片的 tool_result 应翻译成功");
    let tr = find_block(&v, "tool_result").expect("tool_result present");
    let content = tr["content"].as_array().expect("含图片时 content 应为 block 数组");
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "见图：");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
    assert_eq!(content[1]["source"]["media_type"], "image/png");
    assert_eq!(content[1]["source"]["data"], "iVBORw0KGgo=");
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
    // disable_parallel_tool_use should not be true (the field may be absent)
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
    // there should be no type field (Anthropic tools format)
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

#[test]
fn web_search_tool_becomes_anthropic_server_tool() {
    let mut req = base_req();
    // codex's web_search hosted-tool serialized form (includes Responses-side params, which should be dropped)
    req.tools = vec![json!({
        "type": "web_search",
        "external_web_access": true,
    })];
    let v = build(&req, &ctx()).expect("web_search 工具应翻译成功");
    let ws = v["tools"]
        .as_array()
        .and_then(|arr| arr.iter().find(|t| t["type"] == "web_search_20250305"))
        .expect("应有 web_search_20250305 server tool");
    assert_eq!(ws["name"], "web_search");
    // Responses-side params should not leak into the Anthropic tool definition
    assert_eq!(ws.get("external_web_access"), None);
}

#[test]
fn function_and_web_search_tools_coexist() {
    let mut req = base_req();
    req.tools = vec![
        json!({"type":"function","name":"f","parameters":{"type":"object"}}),
        json!({"type":"web_search"}),
    ];
    let v = build(&req, &ctx()).unwrap();
    let tools = v["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "f");
    assert_eq!(tools[0].get("type"), None, "function 工具无顶层 type");
    assert_eq!(tools[1]["type"], "web_search_20250305");
}

// ============================================================
// §4.10 orphan repair (no reordering — only placeholder injection)
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
// Same-role message content block merging
// ============================================================

#[test]
fn consecutive_same_role_blocks_merged() {
    // two FunctionCalls should be merged into the same assistant message
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
    // the two tool_use blocks should be in the same assistant message
    assert_eq!(asst_msgs.len(), 1, "连续同 role 消息应合并为一条");
    assert_eq!(asst_msgs[0]["content"].as_array().unwrap().len(), 2);
}

// ============================================================
// Dropped-on-egress variants (§4.0 / §4.4)
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
// Hard-fail variants (§4.0)
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
fn input_image_base64_becomes_image_block() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputImage {
            image_url: "data:image/jpeg;base64,/9j/4AAQ".into(),
            detail: None,
        }],
        phase: None,
        metadata: None,
    }];
    let v = build(&req, &ctx()).expect("base64 图片输入应翻译成功");
    let img = find_block(&v, "image").expect("image block present");
    assert_eq!(img["source"]["type"], "base64");
    assert_eq!(img["source"]["media_type"], "image/jpeg");
    assert_eq!(img["source"]["data"], "/9j/4AAQ");
}

#[test]
fn input_image_url_becomes_image_block() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputImage {
            image_url: "https://example.com/cat.png".into(),
            detail: None,
        }],
        phase: None,
        metadata: None,
    }];
    let v = build(&req, &ctx()).expect("URL 图片输入应翻译成功");
    let img = find_block(&v, "image").expect("image block present");
    assert_eq!(img["source"]["type"], "url");
    assert_eq!(img["source"]["url"], "https://example.com/cat.png");
}

#[test]
fn input_image_malformed_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputImage {
            image_url: "ftp://nope".into(),
            detail: None,
        }],
        phase: None,
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "无法识别的 image_url 形态应硬失败"
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
// §7.1 field downgrade
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
// §8 golden fixture (self-built, manually checked against the Anthropic Messages API format)
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
    // verify that multiple orphan FunctionCalls inject placeholder tool_results in appearance order
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
    // take the tool_use_id order of all tool_result blocks
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

// ============================================================
// §7.1 text.format → appended system instruction (json_schema-specific test)
// ============================================================

/// Verify that when text.format is json_schema, the top-level system has the JSON schema instruction appended.
/// TextFormatType currently has only the JsonSchema variant (see codex-api/src/common.rs);
/// construct TextControls carrying format via create_text_param_for_request.
#[test]
fn text_format_json_schema_appends_system_instruction() {
    let schema = json!({"type": "object", "properties": {"answer": {"type": "string"}}});
    let text_controls =
        codex_api::create_text_param_for_request(None::<Verbosity>, &Some(schema), false);
    assert!(text_controls.is_some(), "schema 非空时应返回 TextControls");

    let mut req = base_req();
    req.instructions = "You are a helpful assistant.".into();
    req.text = text_controls;

    let v = build(&req, &ctx()).unwrap();
    let system = v["system"].as_str().expect("system 字段应为字符串");
    assert!(
        system.contains("You must respond with valid JSON matching this schema:"),
        "text.format=json_schema 应在 system 中追加 schema 指令，实际 system: {system:?}"
    );
    // the original instructions should also be preserved (append, not replace)
    assert!(
        system.contains("You are a helpful assistant."),
        "system 应保留原始 instructions，实际: {system:?}"
    );
}

// ============================================================
// I2: §4.11 unexpressible forced tool_choice → hard fail
// ============================================================

/// Forcing a specific tool but the target cannot be expressed equivalently (a forced tier that is not the function type) → hard fail.
#[test]
fn forced_unexpressible_tool_choice_hard_fails() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.tool_choice = json!({"type":"allowed_tools","tools":["f"]}).to_string();
    assert!(
        build(&req, &ctx()).is_err(),
        "不可表达的强制 tool_choice 应硬失败（§4.11）"
    );
}

// ============================================================
// I3: §7.1 reasoning config downgrade → thinking
// ============================================================

/// When reasoning.effort is present, it should be downgrade-mapped into anthropic's thinking block (enabled + budget).
#[test]
fn reasoning_effort_maps_to_thinking() {
    use codex_protocol::openai_models::ReasoningEffort;
    let mut req = base_req();
    req.reasoning = Some(codex_api::Reasoning {
        effort: Some(ReasoningEffort::High),
        summary: None,
        context: None,
    });
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(
        v["thinking"]["type"], "enabled",
        "reasoning.effort 存在时应开启 thinking"
    );
    assert!(
        v["thinking"]["budget_tokens"].as_u64().is_some(),
        "thinking 应含 budget_tokens，实际: {}",
        v["thinking"]
    );
}
