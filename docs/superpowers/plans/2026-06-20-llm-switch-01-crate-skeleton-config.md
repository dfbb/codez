# Task 01 — Crate Skeleton and Config

> **For agentic workers:** REQUIRED SUB-SKILL: execute with superpowers:subagent-driven-development or superpowers:executing-plans. Track steps with `- [ ]`. First read the Global Constraints in the [master index](2026-06-20-llm-switch-00-index.md).

**Goal:** Build the `zmod/llm-switch` crate (with a path dependency pointing back into codex-rs), implementing `config.rs` to read the `[llm-switch]` section of `~/.codex/config-zmod.toml`, plus the `enabled()` / `route()` routing decisions and the `Route` type in `lib.rs`. By the end of this task: the crate passes `cargo test -p codez-llm-switch` inside the codex-rs workspace, with test coverage for config parsing and the `auth_key` rejection rule.

> **For the build approach, see the index's "Development-time build and test" decision section (2026-06-20) + CLAUDE.md case B "Development-time testing":** this crate has a reverse dependency on codex-api and is **not compiled standalone**; during development, use the symlink `codex-rs/llm-switch -> ../zmod/llm-switch` plus adding `"llm-switch"` to the members list in `codex-rs/Cargo.toml`, making this crate a real member of the codex-rs workspace so you can run `cargo test -p codez-llm-switch` within it (full support for dev-deps + integration tests). The symlink and members changes are dev-only scaffolding; they are **not committed into codex-rs and not part of any patch**.

**Spec coverage:** §3 (module layout), §5.2 / §5.3 (config-zmod schema, key sources, `auth_key` rejection), §6.1 (build integration case B), §1 (naming).

**Files:**
- Create: `zmod/llm-switch/Cargo.toml`
- Create: `zmod/llm-switch/src/lib.rs`
- Create: `zmod/llm-switch/src/config.rs`
- Create: `zmod/llm-switch/tests/config_test.rs`
- Modify: repo-root `.gitignore` (already has `/codex-rs/llm-switch`, ignoring the dev symlink)
- dev-only scaffolding (already set up by the controller; not committed, not part of any patch): the symlink `codex-rs/llm-switch` and the `"llm-switch"` line in the members list of `codex-rs/Cargo.toml`

**Interfaces:**
- Produces (later tasks depend on these):
  - `pub fn enabled() -> bool`
  - `pub fn route(model_provider_id: &str) -> Option<Route>`
  - `pub struct Route { pub provider_id: String, pub cfg: ProviderCfg }`
  - `pub enum Connector { Chat, Anthropic }` (`responses`/unknown → `route()` returns `None`)
  - `pub struct ProviderCfg { pub connector: Connector, pub base_url: Option<String>, pub auth: AuthKind, pub key_env: Option<String>, pub auth_key: Option<String>, pub path: Option<String>, pub model: Option<String>, pub anthropic_version: Option<String>, pub default_max_tokens: Option<u32> }`
  - `pub enum AuthKind { Bearer, XApiKey }`
  - `pub fn load_config_from_str(toml: &str, allow_inline_key: bool) -> Result<Config, ConfigError>` (shared by tests and runtime; runtime uses `allow_inline_key=false`, the testkey path uses `true`)
  - `pub struct Config { pub enabled: bool, pub providers: HashMap<String, ProviderCfg> }`
- Consumes: none (first task).

---

- [ ] **Step 1: Create the directory and `Cargo.toml`**

Create `zmod/llm-switch/Cargo.toml`. Note: **do not write `[workspace]`**; point the path dependency back into codex-rs; do not use `workspace = true`.

```toml
[package]
name = "codez-llm-switch"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
name = "codez_llm_switch"
path = "src/lib.rs"

[dependencies]
codex-api = { path = "../../codex-rs/codex-api" }
codex-protocol = { path = "../../codex-rs/protocol" }
tokio = { version = "1", features = ["rt", "macros", "sync"] }
reqwest = { version = "0.12", features = ["stream", "json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.9"
thiserror = "2"
tracing = "0.1"

[dev-dependencies]
tokio = { version = "1", features = ["rt", "macros", "sync", "test-util"] }
```

> Note: the crate names for `codex-api` / `codex-protocol` have been verified as `codex-api` and `codex-protocol`, with paths `../../codex-rs/codex-api` and `../../codex-rs/protocol`. Align the reqwest / tokio / toml versions with the codex-rs workspace (`grep -E 'reqwest|^tokio|^toml' codex-rs/Cargo.toml` — e.g. toml is actually `0.9`) to avoid recompilation. Keep the two codex-* path dependencies **active, not commented out**. Declare `[dev-dependencies]` normally — it works under the symlink-member mode (see below).

- [ ] **Step 1b: Confirm the dev symlink scaffolding is in place (already set up by the controller; do not modify codex-rs source)**

The symlink scaffolding that hooks this crate into the codex-rs workspace for development-time testing **has already been set up by the controller** (see the index's "Development-time build and test" / CLAUDE.md case B "Development-time testing"):

```bash
codex-rs/llm-switch -> ../zmod/llm-switch      # symlink (created, gitignored)
codex-rs/Cargo.toml  members contains "llm-switch"   # added (uncommitted dirty)
```

You only need to **confirm** it is in place; do not recreate it or modify codex-rs source:
```bash
ls -l codex-rs/llm-switch && grep -n '"llm-switch"' codex-rs/Cargo.toml
```
If it is missing (e.g. a recent `git reset --hard` removed the members line), rebuild it with the two commands above: `ln -s ../zmod/llm-switch codex-rs/llm-switch` (if the symlink is gone) + append `"llm-switch",` to the end of the members list.

> **Discipline**: the symlink and members line are dev-only scaffolding; **this task's commit must not include `codex-rs/**`**. The `codex-rs/Cargo.toml` (members line) and the build-generated `codex-rs/Cargo.lock` stay dirty throughout Tasks 01–08; **do not `git checkout` to revert them**, and **do not put them into any patch**. Production wiring is handled by the Task 09 patch (core path dependency + client.rs) and is independent of the symlink.

- [ ] **Step 2: Write a failing test (config parsing)**

Create `zmod/llm-switch/tests/config_test.rs`:

```rust
use codez_llm_switch::{load_config_from_str, AuthKind, Connector};

const SAMPLE: &str = r#"
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"

[llm-switch.providers.claude]
connector         = "anthropic"
base_url          = "https://api.anthropic.com"
auth              = "x-api-key"
key_env           = "ANTHROPIC_API_KEY"
anthropic_version = "2023-06-01"
default_max_tokens = 8192
"#;

#[test]
fn parses_providers() {
    let cfg = load_config_from_str(SAMPLE, false).expect("parse ok");
    assert!(cfg.enabled);
    let ds = cfg.providers.get("deepseek").expect("deepseek present");
    assert!(matches!(ds.connector, Connector::Chat));
    assert!(matches!(ds.auth, AuthKind::Bearer));
    assert_eq!(ds.key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
    let cl = cfg.providers.get("claude").expect("claude present");
    assert!(matches!(cl.connector, Connector::Anthropic));
    assert!(matches!(cl.auth, AuthKind::XApiKey));
    assert_eq!(cl.default_max_tokens, Some(8192));
    assert_eq!(cl.anthropic_version.as_deref(), Some("2023-06-01"));
}

#[test]
fn rejects_inline_auth_key_in_prod() {
    let toml = r#"
[llm-switch]
enabled = true
[llm-switch.providers.deepseek]
connector = "chat"
auth = "bearer"
auth_key = "sk-secret"
"#;
    // Runtime path with allow_inline_key=false: must raise a config error and refuse to start
    let err = load_config_from_str(toml, false).unwrap_err();
    assert!(format!("{err}").contains("auth_key"), "err should mention auth_key: {err}");
    // testkey path with allow_inline_key=true: accepts
    let ok = load_config_from_str(toml, true).expect("testkey path accepts inline key");
    assert_eq!(
        ok.providers.get("deepseek").unwrap().auth_key.as_deref(),
        Some("sk-secret")
    );
}

#[test]
fn responses_connector_is_not_routable() {
    let toml = r#"
[llm-switch]
enabled = true
[llm-switch.providers.openai]
connector = "responses"
auth = "bearer"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    // responses does not enter zmod: parsing allows it, but route() does not return it (see the lib.rs test in Step 6)
    assert!(cfg.providers.get("openai").is_none(), "responses provider dropped from routable map");
}

#[test]
fn missing_section_means_disabled() {
    let cfg = load_config_from_str("[other]\nx = 1\n", false).expect("parse ok");
    assert!(!cfg.enabled);
    assert!(cfg.providers.is_empty());
}
```

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cd codex-rs && cargo test -p codez-llm-switch --test config_test`
Expected: compilation failure (`load_config_from_str` etc. are undefined).

- [ ] **Step 4: Implement `config.rs`**

Create `zmod/llm-switch/src/config.rs`:

```rust
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
    base_url: Option<String>,
    auth: String,
    key_env: Option<String>,
    auth_key: Option<String>,
    path: Option<String>,
    model: Option<String>,
    anthropic_version: Option<String>,
    default_max_tokens: Option<u32>,
}

/// Parse config-zmod text. `allow_inline_key=false` is the main runtime path (errors out
/// immediately if auth_key appears); `true` is only for loading from a gitignored tests/testkey.toml.
pub fn load_config_from_str(toml_text: &str, allow_inline_key: bool) -> Result<Config, ConfigError> {
    let root: RawRoot = toml::from_str(toml_text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let Some(sw) = root.llm_switch else {
        return Ok(Config { enabled: false, providers: HashMap::new() });
    };
    let mut providers = HashMap::new();
    for (id, raw) in sw.providers {
        // responses / unknown connectors do not enter the routable map (they take the native branch, spec §4.1)
        let connector = match raw.connector.as_str() {
            "chat" => Connector::Chat,
            "anthropic" => Connector::Anthropic,
            "responses" => continue,
            other => return Err(ConfigError::UnknownConnector(id.clone(), other.to_string())),
        };
        if raw.auth_key.is_some() && !allow_inline_key {
            return Err(ConfigError::InlineAuthKeyForbidden(id.clone()));
        }
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
        });
    }
    Ok(Config { enabled: sw.enabled, providers })
}
```

- [ ] **Step 5: Implement the routing part of `lib.rs`**

Create `zmod/llm-switch/src/lib.rs` (this task only places the config re-exports + `enabled`/`route`/`Route`; `run` is deferred to Task 08 and is not declared here yet, to avoid half-finished signatures):

```rust
mod config;

pub use config::{
    load_config_from_str, AuthKind, Config, ConfigError, Connector, ProviderCfg,
};

use std::sync::OnceLock;

/// Routing result: a hit on some taken-over provider.
#[derive(Debug, Clone)]
pub struct Route {
    pub provider_id: String,
    pub cfg: ProviderCfg,
}

/// Process-level config cache. At runtime, read once from ~/.codex/config-zmod.toml.
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
    // ~/.codex/config-zmod.toml; CODEX_HOME override takes priority (consistent with the codex convention; verify its env var name before running)
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

/// Decide whether to take over based on codex's model_provider_id.
/// Disabled / no match / responses → None (takes the native Responses branch).
pub fn route(model_provider_id: &str) -> Option<Route> {
    let cfg = loaded();
    if !cfg.enabled {
        return None;
    }
    cfg.providers.get(model_provider_id).map(|p| Route {
        provider_id: model_provider_id.to_string(),
        cfg: p.clone(),
    })
}
```

> Note: before running, verify the `CODEX_HOME` env var name used in `dirs_config_zmod_path` with `grep -rn "CODEX_HOME" codex-rs/core/src/config`, to stay consistent with how codex locates its main config.

- [ ] **Step 6: Add unit tests for `route()`**

Append to the end of `zmod/llm-switch/src/lib.rs` (build a `Config` directly with `load_config_from_str` to avoid depending on a real file; `route` uses the global cache and is awkward to test, so test the pure logic helper instead). Refactor: extract `route`'s pure logic into `fn route_in(cfg: &Config, id: &str) -> Option<Route>`, with `route()` calling it; the tests target `route_in`:

```rust
fn route_in(cfg: &Config, model_provider_id: &str) -> Option<Route> {
    if !cfg.enabled { return None; }
    cfg.providers.get(model_provider_id).map(|p| Route {
        provider_id: model_provider_id.to_string(),
        cfg: p.clone(),
    })
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
```

(Change the body of `route()` to `route_in(loaded(), model_provider_id)`.)

- [ ] **Step 7: Run the tests and confirm they pass**

Run: `cd codex-rs && cargo test -p codez-llm-switch`
Expected: all 4 in `config_test` + 2 in lib PASS.

> Because the Step 1b symlink makes this crate a workspace member, this command compiles it inside the codex-rs workspace (integration tests + dev-deps fully available), and codex-api/codex-protocol share the workspace lock/target. `config_test.rs` is a true integration test (under `tests/`), not an in-lib unit test. **If it's a brand-new target, the first compile will pull up the codex-api dependency tree and may take several minutes**; wait patiently with a long enough timeout (you can warm it up first with `cargo build -p codez-llm-switch`). If it genuinely fails due to a dependency conflict (not just slowness), stop and report the exact cargo error; do not work around it by commenting out dependencies.

- [ ] **Step 8: Commit (commit only the crate and codez's own files)**

```bash
git add zmod/llm-switch/Cargo.toml zmod/llm-switch/src/lib.rs zmod/llm-switch/src/config.rs zmod/llm-switch/tests/config_test.rs
git commit -m "feat(llm-switch): crate skeleton + config-zmod parsing and routing"
```

> (The `.gitignore` symlink-ignore line was already committed earlier by the controller and is not included in this task.)

> **Do not** `git add codex-rs/`. The dirty changes to `codex-rs/Cargo.toml` (the extra `"llm-switch"` members line) and the build-generated `codex-rs/Cargo.lock` are dev-only scaffolding; leave them in the working tree, out of any commit. The symlink `codex-rs/llm-switch` is already ignored by `.gitignore`. After committing, `git status` should still show `codex-rs/Cargo.toml` and `codex-rs/Cargo.lock` as modified — this is the expected state.
