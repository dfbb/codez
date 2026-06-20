# Task 01: crate 骨架 + config

> 属于 `2026-06-20-llm-compress-00-index.md`。执行前先读 index 的 Global Constraints / 真实类型 / dev-build 决策。

**Goal:** 建出 `zmod/llm-compress` crate(path 反指 codex-rs),实现 `config.rs` 读取 `~/.codex/config-zmod.toml` 的 `[llm_compress]` 段并提供默认值;`lib.rs` 暴露 `enabled()`。本任务结束时 crate 在 codex-rs workspace 内 `cargo test -p codez-llm-compress` 通过。

**覆盖 spec:** §5(配置)、§11(crate 结构 / workspace 接入)。

**Files:**
- Create: `zmod/llm-compress/Cargo.toml`
- Create: `zmod/llm-compress/.gitignore`
- Create: `zmod/llm-compress/src/lib.rs`
- Create: `zmod/llm-compress/src/config.rs`
- Create: `zmod/llm-compress/tests/config_test.rs`
- Modify(工作树,不提交): `codex-rs/core/Cargo.toml` `[dependencies]` 增一行

**Interfaces:**
- Produces:
  - `pub struct Config { pub enabled: bool, pub min_total_bytes: usize, pub per_item_min_bytes: usize, pub truncate: TruncateCfg, pub json: JsonCfg, pub diff: DiffCfg, pub log: LogCfg }`
  - `pub struct TruncateCfg { pub head_lines: usize, pub tail_lines: usize, pub max_bytes: usize }`
  - `pub struct JsonCfg { pub max_array_items: usize, pub max_depth: usize }`
  - `pub struct DiffCfg { pub context_lines: usize }`
  - `pub struct LogCfg { pub dedup_repeats: bool }`
  - `pub fn load() -> Config` — 读 `~/.codex/config-zmod.toml`,缺节/解析失败 → 返回 `Config::disabled()`(`enabled=false` + 默认阈值) 并 warn;成功 → 用文件值覆盖默认。
  - `pub fn enabled() -> bool`(`lib.rs`) — `load().enabled` 的便捷封装(进程内每次 load,v1 不缓存以求简单;若实现缓存须用 `OnceLock`)。

---

- [ ] **Step 1: 建目录与 Cargo.toml**

Create `zmod/llm-compress/Cargo.toml`:

```toml
[package]
name = "codez-llm-compress"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
name = "codez_llm_compress"
path = "src/lib.rs"

[dependencies]
codex-api = { path = "../../codex-rs/codex-api" }
codex-protocol = { path = "../../codex-rs/protocol" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.9"
chrono = { version = "0.4", features = ["clock"] }
tracing = "0.1"

[dev-dependencies]
insta = "1"
tempfile = "3"
```

- [ ] **Step 2: .gitignore(不提交 Cargo.lock)**

Create `zmod/llm-compress/.gitignore`:

```
/target
Cargo.lock
```

- [ ] **Step 3: 接线开发期构建——软链成为 codex-rs workspace member(情况 B,CLAUDE.md §44-63)**

> **背景(已由 llm-switch 实测验证)**:本 crate 反向依赖 codex-api/codex-protocol(情况 B)。cargo 硬约束:非 member 的 path 依赖**不能声明 `[dev-dependencies]`、不能跑 `tests/*.rs` 集成测试**;而 cargo 又拒绝 codex-rs 根之外的 member。解法是**开发期用软链把 crate 接进 codex-rs workspace 成为真 member**(仅本地测试,不进 patch、不提交进 codex-rs 子树)。

① 建软链(cargo 视其为根下 member,绕过跨根限制):
```bash
cd /Users/dfbb/Sites/skycode/codez
ln -s ../zmod/llm-compress codex-rs/llm-compress
```

② 在 `codex-rs/Cargo.toml` 的 `members` 列表末尾(`]` 之前)加一行——**软链名**,不是 `../` 路径:
```toml
    "llm-compress",
```

③ 在根 `.gitignore` 加(若未加):
```
/codex-rs/llm-compress
```

> **纪律**:软链 `codex-rs/llm-compress` 写进 `.gitignore`,绝不提交进 codex-rs 子树;`codex-rs/Cargo.toml` 的 members 那行与构建产生的 `codex-rs/Cargo.lock` 是 dev-only 脚手架,保持 uncommitted dirty,**不进 `patches/llm-compress.patch`、不被还原**。生产接入(Task 09 patch)走的是另一条路——情况 B 的 `core/Cargo.toml` 外部 path 依赖 + client.rs 调用,与软链无关。软链就位后 `cd codex-rs && cargo test -p codez-llm-compress` 完整支持 dev-deps 与集成测试,共享 codex-rs 的 Cargo.lock 与 target。

- [ ] **Step 4: 写 config.rs(默认值 + 读取 + fail-safe)**

Create `zmod/llm-compress/src/config.rs`:

```rust
//! 读取 ~/.codex/config-zmod.toml 的 [llm_compress] 段。
//! fail-safe:文件/节缺失或解析失败 → enabled=false + 默认阈值。

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub min_total_bytes: usize,
    pub per_item_min_bytes: usize,
    pub truncate: TruncateCfg,
    pub json: JsonCfg,
    pub diff: DiffCfg,
    pub log: LogCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TruncateCfg {
    pub head_lines: usize,
    pub tail_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JsonCfg {
    pub max_array_items: usize,
    pub max_depth: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiffCfg {
    pub context_lines: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LogCfg {
    pub dedup_repeats: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            min_total_bytes: 4096,
            per_item_min_bytes: 1024,
            truncate: TruncateCfg::default(),
            json: JsonCfg::default(),
            diff: DiffCfg::default(),
            log: LogCfg::default(),
        }
    }
}

impl Default for TruncateCfg {
    fn default() -> Self {
        Self { head_lines: 50, tail_lines: 50, max_bytes: 16384 }
    }
}

impl Default for JsonCfg {
    fn default() -> Self {
        Self { max_array_items: 20, max_depth: 6 }
    }
}

impl Default for DiffCfg {
    fn default() -> Self {
        Self { context_lines: 3 }
    }
}

impl Default for LogCfg {
    fn default() -> Self {
        Self { dedup_repeats: true }
    }
}

impl Config {
    pub fn disabled() -> Self {
        Self::default()
    }
}

/// 顶层文件结构:只关心 [llm_compress] 节。
#[derive(Debug, Deserialize)]
struct RootFile {
    #[serde(default)]
    llm_compress: Option<Config>,
}

fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex").join("config-zmod.toml"))
}

/// 从指定路径读取(便于测试注入)。
pub fn load_from(path: &std::path::Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Config::disabled(),
    };
    match toml::from_str::<RootFile>(&text) {
        Ok(root) => root.llm_compress.unwrap_or_else(Config::disabled),
        Err(e) => {
            tracing::warn!("llm-compress: config parse failed, disabling: {e}");
            Config::disabled()
        }
    }
}

/// 从默认路径 ~/.codex/config-zmod.toml 读取。
pub fn load() -> Config {
    match config_path() {
        Some(p) => load_from(&p),
        None => Config::disabled(),
    }
}
```

- [ ] **Step 5: 写 lib.rs(暂只导出 config + enabled)**

Create `zmod/llm-compress/src/lib.rs`:

```rust
//! codez-llm-compress:在 codex LLM 请求边界压缩请求。
//! 入口 transform() 在 Task 08 加入;本任务先建 config 地基。

pub mod config;

/// 是否启用压缩(读 ~/.codex/config-zmod.toml 的 [llm_compress].enabled)。
pub fn enabled() -> bool {
    config::load().enabled
}
```

- [ ] **Step 6: 写失败测试**

Create `zmod/llm-compress/tests/config_test.rs`:

```rust
use codez_llm_compress::config::{load_from, Config};
use std::io::Write;

fn write_tmp(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn enabled_section_overrides_defaults() {
    let f = write_tmp(
        r#"
[llm_compress]
enabled = true
min_total_bytes = 2048

[llm_compress.truncate]
head_lines = 10
tail_lines = 5
max_bytes = 8192

[llm_compress.json]
max_array_items = 7
max_depth = 4

[llm_compress.diff]
context_lines = 1

[llm_compress.log]
dedup_repeats = false
"#,
    );
    let cfg = load_from(f.path());
    assert!(cfg.enabled);
    assert_eq!(cfg.min_total_bytes, 2048);
    assert_eq!(cfg.per_item_min_bytes, 1024); // 未给 → 默认
    assert_eq!(cfg.truncate.head_lines, 10);
    assert_eq!(cfg.truncate.max_bytes, 8192);
    assert_eq!(cfg.json.max_array_items, 7);
    assert_eq!(cfg.json.max_depth, 4);
    assert_eq!(cfg.diff.context_lines, 1);
    assert!(!cfg.log.dedup_repeats);
}

#[test]
fn missing_section_disables() {
    let f = write_tmp("[some_other]\nx = 1\n");
    let cfg = load_from(f.path());
    assert!(!cfg.enabled);
    assert_eq!(cfg.min_total_bytes, 4096); // 默认
}

#[test]
fn missing_file_disables() {
    let cfg = load_from(std::path::Path::new("/nonexistent/zzz/config-zmod.toml"));
    assert!(!cfg.enabled);
}

#[test]
fn malformed_toml_disables() {
    let f = write_tmp("[llm_compress]\nenabled = = true\n");
    let cfg = load_from(f.path());
    assert!(!cfg.enabled);
}

#[test]
fn default_config_is_disabled_with_known_thresholds() {
    let cfg = Config::disabled();
    assert!(!cfg.enabled);
    assert_eq!(cfg.per_item_min_bytes, 1024);
    assert_eq!(cfg.truncate.tail_lines, 50);
    assert!(cfg.log.dedup_repeats);
}
```

- [ ] **Step 7: 跑测试看失败(crate 尚未编译进 workspace 前会先失败/编译错)**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test config_test
```
Expected: 软链+member(Step 3)就位后编译通过、测试运行;若测试逻辑有误则相应断言 FAIL。若报 `package ID specification ... did not match` 说明 Step 3 软链/member 未生效,回到 Step 3。

- [ ] **Step 8: 跑测试看通过**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test config_test
```
Expected: `test result: ok. 5 passed`。

- [ ] **Step 9: 提交(仅 zmod,不含 codex-rs 的 dirty 改动)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-01-crate-skeleton-config.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): crate skeleton + config-zmod [llm_compress] parsing"
```

> **不要** `git add codex-rs/Cargo.toml`(members 行)、`codex-rs/Cargo.lock`、软链 `codex-rs/llm-compress`——它们是 dev-build 软链 member 脚手架,全程保持 dirty/未跟踪,不提交进 codex-rs 子树、不进 patch。`.gitignore` 已忽略软链。
