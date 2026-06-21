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

/// Capability-masking mode: whether a taken-over provider requires codex to disable
/// namespace / web_search / image_generation and other hosted tools (these cannot be
/// expressed by the v1 chat/anthropic connectors and would hard-fail).
/// - `Chat` (default): mask all hosted-tool capabilities (standard third-party Chat/Anthropic backends).
/// - `Response`: pass codex native capabilities through (the egress is still the Responses protocol, which can handle hosted tools).
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
    /// Override codex's context window (tokens) for this provider's models. Third-party models
    /// taken over here are usually absent from codex's built-in table and fall back (hard cap 272k);
    /// set this value to bypass that.
    pub context_window: Option<i64>,
    /// Path to a model catalog JSON specific to this provider. When using this provider, codex uses
    /// this table as the model catalog (so third-party models appear in the /model list with reasoning effort)
    /// instead of the global built-in table.
    pub model_catalog_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub providers: HashMap<String, ProviderCfg>,
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

/// Parse config-zmod text. `allow_inline_key=false` is the runtime main path (any auth_key errors out immediately);
/// `true` is only used when loading from the gitignored tests/testkey.toml.
pub fn load_config_from_str(toml_text: &str, allow_inline_key: bool) -> Result<Config, ConfigError> {
    let root: RawRoot = toml::from_str(toml_text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let Some(sw) = root.llm_switch else {
        return Ok(Config { enabled: false, providers: HashMap::new() });
    };
    let mut providers = HashMap::new();
    for (id, raw) in sw.providers {
        // If any provider (including responses) has an inline auth_key while allow_inline_key=false,
        // error out at parse time — this must be checked before the connector match, so responses' `continue` cannot bypass it.
        if raw.auth_key.is_some() && !allow_inline_key {
            return Err(ConfigError::InlineAuthKeyForbidden(id.clone()));
        }
        // responses / unknown connectors do not enter the routable map (they take the native branch, spec §4.1)
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
        // captype defaults to "chat" (masks hosted tools); "response" passes codex native capabilities through.
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
