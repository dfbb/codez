/// Anthropic outbound request construction tests (Task 06)
///
/// # Fixture source declaration (§8 must be explicit)
/// This file uses a **self-built fixture** approach: manually verified against the Anthropic Messages API official documentation format,
/// frozen into `tests/fixtures/anthropic_req_*.expected.json`, no dependency on third-party converter tools.
/// Rationale: this repository currently has no `../3rd/proxy/llm-rosetta` Python dependency, and the official format has complete documentation support.
///
/// # Reusing types pinned in Task 04 (Step 0)
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

// ── Helper functions ──────────────────────────────────────────────────────────────────

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
        "messages should not have role=system"
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
        "empty instructions should not produce system field"
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
// §4.3 Message role only user/assistant (FunctionCall → assistant content[tool_use])
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
        .expect("assistant message must exist");
    let block = &asst["content"][0];
    assert_eq!(block["type"], "tool_use");
    assert_eq!(block["id"], "call_1");
    assert_eq!(block["name"], "get_weather");
    // arguments string parsed into object (§4.6)
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
        "invalid JSON arguments should hard fail"
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
    // content should be error text
    assert!(tr["content"].is_string(), "tool_result content should be string");
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
    // on success, is_error field should not be present (or false)
    let is_error = tr.get("is_error");
    assert!(
        is_error.is_none() || is_error == Some(&serde_json::Value::Bool(false)),
        "is_error should not be true on success"
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
        "tool_result with image should hard fail (§4.9)"
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
        "tool_result with encrypted content should hard fail"
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
    // disable_parallel_tool_use should not be true (may not have this field)
    let disable = v
        .get("tool_choice")
        .and_then(|tc| tc.get("disable_parallel_tool_use"));
    assert!(
        disable.is_none() || disable == Some(&serde_json::Value::Bool(false)),
        "when parallel_tool_calls=true should not set disable_parallel_tool_use=true"
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
    // should not have type field (Anthropic tools format)
    assert_eq!(v["tools"][0].get("type"), None);
}

#[test]
fn non_function_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"custom","name":"freeform"})];
    assert!(
        build(&req, &ctx()).is_err(),
        "non-function tool types should hard fail (§4.0b)"
    );
}

// ============================================================
// §4.10 Orphan fixup (no reordering—only inject placeholder)
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
        "orphan FunctionCall must get placeholder tool_result block"
    );
    assert_eq!(tr.unwrap()["tool_use_id"], "orphan");
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
    let tr = find_block(&v, "tool_result");
    assert!(tr.is_none(), "orphan result without corresponding call should be dropped");
}

// ============================================================
// Same role message content block merging
// ============================================================

#[test]
fn consecutive_same_role_blocks_merged() {
    // Two FunctionCalls should merge into one assistant message
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
    // Two tool_use blocks should be in the same assistant message
    assert_eq!(asst_msgs.len(), 1, "consecutive same role messages should merge into one");
    assert_eq!(asst_msgs[0]["content"].as_array().unwrap().len(), 2);
}

// ============================================================
// Outbound discard variants (§4.0 / §4.4)
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
// Hard fail variants (§4.0)
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
        "namespaced function call should hard fail"
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
        "image input should hard fail (§4.9)"
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
    assert!(build(&req, &ctx()).is_err(), "LocalShellCall should hard fail");
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
    assert!(build(&req, &ctx()).is_err(), "ToolSearchCall should hard fail");
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
    assert!(build(&req, &ctx()).is_err(), "WebSearchCall should hard fail");
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
    assert!(
        build(&req, &ctx()).is_err(),
        "ImageGenerationCall should hard fail"
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
    assert!(build(&req, &ctx()).is_err(), "CustomToolCall should hard fail");
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
    assert!(
        build(&req, &ctx()).is_err(),
        "CustomToolCallOutput should hard fail"
    );
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
    assert!(build(&req, &ctx()).is_err(), "ToolSearchOutput should hard fail");
}

#[test]
fn compaction_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::Compaction {
        encrypted_content: "enc".into(),
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "Compaction should hard fail (encrypted content)"
    );
}

#[test]
fn context_compaction_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ContextCompaction {
        encrypted_content: Some("enc".into()),
        metadata: None,
    }];
    assert!(
        build(&req, &ctx()).is_err(),
        "ContextCompaction should hard fail (encrypted content)"
    );
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
        "AgentMessage with EncryptedContent should hard fail"
    );
}

#[test]
fn other_variant_hard_fails() {
    let unknown_item: ResponseItem =
        serde_json::from_str(r#"{"type":"unknown_future_variant"}"#).unwrap();
    let mut req = base_req();
    req.input = vec![unknown_item];
    assert!(build(&req, &ctx()).is_err(), "Other unknown variant should hard fail");
}

// ============================================================
// §7.1 Field downgrade
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
// §8 Golden fixture (self-built, manually verified Anthropic Messages API format)
// ============================================================

#[test]
fn golden_system_user_tool_roundtrip() {
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/anthropic_req_system_user_tool_roundtrip.expected.json"
    ))
    .expect("fixture JSON parse failed");

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
        "actual output does not match golden fixture\nactual:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}

#[test]
fn multiple_orphan_calls_injected_in_order() {
    // Verify multiple orphan FunctionCalls inject placeholder tool_results in appearance order
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
    // Get all tool_result block tool_use_id in order
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
    assert_eq!(ids, vec!["orphan_a", "orphan_b"], "multiple orphans should be injected in appearance order");
}

// ============================================================
// §7.1 text.format → system instruction append (json_schema-specific tests)
// ============================================================

/// Verify that when text.format is json_schema, top-level system appends JSON schema instruction.
/// TextFormatType currently has only JsonSchema one variant (see codex-api/src/common.rs),
/// created via create_text_param_for_request with format TextControls.
#[test]
fn text_format_json_schema_appends_system_instruction() {
    let schema = json!({"type": "object", "properties": {"answer": {"type": "string"}}});
    let text_controls =
        codex_api::create_text_param_for_request(None::<Verbosity>, &Some(schema), false);
    assert!(text_controls.is_some(), "should return TextControls when schema is non-null");

    let mut req = base_req();
    req.instructions = "You are a helpful assistant.".into();
    req.text = text_controls;

    let v = build(&req, &ctx()).unwrap();
    let system = v["system"].as_str().expect("system field should be string");
    assert!(
        system.contains("You must respond with valid JSON matching this schema:"),
        "text.format=json_schema should append schema instruction in system, actual system: {system:?}"
    );
    // Original instructions should also be preserved (append not replace)
    assert!(
        system.contains("You are a helpful assistant."),
        "system should preserve original instructions, actual: {system:?}"
    );
}

// ============================================================
// §B2 prompt_cache top-level cache_control
// ============================================================

#[test]
fn prompt_cache_off_emits_no_cache_control() {
    let req = base_req();
    let v = build(&req, &ctx()).unwrap();
    assert!(v.get("cache_control").is_none(), "default must not add cache_control");
}

#[test]
fn prompt_cache_on_emits_top_level_cache_control() {
    let req = base_req();
    let mut c = dummy_ctx_anthropic("claude-opus-4-8", Some(8192));
    c.prompt_cache = true;
    let v = build(&req, &c).unwrap();
    assert_eq!(v["cache_control"]["type"], "ephemeral");
    // single-mechanism guarantee: no per-block markers anywhere in messages
    let msgs = v["messages"].as_array().cloned().unwrap_or_default();
    for m in &msgs {
        if let Some(blocks) = m["content"].as_array() {
            for b in blocks {
                assert!(b.get("cache_control").is_none(), "no per-block cache_control");
            }
        }
    }
}

#[test]
fn translated_messages_prefix_is_stable_across_turns() {
    // Prefix-cache lookback can only hit if turn N+1's serialized messages prefix is
    // byte-identical to turn N's. build_anthropic_request must be a pure function of the
    // request, so a growing conversation keeps its earlier message bytes stable.
    // Use user→assistant alternation so consecutive messages do not coalesce.

    fn user_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: text.into() }],
            phase: None,
            metadata: None,
        }
    }

    fn assistant_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::InputText { text: text.into() }],
            phase: None,
            metadata: None,
        }
    }

    // Turn N: [user("q1")]
    let mut turn_n = base_req();
    turn_n.input = vec![user_msg("first question")];

    // Turn N+1: [user("q1"), assistant("a1"), user("q2")] — realistic conversation growth
    let mut turn_n1 = base_req();
    turn_n1.input = vec![
        user_msg("first question"),
        assistant_msg("first answer"),
        user_msg("second question"),
    ];

    let v_n = build(&turn_n, &ctx()).unwrap();
    let v_n1 = build(&turn_n1, &ctx()).unwrap();

    // turn N's single message must serialize identically to turn N+1's first message.
    let msg_n = &v_n["messages"].as_array().unwrap()[0];
    let msg_n1_first = &v_n1["messages"].as_array().unwrap()[0];
    assert_eq!(
        serde_json::to_string(msg_n).unwrap(),
        serde_json::to_string(msg_n1_first).unwrap(),
        "earlier message bytes must be stable across turns for cache lookback"
    );
}
