# Task 02 — http.rs Egress and Authentication

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the Global Constraints in the [master index](2026-06-20-llm-switch-00-index.md).

**Goal:** Implement `http.rs`: ① endpoint URL assembly (§4.0a `base_url.trim_end('/') + path`); ② auth header shaping (§5.3 / §7.2: `bearer` → `Authorization: Bearer`; `x-api-key` → `x-api-key` + `anthropic-version`); ③ raw key resolution priority (`key_env` → `auth_key`). Pure functions, easy to test.

**Spec coverage:** §4.0a (URL), §5.3 (key source/priority), §7.2 (auth shaping).

**Files:**
- Create: `zmod/llm-switch/src/http.rs`
- Modify: `zmod/llm-switch/src/lib.rs` (add `mod http;` and re-exports)
- Test: `zmod/llm-switch/tests/http_test.rs`

**Interfaces:**
- Consumes (Task 01): `ProviderCfg`, `AuthKind`, `Connector`.
- Produces:
  - `pub fn egress_url(base_url: &str, connector: Connector, path_override: Option<&str>) -> String`
  - `pub fn default_path(connector: Connector) -> &'static str`
  - `pub fn resolve_key(cfg: &ProviderCfg) -> Result<Option<String>, HttpError>` (reads the `key_env` environment variable or `auth_key`; returns `Ok(None)` when neither is present, leaving the bearer fallback open)
  - `pub fn build_headers(auth: AuthKind, key: Option<&str>, anthropic_version: Option<&str>) -> Result<reqwest::header::HeaderMap, HttpError>`
  - `pub enum HttpError { MissingKey, BadHeader(String) }` (`thiserror`)

---

- [ ] **Step 1: Write failing tests**

Create `zmod/llm-switch/tests/http_test.rs`:

```rust
use codez_llm_switch::{build_headers, default_path, egress_url, AuthKind, Connector};

#[test]
fn url_chat_default() {
    assert_eq!(
        egress_url("https://api.deepseek.com/v1", Connector::Chat, None),
        "https://api.deepseek.com/v1/chat/completions"
    );
}

#[test]
fn url_anthropic_default_and_trims_slash() {
    assert_eq!(
        egress_url("https://api.anthropic.com/", Connector::Anthropic, None),
        "https://api.anthropic.com/v1/messages"
    );
}

#[test]
fn url_path_override() {
    assert_eq!(
        egress_url("https://gw.example.com/api", Connector::Anthropic, Some("/custom/messages")),
        "https://gw.example.com/api/custom/messages"
    );
}

#[test]
fn default_paths() {
    assert_eq!(default_path(Connector::Chat), "/chat/completions");
    assert_eq!(default_path(Connector::Anthropic), "/v1/messages");
}

#[test]
fn headers_bearer() {
    let h = build_headers(AuthKind::Bearer, Some("sk-abc"), None).unwrap();
    assert_eq!(h.get("authorization").unwrap(), "Bearer sk-abc");
    assert!(h.get("x-api-key").is_none());
}

#[test]
fn headers_xapikey_with_version() {
    let h = build_headers(AuthKind::XApiKey, Some("sk-xyz"), Some("2023-06-01")).unwrap();
    assert_eq!(h.get("x-api-key").unwrap(), "sk-xyz");
    assert_eq!(h.get("anthropic-version").unwrap(), "2023-06-01");
    assert!(h.get("authorization").is_none());
}

#[test]
fn headers_xapikey_missing_key_errors() {
    assert!(build_headers(AuthKind::XApiKey, None, Some("2023-06-01")).is_err());
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cd zmod/llm-switch && cargo test --test http_test`
Expected: compile failure (undefined symbols).

- [ ] **Step 3: Implement `http.rs`**

```rust
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

pub fn default_path(connector: Connector) -> &'static str {
    match connector {
        Connector::Chat => "/chat/completions",
        Connector::Anthropic => "/v1/messages",
    }
}

/// `base_url.trim_end('/') + path` (§4.0a). The default path is determined by the connector and can be overridden by config.
pub fn egress_url(base_url: &str, connector: Connector, path_override: Option<&str>) -> String {
    let path = path_override.unwrap_or_else(|| default_path(connector));
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// Raw key priority: key_env (read environment variable) → auth_key (inline, testkey only) → None (leaves the bearer fallback open).
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

/// Shape the auth headers according to the auth kind. The connector fetches the raw key itself; it does not rely on codex add_auth_headers.
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
            let val = HeaderValue::from_str(key).map_err(|e| HttpError::BadHeader(e.to_string()))?;
            h.insert(HeaderName::from_static("x-api-key"), val);
            let ver = anthropic_version.unwrap_or("2023-06-01");
            let vv = HeaderValue::from_str(ver).map_err(|e| HttpError::BadHeader(e.to_string()))?;
            h.insert(HeaderName::from_static("anthropic-version"), vv);
        }
    }
    h.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(h)
}
```

- [ ] **Step 4: Mount the module in `lib.rs` and re-export**

In `lib.rs`, next to `mod config;` at the top, add:

```rust
mod http;
pub use http::{build_headers, default_path, egress_url, resolve_key, HttpError};
```

- [ ] **Step 5: Run tests and confirm they pass**

Run: `cd zmod/llm-switch && cargo test --test http_test`
Expected: all 7 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add zmod/llm-switch/src/http.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/http_test.rs
git commit -m "feat(llm-switch): http egress url + auth header shaping"
```
