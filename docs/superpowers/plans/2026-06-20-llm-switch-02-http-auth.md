# Task 02 — http.rs 出口与鉴权

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints。

**Goal:** 实现 `http.rs`:① endpoint URL 拼接(§4.0a `base_url.trim_end('/') + path`);② 鉴权头整形(§5.3 / §7.2:`bearer` → `Authorization: Bearer`;`x-api-key` → `x-api-key` + `anthropic-version`);③ 原始 key 取得优先级(`key_env` → `auth_key`)。纯函数,易测。

**覆盖 spec:** §4.0a(URL)、§5.3(密钥来源/优先级)、§7.2(鉴权整形)。

**Files:**
- Create: `zmod/llm-switch/src/http.rs`
- Modify: `zmod/llm-switch/src/lib.rs`(加 `mod http;` 与重导出)
- Test: `zmod/llm-switch/tests/http_test.rs`

**Interfaces:**
- Consumes(Task 01):`ProviderCfg`、`AuthKind`、`Connector`。
- Produces:
  - `pub fn egress_url(base_url: &str, connector: Connector, path_override: Option<&str>) -> String`
  - `pub fn default_path(connector: Connector) -> &'static str`
  - `pub fn resolve_key(cfg: &ProviderCfg) -> Result<Option<String>, HttpError>`(读 `key_env` 环境变量或 `auth_key`;都没有时返回 `Ok(None)`,留给 bearer 退路)
  - `pub fn build_headers(auth: AuthKind, key: Option<&str>, anthropic_version: Option<&str>) -> Result<reqwest::header::HeaderMap, HttpError>`
  - `pub enum HttpError { MissingKey, BadHeader(String) }`(`thiserror`)

---

- [ ] **Step 1: 写失败测试**

创建 `zmod/llm-switch/tests/http_test.rs`:

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

- [ ] **Step 2: 运行确认失败**

Run: `cd zmod/llm-switch && cargo test --test http_test`
Expected: 编译失败(符号未定义)。

- [ ] **Step 3: 实现 `http.rs`**

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

/// `base_url.trim_end('/') + path`(§4.0a)。path 缺省由 connector 决定,可被 config 覆盖。
pub fn egress_url(base_url: &str, connector: Connector, path_override: Option<&str>) -> String {
    let path = path_override.unwrap_or_else(|| default_path(connector));
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// 原始 key 优先级:key_env(读环境变量)→ auth_key(内联,仅 testkey)→ None(留给 bearer 退路)。
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

/// 按 auth 形态整形鉴权头。原始 key 由连接器自取,不依赖 codex add_auth_headers。
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

- [ ] **Step 4: 在 `lib.rs` 挂模块并重导出**

在 `lib.rs` 顶部 `mod config;` 旁加:

```rust
mod http;
pub use http::{build_headers, default_path, egress_url, resolve_key, HttpError};
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cd zmod/llm-switch && cargo test --test http_test`
Expected: 7 个测试全 PASS。

- [ ] **Step 6: 提交**

```bash
git add zmod/llm-switch/src/http.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/http_test.rs
git commit -m "feat(llm-switch): http egress url + auth header shaping"
```
