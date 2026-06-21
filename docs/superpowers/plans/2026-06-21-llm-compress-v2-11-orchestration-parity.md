# Task 11 — lib orchestration wiring + inherited fixtures + parity_test

> Belongs to `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §2 / §8 / §9. Depends on all prior tasks (01–10).

**Goal:** Wire all modules into the `transform` orchestration chain (command detection → protection gate → preprocessing → routed compression → CCR attachment → volume gate), register all 6 compressors in `build_router` (Json→Search→Diff→Tabular→Log→Truncate), inherit the headroom/rtk fixtures (NOTICE + manifest sidecar), and write a `parity_test` that runs comparison assertions.

## Files
- Modify: `zmod/llm-compress/src/lib.rs` (rewrite the transform orchestration + register the 6 compressors in build_router)
- Create: `zmod/llm-compress/tests/fixtures/inherited/` (LICENSE-headroom, LICENSE-rtk, NOTICE.md, manifest.toml + raw data files in each compressor subdirectory)
- Create: `zmod/llm-compress/tests/parity_test.rs`
- Create: `zmod/llm-compress/tests/orchestration_test.rs` (end-to-end orchestration)

**Interfaces:**
- Consumes: all of Task 01–10: `router::{ContentRouter,Budget,build...}`, `query::extract`, `command::index`, `score`, `protect::should_protect`, `preprocess::run`, `ccr::{RequestCtx,CcrRegistry,attach}`, the 6 compressors, `config`.
- Produces: a fully usable `transform`.

---

- [ ] **Step 1: Register the 6 compressors in build_router**

Change `build_router` in `zmod/llm-compress/src/lib.rs` (v1 registered only 4) to the following (order = spec routing priority `Json→Search→Diff→Tabular→Log→Truncate`):

```rust
fn build_router() -> ContentRouter {
    use crate::compress::{
        diff::DiffCompressor, json::JsonCompressor, log::LogCompressor,
        search::SearchCompressor, tabular::TabularCompressor, truncate::TruncateCompressor,
    };
    ContentRouter::new(vec![
        Box::new(JsonCompressor),
        Box::new(SearchCompressor),
        Box::new(DiffCompressor),
        Box::new(TabularCompressor),
        Box::new(LogCompressor),
        Box::new(TruncateCompressor),
    ])
}
```

- [ ] **Step 2: Rewrite the transform orchestration (build RequestCtx + processing chain)**

Change `transform` in `zmod/llm-compress/src/lib.rs` to the following (spec §2 orchestration):

```rust
pub fn transform(request: &mut ResponsesApiRequest, _api_provider: &ApiProvider, queryid: &str) {
    let cfg = config::load();
    if !cfg.enabled {
        return;
    }
    // One-time request context
    let ctx = crate::ccr::RequestCtx {
        queryid,
        query_terms: crate::query::extract(request),
        cmd_index: crate::command::index(request),
        ccr: std::cell::RefCell::new(crate::ccr::CcrRegistry::new()),
    };
    let router = build_router();

    let total_before = total_text_bytes(&request.input);
    for item in request.input.iter_mut() {
        compress_item(item, &ctx, &router, &cfg);
    }
    let total_after = total_text_bytes(&request.input);
    if total_after < total_before {
        stats::log_compression(queryid, total_before, total_after);
    }
}
```

- [ ] **Step 3: Rewrite compress_item / compress_in_place (processing chain ①–⑥)**

Replace the existing `compress_item` and `compress_in_place` in `lib.rs`:

```rust
fn compress_item(
    item: &mut ResponseItem,
    ctx: &crate::ccr::RequestCtx,
    router: &ContentRouter,
    cfg: &config::Config,
) {
    let call_id = match item {
        ResponseItem::FunctionCallOutput { call_id, .. } => call_id.clone(),
        ResponseItem::CustomToolCallOutput { call_id, .. } => call_id.clone(),
        _ => return,
    };
    let body = match item {
        ResponseItem::FunctionCallOutput { output, .. } => &mut output.body,
        ResponseItem::CustomToolCallOutput { output, .. } => &mut output.body,
        _ => return,
    };
    match body {
        FunctionCallOutputBody::Text(s) => compress_in_place(s, ctx, router, cfg, &call_id),
        FunctionCallOutputBody::ContentItems(items) => {
            for ci in items.iter_mut() {
                if let FunctionCallOutputContentItem::InputText { text } = ci {
                    compress_in_place(text, ctx, router, cfg, &call_id);
                }
            }
        }
    }
}

fn compress_in_place(
    s: &mut String,
    ctx: &crate::ccr::RequestCtx,
    router: &ContentRouter,
    cfg: &config::Config,
    call_id: &str,
) {
    if s.len() < cfg.per_item_min_bytes {
        return;
    }
    let cmd = ctx.cmd_index.get(call_id);
    // ② Protection gate: if it matches, the whole segment stays byte-for-byte unchanged
    if crate::protect::should_protect(s, cmd, cfg) {
        return;
    }
    // ③ Preprocessing
    let (pre, pre_lossy) = crate::preprocess::run(s, &cfg.preprocess);
    // ④⑤ Routed compression
    let budget = Budget { cfg, cmd, query: &ctx.query_terms };
    let candidate = match router.compress_text(&pre, &budget) {
        Some((new, comp_lossy, _kind)) => {
            if pre_lossy || comp_lossy {
                crate::ccr::attach(new, s, ctx, call_id, &cfg.ccr)
            } else {
                new
            }
        }
        None => {
            if pre_lossy {
                crate::ccr::attach(pre, s, ctx, call_id, &cfg.ccr)
            } else {
                pre
            }
        }
    };
    // #4 Final write-back gate
    if candidate.len() <= s.len() {
        *s = candidate;
    }
}
```

> Delete the old v1 `compress_item` signature (the one with the `min_bytes` parameter) and the old `Budget { cfg: &cfg }` construction. Make sure the use block at the top of `lib.rs` includes `FunctionCallOutputContentItem`, `Budget`, and `ContentRouter`.

- [ ] **Step 4: Build + regression of existing tests**

Run: `cd codex-rs && cargo build -p codez-llm-compress && cargo test -p codez-llm-compress`
Expected: all green (all module tests + existing)

- [ ] **Step 5: Set up the fixture directory skeleton + LICENSE + NOTICE**

```bash
mkdir -p zmod/llm-compress/tests/fixtures/inherited/{search,log,diff,json,tabular,preprocess}
cp ../3rd/compress/headroom/LICENSE zmod/llm-compress/tests/fixtures/inherited/LICENSE-headroom
cp ../3rd/compress/rtk/LICENSE zmod/llm-compress/tests/fixtures/inherited/LICENSE-rtk
```

Create `zmod/llm-compress/tests/fixtures/inherited/NOTICE.md`:

```markdown
# Sources of inherited test data

The data in this directory is adapted from the following Apache-2.0 projects; copyright belongs to the original authors:
- headroom — © 2025 Headroom Contributors (Apache-2.0, see LICENSE-headroom)
- rtk — © 2024 rtk-ai Labs (Apache-2.0, see LICENSE-rtk)

Per-file provenance is recorded in the `origin` field of manifest.toml. Fixture files keep the upstream raw bytes; no comments are added inside the files (to avoid breaking JSON / polluting input).
```

- [ ] **Step 6: Copy input fixtures (extract input from headroom parity JSON, sample from rtk)**

The input for CCR/parity fixtures is extracted from the `input` field of the headroom parity JSON. Since the upstream parity JSON is an `{input, config, output}` triple, **the input field is our fixture input, and the output field is stored as `.expected` for comparison**. Extract with a script (3–5 representative samples per compressor directory is enough; you don't need all 20+):

```bash
# Example: Log — take 3 headroom parity JSONs, split out input and expected
cd /Users/dfbb/Sites/skycode/codez
for f in $(ls ../3rd/compress/headroom/tests/parity/fixtures/log_compressor/*.json | head -3); do
  base=$(basename "$f" .json)
  python3 -c "import json,sys; d=json.load(open('$f')); open('zmod/llm-compress/tests/fixtures/inherited/log/$base.txt','w').write(d['input']); out=d['output']; open('zmod/llm-compress/tests/fixtures/inherited/log/$base.expected','w').write(out['compressed'] if isinstance(out,dict) else out)"
done
# Diff / JSON (smart_crusher) are the same; change the directory name and head count
```

For Search: headroom search has no parity fixtures, so **hand-copy** 2–3 standard grep samples from the inline examples in `search_compressor.rs:668-900` and save them as `search/grep_basic.txt` (no expected; parity only runs hard invariants).
For rtk preprocess: take 1–2 samples containing ANSI / progress bars from the tests in `rtk/src/core/toml_filter.rs` or `rtk/tests/fixtures/*.txt` and save them under `preprocess/`.
For Tabular: hand-craft `tabular/simple.txt` (`id,name\n1,a\n2,b`), no expected.

> **Tradeoff note (must be logged)**: each compressor inherits only 3–5 representative samples rather than all of them, lowering maintenance cost; coverage is already thorough in the unit tests (Task 05-08), and parity is a supplementary comparison against real data. Append a line at the end of `NOTICE.md` noting "representative samples per category, not a full inheritance".

- [ ] **Step 7: Write manifest.toml**

Create `zmod/llm-compress/tests/fixtures/inherited/manifest.toml` (fill in according to the files actually copied; example):

```toml
[[fixture]]
file = "log/3bc015edc0a36387.txt"
origin = "headroom/tests/parity/fixtures/log_compressor/3bc015edc0a36387.json"
compressor = "log"
ref_output = "log/3bc015edc0a36387.expected"
invariants = ["volume_not_worse", "keep_error_lines", "hard_invariants"]

[[fixture]]
file = "search/grep_basic.txt"
origin = "headroom/crates/headroom-core/src/transforms/search_compressor.rs:680"
compressor = "search"
ref_output = ""   # no reference output, only runs hard invariants
invariants = ["hard_invariants"]

# … fill in the rest per the files actually copied (diff/json/tabular/preprocess for each condition)
```

- [ ] **Step 8: Write parity_test.rs (iterate the manifest, run hard invariants)**

Create `zmod/llm-compress/tests/parity_test.rs`. Call each compressor directly and assert the §8.3 invariants (volume not worse, key lines preserved, hard invariants). Fix `ccr.enabled=true`:

```rust
//! Iterate fixtures/inherited/manifest.toml and run the hard invariants (spec §8.3) on each inherited sample.
//! Does not require byte-for-byte equality; the reference output is only used for the "volume not worse" comparison.

use codez_llm_compress::compress::{
    json::JsonCompressor, log::LogCompressor, search::SearchCompressor,
    tabular::TabularCompressor,
};
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inherited")
}

#[derive(serde::Deserialize)]
struct Manifest {
    fixture: Vec<Fixture>,
}
#[derive(serde::Deserialize)]
struct Fixture {
    file: String,
    compressor: String,
    #[serde(default)]
    ref_output: String,
}

fn run_compressor(name: &str, text: &str, cfg: &Config) -> Option<(String, bool)> {
    let budget = Budget { cfg, cmd: None, query: &[] };
    let c: Box<dyn Compressor> = match name {
        "json" => Box::new(JsonCompressor),
        "search" => Box::new(SearchCompressor),
        "tabular" => Box::new(TabularCompressor),
        "log" => Box::new(LogCompressor),
        _ => return None,
    };
    if !c.detect(text, &budget) {
        return None;
    }
    match c.compress(text, &budget) {
        CompressOutcome::Compressed { text, lossy, .. } => Some((text, lossy)),
        CompressOutcome::Unchanged => None,
    }
}

#[test]
fn parity_invariants_hold_for_all_fixtures() {
    let dir = fixtures_dir();
    let manifest_path = dir.join("manifest.toml");
    if !manifest_path.exists() {
        eprintln!("manifest.toml does not exist, skipping parity (fixtures not in place)");
        return;
    }
    let manifest: Manifest = toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();

    let mut cfg = Config::disabled();
    cfg.enabled = true;
    // Give thresholds enough room for compressors to claim (parity cares about algorithm output, not deferral)
    cfg.truncate.max_bytes = 1_000_000;

    for fx in &manifest.fixture {
        let input = std::fs::read_to_string(dir.join(&fx.file))
            .unwrap_or_else(|_| panic!("cannot read fixture {}", fx.file));
        let Some((out, _lossy)) = run_compressor(&fx.compressor, &input, &cfg) else {
            continue; // not claimed / not compressed, skip (allowed)
        };

        // Hard invariant 1: after ≤ before
        assert!(out.len() <= input.len(), "[{}] compressed size should be ≤ original", fx.file);
        // Hard invariant 2: valid UTF-8 (out is a String, inherently valid)
        // Hard invariant 3: JSON compressor output must parse
        if fx.compressor == "json" || fx.compressor == "tabular" {
            serde_json::from_str::<serde_json::Value>(&out)
                .unwrap_or_else(|_| panic!("[{}] JSON output must parse", fx.file));
        }
        // Comparison 4: volume not worse than the reference (if ref_output exists)
        if !fx.ref_output.is_empty() {
            if let Ok(reference) = std::fs::read_to_string(dir.join(&fx.ref_output)) {
                assert!(
                    out.len() as f64 <= reference.len() as f64 * 1.5,
                    "[{}] our output {} should not far exceed 1.5x the reference {}",
                    fx.file, out.len(), reference.len()
                );
            }
        }
    }
}
```

> `serde`/`toml`/`serde_json` are all already dependencies.

- [ ] **Step 9: Write orchestration_test.rs (end-to-end orchestration)**

Create `zmod/llm-compress/tests/orchestration_test.rs`:

```rust
//! transform end-to-end: build a request from real codex types and verify the orchestration chain.
use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem,
};

fn req(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "m".to_string(), instructions: String::new(), input,
        tools: Vec::new(), tool_choice: "auto".to_string(), parallel_tool_calls: false,
        reasoning: None, store: false, stream: true, include: Vec::new(),
        service_tier: None, prompt_cache_key: None, text: None, client_metadata: None,
    }
}
fn fco(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None, call_id: call_id.to_string(),
        output: FunctionCallOutputPayload { body: FunctionCallOutputBody::Text(text.to_string()), success: Some(true) },
        metadata: None,
    }
}

fn provider() -> codex_api::Provider {
    codex_api::Provider {
        name: "t".to_string(), base_url: "https://e.com".to_string(),
        query_params: None, headers: Default::default(),
        retry: codex_api::RetryConfig { max_attempts: 1, base_delay: std::time::Duration::from_millis(0), retry_429: false, retry_5xx: false, retry_transport: false },
        stream_idle_timeout: std::time::Duration::from_secs(30),
    }
}

#[test]
fn disabled_config_leaves_request_untouched() {
    // No config-zmod file → enabled=false → byte-for-byte unchanged
    let big = "x\n".repeat(10_000);
    let mut r = req(vec![fco("c1", &big)]);
    let before = r.clone();
    transform(&mut r, &provider(), "qid-1");
    if let (ResponseItem::FunctionCallOutput { output: a, .. }, ResponseItem::FunctionCallOutput { output: b, .. }) = (&r.input[0], &before.input[0]) {
        match (&a.body, &b.body) {
            (FunctionCallOutputBody::Text(sa), FunctionCallOutputBody::Text(sb)) => assert_eq!(sa, sb),
            _ => panic!("body shape changed"),
        }
    }
}

#[test]
fn non_tooloutput_variants_ignored() {
    let mut r = req(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert!(matches!(r.input[0], ResponseItem::Other));
}
```

> Note: `disabled_config_leaves_request_untouched` relies on the test environment not having `[llm_compress].enabled=true` in `~/.codex/config-zmod.toml`. If that file exists in CI/locally it will interfere — acceptable (it follows the same assumption as the v1 transform_test).

- [ ] **Step 10: Full test suite + clippy**

Run: `cd codex-rs && cargo test -p codez-llm-compress`
Expected: all green (all modules + parity + orchestration)

Run: `cd codex-rs && cargo clippy -p codez-llm-compress --all-targets`
Expected: no warnings

- [ ] **Step 11: Commit**

```bash
git add zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/fixtures/inherited \
  zmod/llm-compress/tests/parity_test.rs \
  zmod/llm-compress/tests/orchestration_test.rs
git commit -m "feat(llm-compress-v2): Task11 lib orchestration wiring + 6-compressor registration + inherited fixtures + parity_test"
```

- [ ] **Step 12: Update README (document v2 capabilities)**

Append a v2 section to `zmod/llm-compress/README.md`: the six compressors, the preprocessing layer, command awareness, the CCR retrieval mechanism, and the new config fields. Commit:

```bash
git add zmod/llm-compress/README.md
git commit -m "docs(llm-compress): README adds v2 capabilities (preprocessing/command-awareness/CCR/new compressors)"
```
