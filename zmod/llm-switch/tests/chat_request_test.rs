/// chat outbound request construction test (Task 04)
///
/// Step 0 verified real type variants:
/// - ContentItem: InputText{text} | InputImage{image_url,detail} | OutputText{text}
/// - FunctionCallOutputContentItem: InputText{text} | InputImage{image_url,detail} | EncryptedContent{encrypted_content}
/// - FunctionCallOutputBody: Text(String) | ContentItems(Vec<FunctionCallOutputContentItem>)
/// - FunctionCallOutputPayload: { body: FunctionCallOutputBody, success: Option<bool> }
/// - ResponseItem variants (16 total): Message/AgentMessage/Reasoning/LocalShellCall/FunctionCall/
///   ToolSearchCall/FunctionCallOutput/CustomToolCall/CustomToolCallOutput/ToolSearchOutput/
///   WebSearchCall/ImageGenerationCall/Compaction/CompactionTrigger/ContextCompaction/Other
use codex_protocol::models::{
    AgentMessageInputContent, ContentItem, FunctionCallOutputBody, FunctionCallOutputContentItem,
    FunctionCallOutputPayload, ResponseItem,
};
use serde_json::json;

use codez_llm_switch::testing::build_chat_request_for_test as build;

fn ctx() -> codez_llm_switch::EgressCtx {
    codez_llm_switch::testing::dummy_ctx("deepseek-v4-pro")
}

fn base_req() -> codex_api::ResponsesApiRequest {
    let mut r = codez_llm_switch::testing::sample_request();
    r.model = "deepseek-v4-pro".into();
    r
}

// ============================================================
// Basic mapping
// ============================================================

#[test]
fn instructions_become_system_message() {
    let mut req = base_req();
    req.instructions = "You are helpful".into();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputText { text: "hi".into() }],
        phase: None,
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "You are helpful");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"], "hi");
    assert_eq!(v["stream"], true);
    assert_eq!(v["stream_options"]["include_usage"], true);
    assert_eq!(v["model"], "deepseek-v4-pro");
}

#[test]
fn empty_instructions_no_system_message() {
    let mut req = base_req();
    req.instructions = "".into();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputText { text: "hi".into() }],
        phase: None,
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "user", "empty instructions should not produce system message");
}

#[test]
fn output_text_content_item_maps() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "assistant".into(),
        content: vec![ContentItem::OutputText { text: "result".into() }],
        phase: None,
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["content"], "result");
}

// ============================================================
// FunctionCall + FunctionCallOutput pairing
// ============================================================

#[test]
fn function_call_and_output_pair_by_call_id() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "get_weather".into(),
            namespace: None,
            arguments: "{\"city\":\"SF\"}".into(),
            call_id: "call_1".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            call_id: "call_1".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("sunny".into()),
                success: Some(true),
            },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    // assistant tool_calls
    let asst = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(asst["tool_calls"][0]["id"], "call_1");
    assert_eq!(asst["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(asst["tool_calls"][0]["function"]["arguments"], "{\"city\":\"SF\"}");
    // tool result follows assistant immediately (§4.10 reordering)
    let asst_idx = msgs.iter().position(|m| m["role"] == "assistant").unwrap();
    assert_eq!(msgs[asst_idx + 1]["role"], "tool");
    assert_eq!(msgs[asst_idx + 1]["tool_call_id"], "call_1");
    assert_eq!(msgs[asst_idx + 1]["content"], "sunny");
}

#[test]
fn tool_output_failure_prefixes_marker() {
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
            call_id: "c".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("boom".into()),
                success: Some(false),
            },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let tool = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert!(
        tool["content"].as_str().unwrap().starts_with("[tool error]"),
        "failed result should start with [tool error]"
    );
}

#[test]
fn function_call_output_content_items_text_only_maps() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c2".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            call_id: "c2".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::ContentItems(vec![
                    FunctionCallOutputContentItem::InputText { text: "part1".into() },
                    FunctionCallOutputContentItem::InputText { text: "part2".into() },
                ]),
                success: None,
            },
            metadata: None,
        },
    ];
    let v = build(&req, &ctx()).unwrap();
    let tool = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert_eq!(tool["content"], "part1part2");
}

// ============================================================
// Orphan repair (§4.10)
// ============================================================

#[test]
fn orphan_tool_call_gets_placeholder_result() {
    // Call without result (compression damaged) → inject synthetic placeholder result, don't hard-fail (§4.10)
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
    let tool = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool");
    assert!(tool.is_some(), "orphan call must receive placeholder tool result");
    assert_eq!(tool.unwrap()["tool_call_id"], "orphan");
}

#[test]
fn orphan_tool_result_is_dropped() {
    let mut req = base_req();
    req.input = vec![ResponseItem::FunctionCallOutput {
        call_id: "ghost".into(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("x".into()),
            success: None,
        },
        metadata: None,
    }];
    let v = build(&req, &ctx()).unwrap();
    let has_tool = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool");
    assert!(!has_tool, "orphan result without matching call should be dropped");
}

// ============================================================
// Tool definitions
// ============================================================

#[test]
fn tool_choice_none_when_no_tools() {
    // Has tool_choice but tools is empty → strip (§4.10)
    let mut req = base_req();
    req.tool_choice = "required".into();
    req.tools = vec![];
    let v = build(&req, &ctx()).unwrap();
    assert!(
        v.get("tool_choice").is_none(),
        "tool_choice should be stripped when no tools"
    );
}

#[test]
fn function_tool_definition_maps() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"function","name":"f","description":"d","parameters":{"type":"object"}})];
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tools"][0]["type"], "function");
    assert_eq!(v["tools"][0]["function"]["name"], "f");
}

#[test]
fn parallel_tool_calls_passthrough() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.parallel_tool_calls = false;
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["parallel_tool_calls"], false);
}

// ============================================================
// Outbound dropped variants (no error, not in messages)
// ============================================================

#[test]
fn reasoning_item_is_discarded_silently() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::Reasoning {
            id: String::new(),
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
    // no error
    let msgs = v["messages"].as_array().unwrap();
    // only user message, no reasoning-related messages
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
fn custom_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"custom","name":"freeform"})];
    assert!(build(&req, &ctx()).is_err(), "custom tool type should hard-fail");
}

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
    assert!(build(&req, &ctx()).is_err(), "namespaced function call should hard-fail");
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
    assert!(build(&req, &ctx()).is_err(), "image input should hard-fail (v1 capability marker)");
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
    assert!(build(&req, &ctx()).is_err(), "LocalShellCall should hard-fail");
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
    assert!(build(&req, &ctx()).is_err(), "ToolSearchCall should hard-fail");
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
    assert!(build(&req, &ctx()).is_err(), "WebSearchCall should hard-fail");
}

#[test]
fn image_generation_call_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ImageGenerationCall {
        id: String::new(),
        status: "completed".into(),
        revised_prompt: None,
        result: "base64data".into(),
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "ImageGenerationCall should hard-fail");
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
    assert!(build(&req, &ctx()).is_err(), "CustomToolCall should hard-fail");
}

#[test]
fn custom_tool_call_output_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::CustomToolCallOutput {
        call_id: "c".into(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("x".into()),
            success: None,
        },
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "CustomToolCallOutput should hard-fail");
}

#[test]
fn tool_search_output_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ToolSearchOutput {
        call_id: None,
        status: "done".into(),
        execution: "{}".into(),
        tools: vec![],
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "ToolSearchOutput should hard-fail");
}

#[test]
fn compaction_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Compaction {
        encrypted_content: "enc".into(),
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "Compaction should hard-fail (encrypted content)");
}

#[test]
fn context_compaction_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ContextCompaction {
        encrypted_content: Some("enc".into()),
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "ContextCompaction should hard-fail (encrypted content)");
}

#[test]
fn agent_message_encrypted_content_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::AgentMessage {
        author: "agent".into(),
        recipient: "user".into(),
        content: vec![AgentMessageInputContent::EncryptedContent {
            encrypted_content: "enc".into(),
        }],
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "AgentMessage with EncryptedContent should hard-fail"
    );
}

#[test]
fn other_variant_hard_fails() {
    // ResponseItem::Other is produced by #[serde(other)], only constructible via JSON deserialization
    let unknown_item: ResponseItem = serde_json::from_str(r#"{"type":"unknown_future_variant"}"#).unwrap();
    let mut req = base_req();
    req.input = vec![unknown_item];
    assert!(build(&req, &ctx()).is_err(), "Other unknown variant should hard-fail");
}

// ============================================================
// Image/encrypted content in FunctionCallOutput hard-fails
// ============================================================

#[test]
fn function_call_output_image_content_hard_fails() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c3".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            call_id: "c3".into(),
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
    assert!(build(&req, &ctx()).is_err(), "tool result with image content should hard-fail");
}

#[test]
fn function_call_output_encrypted_content_hard_fails() {
    let mut req = base_req();
    req.input = vec![
        ResponseItem::FunctionCall {
            id: None,
            name: "f".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c4".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            call_id: "c4".into(),
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
    assert!(build(&req, &ctx()).is_err(), "tool result with encrypted content should hard-fail");
}

// ============================================================
// tool_choice mapping
// ============================================================

#[test]
fn tool_choice_auto_maps() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.tool_choice = "auto".into();
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tool_choice"], "auto");
}

#[test]
fn tool_choice_none_maps() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.tool_choice = "none".into();
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tool_choice"], "none");
}

#[test]
fn tool_choice_required_maps() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.tool_choice = "required".into();
    let v = build(&req, &ctx()).unwrap();
    assert_eq!(v["tool_choice"], "required");
}

#[test]
fn tool_choice_empty_no_key() {
    let mut req = base_req();
    req.tools =
        vec![json!({"type":"function","name":"f","parameters":{"type":"object"}})];
    req.tool_choice = "".into();
    let v = build(&req, &ctx()).unwrap();
    assert!(v.get("tool_choice").is_none(), "empty tool_choice should not be written to JSON");
}

// ============================================================
// C1: Golden fixture comparison tests
// ============================================================

/// Complete round-trip: system+user+function call+tool result+assistant, compare against fixture
#[test]
fn fixture_system_user_tool_roundtrip() {
    let expected: serde_json::Value = serde_json::from_str(
        include_str!("fixtures/chat_req_system_user_tool_roundtrip.expected.json"),
    )
    .expect("fixture JSON parse failed");

    let mut req = base_req();
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

    let actual = build(&req, &ctx()).unwrap();
    assert_eq!(actual, expected);
}

/// Two sequential tool round-trips (§4.10 reordering), compare against fixture
#[test]
fn fixture_multi_tool_sequential() {
    let expected: serde_json::Value = serde_json::from_str(
        include_str!("fixtures/chat_req_multi_tool.expected.json"),
    )
    .expect("fixture JSON parse failed");

    let mut req = base_req();
    req.tools = vec![
        json!({"type": "function", "name": "tool_a"}),
        json!({"type": "function", "name": "tool_b"}),
    ];
    req.tool_choice = "auto".into();
    req.input = vec![
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: "Run two tools.".into(),
            }],
            phase: None,
            metadata: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            name: "tool_a".into(),
            namespace: None,
            arguments: "{\"x\":1}".into(),
            call_id: "call_a".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            call_id: "call_a".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("result_a".into()),
                success: Some(true),
            },
            metadata: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            name: "tool_b".into(),
            namespace: None,
            arguments: "{\"y\":2}".into(),
            call_id: "call_b".into(),
            metadata: None,
        },
        ResponseItem::FunctionCallOutput {
            call_id: "call_b".into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("result_b".into()),
                success: Some(true),
            },
            metadata: None,
        },
    ];

    let actual = build(&req, &ctx()).unwrap();
    assert_eq!(actual, expected);
}

// ============================================================
// I1: response_format correctly maps (json_schema)
// ============================================================

/// When text.format contains json_schema, response_format should be
/// {"type":"json_schema","json_schema":{"name":...,"schema":...,"strict":...}}
#[test]
fn text_format_json_schema_maps_to_response_format() {
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"]
    });
    let text_controls =
        codex_api::create_text_param_for_request(None, &Some(schema.clone()), true);

    let mut req = base_req();
    req.text = text_controls;

    let v = build(&req, &ctx()).unwrap();
    let rf = v
        .get("response_format")
        .expect("text.format should produce response_format field");
    assert_eq!(
        rf["type"], "json_schema",
        "response_format.type should be json_schema"
    );
    let inner = rf
        .get("json_schema")
        .expect("response_format should contain json_schema object");
    assert_eq!(inner["schema"], schema, "json_schema.schema should match input schema");
    assert_eq!(inner["strict"], true, "json_schema.strict should be true");
    assert_eq!(
        inner["name"], "codex_output_schema",
        "json_schema.name should match TextFormat.name"
    );
    // should not expose raw TextFormat fields at top level
    assert!(
        rf.get("schema").is_none(),
        "response_format should not contain bare schema field (nested format error)"
    );
}

// ============================================================
// I2: Non-function tool definitions hard-fail (§4.0b)
// ============================================================

/// native type tool definition → hard-fail
#[test]
fn native_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type": "native", "name": "shell"})];
    assert!(build(&req, &ctx()).is_err(), "native tool type should hard-fail");
}

/// provider type tool definition → hard-fail
#[test]
fn provider_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type": "provider", "name": "web_search"})];
    assert!(build(&req, &ctx()).is_err(), "provider tool type should hard-fail");
}

/// freeform type tool definition → hard-fail
#[test]
fn freeform_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type": "freeform", "name": "anything"})];
    assert!(build(&req, &ctx()).is_err(), "freeform tool type should hard-fail");
}

/// tool definition without type field → hard-fail
#[test]
fn no_type_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"name": "mystery_tool", "parameters": {"type": "object"}})];
    assert!(build(&req, &ctx()).is_err(), "tool definition missing type field should hard-fail");
}
