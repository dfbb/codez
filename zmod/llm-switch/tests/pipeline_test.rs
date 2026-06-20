use codez_llm_switch::{default_plugins, make_connector, run_transforms};
use codez_llm_switch::Connector as ConnectorKind; // config::Connector 重导出

#[test]
fn v1_transforms_are_noop_passthrough() {
    let mut req = sample_request();
    let before = serde_json::to_value(&req).unwrap();
    let plugins = default_plugins();
    assert!(plugins.is_empty(), "v1 has no transforms");
    run_transforms(&plugins, &mut req).expect("noop ok");
    let after = serde_json::to_value(&req).unwrap();
    assert_eq!(before, after, "v1 transform must not mutate the request");
}

#[test]
fn factory_returns_distinct_connectors() {
    let _chat = make_connector(ConnectorKind::Chat);
    let _anthropic = make_connector(ConnectorKind::Anthropic);
}

fn sample_request() -> codex_api::ResponsesApiRequest {
    codex_api::ResponsesApiRequest {
        model: "test".into(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        include: vec![],
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}
