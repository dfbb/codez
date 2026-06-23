mod config;
mod http;
mod namespace;
mod pipeline;
mod purpose;
mod transform;
mod connector;
mod sse;

/// Test helper module (integration test entry point; not part of the official public API).
#[doc(hidden)]
pub mod testing {
    use crate::connector::{ConnError, EgressCtx};

    /// Construct a minimal `ResponsesApiRequest` sample (for reuse in integration tests).
    pub fn sample_request() -> codex_api::ResponsesApiRequest {
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

    /// Construct a minimal `EgressCtx` (for reuse in integration tests).
    pub fn dummy_ctx(model: &str) -> EgressCtx {
        EgressCtx {
            base_url: "https://api.example.com".into(),
            model: model.to_string(),
            auth: crate::AuthKind::Bearer,
            key: Some("test-key".into()),
            anthropic_version: None,
            path_override: None,
            default_max_tokens: None,
            prompt_cache: false,
            http: reqwest::Client::new(),
            auth_fallback: None,
        }
    }

    /// Forward `build_chat_request` for integration tests to call internal logic.
    pub fn build_chat_request_for_test(
        req: &codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<serde_json::Value, ConnError> {
        crate::connector::chat::build_chat_request(req, ctx)
    }

    /// Construct a minimal `EgressCtx` for the anthropic connector.
    pub fn dummy_ctx_anthropic(model: &str, default_max_tokens: Option<u32>) -> EgressCtx {
        EgressCtx {
            base_url: "https://api.anthropic.com".into(),
            model: model.to_string(),
            auth: crate::AuthKind::XApiKey,
            key: Some("test-key".into()),
            anthropic_version: Some("2023-06-01".into()),
            path_override: None,
            default_max_tokens,
            prompt_cache: false,
            http: reqwest::Client::new(),
            auth_fallback: None,
        }
    }

    /// Forward `build_anthropic_request` for integration tests to call internal logic.
    pub fn build_anthropic_request_for_test(
        req: &codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<serde_json::Value, ConnError> {
        crate::connector::anthropic::build_anthropic_request(req, ctx)
    }

    /// Run through an entire SSE chunk sequence and return all `ResponseEvent`s.
    /// When `done = true`, automatically call `finish()` (simulating `[DONE]`).
    /// For use in integration tests (integration tests cannot access functions inside `#[cfg(test)]`).
    pub fn translate_chat_sse_for_test(
        chunks: &[serde_json::Value],
        done: bool,
    ) -> Result<Vec<codex_api::ResponseEvent>, ConnError> {
        crate::connector::chat_sse::translate_chat_sse(chunks, done)
    }

    /// Run through an entire Anthropic Messages SSE event sequence and return all `ResponseEvent`s.
    /// When `done = true`, automatically call `finish()` (simulating `message_stop`).
    /// For use in integration tests (integration tests cannot access functions inside `#[cfg(test)]`).
    pub fn translate_anthropic_sse_for_test(
        events: &[serde_json::Value],
        done: bool,
    ) -> Result<Vec<codex_api::ResponseEvent>, ConnError> {
        crate::connector::anthropic_sse::translate_anthropic_sse(events, done)
    }

    /// Return a `SseTranslator` instance for the chat connector (for use in `run_egress_for_test`).
    pub fn chat_translator() -> Box<dyn crate::SseTranslator> {
        Box::new(crate::connector::chat_sse::ChatSseState::default())
    }

    /// Construct minimal valid auth headers (Bearer + test key).
    pub fn dummy_headers() -> reqwest::header::HeaderMap {
        crate::http::build_headers(crate::AuthKind::Bearer, Some("testkey"), None)
            .expect("dummy_headers should not fail")
    }

    /// Forward `sse::run_egress` for use in the `run_test.rs` integration test.
    pub async fn run_egress_for_test(
        url: String,
        headers: reqwest::header::HeaderMap,
        body: serde_json::Value,
        translator: Box<dyn crate::SseTranslator>,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        crate::sse::run_egress(url, headers, body, reqwest::Client::new(), translator).await
    }

    /// Noop `AuthProvider` that never writes auth headers (for use in live tests; testkey includes auth_key, no fallback needed).
    pub fn noop_auth_provider() -> codex_api::SharedAuthProvider {
        struct Noop;
        impl codex_api::AuthProvider for Noop {
            fn add_auth_headers(&self, _h: &mut reqwest::header::HeaderMap) {}
        }
        std::sync::Arc::new(Noop)
    }
}

pub use config::{
    load_config_from_str, AuthKind, Config, ConfigError, Connector, ProviderCfg,
};
pub use http::{build_headers, default_path, egress_url, resolve_key, HttpError};
pub use namespace::request_has_namespace_tools;
pub use pipeline::{default_plugins, run_transforms, TransformPlugin};
pub use connector::{make_connector, ConnError, Connector as ConnectorTrait, EgressCtx, SseTranslator};
pub use purpose::{purpose_from_source, Purpose};

use codex_protocol::protocol::SessionSource;
use std::sync::OnceLock;

/// For live tests / standalone runs only: load config from gitignored testkey.toml, allowing inline auth_key.
pub fn load_testkey_config(path: &std::path::Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    load_config_from_str(&text, true)
}

/// Route result: hits a managed provider.
#[derive(Debug, Clone)]
pub struct Route {
    pub provider_id: String,
    pub cfg: ProviderCfg,
}

/// Main entry point (Task 09 patch call contract; signature is fixed exactly).
///
/// Pipeline: transform layer → assemble `EgressCtx` (with key fallback) → dispatch connector.
/// `base_url` is required; return `ApiError::InvalidRequest` early if missing.
pub async fn run(
    rt: Route,
    mut request: codex_api::ResponsesApiRequest,
    api_auth: codex_api::SharedAuthProvider,
) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
    // ① Transform layer (v1 pass-through)
    let plugins = pipeline::default_plugins();
    pipeline::run_transforms(&plugins, &mut request)
        .map_err(codex_api::ApiError::from)?;

    // ② base_url required validation (see plan Step 6 notes)
    let base_url = rt.cfg.base_url.clone().ok_or_else(|| {
        codex_api::ApiError::InvalidRequest {
            message: format!("provider {} missing base_url", rt.provider_id),
        }
    })?;

    // ③ Key resolution
    let key = http::resolve_key(&rt.cfg)
        .map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() })?;

    // ④ Egress model: config override > model in request
    let model = rt.cfg.model.clone().unwrap_or_else(|| request.model.clone());

    let ctx = connector::EgressCtx {
        base_url,
        model,
        auth: rt.cfg.auth,
        key,
        anthropic_version: rt.cfg.anthropic_version.clone(),
        path_override: rt.cfg.path.clone(),
        default_max_tokens: rt.cfg.default_max_tokens,
        prompt_cache: rt.cfg.prompt_cache,
        http: shared_http_client(),
        auth_fallback: Some(api_auth),
    };
    let connector = connector::make_connector(rt.cfg.connector);
    connector.run(request, &ctx).await
}

fn shared_http_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new).clone()
}

/// Process-level config cache. Read once at runtime from ~/.codex/config-zmod.toml.
fn loaded() -> &'static Config {
    static CACHE: OnceLock<Config> = OnceLock::new();
    CACHE.get_or_init(|| {
        let path = dirs_config_zmod_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => load_config_from_str(&text, false).unwrap_or_else(|e| {
                tracing::warn!("llm-switch disabled: bad config-zmod.toml: {e}");
                Config { enabled: false, providers: Default::default(), purpose: Default::default() }
            }),
            Err(_) => Config { enabled: false, providers: Default::default(), purpose: Default::default() }, // missing file = disabled
        }
    })
}

fn dirs_config_zmod_path() -> std::path::PathBuf {
    // ~/.codex/config-zmod.toml; CODEX_HOME override takes precedence (consistent with codex convention)
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")))
        .unwrap_or_else(|| std::path::PathBuf::from(".codex"));
    home.join("config-zmod.toml")
}

/// Global toggle: `[llm-switch].enabled`.
pub fn enabled() -> bool {
    loaded().enabled
}

/// Two-level pure function route (spec §4): purpose priority -> provider-id fallback -> None.
/// `has_ns_tools` is pre-computed by caller using request (spec §4.1).
fn route_in(
    cfg: &Config,
    provider_id: &str,
    purpose: Option<Purpose>,
    has_ns_tools: bool,
) -> Option<Route> {
    if !cfg.enabled {
        return None;
    }
    // Step 3: purpose branch
    if let Some(p) = purpose {
        if let Some(target) = cfg.purpose.get(p.as_key()) {
            match cfg.providers.get(target) {
                None => {
                    tracing::warn!(
                        "llm-switch purpose '{}' -> unknown provider '{}', falling back to provider-id routing",
                        p.as_key(),
                        target
                    );
                }
                Some(_) if has_ns_tools => {
                    tracing::warn!(
                        "llm-switch purpose '{}' matched but request contains inexpressible tools, abandon purpose routing and fall back to provider-id",
                        p.as_key()
                    );
                }
                Some(pc) => {
                    return Some(Route { provider_id: target.clone(), cfg: pc.clone() });
                }
            }
        }
    }
    // Step 4: provider-id branch (ignore has_ns_tools, preserve v1 hard-fail contract)
    cfg.providers.get(provider_id).map(|p| Route {
        provider_id: provider_id.to_string(),
        cfg: p.clone(),
    })
}

/// WebSocket bypass pure function (spec §4.2): only check source/purpose, do not pre-check namespace.
///
/// `_provider_id` parameter is retained only to maintain consistency with `route_in` / public entry point signature;
/// bypass determination only checks source/purpose and does not consume provider id.
fn should_bypass_in(cfg: &Config, _provider_id: &str, purpose: Option<Purpose>) -> bool {
    if !cfg.enabled {
        return false;
    }
    match purpose {
        Some(p) => cfg
            .purpose
            .get(p.as_key())
            .map(|target| cfg.providers.contains_key(target))
            .unwrap_or(false),
        None => false,
    }
}

/// Two-level route entry point (Task 5 patch call contract, signature is fixed exactly).
/// purpose is parsed from source; namespace pre-check applies to purpose branch (spec §4 / §4.1).
pub fn route(
    provider_id: &str,
    source: Option<&SessionSource>,
    request: &codex_api::ResponsesApiRequest,
) -> Option<Route> {
    let cfg = loaded();
    let purpose = source.and_then(purpose_from_source);
    let has_ns_tools = purpose.is_some() && request_has_namespace_tools(request);
    route_in(cfg, provider_id, purpose, has_ns_tools)
}

/// Transport layer bypass determination (Task 5 patch call contract, signature is fixed exactly).
/// Return true when purpose matches and the mapped target exists, causing stream() to skip WebSocket and use HTTP (spec §4.2).
pub fn should_bypass_websocket(
    provider_id: &str,
    source: Option<&SessionSource>,
) -> bool {
    let cfg = loaded();
    let purpose = source.and_then(purpose_from_source);
    should_bypass_in(cfg, provider_id, purpose)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_never_routes() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=false\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert!(route_in(&cfg, "x", None, false).is_none());
    }
    #[test]
    fn enabled_routes_known_provider() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert!(route_in(&cfg, "x", None, false).is_some());
        assert!(route_in(&cfg, "unknown", None, false).is_none());
    }

    fn cfg_with_purpose() -> Config {
        // providers: gpt (primary), cheap (purpose target); purpose: compact->cheap, review->nonexist
        load_config_from_str(
            r#"
[llm-switch]
enabled = true
[llm-switch.providers.gpt]
connector = "chat"
auth = "bearer"
[llm-switch.providers.cheap]
connector = "chat"
auth = "bearer"
[llm-switch.purpose]
compact = "cheap"
review  = "nonexist"
"#,
            false,
        )
        .unwrap()
    }

    #[test]
    fn purpose_hit_routes_to_target() {
        let cfg = cfg_with_purpose();
        // compact matched -> target cheap, no ns tools
        let r = route_in(&cfg, "gpt", Some(Purpose::Compact), false).expect("route some");
        assert_eq!(r.provider_id, "cheap");
    }

    #[test]
    fn purpose_bad_mapping_falls_back_to_provider_id() {
        let cfg = cfg_with_purpose();
        // review -> "nonexist" does not exist -> fall back to provider-id (gpt exists)
        let r = route_in(&cfg, "gpt", Some(Purpose::Review), false).expect("route some");
        assert_eq!(r.provider_id, "gpt");
    }

    #[test]
    fn purpose_with_ns_tools_falls_back_to_provider_id() {
        let cfg = cfg_with_purpose();
        // compact matched but contains ns tools -> abandon purpose routing, fall back to provider-id
        let r = route_in(&cfg, "gpt", Some(Purpose::Compact), true).expect("route some");
        assert_eq!(r.provider_id, "gpt");
    }

    #[test]
    fn no_purpose_uses_provider_id() {
        let cfg = cfg_with_purpose();
        let r = route_in(&cfg, "gpt", None, false).expect("route some");
        assert_eq!(r.provider_id, "gpt");
    }

    #[test]
    fn no_purpose_unknown_provider_is_none() {
        let cfg = cfg_with_purpose();
        assert!(route_in(&cfg, "unknown", None, false).is_none());
    }

    #[test]
    fn purpose_memory_unmapped_falls_back_to_provider_id() {
        let cfg = cfg_with_purpose();
        // cfg_with_purpose does not configure memory purpose -> fall back to provider-id (gpt exists)
        let r = route_in(&cfg, "gpt", Some(Purpose::Memory), false).expect("route some");
        assert_eq!(r.provider_id, "gpt");
    }

    #[test]
    fn purpose_hit_unknown_provider_id_still_routes_to_purpose() {
        let cfg = cfg_with_purpose();
        // primary provider does not exist, but compact hits cheap -> purpose routing still applies
        let r = route_in(&cfg, "unknown-main", Some(Purpose::Compact), false).expect("route some");
        assert_eq!(r.provider_id, "cheap");
    }

    #[test]
    fn disabled_never_routes_two_level() {
        let cfg = load_config_from_str(
            "[llm-switch]\nenabled=false\n[llm-switch.providers.cheap]\nconnector=\"chat\"\nauth=\"bearer\"\n[llm-switch.purpose]\ncompact=\"cheap\"\n",
            false,
        ).unwrap();
        assert!(route_in(&cfg, "gpt", Some(Purpose::Compact), false).is_none());
    }

    #[test]
    fn bypass_ws_true_when_purpose_target_exists() {
        let cfg = cfg_with_purpose();
        assert!(should_bypass_in(&cfg, "gpt", Some(Purpose::Compact)));
    }

    #[test]
    fn bypass_ws_false_when_no_purpose() {
        let cfg = cfg_with_purpose();
        assert!(!should_bypass_in(&cfg, "gpt", None));
    }

    #[test]
    fn bypass_ws_false_on_bad_mapping() {
        let cfg = cfg_with_purpose();
        // review -> nonexist: target does not exist -> do not bypass WS (will fall back to provider-id, can use native WS)
        assert!(!should_bypass_in(&cfg, "gpt", Some(Purpose::Review)));
    }

    #[test]
    fn bypass_ws_false_when_disabled() {
        let cfg = load_config_from_str(
            "[llm-switch]\nenabled=false\n[llm-switch.providers.cheap]\nconnector=\"chat\"\nauth=\"bearer\"\n[llm-switch.purpose]\ncompact=\"cheap\"\n",
            false,
        ).unwrap();
        assert!(!should_bypass_in(&cfg, "gpt", Some(Purpose::Compact)));
    }
}
