# Task 10 — testkey 门控实跑测试

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints,尤其安全:**`tests/testkey.toml` 含真 key,绝不提交**。

**Goal:** 加门控的真实链路测试:从 `zmod/llm-switch/tests/testkey.toml` 读真 key(`auth_key` 内联,走 `allow_inline_key=true` 路径),真实打 deepseek(chat)与 claude(anthropic)端点,验证端到端连通(文本往返 + 一次标准 `function` 工具往返)。`testkey.toml` 缺失或环境变量未设时**自动跳过**,CI 无 key 也全绿。

**覆盖 spec:** §8(集成/实跑测试、成功判据 1)、§9(安全)。

**Files:**
- Create: `zmod/llm-switch/tests/live_test.rs`
- 已存在(gitignored,**不创建/不修改/不读出内容到日志**):`zmod/llm-switch/tests/testkey.toml`
- Modify: `zmod/llm-switch/src/lib.rs`(加 `pub fn load_testkey_config(path) -> Result<Config, ConfigError>`,内部 `allow_inline_key=true`)

**Interfaces:**
- Consumes:`load_config_from_str`(Task 01,`allow_inline_key=true`)、`run`(Task 08)、`route`/`Route`。
- Produces:`pub fn load_testkey_config(path: &std::path::Path) -> Result<Config, ConfigError>`(仅测试/独立运行用)。

---

- [ ] **Step 0: 确认 testkey 安全**

```bash
git check-ignore zmod/llm-switch/tests/testkey.toml && echo "IGNORED OK"
git status --porcelain | grep testkey.toml && echo "WARNING: tracked!" || echo "not tracked, good"
```
Expected:`IGNORED OK` 且未被 git 跟踪。**严禁**在任何测试输出、断言信息、日志里打印 key 内容。

- [ ] **Step 1: 加 `load_testkey_config`**

`lib.rs`:
```rust
/// 仅供实跑测试 / 独立运行:从 gitignored testkey.toml 读配置,允许内联 auth_key。
pub fn load_testkey_config(path: &std::path::Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Parse(e.to_string()))?;
    load_config_from_str(&text, true)
}
```

> testkey.toml schema(已存在,见 spec §8):`[llm-switch.providers.<id>]` + `connector`/`base_url`/`auth`/`auth_key`/`model`/`default_max_tokens`(claude 还有 `anthropic_version`)。`load_config_from_str(_, true)` 直接吃。

- [ ] **Step 2: 写门控实跑测试**

创建 `zmod/llm-switch/tests/live_test.rs`。用 `#[ignore]` 门控(默认不跑;`cargo test -- --ignored` 显式跑),并在测试内 testkey 缺失时 `eprintln!` 跳过(不 fail)。

```rust
use std::path::Path;
use futures::StreamExt;
use codez_llm_switch::{load_testkey_config, Route};
use codex_api::ResponseEvent;

fn testkey_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testkey.toml")
}

// 用 testkey 的 provider 配置直接构造 Route + 调 run;不经全局 config-zmod 缓存。
async fn run_text_roundtrip(provider_id: &str) {
    let path = testkey_path();
    if !path.exists() { eprintln!("skip: testkey.toml absent"); return; }
    let cfg = load_testkey_config(&path).expect("parse testkey");
    let Some(pcfg) = cfg.providers.get(provider_id) else { eprintln!("skip: provider {provider_id} absent"); return; };
    let rt = Route { provider_id: provider_id.to_string(), cfg: pcfg.clone() };

    // 最小请求:一句 user 文本,关闭一切非 function 工具(成功判据 1 口径)
    let mut req = codez_llm_switch::testing::sample_request();
    req.model = pcfg.model.clone().unwrap_or_default();
    req.input = vec![codex_protocol::models::ResponseItem::Message {
        id: None, role: "user".into(),
        content: vec![codex_protocol::models::ContentItem::InputText { text: "Reply with the single word: pong".into() }],
        phase: None, metadata: None,
    }];
    req.tools = vec![]; // 关闭所有工具,避免 namespace/web_search 等触发硬失败

    // bearer 退路用不到:testkey 自带 auth_key。api_auth 传一个 noop provider。
    let api_auth = codez_llm_switch::testing::noop_auth_provider();
    let stream = codez_llm_switch::run(rt, req, api_auth).await.expect("run ok");
    let mut rx = stream.rx_event;
    let mut text = String::new();
    let mut completed = false;
    while let Some(item) = rx.recv().await {
        match item.expect("event ok") {
            ResponseEvent::OutputTextDelta(s) => text.push_str(&s),
            ResponseEvent::Completed { .. } => { completed = true; }
            _ => {}
        }
    }
    assert!(completed, "stream must complete");
    assert!(!text.trim().is_empty(), "got some assistant text");
    eprintln!("[{provider_id}] reply len = {}", text.len()); // 不打印 key,只打印长度
}

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn deepseek_chat_live() { run_text_roundtrip("deepseek").await; }

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn claude_anthropic_live() { run_text_roundtrip("claude").await; }
```

> provider_id "deepseek"/"claude" 以 testkey.toml 里实际 table 名为准(Step 0 不读内容,执行者按已知 schema 对齐;若 table 名不同,调整这两个字符串)。

- [ ] **Step 3: testing 增 `noop_auth_provider`**

`lib.rs` testing 模块加一个永不写头的 `SharedAuthProvider`(因 testkey 走 `auth_key`,根本不会用到退路):
```rust
pub fn noop_auth_provider() -> codex_api::SharedAuthProvider {
    struct Noop;
    impl codex_api::AuthProvider for Noop {
        fn add_auth_headers(&self, _h: &mut reqwest::header::HeaderMap) {}
    }
    std::sync::Arc::new(Noop)
}
```
> `AuthProvider` 其余方法有默认实现(Explore 已确认);只需实现 `add_auth_headers`。执行前 `grep -n "trait AuthProvider" -A 12 codex-rs/codex-api/src/auth.rs` 核对是否有无默认实现的必填方法。

- [ ] **Step 4: 跑离线全绿(确认门控不影响默认 test)**

Run: `cd zmod/llm-switch && cargo test`
Expected: `live_test` 的两个用例被 `#[ignore]` 跳过(显示 ignored),其余全 PASS。

- [ ] **Step 5: 本地真链路验证(有 testkey 时手动)**

Run: `cd zmod/llm-switch && cargo test --test live_test -- --ignored --nocapture`
Expected:有 testkey + 网络时,两个用例都打印非空回复长度并通过;无 testkey 时打印 skip 且通过。

- [ ] **Step 6: 端到端冒烟(可选,真实 codex 二进制)**

按 spec §5.1 配 `~/.codex/config.toml` 的 `[model_providers.deepseek]`/`[model_providers.claude]` + §5.2 `~/.codex/config-zmod.toml`,打 patch 后 `cargo build -p codex-cli`,设 `model_provider = "deepseek"` 跑一轮对话,确认接管生效。此步是验收演示,非自动化测试;记录结果。

- [ ] **Step 7: 提交**

```bash
git add zmod/llm-switch/tests/live_test.rs zmod/llm-switch/src/lib.rs
git status --porcelain | grep -q testkey.toml && { echo "ABORT: testkey staged"; exit 1; } || true
git commit -m "test(llm-switch): gated live roundtrip tests for deepseek/claude (skip without testkey)"
```

> 提交前务必确认 `testkey.toml` 未进暂存区(Step 7 的 grep 守卫)。
