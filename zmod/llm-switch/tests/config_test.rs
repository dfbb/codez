use codez_llm_switch::{load_config_from_str, AuthKind, Connector};

const SAMPLE: &str = r#"
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"

[llm-switch.providers.claude]
connector         = "anthropic"
base_url          = "https://api.anthropic.com"
auth              = "x-api-key"
key_env           = "ANTHROPIC_API_KEY"
anthropic_version = "2023-06-01"
default_max_tokens = 8192
"#;

#[test]
fn parses_providers() {
    let cfg = load_config_from_str(SAMPLE, false).expect("parse ok");
    assert!(cfg.enabled);
    let ds = cfg.providers.get("deepseek").expect("deepseek present");
    assert!(matches!(ds.connector, Connector::Chat));
    assert!(matches!(ds.auth, AuthKind::Bearer));
    assert_eq!(ds.key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
    let cl = cfg.providers.get("claude").expect("claude present");
    assert!(matches!(cl.connector, Connector::Anthropic));
    assert!(matches!(cl.auth, AuthKind::XApiKey));
    assert_eq!(cl.default_max_tokens, Some(8192));
    assert_eq!(cl.anthropic_version.as_deref(), Some("2023-06-01"));
}

#[test]
fn rejects_inline_auth_key_in_prod() {
    let toml = r#"
[llm-switch]
enabled = true
[llm-switch.providers.deepseek]
connector = "chat"
auth = "bearer"
auth_key = "sk-secret"
"#;
    // 运行时路径 allow_inline_key=false:必须报配置错误拒绝启动
    let err = load_config_from_str(toml, false).unwrap_err();
    assert!(format!("{err}").contains("auth_key"), "err should mention auth_key: {err}");
    // testkey 路径 allow_inline_key=true:接受
    let ok = load_config_from_str(toml, true).expect("testkey path accepts inline key");
    assert_eq!(
        ok.providers.get("deepseek").unwrap().auth_key.as_deref(),
        Some("sk-secret")
    );
}

#[test]
fn responses_connector_is_not_routable() {
    let toml = r#"
[llm-switch]
enabled = true
[llm-switch.providers.openai]
connector = "responses"
auth = "bearer"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    // responses 不进 zmod:解析允许,但 route() 不返回它(见 lib.rs 测试 Step 6)
    assert!(cfg.providers.get("openai").is_none(), "responses provider dropped from routable map");
}

#[test]
fn missing_section_means_disabled() {
    let cfg = load_config_from_str("[other]\nx = 1\n", false).expect("parse ok");
    assert!(!cfg.enabled);
    assert!(cfg.providers.is_empty());
}

#[test]
fn unknown_connector_is_rejected() {
    let toml = "[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"anthropics\"\nauth=\"bearer\"\n";
    let err = load_config_from_str(toml, false).unwrap_err();
    assert!(matches!(err, codez_llm_switch::ConfigError::UnknownConnector(_, _)), "应为 UnknownConnector: {err:?}");
}

#[test]
fn responses_provider_with_auth_key_is_rejected() {
    let toml = "[llm-switch]\nenabled=true\n[llm-switch.providers.openai]\nconnector=\"responses\"\nauth=\"bearer\"\nauth_key=\"sk-secret\"\n";
    let err = load_config_from_str(toml, false).unwrap_err();
    assert!(format!("{err}").contains("auth_key"), "应提到 auth_key: {err}");
}

#[test]
fn parses_purpose_table() {
    let toml = r#"
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"

[llm-switch.purpose]
compact = "deepseek"
review  = "deepseek"
memory  = "deepseek"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    assert_eq!(cfg.purpose.get("compact").map(String::as_str), Some("deepseek"));
    assert_eq!(cfg.purpose.get("review").map(String::as_str), Some("deepseek"));
    assert_eq!(cfg.purpose.get("memory").map(String::as_str), Some("deepseek"));
}

#[test]
fn purpose_table_absent_is_empty_not_error() {
    let toml = r#"
[llm-switch]
enabled = true

[llm-switch.providers.x]
connector = "chat"
auth = "bearer"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    assert!(cfg.purpose.is_empty());
}

#[test]
fn purpose_value_to_unknown_provider_is_kept_not_rejected() {
    // 坏映射不在解析层拒绝;route() 运行时再 warn+回退(spec §4 第 3a)
    let toml = r#"
[llm-switch]
enabled = true

[llm-switch.providers.x]
connector = "chat"
auth = "bearer"

[llm-switch.purpose]
compact = "does-not-exist"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    assert_eq!(cfg.purpose.get("compact").map(String::as_str), Some("does-not-exist"));
}
