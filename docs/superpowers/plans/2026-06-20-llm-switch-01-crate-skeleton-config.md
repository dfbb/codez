# Task 01 — crate 骨架与配置

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development 或 superpowers:executing-plans 执行。步骤用 `- [ ]` 追踪。先读 [总索引](2026-06-20-llm-switch-00-index.md) 的 Global Constraints。

**Goal:** 建出 `zmod/llm-switch` crate(path 反指 codex-rs),实现 `config.rs` 读取 `~/.codex/config-zmod.toml` 的 `[llm-switch]` 段、`lib.rs` 的 `enabled()` / `route()` 路由判定与 `Route` 类型。本任务结束时:crate 在 codex-rs workspace 内 `cargo test -p codez-llm-switch` 通过,配置解析与 `auth_key` 拒绝规则有测试覆盖。

> **构建方式见索引「开发期构建与测试」决策段(2026-06-20)**:本 crate 反向依赖 codex-api,**不独立编译**,而是通过 Task 09 patch 第①部分(`core/Cargo.toml` 加 path 依赖)**提前应用到 codex-rs 工作树**,被拉进 workspace 图统一编译。codex-rs 工作树因此在 Task 01–08 期间故意 dirty,**不提交进 codex-rs、不还原**。

**覆盖 spec:** §3(模块布局)、§5.2 / §5.3(config-zmod schema、密钥来源、`auth_key` 拒绝)、§6.1(构建集成情况 B)、§1(命名)。

**Files:**
- Create: `zmod/llm-switch/Cargo.toml`
- Create: `zmod/llm-switch/src/lib.rs`
- Create: `zmod/llm-switch/src/config.rs`
- Create: `zmod/llm-switch/tests/config_test.rs`
- 临时改(dev-build 使能,不提交进 codex-rs、不还原):`codex-rs/core/Cargo.toml`(加一行 path 依赖)
- Modify: 仓库根 `.gitignore`(忽略 `zmod/llm-switch/Cargo.lock`)

**Interfaces:**
- Produces(后续任务依赖):
  - `pub fn enabled() -> bool`
  - `pub fn route(model_provider_id: &str) -> Option<Route>`
  - `pub struct Route { pub provider_id: String, pub cfg: ProviderCfg }`
  - `pub enum Connector { Chat, Anthropic }`(`responses`/未知 → `route()` 返回 `None`)
  - `pub struct ProviderCfg { pub connector: Connector, pub base_url: Option<String>, pub auth: AuthKind, pub key_env: Option<String>, pub auth_key: Option<String>, pub path: Option<String>, pub model: Option<String>, pub anthropic_version: Option<String>, pub default_max_tokens: Option<u32> }`
  - `pub enum AuthKind { Bearer, XApiKey }`
  - `pub fn load_config_from_str(toml: &str, allow_inline_key: bool) -> Result<Config, ConfigError>`(供测试与运行时复用;运行时 `allow_inline_key=false`,testkey 路径 `true`)
  - `pub struct Config { pub enabled: bool, pub providers: HashMap<String, ProviderCfg> }`
- Consumes:无(首个任务)。

---

- [ ] **Step 1: 建目录与 `Cargo.toml`**

创建 `zmod/llm-switch/Cargo.toml`。注意:**不写 `[workspace]`**;path 反指 codex-rs;不用 `workspace = true`。

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
toml = "0.8"
thiserror = "2"
tracing = "0.1"

[dev-dependencies]
tokio = { version = "1", features = ["rt", "macros", "sync", "test-util"] }
```

> 注:`codex-api` / `codex-protocol` 的具体 crate 名以 `codex-rs/codex-api/Cargo.toml`、`codex-rs/protocol/Cargo.toml` 的 `[package] name` 为准(已核实:`codex-api`、`codex-protocol`;路径 `../../codex-rs/codex-api`、`../../codex-rs/protocol`)。reqwest / tokio 版本对齐 codex-rs workspace(`grep -E 'reqwest|^tokio' codex-rs/Cargo.toml`),避免重复编译。两条 codex-* path 依赖**保持激活,不注释**。

并在仓库根 `.gitignore` 加一行忽略本 crate 独立产物锁:`zmod/llm-switch/Cargo.lock`(本 crate 在 workspace 内编译,不携带自己的 lock)。

- [ ] **Step 1b: 应用 dev-build 构建接线(让 crate 进 workspace 图)**

把 Task 09 patch 第①部分提前应用到 codex-rs 工作树——在 `codex-rs/core/Cargo.toml` 的 `[dependencies]` 末尾加一行:

```toml
codez-llm-switch = { path = "../../zmod/llm-switch" }
```

这让本 crate 作为 core 的 path 依赖被拉进 codex-rs workspace 图,从而可用 `cargo test -p codez-llm-switch`(crate 不入 `members`,情况 B)。

> **纪律**:此改动是 dev-build 使能器,**仅留在 codex-rs 工作树(uncommitted)**,本任务提交**不含** `codex-rs/**`。`codex-rs/core/Cargo.toml`(及构建生成的 `codex-rs/Cargo.lock`)在 Task 01–08 全程保持 dirty,**不得 `git checkout` 还原**——Task 09 才连同 client.rs 调用点一起导出进 `patches/llm-switch.patch` 并还原 codex-rs。core 此时只多一个未被引用的依赖(无 warning),不影响 `-p codez-llm-switch` 编译。

- [ ] **Step 2: 写失败测试(配置解析)**

创建 `zmod/llm-switch/tests/config_test.rs`:

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
    // 运行时路径 allow_inline_key=false:必须报配置错误拒绝启动
    let err = load_config_from_str(toml, false).unwrap_err();
    assert!(format!("{err}").contains("auth_key"), "err should mention auth_key: {err}");
    // testkey 路径 allow_inline_key=true:接受
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
    // responses 不进 zmod:解析允许,但 route() 不返回它(见 lib.rs 测试 Step 6)
    assert!(cfg.providers.get("openai").is_none(), "responses provider dropped from routable map");
}

#[test]
fn missing_section_means_disabled() {
    let cfg = load_config_from_str("[other]\nx = 1\n", false).expect("parse ok");
    assert!(!cfg.enabled);
    assert!(cfg.providers.is_empty());
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-switch --test config_test`
Expected: 编译失败(`load_config_from_str` 等未定义)。

- [ ] **Step 4: 实现 `config.rs`**

创建 `zmod/llm-switch/src/config.rs`:

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
    base_url: Option<String>,
    auth: String,
    key_env: Option<String>,
    auth_key: Option<String>,
    path: Option<String>,
    model: Option<String>,
    anthropic_version: Option<String>,
    default_max_tokens: Option<u32>,
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
        // responses / 未知 connector 不进可路由表(走原生分支,spec §4.1)
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

- [ ] **Step 5: 实现 `lib.rs` 的路由部分**

创建 `zmod/llm-switch/src/lib.rs`(本任务只放 config 重导出 + `enabled`/`route`/`Route`;`run` 留到 Task 08,此处先不声明,避免半成品签名):

```rust
mod config;

pub use config::{
    load_config_from_str, AuthKind, Config, ConfigError, Connector, ProviderCfg,
};

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
    // ~/.codex/config-zmod.toml;CODEX_HOME 覆盖优先(与 codex 约定一致,执行前核对其环境变量名)
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

/// 按 codex 的 model_provider_id 判定是否接管。
/// 未启用 / 未命中 / responses → None(走原生 Responses 分支)。
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

> 注:`dirs_config_zmod_path` 的 `CODEX_HOME` 环境变量名执行前用 `grep -rn "CODEX_HOME" codex-rs/core/src/config` 核对,与 codex 主配置定位保持一致。

- [ ] **Step 6: 加 `route()` 的单元测试**

在 `zmod/llm-switch/src/lib.rs` 末尾追加(用 `load_config_from_str` 直接构造 `Config`,避免依赖真实文件;`route` 走全局缓存不便测,改测纯逻辑 helper)。重构:把 `route` 的纯逻辑抽成 `fn route_in(cfg: &Config, id: &str) -> Option<Route>`,`route()` 调它;测试测 `route_in`:

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

(把 `route()` 主体改为 `route_in(loaded(), model_provider_id)`。)

- [ ] **Step 7: 运行测试确认通过**

Run: `cd codex-rs && cargo test -p codez-llm-switch`
Expected: `config_test` 4 个 + lib 内 2 个全 PASS。

> 因 Step 1b 已把 path 依赖接进 workspace 图,本命令在 codex-rs workspace 内编译本 crate,codex-api/codex-protocol 共享 workspace 锁与版本。**首次编译会拉起 codex-api 依赖树,可能数分钟**,这是已知且可接受的;用足够长的超时耐心等待,不要因为慢就注释依赖。若真因依赖冲突失败(非单纯慢),停下并报告精确 cargo 错误。

- [ ] **Step 8: 提交(只提交 crate 与 codez 自有文件)**

```bash
git add zmod/llm-switch/Cargo.toml zmod/llm-switch/src/lib.rs zmod/llm-switch/src/config.rs zmod/llm-switch/tests/config_test.rs .gitignore
git commit -m "feat(llm-switch): crate skeleton + config-zmod parsing and routing"
```

> **不要** `git add codex-rs/`。`codex-rs/core/Cargo.toml` 与 `codex-rs/Cargo.lock` 的 dirty 改动是 dev-build 使能器,留在工作树、不进任何提交(Task 09 才导出 patch)。提交后 `git status` 应仍显示 codex-rs 下这两个文件为 modified——这是预期状态。
