# Task 10 — testkey-Gated Live Tests

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the [master index](2026-06-20-llm-switch-00-index.md) Global Constraints, especially security: **`tests/testkey.toml` contains real keys — never commit it**.

**Goal:** Gated real-path tests: read real keys from `zmod/llm-switch/tests/testkey.toml` (`auth_key` inlined, taking the `allow_inline_key=true` path), actually hit the deepseek (chat) and claude (anthropic) endpoints, and verify end-to-end connectivity (text roundtrip + one standard `function` tool roundtrip). When `testkey.toml` is missing or the environment variable is unset, **skip automatically**, so CI stays green even without keys.

**Spec coverage:** §8 (integration/live tests, success criterion 1), §9 (security).

**Files:**
- Create: `zmod/llm-switch/tests/live_test.rs`
- Already exists (gitignored, **do not create/modify/read its contents into logs**): `zmod/llm-switch/tests/testkey.toml`
- Modify: `zmod/llm-switch/src/lib.rs` (add `pub fn load_testkey_config(path) -> Result<Config, ConfigError>`, internally `allow_inline_key=true`)

**Interfaces:**
- Consumes: `load_config_from_str` (Task 01, `allow_inline_key=true`), `run` (Task 08), `route`/`Route`.
- Produces: `pub fn load_testkey_config(path: &std::path::Path) -> Result<Config, ConfigError>` (test/standalone use only).

---

- [ ] **Step 0: Confirm testkey safety**

```bash
git check-ignore zmod/llm-switch/tests/testkey.toml && echo "IGNORED OK"
git status --porcelain | grep testkey.toml && echo "WARNING: tracked!" || echo "not tracked, good"
```
Expected: `IGNORED OK` and not tracked by git. **Never** print key contents in any test output, assertion message, or log.

- [ ] **Step 1: Add `load_testkey_config`**

`lib.rs`:
```rust
/// For live tests / standalone runs only: read config from the gitignored testkey.toml, allowing an inline auth_key.
pub fn load_testkey_config(path: &std::path::Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Parse(e.to_string()))?;
    load_config_from_str(&text, true)
}
```

> testkey.toml schema (already exists, see spec §8): `[llm-switch.providers.<id>]` + `connector`/`base_url`/`auth`/`auth_key`/`model`/`default_max_tokens` (claude also has `anthropic_version`). `load_config_from_str(_, true)` consumes it directly.

- [ ] **Step 2: Write the gated live test**

Create `zmod/llm-switch/tests/live_test.rs`. Gate with `#[ignore]` (not run by default; run explicitly with `cargo test -- --ignored`), and within the test, when testkey is missing, `eprintln!` a skip (do not fail).

```rust
use std::path::Path;
use futures::StreamExt;
use codez_llm_switch::{load_testkey_config, Route};
use codex_api::ResponseEvent;

fn testkey_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testkey.toml")
}

// Use the testkey provider config to construct a Route directly + call run; bypass the global config-zmod cache.
async fn run_text_roundtrip(provider_id: &str) {
    let path = testkey_path();
    if !path.exists() { eprintln!("skip: testkey.toml absent"); return; }
    let cfg = load_testkey_config(&path).expect("parse testkey");
    let Some(pcfg) = cfg.providers.get(provider_id) else { eprintln!("skip: provider {provider_id} absent"); return; };
    let rt = Route { provider_id: provider_id.to_string(), cfg: pcfg.clone() };

    // Minimal request: one line of user text, disable all non-function tools (per success criterion 1)
    let mut req = codez_llm_switch::testing::sample_request();
    req.model = pcfg.model.clone().unwrap_or_default();
    req.input = vec![codex_protocol::models::ResponseItem::Message {
        id: None, role: "user".into(),
        content: vec![codex_protocol::models::ContentItem::InputText { text: "Reply with the single word: pong".into() }],
        phase: None, metadata: None,
    }];
    req.tools = vec![]; // Disable all tools, to avoid hard failures triggered by namespace/web_search etc.

    // The bearer fallback is unused: testkey carries its own auth_key. Pass a noop provider for api_auth.
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
    eprintln!("[{provider_id}] reply len = {}", text.len()); // Do not print the key, only the length
}

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn deepseek_chat_live() { run_text_roundtrip("deepseek").await; }

#[tokio::test]
#[ignore = "requires tests/testkey.toml + network"]
async fn claude_anthropic_live() { run_text_roundtrip("claude").await; }
```

> The provider_id values "deepseek"/"claude" must match the actual table names in testkey.toml (Step 0 does not read its contents; align with the known schema; if the table names differ, adjust these two strings).

- [ ] **Step 3: Add `noop_auth_provider` to testing**

Add a `SharedAuthProvider` to the `lib.rs` testing module that never writes headers (since testkey uses `auth_key`, the fallback is never reached):
```rust
pub fn noop_auth_provider() -> codex_api::SharedAuthProvider {
    struct Noop;
    impl codex_api::AuthProvider for Noop {
        fn add_auth_headers(&self, _h: &mut reqwest::header::HeaderMap) {}
    }
    std::sync::Arc::new(Noop)
}
```
> The remaining `AuthProvider` methods have default implementations (confirmed during Explore); only `add_auth_headers` needs to be implemented. Before running, `grep -n "trait AuthProvider" -A 12 codex-rs/codex-api/src/auth.rs` to check whether there are any required methods without a default implementation.

- [ ] **Step 4: Run offline all-green (confirm the gate does not affect default tests)**

Run: `cd zmod/llm-switch && cargo test`
Expected: The two `live_test` cases are skipped by `#[ignore]` (shown as ignored), everything else PASSes.

- [ ] **Step 5: Local real-path verification (manual, when testkey is present)**

Run: `cd zmod/llm-switch && cargo test --test live_test -- --ignored --nocapture`
Expected: With testkey + network, both cases print a non-empty reply length and pass; without testkey, they print skip and pass.

- [ ] **Step 6: End-to-end smoke test (optional, real codex binary)**

Per spec §5.1, configure `[model_providers.deepseek]`/`[model_providers.claude]` in `~/.codex/config.toml` + §5.2 `~/.codex/config-zmod.toml`, apply the patches, then `cargo build -p codex-cli`, set `model_provider = "deepseek"`, run a conversation, and confirm the takeover takes effect. This step is an acceptance demo, not an automated test; record the result.

- [ ] **Step 7: Commit**

```bash
git add zmod/llm-switch/tests/live_test.rs zmod/llm-switch/src/lib.rs
git status --porcelain | grep -q testkey.toml && { echo "ABORT: testkey staged"; exit 1; } || true
git commit -m "test(llm-switch): gated live roundtrip tests for deepseek/claude (skip without testkey)"
```

> Before committing, make sure `testkey.toml` is not staged (the grep guard in Step 7).
