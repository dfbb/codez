use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use codez_llm_switch::testing::translate_anthropic_sse_for_test as run;
use serde_json::json;

// ─── Unit Tests ─────────────────────────────────────────────────────────────────

#[test]
fn text_stream_synthesizes_message_and_completed() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":3,"cache_read_input_tokens":7,"cache_creation_input_tokens":4}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();

    // OutputTextDelta streamed output
    let deltas: Vec<&String> = out
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::OutputTextDelta(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Hel", "lo"]);

    // Synthesize assistant message completion item (§4.5)
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

    // Completed: response_id, usage, end_turn
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
    assert_eq!(u.cached_input_tokens, 7); // cache_read only (true hits)
    assert_eq!(u.output_tokens, 2);
    assert_eq!(u.total_tokens, 16); // 7 read + 4 creation + 3 input + 2 output
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

    // FunctionCall completion item
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
    assert_eq!(fc.1, "{\"city\":\"SF\"}"); // partial_json aggregated → arguments string (§4.3)
    assert_eq!(fc.2, "toolu_1"); // tool_use.id → call_id (§4.8)

    // tool_use → end_turn=false
    let end = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { end_turn, .. } => Some(*end_turn),
            _ => None,
        })
        .unwrap();
    assert_eq!(end, Some(false));

    // No text, do not synthesize assistant Message
    let has_msg = out.iter().any(|e| {
        matches!(
            e,
            ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
        )
    });
    assert!(
        !has_msg,
        "pure tool-call response should not emit Message item"
    );
}

#[test]
fn max_tokens_stop_reason_maps_end_turn_to_none() {
    // Truncation (max_tokens) is neither model-initiated end nor tool use → end_turn three-state takes None.
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_3","usage":{"input_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"truncat"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ];
    let out = run(&events, true).unwrap();
    let end = out
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Completed { end_turn, .. } => Some(*end_turn),
            _ => None,
        })
        .expect("Completed");
    assert_eq!(end, None); // max_tokens → end_turn three-state None
}

#[test]
fn error_event_fails() {
    let events = vec![json!({"type":"error","error":{"type":"overloaded_error","message":"x"}})];
    assert!(run(&events, false).is_err());
}

#[test]
fn missing_response_id_synthesizes_id() {
    // When message_start has no id field, finish() synthesizes llmswitch-resp-N
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
    // When partial_json is empty (no-arg tool), arguments is filled with "{}"
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

// ─── Fixture-based Golden Tests ───────────────────────────────────────────────────

/// Parse JSON events line-by-line from a JSONL fixture file.
fn load_jsonl(name: &str) -> Vec<serde_json::Value> {
    // Fixture is validated at compile-time via include_str!, runtime parsing driven by fs::read_to_string state machine,
    // output verified by assert_eq!. Here we use runtime path for reading (integration test fixture path convention), consistent with chat_sse_test.rs.
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
    // Golden fixture: anthropic_sse_text.jsonl (constructed per Anthropic Messages streaming official spec)
    // Embedded via include_str!, compile-time verification that file exists
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

    // assistant message accumulated correctly
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

    // response_id correct
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

    // end_turn = true (stop_reason=end_turn)
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
    // Golden fixture: anthropic_sse_tool_call.jsonl (constructed per Anthropic Messages streaming official spec)
    // Embedded via include_str!, compile-time verification that file exists
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
