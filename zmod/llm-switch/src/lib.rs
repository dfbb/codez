mod config;
mod http;
mod pipeline;
mod transform;
mod connector;
mod sse;

/// 测试辅助模块（集成测试入口；不进入正式公共 API）。
#[doc(hidden)]
pub mod testing {
    use crate::connector::{ConnError, EgressCtx};

    /// 构造最小 `ResponsesApiRequest` 样本（供各集成测试复用）。
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

    /// 构造最小 `EgressCtx`（供各集成测试复用）。
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

    /// 转发 `build_chat_request`，供集成测试调用内部逻辑。
    pub fn build_chat_request_for_test(
        req: &codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<serde_json::Value, ConnError> {
        crate::connector::chat::build_chat_request(req, ctx)
    }

    /// 构造针对 anthropic connector 的最小 `EgressCtx`。
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

    /// 转发 `build_anthropic_request`，供集成测试调用内部逻辑。
    pub fn build_anthropic_request_for_test(
        req: &codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<serde_json::Value, ConnError> {
        crate::connector::anthropic::build_anthropic_request(req, ctx)
    }

    /// 把整段 SSE chunk 序列跑完，返回所有 `ResponseEvent`。
    /// `done = true` 时自动调用 `finish()`（模拟 `[DONE]`）。
    /// 供集成测试使用（集成测试访问不到 `#[cfg(test)]` 内的函数）。
    pub fn translate_chat_sse_for_test(
        chunks: &[serde_json::Value],
        done: bool,
    ) -> Result<Vec<codex_api::ResponseEvent>, ConnError> {
        crate::connector::chat_sse::translate_chat_sse(chunks, done)
    }

    /// 把整段 Anthropic Messages SSE 事件序列跑完，返回所有 `ResponseEvent`。
    /// `done = true` 时自动调用 `finish()`（模拟 `message_stop`）。
    /// 供集成测试使用（集成测试访问不到 `#[cfg(test)]` 内的函数）。
    pub fn translate_anthropic_sse_for_test(
        events: &[serde_json::Value],
        done: bool,
    ) -> Result<Vec<codex_api::ResponseEvent>, ConnError> {
        crate::connector::anthropic_sse::translate_anthropic_sse(events, done)
    }

    /// 返回 chat 连接器的 `SseTranslator` 实例（供 `run_egress_for_test` 使用）。
    pub fn chat_translator() -> Box<dyn crate::SseTranslator> {
        Box::new(crate::connector::chat_sse::ChatSseState::default())
    }

    /// 构造最小合法鉴权头（Bearer + 测试密钥）。
    pub fn dummy_headers() -> reqwest::header::HeaderMap {
        crate::http::build_headers(crate::AuthKind::Bearer, Some("testkey"), None)
            .expect("dummy_headers should not fail")
    }

    /// 转发 `sse::run_egress`，供 `run_test.rs` 集成测试使用。
    pub async fn run_egress_for_test(
        url: String,
        headers: reqwest::header::HeaderMap,
        body: serde_json::Value,
        translator: Box<dyn crate::SseTranslator>,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        crate::sse::run_egress(url, headers, body, reqwest::Client::new(), translator).await
    }

    /// 永不写鉴权头的 Noop `AuthProvider`（供实跑测试使用；testkey 自带 auth_key，无需退路）。
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
pub use pipeline::{default_plugins, run_transforms, TransformPlugin};
pub use connector::{make_connector, ConnError, Connector as ConnectorTrait, EgressCtx, SseTranslator};

use std::sync::OnceLock;

/// 仅供实跑测试 / 独立运行：从 gitignored testkey.toml 读配置，允许内联 auth_key。
pub fn load_testkey_config(path: &std::path::Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    load_config_from_str(&text, true)
}

/// 路由结果:命中某个被接管的 provider。
#[derive(Debug, Clone)]
pub struct Route {
    pub provider_id: String,
    pub cfg: ProviderCfg,
}

/// 主入口（Task 09 patch 调用契约，签名逐字固定）。
///
/// 流水线：变换层 → 组装 `EgressCtx`（含密钥退路）→ 派发连接器。
/// `base_url` 必填；缺失时早返回 `ApiError::InvalidRequest`。
pub async fn run(
    rt: Route,
    mut request: codex_api::ResponsesApiRequest,
    api_auth: codex_api::SharedAuthProvider,
) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
    // ① 变换层（v1 直通）
    let plugins = pipeline::default_plugins();
    pipeline::run_transforms(&plugins, &mut request)
        .map_err(codex_api::ApiError::from)?;

    // ② base_url 必填校验（§ plan Step 6 注）
    let base_url = rt.cfg.base_url.clone().ok_or_else(|| {
        codex_api::ApiError::InvalidRequest {
            message: format!("provider {} missing base_url", rt.provider_id),
        }
    })?;

    // ③ 密钥解析
    let key = http::resolve_key(&rt.cfg)
        .map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() })?;

    // ④ 出口模型：config 覆盖 > 请求里的 model
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

/// 进程级配置缓存。运行时从 ~/.codex/config-zmod.toml 读一次。
fn loaded() -> &'static Config {
    static CACHE: OnceLock<Config> = OnceLock::new();
    CACHE.get_or_init(|| {
        let path = dirs_config_zmod_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => load_config_from_str(&text, false).unwrap_or_else(|e| {
                tracing::warn!("llm-switch disabled: bad config-zmod.toml: {e}");
                Config { enabled: false, providers: Default::default() }
            }),
            Err(_) => Config { enabled: false, providers: Default::default() }, // 缺文件 = 关闭
        }
    })
}

fn dirs_config_zmod_path() -> std::path::PathBuf {
    // ~/.codex/config-zmod.toml;CODEX_HOME 覆盖优先(与 codex 约定一致)
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")))
        .unwrap_or_else(|| std::path::PathBuf::from(".codex"));
    home.join("config-zmod.toml")
}

/// 全局开关:`[llm-switch].enabled`。
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

/// 按 codex 的 model_provider_id 判定是否接管。
/// 未启用 / 未命中 / responses → None(走原生 Responses 分支)。
pub fn route(model_provider_id: &str) -> Option<Route> {
    route_in(loaded(), model_provider_id)
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
}
