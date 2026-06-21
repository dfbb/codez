use codez_llm_compress::query::extract;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{ContentItem, ResponseItem};

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

fn user_msg(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: text.to_string() }],
        phase: None,
        metadata: None,
    }
}

#[test]
fn extracts_keywords_from_last_user_message() {
    let r = req(vec![
        user_msg("first old request about parsing"),
        user_msg("fix the failing database connection timeout"),
    ]);
    let kw = extract(&r);
    // 取最后一条 user;去停用词 the；去长度<=2；小写
    assert!(kw.contains(&"failing".to_string()));
    assert!(kw.contains(&"database".to_string()));
    assert!(kw.contains(&"connection".to_string()));
    assert!(!kw.contains(&"the".to_string()));
}

#[test]
fn no_user_message_returns_empty() {
    let r = req(vec![ResponseItem::Other]);
    assert!(extract(&r).is_empty());
}

#[test]
fn empty_input_returns_empty() {
    let r = req(vec![]);
    assert!(extract(&r).is_empty());
}
