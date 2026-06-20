use codez_llm_switch::testing::translate_chat_sse_for_test as run;
use codex_api::ResponseEvent;
use codex_protocol::models::{ContentItem, ResponseItem};
use serde_json::json;

/// 从 JSONL fixture 文件逐行解析 JSON chunk。
fn load_jsonl(name: &str) -> Vec<serde_json::Value> {
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

fn text_chunk(id: &str, delta: &str) -> serde_json::Value {
    json!({"id": id, "choices":[{"index":0,"delta":{"content": delta},"finish_reason": null}]})
}

#[test]
fn accumulates_text_and_synthesizes_assistant_message() {
    let chunks = vec![
        text_chunk("resp-1", "Hello"),
        text_chunk("resp-1", " world"),
        json!({"id":"resp-1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
               "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}),
    ];
    let events = run(&chunks, true).unwrap();
    // 至少两条 OutputTextDelta（展示）
    let deltas: Vec<&String> = events.iter().filter_map(|e| match e {
        ResponseEvent::OutputTextDelta(s) => Some(s), _ => None }).collect();
    assert_eq!(deltas, vec!["Hello", " world"]);
    // 合成 assistant message 完成项（§4.5）
    let synth = events.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. }) if role == "assistant" => Some(content),
        _ => None }).expect("synth assistant message present");
    assert!(matches!(&synth[0], ContentItem::OutputText { text } if text == "Hello world"));
    // Completed 三字段
    let completed = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { response_id, token_usage, end_turn } => Some((response_id, token_usage, end_turn)),
        _ => None }).expect("Completed present");
    assert_eq!(completed.0, "resp-1");
    assert_eq!(*completed.2, Some(true)); // finish_reason=stop → end_turn=true
    assert_eq!(completed.1.as_ref().unwrap().output_tokens, 2);
}

#[test]
fn aggregates_tool_call_arguments_by_index() {
    let chunks = vec![
        json!({"id":"r","choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"ci"}}]},"finish_reason":null}]}),
        json!({"id":"r","choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"ty\":\"SF\"}"}}]},"finish_reason":null}]}),
        json!({"id":"r","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
               "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
    ];
    let events = run(&chunks, true).unwrap();
    let fc = events.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name, arguments, call_id, .. }) => Some((name, arguments, call_id)),
        _ => None }).expect("FunctionCall present");
    assert_eq!(fc.0, "get_weather");
    assert_eq!(fc.1, "{\"city\":\"SF\"}"); // 按 index 聚合完整 arguments
    assert_eq!(fc.2, "call_1");            // call_id 回填（§4.8）
    // tool_calls 时 end_turn=false
    let completed2 = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { end_turn, .. } => Some(end_turn), _ => None }).unwrap();
    assert_eq!(*completed2, Some(false));
}

#[test]
fn missing_id_synthesizes_response_id() {
    let chunks = vec![json!({"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":"stop"}]})];
    let events = run(&chunks, true).unwrap();
    let rid = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { response_id, .. } => Some(response_id.clone()), _ => None }).unwrap();
    assert!(rid.starts_with("llmswitch-"), "synth id when upstream omits id: {rid}");
}

#[test]
fn cached_tokens_in_prompt_tokens_details_is_mapped() {
    // usage 含 prompt_tokens_details.cached_tokens 时，
    // 合成 Completed 的 token_usage.cached_input_tokens 应等于该值。
    let chunks = vec![
        text_chunk("resp-c", "Hi"),
        json!({"id":"resp-c","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
               "usage":{
                   "prompt_tokens": 10,
                   "completion_tokens": 3,
                   "total_tokens": 13,
                   "prompt_tokens_details": {"cached_tokens": 7}
               }}),
    ];
    let events = run(&chunks, true).unwrap();
    let usage = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { token_usage, .. } => token_usage.as_ref(),
        _ => None,
    }).expect("Completed with usage");
    assert_eq!(usage.cached_input_tokens, 7);
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 3);
}

#[test]
fn finish_reason_length_gives_end_turn_false() {
    // finish_reason="length" 表示 token 截断，end_turn 应为 Some(false)。
    let chunks = vec![
        text_chunk("resp-l", "Truncated"),
        json!({"id":"resp-l","choices":[{"index":0,"delta":{},"finish_reason":"length"}],
               "usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}),
    ];
    let events = run(&chunks, true).unwrap();
    let end_turn = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { end_turn, .. } => Some(*end_turn),
        _ => None,
    }).expect("Completed present");
    assert_eq!(end_turn, Some(false));
}

// ─── Fixture-based 黄金测试 ───────────────────────────────────────────────────

#[test]
fn fixture_text_stream_produces_correct_events() {
    let chunks = load_jsonl("chat_sse_text.jsonl");
    let events = run(&chunks, true).unwrap();
    // 有两条 OutputTextDelta（"Hello" 和 ", world"；第一个 empty delta 被跳过）
    let deltas: Vec<&String> = events.iter().filter_map(|e| match e {
        ResponseEvent::OutputTextDelta(s) => Some(s), _ => None }).collect();
    assert_eq!(deltas, vec!["Hello", ", world"]);
    // assistant message 累计正确
    let msg = events.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. }) if role == "assistant" => Some(content),
        _ => None }).expect("assistant message");
    assert!(matches!(&msg[0], ContentItem::OutputText { text } if text == "Hello, world"));
    // response_id 正确
    let rid = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { response_id, .. } => Some(response_id.as_str()), _ => None }).unwrap();
    assert_eq!(rid, "chatcmpl-text1");
}

#[test]
fn fixture_tool_call_stream_produces_correct_events() {
    let chunks = load_jsonl("chat_sse_tool_call.jsonl");
    let events = run(&chunks, true).unwrap();
    let fc = events.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name, arguments, call_id, .. }) => Some((name.as_str(), arguments.as_str(), call_id.as_str())),
        _ => None }).expect("FunctionCall");
    assert_eq!(fc.0, "get_weather");
    assert_eq!(fc.1, "{\"city\":\"SF\"}");
    assert_eq!(fc.2, "call_1");
    let end_turn = events.iter().find_map(|e| match e {
        ResponseEvent::Completed { end_turn, .. } => Some(end_turn), _ => None }).unwrap();
    assert_eq!(*end_turn, Some(false));
}
