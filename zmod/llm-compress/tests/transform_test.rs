//! transform 端到端编排测试。用真实 codex 类型构造 request。

use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseItem,
};

/// 构造一个最小的 ResponsesApiRequest,input 由调用者给。
/// 字段以 codex-api/src/common.rs 为准,值取空/false/None。
fn req_with(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "gpt-test".to_string(),
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

/// Provider 无 Default、无 new():手动构造最小实例。
/// transform 的第二参当前未被读取(仅判别预留),内容不影响测试。
fn provider() -> codex_api::Provider {
    codex_api::Provider {
        name: "test".to_string(),
        base_url: "https://example.com".to_string(),
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

fn fco_text(call_id: &str, text: &str) -> ResponseItem {
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

#[test]
fn disabled_config_leaves_request_untouched() {
    // 无 config-zmod 文件 → enabled=false → request 不变。
    let big = "x\n".repeat(10_000);
    let mut r = req_with(vec![fco_text("c1", &big)]);
    let before = r.clone();
    transform(&mut r, &provider(), "qid-1");
    assert_eq!(r.input.len(), before.input.len());
    if let (
        ResponseItem::FunctionCallOutput { output: a, .. },
        ResponseItem::FunctionCallOutput { output: b, .. },
    ) = (&r.input[0], &before.input[0])
    {
        // 关闭时逐字节不变
        match (&a.body, &b.body) {
            (FunctionCallOutputBody::Text(sa), FunctionCallOutputBody::Text(sb)) => {
                assert_eq!(sa, sb)
            }
            _ => panic!("body shape changed"),
        }
    } else {
        panic!("variant changed");
    }
}

#[test]
fn non_tooloutput_variants_are_ignored() {
    // Other 等变体不处理:只验证不 panic、长度不变。
    let mut r = req_with(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert_eq!(r.input.len(), 1);
    assert!(matches!(r.input[0], ResponseItem::Other));
}

#[test]
fn contentitems_image_preserved() {
    // ContentItems 含 InputText + InputImage:图片必须原样保留。
    let mut r = req_with(vec![ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "c3".to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText { text: "short".to_string() },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AAAA".to_string(),
                    detail: None,
                },
            ]),
            success: None,
        },
        metadata: None,
    }]);
    transform(&mut r, &provider(), "qid-3");
    if let ResponseItem::FunctionCallOutput { output, .. } = &r.input[0] {
        if let FunctionCallOutputBody::ContentItems(items) = &output.body {
            // 图片项原样保留
            assert!(items.iter().any(|it| matches!(
                it,
                FunctionCallOutputContentItem::InputImage { image_url, .. } if image_url == "data:image/png;base64,AAAA"
            )));
        } else {
            panic!("body shape changed");
        }
    } else {
        panic!("variant changed");
    }
}
