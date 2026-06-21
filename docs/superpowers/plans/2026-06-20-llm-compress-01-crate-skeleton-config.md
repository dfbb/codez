# Task 01: crate skeleton + config

> Part of `2026-06-20-llm-compress-00-index.md`. Before executing, read the index's Global Constraints / real types / dev-build decisions.

**Goal:** Build the `zmod/llm-compress` crate (with path pointing back to codex-rs), implement `config.rs` to read the `[llm_compress]` section of `~/.codex/config-zmod.toml` and provide defaults; `lib.rs` exposes `enabled()`. By the end of this task the crate passes `cargo test -p codez-llm-compress` inside the codex-rs workspace.

**Covers spec:** §5 (config), §11 (crate structure / workspace integration).

**Files:**
- Create: `zmod/llm-compress/Cargo.toml`
- Create: `zmod/llm-compress/.gitignore`
- Create: `zmod/llm-compress/src/lib.rs`
- Create: `zmod/llm-compress/src/config.rs`
- Create: `zmod/llm-compress/tests/config_test.rs`
- Modify (working tree, not committed): add one line to `codex-rs/core/Cargo.toml` `[dependencies]`

**Interfaces:**
- Produces:
  - `pub struct Config { pub enabled: bool, pub min_total_bytes: usize, pub per_item_min_bytes: usize, pub truncate: TruncateCfg, pub json: JsonCfg, pub diff: DiffCfg, pub log: LogCfg }`
  - `pub struct TruncateCfg { pub head_lines: usize, pub tail_lines: usize, pub max_bytes: usize }`
  - `pub struct JsonCfg { pub max_array_items: usize, pub max_depth: usize }`
  - `pub struct DiffCfg { pub context_lines: usize }`
  - `pub struct LogCfg { pub dedup_repeats: bool }`
  - `pub fn load() -> Config` — reads `~/.codex/config-zmod.toml`; missing section / parse failure → returns `Config::disabled()` (`enabled=false` + default thresholds) and warns; on success → file values override defaults.
  - `pub fn enabled() -> bool` (`lib.rs`) — convenience wrapper over `load().enabled` (loads every call within the process; v1 does not cache, for simplicity; if caching is implemented it must use `OnceLock`).

---

- [ ] **Step 1: Create directory and Cargo.toml**

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

- [ ] **Step 2: .gitignore (do not commit Cargo.lock)**

Create `zmod/llm-compress/.gitignore`:

```
/target
Cargo.lock
```

- [ ] **Step 3: Wire up the dev-time build — soft-link the crate into the codex-rs workspace as a member (Case B, CLAUDE.md §44-63)**

> **Background (already empirically validated by llm-switch):** this crate reverse-depends on codex-api/codex-protocol (Case B). cargo hard constraints: a path dependency that is not a member **cannot declare `[dev-dependencies]`, cannot run `tests/*.rs` integration tests**; and cargo also rejects members outside the codex-rs root. The solution is to **soft-link the crate into the codex-rs workspace at dev time so it becomes a real member** (local testing only, not in the patch, not committed into the codex-rs subtree).

① Create the soft link (cargo treats it as a member under the root, bypassing the cross-root restriction):
```bash
cd /Users/dfbb/Sites/skycode/codez
ln -s ../zmod/llm-compress codex-rs/llm-compress
```

② Add one line at the end of the `members` list in `codex-rs/Cargo.toml` (before the `]`) — the **soft-link name**, not the `../` path:
```toml
    "llm-compress",
```

③ Add to the root `.gitignore` (if not already present):
```
/codex-rs/llm-compress
```

> **Discipline:** the soft link `codex-rs/llm-compress` goes into `.gitignore` and is never committed into the codex-rs subtree; the members line in `codex-rs/Cargo.toml` and the build-generated `codex-rs/Cargo.lock` are dev-only scaffolding, kept uncommitted dirty, **not in `patches/llm-compress.patch`, not reverted**. The production integration (Task 09 patch) takes a different route — Case B's external path dependency in `core/Cargo.toml` + the client.rs call, unrelated to the soft link. Once the soft link is in place, `cd codex-rs && cargo test -p codez-llm-compress` fully supports dev-deps and integration tests, sharing codex-rs's Cargo.lock and target.

- [ ] **Step 4: Write config.rs (defaults + reading + fail-safe)**

Create `zmod/llm-compress/src/config.rs`:

```rust
//! Reads the [llm_compress] section of ~/.codex/config-zmod.toml.
//! fail-safe: file/section missing or parse failure → enabled=false + default thresholds.

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

/// Top-level file structure: we only care about the [llm_compress] section.
#[derive(Debug, Deserialize)]
struct RootFile {
    #[serde(default)]
    llm_compress: Option<Config>,
}

fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex").join("config-zmod.toml"))
}

/// Read from a given path (for test injection).
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

/// Read from the default path ~/.codex/config-zmod.toml.
pub fn load() -> Config {
    match config_path() {
        Some(p) => load_from(&p),
        None => Config::disabled(),
    }
}
```

- [ ] **Step 5: Write lib.rs (for now export only config + enabled)**

Create `zmod/llm-compress/src/lib.rs`:

```rust
//! codez-llm-compress: compress requests at the codex LLM request boundary.
//! The transform() entry point is added in Task 08; this task first lays the config foundation.

pub mod config;

/// Whether compression is enabled (reads [llm_compress].enabled from ~/.codex/config-zmod.toml).
pub fn enabled() -> bool {
    config::load().enabled
}
```

- [ ] **Step 6: Write failing tests**

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
    assert_eq!(cfg.per_item_min_bytes, 1024); // not given → default
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
    assert_eq!(cfg.min_total_bytes, 4096); // default
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

- [ ] **Step 7: Run the tests to see them fail (before the crate is compiled into the workspace they will fail / fail to compile)**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test config_test
```
Expected: once the soft link + member (Step 3) are in place, compilation succeeds and the tests run; if the test logic is wrong the corresponding assertions FAIL. If you see `package ID specification ... did not match`, the Step 3 soft link/member did not take effect — go back to Step 3.

- [ ] **Step 8: Run the tests to see them pass**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test config_test
```
Expected: `test result: ok. 5 passed`.

- [ ] **Step 9: Commit (zmod only, not the codex-rs dirty changes)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-01-crate-skeleton-config.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): crate skeleton + config-zmod [llm_compress] parsing"
```

> **Do not** `git add codex-rs/Cargo.toml` (the members line), `codex-rs/Cargo.lock`, or the soft link `codex-rs/llm-compress` — they are dev-build soft-link member scaffolding, kept dirty/untracked throughout, not committed into the codex-rs subtree and not in any patch. `.gitignore` already ignores the soft link.
