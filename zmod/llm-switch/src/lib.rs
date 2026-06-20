mod config;
mod http;
mod pipeline;
mod transform;
mod connector;

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
            http: reqwest::Client::new(),
        }
    }

    /// 转发 `build_chat_request`，供集成测试调用内部逻辑。
    pub fn build_chat_request_for_test(
        req: &codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<serde_json::Value, ConnError> {
        crate::connector::chat::build_chat_request(req, ctx)
    }
}

pub use config::{
    load_config_from_str, AuthKind, Config, ConfigError, Connector, ProviderCfg,
};
pub use http::{build_headers, default_path, egress_url, resolve_key, HttpError};
pub use pipeline::{default_plugins, run_transforms, TransformPlugin};
pub use connector::{make_connector, ConnError, Connector as ConnectorTrait, EgressCtx};

use std::sync::OnceLock;

/// 路由结果:命中某个被接管的 provider。
#[derive(Debug, Clone)]
pub struct Route {
    pub provider_id: String,
    pub cfg: ProviderCfg,
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
