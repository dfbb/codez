# Task 1: config.rs — purpose 映射表解析

> 隶属 [llm-router 实现计划](README.md)。先读 README 的 Global Constraints。

**Files:**
- Modify: `zmod/llm-switch/src/config.rs`
- Test: `zmod/llm-switch/tests/config_test.rs`(追加测试)

**Interfaces:**
- Consumes:(无,首个 task)
- Produces:
  - `Config` 新增字段 `pub purpose: std::collections::HashMap<String, String>`(key = 用途名 `"compact"|"review"|"memory"`,value = provider id 字符串,原样保留)。
  - `load_config_from_str(text, allow_inline_key) -> Result<Config, ConfigError>`(签名不变,行为扩展:解析 `[llm-switch.purpose]`)。
  - 现有 `Config { enabled, providers }` 两处构造点(`lib.rs:209`、`lib.rs:211`)、`config.rs:75`、`config.rs:108` 需补 `purpose` 字段——本 task 内全部改掉,否则编译失败。

---

## 背景(给零上下文工程师)

`config.rs` 现在把 `~/.codex/config-zmod.toml` 解析成 `Config { enabled: bool, providers: HashMap<String, ProviderCfg> }`。spec §3 要在 `[llm-switch]` 下新增一张 `purpose` 表:

```toml
[llm-switch.purpose]
compact = "deepseek-cheap"
review  = "claude-sonnet"
memory  = "deepseek-cheap"
```

value 是 `providers` 里的 id。本 task 只负责**解析并存进 `Config.purpose`**;「value 指向不存在 provider 怎么办」「key 不是合法用途名怎么办」**不在解析层判断**——按 spec §4 第 3a 步,坏映射在 `route()` 运行时 warn+回退(Task 4),解析层原样保留所有键值对(fail-safe:不因坏映射拒绝整份配置)。

当前 `Config` 是手写结构 + 私有 `RawRoot/RawSwitch/RawProvider` 反序列化层。`RawSwitch` 已有 `#[serde(default)] providers`,照同样模式加 `purpose`。

---

- [ ] **Step 1: 建立开发期测试脚手架(软链 + member)**

情况 B crate 必须接进 codex-rs workspace 才能跑 `tests/*.rs`。在 worktree 根执行:

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
ln -sfn ../zmod/llm-switch codex-rs/llm-switch
```

然后编辑 `codex-rs/Cargo.toml`,在 `members = [` 数组末尾(`]` 之前)加一行:

```toml
    "llm-switch",
```

验证脚手架可用(此时尚无新代码,跑现有测试应通过):

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -15
```

Expected: 现有测试编译并通过(若首次拉起 codex-api 树,编译耗时较长属正常)。

> 纪律:`codex-rs/llm-switch` 软链已在根 `.gitignore`;`codex-rs/Cargo.toml` 的 members 那行与 `codex-rs/Cargo.lock` 改动 **dev-only,保持 uncommitted,绝不进 patch、绝不提交进 codex-rs 子树**。

- [ ] **Step 2: 写失败测试 — 解析 purpose 表**

在 `zmod/llm-switch/tests/config_test.rs` 末尾追加:

```rust
#[test]
fn parses_purpose_table() {
    let toml = r#"
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"

[llm-switch.purpose]
compact = "deepseek"
review  = "deepseek"
memory  = "deepseek"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    assert_eq!(cfg.purpose.get("compact").map(String::as_str), Some("deepseek"));
    assert_eq!(cfg.purpose.get("review").map(String::as_str), Some("deepseek"));
    assert_eq!(cfg.purpose.get("memory").map(String::as_str), Some("deepseek"));
}

#[test]
fn purpose_table_absent_is_empty_not_error() {
    let toml = r#"
[llm-switch]
enabled = true

[llm-switch.providers.x]
connector = "chat"
auth = "bearer"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    assert!(cfg.purpose.is_empty());
}

#[test]
fn purpose_value_to_unknown_provider_is_kept_not_rejected() {
    // 坏映射不在解析层拒绝;route() 运行时再 warn+回退(spec §4 第 3a)
    let toml = r#"
[llm-switch]
enabled = true

[llm-switch.providers.x]
connector = "chat"
auth = "bearer"

[llm-switch.purpose]
compact = "does-not-exist"
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    assert_eq!(cfg.purpose.get("compact").map(String::as_str), Some("does-not-exist"));
}
```

- [ ] **Step 3: 运行测试,确认编译失败**

```bash
cd codex-rs && cargo test -p codez-llm-switch --test config_test 2>&1 | tail -20
```

Expected: 编译失败,`no field 'purpose' on type 'Config'`(字段尚不存在)。

- [ ] **Step 4: 给 Config 加 purpose 字段并解析**

编辑 `zmod/llm-switch/src/config.rs`:

(a) `Config` 结构(当前 36-40 行)改为:

```rust
#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub providers: HashMap<String, ProviderCfg>,
    pub purpose: HashMap<String, String>,
}
```

(b) `RawSwitch`(当前 49-55 行)加 `purpose` 字段:

```rust
#[derive(Deserialize)]
struct RawSwitch {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    providers: HashMap<String, RawProvider>,
    #[serde(default)]
    purpose: HashMap<String, String>,
}
```

(c) `load_config_from_str` 里「无 `[llm-switch]`」的早返回(当前 75 行)补字段:

```rust
    let Some(sw) = root.llm_switch else {
        return Ok(Config { enabled: false, providers: HashMap::new(), purpose: HashMap::new() });
    };
```

(d) 函数末尾 `Ok(Config { ... })`(当前 108 行)改为:

```rust
    Ok(Config { enabled: sw.enabled, providers, purpose: sw.purpose })
}
```

- [ ] **Step 5: 修复 lib.rs 两处 Config 构造点(编译必需)**

编辑 `zmod/llm-switch/src/lib.rs`,`loaded()` 内两处 fail-safe 构造(当前 209、211 行)各补 `purpose: Default::default()`:

```rust
            Ok(text) => load_config_from_str(&text, false).unwrap_or_else(|e| {
                tracing::warn!("llm-switch disabled: bad config-zmod.toml: {e}");
                Config { enabled: false, providers: Default::default(), purpose: Default::default() }
            }),
            Err(_) => Config { enabled: false, providers: Default::default(), purpose: Default::default() }, // 缺文件 = 关闭
```

- [ ] **Step 6: 运行测试,确认通过**

```bash
cd codex-rs && cargo test -p codez-llm-switch --test config_test 2>&1 | tail -20
```

Expected: 全部 PASS(含原有 config 测试与新增 3 个)。

- [ ] **Step 7: 跑 clippy 确认无新增告警**

```bash
cd codex-rs && cargo clippy -p codez-llm-switch --all-targets 2>&1 | tail -15
```

Expected: 无 error;无本 task 引入的新 warning。

- [ ] **Step 8: 提交**

只提交 crate 源码与测试,**不提交** dev 脚手架(软链被 ignore;members 行与 Cargo.lock 保持 dirty):

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git add zmod/llm-switch/src/config.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/config_test.rs
git commit -m "feat(llm-switch): 解析 [llm-switch.purpose] 用途->provider 映射表"
```
