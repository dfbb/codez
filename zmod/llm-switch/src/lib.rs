mod config;
mod http;
mod pipeline;
mod transform;
mod connector;
mod sse;

/// Test helper module (integration-test entry point; not part of the formal public API).
#[doc(hidden)]
pub mod testing {
    use crate::connector::{ConnError, EgressCtx};

    /// Build a minimal `ResponsesApiRequest` sample (reused across integration tests).
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

    /// Build a minimal `EgressCtx` (reused across integration tests).
    pub fn dummy_ctx(model: &str) -> EgressCtx {
        EgressCtx {
            base_url: "https://api.example.com".into(),
            model: model.to_string(),
            auth: crate::AuthKind::Bearer,
            key: Some("test-key".into()),
            anthropic_version: None,
            path_override: None,
            default_max_tokens: None,
            http: reqwest::Client::new(),
            auth_fallback: None,
        }
    }

    /// Forward to `build_chat_request`, so integration tests can call the internal logic.
    pub fn build_chat_request_for_test(
        req: &codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<serde_json::Value, ConnError> {
        crate::connector::chat::build_chat_request(req, ctx)
    }

    /// Build a minimal `EgressCtx` for the anthropic connector.
    pub fn dummy_ctx_anthropic(model: &str, default_max_tokens: Option<u32>) -> EgressCtx {
        EgressCtx {
            base_url: "https://api.anthropic.com".into(),
            model: model.to_string(),
            auth: crate::AuthKind::XApiKey,
            key: Some("test-key".into()),
            anthropic_version: Some("2023-06-01".into()),
            path_override: None,
            default_max_tokens,
            http: reqwest::Client::new(),
            auth_fallback: None,
        }
    }

    /// Forward to `build_anthropic_request`, so integration tests can call the internal logic.
    pub fn build_anthropic_request_for_test(
        req: &codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<serde_json::Value, ConnError> {
        crate::connector::anthropic::build_anthropic_request(req, ctx)
    }

    /// Run an entire SSE chunk sequence to completion, returning all `ResponseEvent`s.
    /// When `done = true`, automatically calls `finish()` (simulating `[DONE]`).
    /// For integration tests (which cannot access functions inside `#[cfg(test)]`).
    pub fn translate_chat_sse_for_test(
        chunks: &[serde_json::Value],
        done: bool,
    ) -> Result<Vec<codex_api::ResponseEvent>, ConnError> {
        crate::connector::chat_sse::translate_chat_sse(chunks, done)
    }

    /// Run an entire Anthropic Messages SSE event sequence to completion, returning all `ResponseEvent`s.
    /// When `done = true`, automatically calls `finish()` (simulating `message_stop`).
    /// For integration tests (which cannot access functions inside `#[cfg(test)]`).
    pub fn translate_anthropic_sse_for_test(
        events: &[serde_json::Value],
        done: bool,
    ) -> Result<Vec<codex_api::ResponseEvent>, ConnError> {
        crate::connector::anthropic_sse::translate_anthropic_sse(events, done)
    }

    /// Return the chat connector's `SseTranslator` instance (for `run_egress_for_test`).
    pub fn chat_translator() -> Box<dyn crate::SseTranslator> {
        Box::new(crate::connector::chat_sse::ChatSseState::default())
    }

    /// Build minimal valid auth headers (Bearer + test key).
    pub fn dummy_headers() -> reqwest::header::HeaderMap {
        crate::http::build_headers(crate::AuthKind::Bearer, Some("testkey"), None)
            .expect("dummy_headers should not fail")
    }

    /// Forward to `sse::run_egress`, for the `run_test.rs` integration test.
    pub async fn run_egress_for_test(
        url: String,
        headers: reqwest::header::HeaderMap,
        body: serde_json::Value,
        translator: Box<dyn crate::SseTranslator>,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        crate::sse::run_egress(url, headers, body, reqwest::Client::new(), translator).await
    }

    /// A Noop `AuthProvider` that never writes auth headers (for live tests; testkey carries its own auth_key, no fallback needed).
    pub fn noop_auth_provider() -> codex_api::SharedAuthProvider {
        struct Noop;
        impl codex_api::AuthProvider for Noop {
            fn add_auth_headers(&self, _h: &mut reqwest::header::HeaderMap) {}
        }
        std::sync::Arc::new(Noop)
    }
}

pub use config::{
    load_config_from_str, AuthKind, CapType, Config, ConfigError, Connector, ProviderCfg,
};
pub use http::{build_headers, default_path, egress_url, resolve_key, HttpError};
pub use pipeline::{default_plugins, run_transforms, TransformPlugin};
pub use connector::{make_connector, ConnError, Connector as ConnectorTrait, EgressCtx, SseTranslator};

use std::sync::OnceLock;

/// For live tests / standalone runs only: read config from the gitignored testkey.toml, allowing inline auth_key.
pub fn load_testkey_config(path: &std::path::Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    load_config_from_str(&text, true)
}

/// Routing result: matched a taken-over provider.
#[derive(Debug, Clone)]
pub struct Route {
    pub provider_id: String,
    pub cfg: ProviderCfg,
}

/// Main entry point (Task 09 patch call contract; signature fixed verbatim).
///
/// Pipeline: transform layer → assemble `EgressCtx` (with key fallback) → dispatch to the connector.
/// `base_url` is required; returns `ApiError::InvalidRequest` early if missing.
pub async fn run(
    rt: Route,
    mut request: codex_api::ResponsesApiRequest,
    api_auth: codex_api::SharedAuthProvider,
) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
    // ① Transform layer (passthrough in v1)
    let plugins = pipeline::default_plugins();
    pipeline::run_transforms(&plugins, &mut request)
        .map_err(codex_api::ApiError::from)?;

    // ② base_url required check (see § plan Step 6 note)
    let base_url = rt.cfg.base_url.clone().ok_or_else(|| {
        codex_api::ApiError::InvalidRequest {
            message: format!("provider {} missing base_url", rt.provider_id),
        }
    })?;

    // ③ Key resolution
    let key = http::resolve_key(&rt.cfg)
        .map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() })?;

    // ④ Egress model: config override > model in the request
    let model = rt.cfg.model.clone().unwrap_or_else(|| request.model.clone());

    let ctx = connector::EgressCtx {
        base_url,
        model,
        auth: rt.cfg.auth,
        key,
        anthropic_version: rt.cfg.anthropic_version.clone(),
        path_override: rt.cfg.path.clone(),
        default_max_tokens: rt.cfg.default_max_tokens,
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
                Config { enabled: false, providers: Default::default() }
            }),
            Err(_) => Config { enabled: false, providers: Default::default() }, // missing file = disabled
        }
    })
}

fn dirs_config_zmod_path() -> std::path::PathBuf {
    // ~/.codex/config-zmod.toml; CODEX_HOME override takes precedence (consistent with codex's convention)
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")))
        .unwrap_or_else(|| std::path::PathBuf::from(".codex"));
    home.join("config-zmod.toml")
}

/// Global switch: `[llm-switch].enabled`.
pub fn enabled() -> bool {
    loaded().enabled
}

fn route_in(cfg: &Config, model_provider_id: &str) -> Option<Route> {
    if !cfg.enabled { return None; }
    cfg.providers.get(model_provider_id).map(|p| Route {
        provider_id: model_provider_id.to_string(),
        cfg: p.clone(),
    })
}

/// Decide whether to take over based on codex's model_provider_id.
/// Not enabled / no match / responses → None (takes the native Responses branch).
pub fn route(model_provider_id: &str) -> Option<Route> {
    route_in(loaded(), model_provider_id)
}

fn suppress_hosted_tools_in(cfg: &Config, model_provider_id: &str) -> bool {
    route_in(cfg, model_provider_id)
        .is_some_and(|rt| rt.cfg.captype == CapType::Chat)
}

/// For codex-side capability gating: a taken-over provider with `captype = "chat"` (default) requires codex
/// to disable namespace / web_search / image_generation and other hosted tools (the v1 connectors cannot
/// express them, otherwise they hard-fail). `captype = "response"` or not taken over → false (pass native capabilities through).
pub fn suppress_hosted_tools(model_provider_id: &str) -> bool {
    suppress_hosted_tools_in(loaded(), model_provider_id)
}

fn allow_anthropic_web_search_in(cfg: &Config, model_provider_id: &str) -> bool {
    route_in(cfg, model_provider_id)
        .is_some_and(|rt| rt.cfg.connector == Connector::Anthropic)
}

/// For codex-side capability gating: even when `suppress_hosted_tools` is true (captype=chat), as long as
/// this provider uses the anthropic connector, codex is allowed to emit the web_search hosted tool —
/// the anthropic connector translates it into Anthropic's native `web_search_20250305` server tool.
/// Only web_search is allowed; namespace / image_generation stay disabled along with suppress (Anthropic
/// does not support image generation, and namespace function calls cannot be expressed by the v1 connectors).
/// Non-anthropic connectors / not taken over → false (filtering behavior unchanged).
pub fn allow_anthropic_web_search(model_provider_id: &str) -> bool {
    allow_anthropic_web_search_in(loaded(), model_provider_id)
}

fn context_window_in(cfg: &Config, model_provider_id: &str) -> Option<i64> {
    route_in(cfg, model_provider_id).and_then(|rt| rt.cfg.context_window)
}

/// For codex-side model metadata: when a taken-over provider has `context_window` configured, return it so codex
/// uses this value to override the model's context window (bypassing the 272k hard cap of the unknown-model fallback).
/// Not taken over / not configured → None (codex uses its own metadata).
pub fn context_window(model_provider_id: &str) -> Option<i64> {
    context_window_in(loaded(), model_provider_id)
}

fn model_catalog_json_in(cfg: &Config, model_provider_id: &str) -> Option<String> {
    route_in(cfg, model_provider_id).and_then(|rt| rt.cfg.model_catalog_json)
}

/// For codex-side model catalog: when a taken-over provider has `model_catalog_json` configured, return that path so
/// codex uses this table as the model catalog (third-party models appear in the /model list with reasoning effort).
/// Not taken over / not configured → None (codex uses the global built-in / config.toml catalog).
pub fn model_catalog_json(model_provider_id: &str) -> Option<String> {
    model_catalog_json_in(loaded(), model_provider_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_never_routes() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=false\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert!(route_in(&cfg, "x").is_none());
    }
    #[test]
    fn enabled_routes_known_provider() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert!(route_in(&cfg, "x").is_some());
        assert!(route_in(&cfg, "unknown").is_none());
    }
    #[test]
    fn captype_defaults_to_chat_suppresses_hosted_tools() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert_eq!(cfg.providers["x"].captype, CapType::Chat);
        assert!(suppress_hosted_tools_in(&cfg, "x"));
    }
    #[test]
    fn captype_response_passes_through() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\ncaptype=\"response\"\nauth=\"bearer\"\n", false).unwrap();
        assert_eq!(cfg.providers["x"].captype, CapType::Response);
        assert!(!suppress_hosted_tools_in(&cfg, "x"));
    }
    #[test]
    fn unrouted_provider_never_suppresses() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert!(!suppress_hosted_tools_in(&cfg, "unknown"));
    }
    #[test]
    fn anthropic_connector_allows_web_search_even_when_chat() {
        // captype defaults to chat → suppress is true, but the anthropic connector still allows web_search
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"anthropic\"\nauth=\"x-api-key\"\n", false).unwrap();
        assert!(suppress_hosted_tools_in(&cfg, "x"));
        assert!(allow_anthropic_web_search_in(&cfg, "x"));
    }
    #[test]
    fn chat_connector_never_allows_web_search() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert!(!allow_anthropic_web_search_in(&cfg, "x"));
    }
    #[test]
    fn unrouted_provider_never_allows_web_search() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"anthropic\"\nauth=\"x-api-key\"\n", false).unwrap();
        assert!(!allow_anthropic_web_search_in(&cfg, "unknown"));
    }
    #[test]
    fn context_window_read_when_configured() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\ncontext_window=1000000\n", false).unwrap();
        assert_eq!(context_window_in(&cfg, "x"), Some(1_000_000));
    }
    #[test]
    fn context_window_none_when_unset_or_unrouted() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\n", false).unwrap();
        assert_eq!(context_window_in(&cfg, "x"), None);
        assert_eq!(context_window_in(&cfg, "unknown"), None);
    }
    #[test]
    fn model_catalog_json_read_when_configured() {
        let cfg = load_config_from_str("[llm-switch]\nenabled=true\n[llm-switch.providers.x]\nconnector=\"chat\"\nauth=\"bearer\"\nmodel_catalog_json=\"/tmp/x.json\"\n", false).unwrap();
        assert_eq!(model_catalog_json_in(&cfg, "x"), Some("/tmp/x.json".to_string()));
        assert_eq!(model_catalog_json_in(&cfg, "unknown"), None);
    }
}
