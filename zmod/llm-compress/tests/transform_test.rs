//! End-to-end orchestration tests for transform. Constructs requests using real codex types.

use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseItem,
};

/// Constructs a minimal ResponsesApiRequest with input provided by the caller.
/// Fields follow codex-api/src/common.rs, values default to empty/false/None.
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

/// Provider has no Default, no new(): construct minimal instance manually.
/// The second parameter of transform is currently not read (only distinguished for future use),
/// content does not affect tests.
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
    // No config-zmod file → enabled=false → request unchanged.
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
        // When disabled, unchanged byte-for-byte
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
    // Other variants are not processed: only verify no panic and length unchanged.
    let mut r = req_with(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert_eq!(r.input.len(), 1);
    assert!(matches!(r.input[0], ResponseItem::Other));
}

#[test]
fn contentitems_image_preserved() {
    // ContentItems containing InputText + InputImage: images must be preserved as-is.
    let mut r = req_with(vec![ResponseItem::FunctionCallOutput {
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
            // Image items preserved as-is
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
