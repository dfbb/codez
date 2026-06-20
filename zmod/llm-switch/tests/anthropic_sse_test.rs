use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codez_llm_switch::testing::translate_anthropic_sse_for_test as run;
use serde_json::json;

// ─── 单元测试 ─────────────────────────────────────────────────────────────────

#[test]
fn text_stream_synthesizes_message_and_completed() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":3}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();

    // OutputTextDelta 流式输出
    let deltas: Vec<&String> = out
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::OutputTextDelta(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Hel", "lo"]);

    // 合成 assistant message 完成项（§4.5）
    let synth = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. })
                if role == "assistant" =>
            {
                Some(content)
            }
            _ => None,
        })
        .expect("synth assistant message");
    assert!(matches!(&synth[0], ContentItem::OutputText { text } if text == "Hello"));

    // Completed：response_id、usage、end_turn
    let (rid, usage, end) = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed {
                response_id,
                token_usage,
                end_turn,
            } => Some((response_id, token_usage, end_turn)),
            _ => None,
        })
        .expect("Completed");
    assert_eq!(rid, "msg_1");
    assert_eq!(*end, Some(true)); // end_turn → true
    let u = usage.as_ref().unwrap();
    assert_eq!(u.input_tokens, 3);
    assert_eq!(u.output_tokens, 2);
    assert_eq!(u.total_tokens, 5);
}

#[test]
fn tool_use_aggregates_partial_json_to_arguments_string() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_2","usage":{"input_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"ci"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ty\":\"SF\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();

    // FunctionCall 完成项
    let fc = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }) => Some((name, arguments, call_id)),
            _ => None,
        })
        .expect("FunctionCall");
    assert_eq!(fc.0, "get_weather");
    assert_eq!(fc.1, "{\"city\":\"SF\"}"); // partial_json 聚合 → arguments 字符串（§4.3）
    assert_eq!(fc.2, "toolu_1"); // tool_use.id → call_id（§4.8）

    // tool_use → end_turn=false
    let end = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { end_turn, .. } => Some(*end_turn),
            _ => None,
        })
        .unwrap();
    assert_eq!(end, Some(false));

    // 无文本，不合成 assistant Message
    let has_msg = out.iter().any(|e| {
        matches!(
            e,
            ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
        )
    });
    assert!(!has_msg, "pure tool-call response should not emit Message item");
}

#[test]
fn error_event_fails() {
    let events =
        vec![json!({"type":"error","error":{"type":"overloaded_error","message":"x"}})];
    assert!(run(&events, false).is_err());
}

#[test]
fn missing_response_id_synthesizes_id() {
    // message_start 没有 id 字段时，finish() 合成 llmswitch-resp-N
    let events = vec![
        json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();
    let rid = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { response_id, .. } => Some(response_id.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        rid.starts_with("llmswitch-"),
        "synth id when upstream omits id: {rid}"
    );
}

#[test]
fn no_arg_tool_use_gets_empty_object() {
    // partial_json 为空（无参数工具）时 arguments 补 "{}"
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_3","usage":{"input_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_2","name":"no_args_tool"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();
    let fc = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { arguments, .. }) => {
                Some(arguments)
            }
            _ => None,
        })
        .expect("FunctionCall");
    assert_eq!(fc, "{}");
}

// ─── Fixture-based 黄金测试 ───────────────────────────────────────────────────

/// 从 JSONL fixture 文件逐行解析 JSON 事件。
fn load_jsonl(name: &str) -> Vec<serde_json::Value> {
    // 黄金 fixture 必须用 include_str! 加载，确保文件内容在编译时嵌入并 assert_eq! 完整对比。
    // 此处用运行时路径读取（集成测试的 fixtures 路径约定），与 chat_sse_test.rs 保持一致。
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {name}: {e}"));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad JSON in {name}: {e}")))
        .collect()
}

#[test]
fn fixture_text_stream_produces_correct_events() {
    // 黄金 fixture：anthropic_sse_text.jsonl（基于 Anthropic Messages streaming 官方规范构造）
    // 用 include_str! 嵌入，编译期验证文件存在
    let _raw = include_str!("fixtures/anthropic_sse_text.jsonl");
    let events = load_jsonl("anthropic_sse_text.jsonl");
    let out = run(&events, true).unwrap();

    // OutputTextDelta
    let deltas: Vec<&String> = out
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::OutputTextDelta(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Hello", ", world"]);

    // assistant message 累计正确
    let msg = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. })
                if role == "assistant" =>
            {
                Some(content)
            }
            _ => None,
        })
        .expect("assistant message");
    assert!(matches!(&msg[0], ContentItem::OutputText { text } if text == "Hello, world"));

    // response_id 正确
    let rid = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { response_id, .. } => Some(response_id.as_str()),
            _ => None,
        })
        .unwrap();
    assert_eq!(rid, "msg_text1");

    // usage
    let usage = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { token_usage, .. } => token_usage.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage.input_tokens, 5);
    assert_eq!(usage.output_tokens, 4);

    // end_turn = true（stop_reason=end_turn）
    let end_turn = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { end_turn, .. } => Some(*end_turn),
            _ => None,
        })
        .unwrap();
    assert_eq!(end_turn, Some(true));
}

#[test]
fn fixture_tool_call_stream_produces_correct_events() {
    // 黄金 fixture：anthropic_sse_tool_call.jsonl（基于 Anthropic Messages streaming 官方规范构造）
    // 用 include_str! 嵌入，编译期验证文件存在
    let _raw = include_str!("fixtures/anthropic_sse_tool_call.jsonl");
    let events = load_jsonl("anthropic_sse_tool_call.jsonl");
    let out = run(&events, true).unwrap();

    let fc = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }) => Some((name.as_str(), arguments.as_str(), call_id.as_str())),
            _ => None,
        })
        .expect("FunctionCall");
    assert_eq!(fc.0, "get_weather");
    assert_eq!(fc.1, "{\"city\":\"SF\"}");
    assert_eq!(fc.2, "toolu_abc");

    let end_turn = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { end_turn, .. } => Some(*end_turn),
            _ => None,
        })
        .unwrap();
    assert_eq!(end_turn, Some(false)); // tool_use → end_turn=false

    // usage
    let usage = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { token_usage, .. } => token_usage.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 7);
}
