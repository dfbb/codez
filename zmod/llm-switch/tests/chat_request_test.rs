/// chat 出站请求构造测试（Task 04）
///
/// Step 0 核实的真实类型变体：
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
// 基本映射
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
    assert_eq!(msgs[0]["role"], "user", "空 instructions 不应产生 system 消息");
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
// FunctionCall + FunctionCallOutput 配对
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
            id: None,
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
    // tool result 紧跟 assistant（§4.10 重排）
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
    let tool = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert!(
        tool["content"].as_str().unwrap().starts_with("[tool error]"),
        "失败结果应以 [tool error] 开头"
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
            id: None,
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
// 孤儿修复（§4.10）
// ============================================================

#[test]
fn orphan_tool_call_gets_placeholder_result() {
    // 有调用、无结果（压缩破坏）→ 注入合成占位结果，不硬失败（§4.10）
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
    assert!(tool.is_some(), "孤儿 call 必须获得占位 tool result");
    assert_eq!(tool.unwrap()["tool_call_id"], "orphan");
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
    let has_tool = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool");
    assert!(!has_tool, "没有对应 call 的孤儿 result 应被丢弃");
}

// ============================================================
// 工具定义
// ============================================================

#[test]
fn tool_choice_none_when_no_tools() {
    // 有 tool_choice 但 tools 为空 → strip（§4.10）
    let mut req = base_req();
    req.tool_choice = "required".into();
    req.tools = vec![];
    let v = build(&req, &ctx()).unwrap();
    assert!(
        v.get("tool_choice").is_none(),
        "tool_choice 在无 tools 时应被 strip"
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
// 出站丢弃变体（不报错，不出现在 messages）
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
    // 不报错
    let msgs = v["messages"].as_array().unwrap();
    // 只有 user 消息，没有 reasoning 相关的消息
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
fn custom_tool_definition_hard_fails() {
    let mut req = base_req();
    req.tools = vec![json!({"type":"custom","name":"freeform"})];
    assert!(build(&req, &ctx()).is_err(), "custom 工具类型应硬失败");
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
    assert!(build(&req, &ctx()).is_err(), "命名空间函数调用应硬失败");
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
    assert!(build(&req, &ctx()).is_err(), "图片输入应硬失败（v1 无能力标志）");
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
    assert!(build(&req, &ctx()).is_err(), "ImageGenerationCall 应硬失败");
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
    assert!(build(&req, &ctx()).is_err(), "CustomToolCallOutput 应硬失败");
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
    assert!(build(&req, &ctx()).is_err(), "Compaction 应硬失败（加密内容）");
}

#[test]
fn context_compaction_hard_fails() {
    let mut req = base_req();
    req.input = vec![ResponseItem::ContextCompaction {
        id: None,
        encrypted_content: Some("enc".into()),
        metadata: None,
    }];
    assert!(build(&req, &ctx()).is_err(), "ContextCompaction 应硬失败（加密内容）");
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
    // ResponseItem::Other 由 #[serde(other)] 产生，只能用 JSON 反序列化构造
    let unknown_item: ResponseItem = serde_json::from_str(r#"{"type":"unknown_future_variant"}"#).unwrap();
    let mut req = base_req();
    req.input = vec![unknown_item];
    assert!(build(&req, &ctx()).is_err(), "Other 未知变体应硬失败");
}

// ============================================================
// FunctionCallOutput 中的图片/加密内容硬失败
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
            id: None,
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
    assert!(build(&req, &ctx()).is_err(), "工具结果含图片内容应硬失败");
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
            id: None,
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
    assert!(build(&req, &ctx()).is_err(), "工具结果含加密内容应硬失败");
}

// ============================================================
// tool_choice 映射
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
    assert!(v.get("tool_choice").is_none(), "空 tool_choice 不应写入 JSON");
}
