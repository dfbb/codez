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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector { Chat, Anthropic }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind { Bearer, XApiKey }

#[derive(Debug, Clone)]
pub struct ProviderCfg {
    pub connector: Connector,
    pub base_url: Option<String>,
    pub auth: AuthKind,
    pub key_env: Option<String>,
    pub auth_key: Option<String>,
    pub path: Option<String>,
    pub model: Option<String>,
    pub anthropic_version: Option<String>,
    pub default_max_tokens: Option<u32>,
    pub prompt_cache: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub providers: HashMap<String, ProviderCfg>,
    pub purpose: HashMap<String, String>,
}

// ---- Raw TOML deserialization layer (private) ----
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
    #[serde(default)]
    purpose: HashMap<String, String>,
}

#[derive(Deserialize)]
struct RawProvider {
    connector: String,
    base_url: Option<String>,
    auth: String,
    key_env: Option<String>,
    auth_key: Option<String>,
    path: Option<String>,
    model: Option<String>,
    anthropic_version: Option<String>,
    default_max_tokens: Option<u32>,
    #[serde(default)]
    prompt_cache: bool,
}

/// Parse config-zmod text. `allow_inline_key=false` is the main runtime path (inline auth_key causes immediate error);
/// `true` is only used when loading from gitignored tests/testkey.toml.
pub fn load_config_from_str(toml_text: &str, allow_inline_key: bool) -> Result<Config, ConfigError> {
    let root: RawRoot = toml::from_str(toml_text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let Some(sw) = root.llm_switch else {
        return Ok(Config { enabled: false, providers: HashMap::new(), purpose: HashMap::new() });
    };
    let mut providers = HashMap::new();
    for (id, raw) in sw.providers {
        // When any provider (including responses) has inline auth_key and allow_inline_key=false,
        // report error during parsing — must check before connector match to prevent responses' continue from bypassing.
        if raw.auth_key.is_some() && !allow_inline_key {
            return Err(ConfigError::InlineAuthKeyForbidden(id.clone()));
        }
        // responses / unknown connector do not enter routable table (use native branch, spec §4.1)
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
        providers.insert(id, ProviderCfg {
            connector,
            base_url: raw.base_url,
            auth,
            key_env: raw.key_env,
            auth_key: raw.auth_key,
            path: raw.path,
            model: raw.model,
            anthropic_version: raw.anthropic_version,
            default_max_tokens: raw.default_max_tokens,
            prompt_cache: raw.prompt_cache,
        });
    }
    Ok(Config { enabled: sw.enabled, providers, purpose: sw.purpose })
}
