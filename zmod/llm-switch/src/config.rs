use std::collections::HashMap;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config-zmod parse error: {0}")]
    Parse(String),
    #[error("provider '{0}': inline `auth_key` is forbidden in ~/.codex/config-zmod.toml (only allowed in gitignored tests/testkey.toml)")]
    InlineAuthKeyForbidden(String),
    #[error("provider '{0}': unknown connector '{1}'")]
    UnknownConnector(String, String),
    #[error("provider '{0}': unknown auth '{1}'")]
    UnknownAuth(String, String),
    #[error("provider '{0}': unknown captype '{1}' (expected \"chat\" or \"response\")")]
    UnknownCapType(String, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector { Chat, Anthropic }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind { Bearer, XApiKey }

/// 能力屏蔽模式：被接管的 provider 是否要求 codex 关闭命名空间 / web_search /
/// image_generation 等托管工具（这些在 v1 chat/anthropic 连接器中无法表达，会硬失败）。
/// - `Chat`（缺省）：屏蔽所有托管工具能力（标准第三方 Chat/Anthropic 后端）。
/// - `Response`：透传 codex 原生能力（出口仍是 Responses 协议，能处理托管工具）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapType { Chat, Response }

#[derive(Debug, Clone)]
pub struct ProviderCfg {
    pub connector: Connector,
    pub captype: CapType,
    pub base_url: Option<String>,
    pub auth: AuthKind,
    pub key_env: Option<String>,
    pub auth_key: Option<String>,
    pub path: Option<String>,
    pub model: Option<String>,
    pub anthropic_version: Option<String>,
    pub default_max_tokens: Option<u32>,
    /// 覆盖 codex 对该 provider 模型的上下文窗口（token）。被接管的第三方模型
    /// 通常不在 codex 内置表里、走 fallback（硬上限 272k），配此值可绕过。
    pub context_window: Option<i64>,
    /// 该 provider 专属的模型 catalog JSON 路径。用此 provider 时，codex 用这张表
    /// 作为模型目录（使第三方模型进 /model 列表、带推理强度），而非全局内置表。
    pub model_catalog_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub providers: HashMap<String, ProviderCfg>,
}

// ---- 原始 TOML 反序列化层(私有) ----
#[derive(Deserialize)]
struct RawRoot {
    #[serde(rename = "llm-switch")]
    llm_switch: Option<RawSwitch>,
}

#[derive(Deserialize)]
struct RawSwitch {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    providers: HashMap<String, RawProvider>,
}

#[derive(Deserialize)]
struct RawProvider {
    connector: String,
    #[serde(default)]
    captype: Option<String>,
    base_url: Option<String>,
    auth: String,
    key_env: Option<String>,
    auth_key: Option<String>,
    path: Option<String>,
    model: Option<String>,
    anthropic_version: Option<String>,
    default_max_tokens: Option<u32>,
    context_window: Option<i64>,
    model_catalog_json: Option<String>,
}

/// 解析 config-zmod 文本。`allow_inline_key=false` 为运行时主路径(出现 auth_key 直接报错);
/// `true` 仅供从 gitignored tests/testkey.toml 加载时使用。
pub fn load_config_from_str(toml_text: &str, allow_inline_key: bool) -> Result<Config, ConfigError> {
    let root: RawRoot = toml::from_str(toml_text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let Some(sw) = root.llm_switch else {
        return Ok(Config { enabled: false, providers: HashMap::new() });
    };
    let mut providers = HashMap::new();
    for (id, raw) in sw.providers {
        // 任何 provider(含 responses)出现内联 auth_key 且 allow_inline_key=false 时,
        // 都在解析期报错——必须在 connector match 之前检测,避免 responses 的 continue 绕过。
        if raw.auth_key.is_some() && !allow_inline_key {
            return Err(ConfigError::InlineAuthKeyForbidden(id.clone()));
        }
        // responses / 未知 connector 不进可路由表(走原生分支,spec §4.1)
        let connector = match raw.connector.as_str() {
            "chat" => Connector::Chat,
            "anthropic" => Connector::Anthropic,
            "responses" => continue,
            other => return Err(ConfigError::UnknownConnector(id, other.to_string())),
        };
        let auth = match raw.auth.as_str() {
            "bearer" => AuthKind::Bearer,
            "x-api-key" => AuthKind::XApiKey,
            other => return Err(ConfigError::UnknownAuth(id.clone(), other.to_string())),
        };
        // captype 缺省为 "chat"(屏蔽托管工具);"response" 透传 codex 原生能力。
        let captype = match raw.captype.as_deref() {
            None | Some("chat") => CapType::Chat,
            Some("response") => CapType::Response,
            Some(other) => return Err(ConfigError::UnknownCapType(id.clone(), other.to_string())),
        };
        providers.insert(id, ProviderCfg {
            connector,
            captype,
            base_url: raw.base_url,
            auth,
            key_env: raw.key_env,
            auth_key: raw.auth_key,
            path: raw.path,
            model: raw.model,
            anthropic_version: raw.anthropic_version,
            default_max_tokens: raw.default_max_tokens,
            context_window: raw.context_window,
            model_catalog_json: raw.model_catalog_json,
        });
    }
    Ok(Config { enabled: sw.enabled, providers })
}
