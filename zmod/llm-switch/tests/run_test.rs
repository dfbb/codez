use codez_llm_switch::testing::{run_egress_for_test, chat_translator, dummy_headers};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn non_2xx_returns_err_synchronously() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
        .mount(&server).await;
    let url = format!("{}/chat/completions", server.uri());
    let res = run_egress_for_test(url, dummy_headers(), serde_json::json!({}), chat_translator()).await;
    assert!(res.is_err(), "non-2xx must Err synchronously (before any spawn)");
}

#[tokio::test]
async fn happy_path_streams_events() {
    let server = MockServer::start().await;
    let sse = "data: {\"id\":\"r\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse))
        .mount(&server).await;
    let url = format!("{}/chat/completions", server.uri());
    let stream = run_egress_for_test(url, dummy_headers(), serde_json::json!({}), chat_translator()).await.unwrap();
    let mut rx = stream.rx_event;
    let mut kinds = Vec::new();
    while let Some(item) = rx.recv().await {
        let ev = item.unwrap();
        kinds.push(format!("{ev:?}"));
    }
    assert!(kinds.iter().any(|k| k.contains("OutputTextDelta")));
    assert!(kinds.iter().any(|k| k.contains("Completed")));
}

#[tokio::test]
async fn happy_path_chinese_content_not_corrupted() {
    let server = MockServer::start().await;
    // delta content 含中文，验证字节缓冲路径不会产生 U+FFFD 替换字符
    let sse = "data: {\"id\":\"r\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好世界\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":4,\"total_tokens\":5}}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse))
        .mount(&server).await;
    let url = format!("{}/chat/completions", server.uri());
    let stream = run_egress_for_test(url, dummy_headers(), serde_json::json!({}), chat_translator()).await.unwrap();
    let mut rx = stream.rx_event;
    let mut kinds = Vec::new();
    while let Some(item) = rx.recv().await {
        let ev = item.unwrap();
        kinds.push(format!("{ev:?}"));
    }
    // 中文内容必须原样透传，不得被替换字符破坏
    assert!(kinds.iter().any(|k| k.contains("OutputTextDelta")));
    assert!(kinds.iter().any(|k| k.contains("你好")), "中文内容应原样透传，实际 kinds: {kinds:?}");
}
