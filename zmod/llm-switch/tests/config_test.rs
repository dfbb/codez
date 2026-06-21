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
    // Runtime path allow_inline_key=false: must report a config error and refuse to start
    let err = load_config_from_str(toml, false).unwrap_err();
    assert!(format!("{err}").contains("auth_key"), "err should mention auth_key: {err}");
    // testkey path allow_inline_key=true: accepted
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
    // responses does not enter zmod: parsing is allowed, but route() does not return it (see lib.rs test Step 6)
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
