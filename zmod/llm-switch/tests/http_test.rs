use codez_llm_switch::{build_headers, default_path, egress_url, AuthKind, Connector};

#[test]
fn url_chat_default() {
    assert_eq!(
        egress_url("https://api.deepseek.com/v1", Connector::Chat, None),
        "https://api.deepseek.com/v1/chat/completions"
    );
}

#[test]
fn url_anthropic_default_and_trims_slash() {
    assert_eq!(
        egress_url("https://api.anthropic.com/", Connector::Anthropic, None),
        "https://api.anthropic.com/v1/messages"
    );
}

#[test]
fn url_path_override() {
    assert_eq!(
        egress_url("https://gw.example.com/api", Connector::Anthropic, Some("/custom/messages")),
        "https://gw.example.com/api/custom/messages"
    );
}

#[test]
fn default_paths() {
    assert_eq!(default_path(Connector::Chat), "/chat/completions");
    assert_eq!(default_path(Connector::Anthropic), "/v1/messages");
}

#[test]
fn headers_bearer() {
    let h = build_headers(AuthKind::Bearer, Some("sk-abc"), None).unwrap();
    assert_eq!(h.get("authorization").unwrap(), "Bearer sk-abc");
    assert!(h.get("x-api-key").is_none());
}

#[test]
fn headers_xapikey_with_version() {
    let h = build_headers(AuthKind::XApiKey, Some("sk-xyz"), Some("2023-06-01")).unwrap();
    assert_eq!(h.get("x-api-key").unwrap(), "sk-xyz");
    assert_eq!(h.get("anthropic-version").unwrap(), "2023-06-01");
    assert!(h.get("authorization").is_none());
}

#[test]
fn headers_xapikey_missing_key_errors() {
    assert!(build_headers(AuthKind::XApiKey, None, Some("2023-06-01")).is_err());
}
