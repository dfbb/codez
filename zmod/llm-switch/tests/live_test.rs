/// Live test with gating: requires tests/testkey.toml (gitignored, contains real auth_key) + network.
/// Automatically skipped (no fail) when testkey.toml is missing or provider does not exist.
/// Explicit run: cargo test -p codez-llm-switch --test live_test -- --ignored --nocapture
use std::path::Path;
use codez_llm_switch::{load_testkey_config, Route};
use codex_api::ResponseEvent;
use codex_protocol::models::ResponseItem;
use serde_json::json;

fn testkey_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testkey.toml")
}

/// Construct Route + call run, assert stream completes with non-empty text.
/// When testkey.toml is missing or provider does not exist, print skip and return directly (no fail).
async fn run_text_roundtrip(provider_id: &str) {
    let path = testkey_path();
    if !path.exists() {
        eprintln!("skip: testkey.toml absent");
        return;
    }
    let cfg = load_testkey_config(&path).expect("parse testkey");
    let Some(pcfg) = cfg.providers.get(provider_id) else {
        eprintln!("skip: provider '{provider_id}' absent in testkey.toml");
        return;
    };
    let rt = Route { provider_id: provider_id.to_string(), cfg: pcfg.clone() };

    // Minimal request: a single user text, without any tools
    let mut req = codez_llm_switch::testing::sample_request();
    req.model = pcfg.model.clone().unwrap_or_default();
    req.input = vec![codex_protocol::models::ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![codex_protocol::models::ContentItem::InputText {
            text: "Reply with the single word: pong".into(),
        }],
        phase: None,
        metadata: None,
    }];
    req.tools = vec![]; // Disable all tools to avoid hard failures from namespace/web_search etc.

    // testkey includes auth_key, noop provider serves as fallback placeholder only
    let api_auth = codez_llm_switch::testing::noop_auth_provider();
    let stream = codez_llm_switch::run(rt, req, api_auth)
        .await
        .expect("run ok");
    let mut rx = stream.rx_event;
    let mut text = String::new();
    let mut completed = false;
    while let Some(item) = rx.recv().await {
        match item.expect("event ok") {
            ResponseEvent::OutputTextDelta(s) => text.push_str(&s),
            ResponseEvent::Completed { .. } => {
                completed = true;
            }
            _ => {}
        }
    }
    assert!(completed, "stream must complete");
    assert!(!text.trim().is_empty(), "got some assistant text");
    // Only print reply length, do not print key or reply content
    eprintln!("[{provider_id}] reply len = {}", text.len());
}

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn deepseek_chat_live() {
    run_text_roundtrip("deepseek").await;
}

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn claude_anthropic_live() {
    run_text_roundtrip("claude").await;
}

/// Construct Route + call run, assert standard function tool call appears in stream (OutputItemDone(FunctionCall{..})) and stream completes.
/// When testkey.toml is missing or provider does not exist, print skip and return directly (no fail).
async fn run_tool_roundtrip(provider_id: &str) {
    let path = testkey_path();
    if !path.exists() {
        eprintln!("skip: testkey.toml absent");
        return;
    }
    let cfg = load_testkey_config(&path).expect("parse testkey");
    let Some(pcfg) = cfg.providers.get(provider_id) else {
        eprintln!("skip: provider '{provider_id}' absent in testkey.toml");
        return;
    };
    let rt = Route { provider_id: provider_id.to_string(), cfg: pcfg.clone() };

    // Request: prompt the model to call get_weather tool; req.tools is Vec<serde_json::Value>, construct directly
    let mut req = codez_llm_switch::testing::sample_request();
    req.model = pcfg.model.clone().unwrap_or_default();
    req.input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![codex_protocol::models::ContentItem::InputText {
            text: "What's the weather in Paris? Use the get_weather tool.".into(),
        }],
        phase: None,
        metadata: None,
    }];
    // Standard function tool: type="function", consistent with input format expected by map_tools in chat_req.rs
    req.tools = vec![json!({
        "type": "function",
        "name": "get_weather",
        "description": "Get the current weather for a city.",
        "parameters": {
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        }
    })];

    let api_auth = codez_llm_switch::testing::noop_auth_provider();
    let stream = codez_llm_switch::run(rt, req, api_auth)
        .await
        .expect("run ok");
    let mut rx = stream.rx_event;
    let mut tool_call_name: Option<String> = None;
    let mut completed = false;
    while let Some(item) = rx.recv().await {
        match item.expect("event ok") {
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name, .. }) => {
                tool_call_name = Some(name.clone());
                eprintln!("[{provider_id}] tool call: {name}");
            }
            ResponseEvent::Completed { .. } => {
                completed = true;
            }
            _ => {}
        }
    }
    assert!(completed, "stream must complete");
    assert!(
        tool_call_name.is_some(),
        "expected at least one FunctionCall item in the stream"
    );
    assert_eq!(
        tool_call_name.as_deref(),
        Some("get_weather"),
        "tool call name must be get_weather"
    );
}

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn deepseek_tool_live() {
    run_tool_roundtrip("deepseek").await;
}

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn claude_tool_live() {
    run_tool_roundtrip("claude").await;
}
