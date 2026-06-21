//! transform 端到端:用真实 codex 类型构造 request,验证编排链。
use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem,
};

fn req(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "m".to_string(),
        instructions: String::new(),
        input,
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}
fn fco(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success: Some(true),
        },
        metadata: None,
    }
}

fn provider() -> codex_api::Provider {
    codex_api::Provider {
        name: "t".to_string(),
        base_url: "https://e.com".to_string(),
        query_params: None,
        headers: Default::default(),
        retry: codex_api::RetryConfig {
            max_attempts: 1,
            base_delay: std::time::Duration::from_millis(0),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: std::time::Duration::from_secs(30),
    }
}

#[test]
fn disabled_config_leaves_request_untouched() {
    // 无 config-zmod 文件 → enabled=false → 逐字节不变
    let big = "x\n".repeat(10_000);
    let mut r = req(vec![fco("c1", &big)]);
    let before = r.clone();
    transform(&mut r, &provider(), "qid-1");
    if let (
        ResponseItem::FunctionCallOutput { output: a, .. },
        ResponseItem::FunctionCallOutput { output: b, .. },
    ) = (&r.input[0], &before.input[0])
    {
        match (&a.body, &b.body) {
            (FunctionCallOutputBody::Text(sa), FunctionCallOutputBody::Text(sb)) => {
                assert_eq!(sa, sb)
            }
            _ => panic!("body shape changed"),
        }
    }
}

#[test]
fn non_tooloutput_variants_ignored() {
    let mut r = req(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert!(matches!(r.input[0], ResponseItem::Other));
}
