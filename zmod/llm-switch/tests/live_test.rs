/// 门控实跑测试：需要 tests/testkey.toml（gitignored，含真实 auth_key）+ 网络。
/// testkey.toml 缺失或 provider 不存在时自动跳过（不 fail）。
/// 显式跑：cargo test -p codez-llm-switch --test live_test -- --ignored --nocapture
use std::path::Path;
use codez_llm_switch::{load_testkey_config, Route};
use codex_api::ResponseEvent;
use codex_protocol::models::ResponseItem;
use serde_json::json;

fn testkey_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testkey.toml")
}

/// 构造 Route + 调 run，断言流 Completed 且有非空文本。
/// testkey.toml 缺失或 provider 不存在时打印 skip 并直接返回（不 fail）。
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

    // 最小请求：一句 user 文本，不带任何工具
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
    req.tools = vec![]; // 关闭所有工具，避免 namespace/web_search 等触发硬失败

    // testkey 自带 auth_key，noop provider 仅作占位退路
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
    // 只打印回复长度，不打印 key 或回复内容
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

/// 构造 Route + 调 run，断言流中出现标准 function 工具调用（OutputItemDone(FunctionCall{..})）且流 Completed。
/// testkey.toml 缺失或 provider 不存在时打印 skip 并直接返回（不 fail）。
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

    // 请求：提示模型调用 get_weather 工具；req.tools 是 Vec<serde_json::Value>，直接构造
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
    // 标准 function 工具：type="function"，与 map_tools 在 chat_req.rs 中期望的输入格式一致
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
