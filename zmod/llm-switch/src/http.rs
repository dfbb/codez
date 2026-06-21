use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use thiserror::Error;

use crate::config::{AuthKind, Connector, ProviderCfg};

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("missing API key (set key_env or auth_key)")]
    MissingKey,
    #[error("invalid header value: {0}")]
    BadHeader(String),
}

/// Default outbound path. Chat → `/chat/completions`; Anthropic → `/v1/messages`.
pub fn default_path(connector: Connector) -> &'static str {
    match connector {
        Connector::Chat => "/chat/completions",
        Connector::Anthropic => "/v1/messages",
    }
}

/// `base_url.trim_end('/') + path` (§4.0a).
/// The default path is determined by the connector and can be overridden by path_override.
pub fn egress_url(base_url: &str, connector: Connector, path_override: Option<&str>) -> String {
    let path = path_override.unwrap_or_else(|| default_path(connector));
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// Raw key precedence: key_env (read from environment) → auth_key (inline, testkey only) → None (left to the bearer fallback).
pub fn resolve_key(cfg: &ProviderCfg) -> Result<Option<String>, HttpError> {
    if let Some(env_name) = &cfg.key_env {
        if let Ok(v) = std::env::var(env_name) {
            if !v.is_empty() {
                return Ok(Some(v));
            }
        }
    }
    if let Some(k) = &cfg.auth_key {
        return Ok(Some(k.clone()));
    }
    Ok(None)
}

/// Shape the auth headers according to the auth kind (§7.2).
/// - `Bearer` → `Authorization: Bearer <key>`
/// - `XApiKey` → `x-api-key: <key>` + `anthropic-version: <ver>`
/// Also injects `Content-Type: application/json`.
pub fn build_headers(
    auth: AuthKind,
    key: Option<&str>,
    anthropic_version: Option<&str>,
) -> Result<HeaderMap, HttpError> {
    let mut h = HeaderMap::new();
    match auth {
        AuthKind::Bearer => {
            let key = key.ok_or(HttpError::MissingKey)?;
            let val = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|e| HttpError::BadHeader(e.to_string()))?;
            h.insert(reqwest::header::AUTHORIZATION, val);
        }
        AuthKind::XApiKey => {
            let key = key.ok_or(HttpError::MissingKey)?;
            let val = HeaderValue::from_str(key)
                .map_err(|e| HttpError::BadHeader(e.to_string()))?;
            h.insert(
                HeaderName::from_static("x-api-key"),
                val,
            );
            let ver = anthropic_version.unwrap_or("2023-06-01");
            let vv = HeaderValue::from_str(ver)
                .map_err(|e| HttpError::BadHeader(e.to_string()))?;
            h.insert(HeaderName::from_static("anthropic-version"), vv);
        }
    }
    h.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(h)
}
