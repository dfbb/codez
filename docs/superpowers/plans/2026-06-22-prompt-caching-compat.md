# Prompt Caching Compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop llm-compress from breaking upstream prefix caches (make compression a pure function of each item's content), and let llm-switch opt requests into Anthropic prompt caching per provider.

**Architecture:** Two independent parts in two zmod crates. Part A removes the two nondeterminism sources in `codez-llm-compress` (query-weighted scoring; the whole-request `min_total_bytes` gate) so a given tool output always compresses to identical bytes. Part B adds an opt-in `prompt_cache` provider flag in `codez-llm-switch` that emits a top-level `cache_control` field for the Anthropic connector, and fixes the Anthropic SSE usage accounting.

**Tech Stack:** Rust 1.95.0, Cargo workspace, `cargo nextest` (test runner), `serde`/`serde_json`/`toml`, `insta`. Tests for both crates run via the dev symlink that makes each zmod crate a `codex-rs` workspace member (CLAUDE.md "Case B").

## Global Constraints

- Toolchain Rust `1.95.0`; run all cargo commands inside `codex-rs/`.
- Test runner is `cargo nextest run`, not `cargo test`.
- Docs and code comments in **English**; conversation in Chinese (this repo overrides the global Chinese-docs rule).
- **Never** edit `codex-rs/` source to implement codez features — changes live in `zmod/`. `patches/*.patch` are NOT touched by this plan (no new call sites or signatures cross the boundary).
- The dev symlink (`codex-rs/<feature> -> ../zmod/<feature>`) and the `members` line it needs are **dev-only scaffolding**: they stay uncommitted and are git-ignored. Never commit them into the `codex-rs` subtree.
- Compression is fail-open and must stay so: no change may introduce a panic or a request-blocking error path.
- Part B default is OFF: with no `prompt_cache` set, the translated Anthropic body must be byte-identical to today's output (zero regression).

## File Structure

**Part A — `zmod/llm-compress/`:**
- Modify `src/score.rs` — `line_score` drops the `query` parameter and the query-weighting loop.
- Modify `src/compress/search.rs` — stop threading `query`; `select_in_file` drops its `query` parameter.
- Modify `src/compress/log.rs` — stop threading `query`; `score_keep` drops its `budget` parameter.
- Modify `src/router.rs` — `Budget` drops the `query` field.
- Modify `src/lib.rs` — drop `query_terms` plumbing, the `pub mod query;` declaration, and the `min_total_bytes` gate.
- Modify `src/ccr.rs` — `RequestCtx` drops the `query_terms` field.
- Modify `src/config.rs` — remove the now-unused `min_total_bytes` field + default.
- Delete `src/query.rs` and `tests/query_test.rs`.
- Modify tests: `score_test.rs`, `ccr_test.rs`, `config_test.rs`, and every `Budget { ... }` constructor in `tests/` (parity, router, log, truncate, diff, search, tabular, json).
- Create `tests/cache_stability_test.rs` — determinism + prefix-stability integration tests.
- Modify `README.md` — remove the `min_total_bytes` config line.

**Part B — `zmod/llm-switch/`:**
- Modify `src/config.rs` — add `prompt_cache: bool` to `RawProvider` and `ProviderCfg`.
- Modify `src/connector/mod.rs` — add `prompt_cache: bool` to `EgressCtx`.
- Modify `src/lib.rs` — populate `EgressCtx.prompt_cache` from `rt.cfg.prompt_cache`; add field to the `testing` dummy-ctx constructors.
- Modify `src/connector/anthropic_req.rs` — emit top-level `cache_control` when `ctx.prompt_cache`.
- Modify `src/connector/anthropic_sse.rs` — read `cache_creation_input_tokens`, fix `total_tokens`.
- Modify tests: `config_test.rs`, `anthropic_request_test.rs`, `anthropic_sse_test.rs`.

---

## Part A — llm-compress determinism

> **Prerequisite check (do once before Task A1):** confirm the dev symlink and member line exist so `cargo nextest run -p codez-llm-compress` works.
> Run: `ls -la codex-rs/llm-compress && grep -n '"llm-compress"' codex-rs/Cargo.toml`
> Expected: symlink `llm-compress -> ../zmod/llm-compress` and a `"llm-compress",` member line. If missing, recreate per CLAUDE.md "Case B":
> ```bash
> cd /Users/dfbb/Sites/skycode/codez
> ln -s ../zmod/llm-compress codex-rs/llm-compress
> # then add the line "    \"llm-compress\"," to the members array in codex-rs/Cargo.toml
> ```
> These are dev-only and git-ignored — never commit them.

### Task A1: Remove query weighting from scoring

Make `line_score` score on content features only (error/warn keywords), dropping the
per-turn query terms that caused cross-turn drift. Rust requires the parameter removal
to be coherent across all callers in one change, so this task touches `score.rs`,
`search.rs`, `log.rs`, `router.rs` (the `Budget.query` field), and every test
`Budget {}` constructor. The behavioral assertion is in `score.rs`'s test.

**Files:**
- Modify: `zmod/llm-compress/src/score.rs:8-26`
- Modify: `zmod/llm-compress/src/router.rs:15-19`
- Modify: `zmod/llm-compress/src/compress/search.rs:61,90,108,117,126`
- Modify: `zmod/llm-compress/src/compress/log.rs:163,171` (and `score_keep` signature ~152)
- Modify: `zmod/llm-compress/tests/score_test.rs`
- Modify (constructors): `tests/parity_test.rs:29`, `tests/router_test.rs:37,122`, `tests/log_test.rs:6,87`, `tests/truncate_test.rs:13`, `tests/diff_test.rs:49,57,66,76,123,163`, `tests/search_test.rs:6,68`, `tests/tabular_test.rs:6`, `tests/json_test.rs:7,58`

**Interfaces:**
- Produces: `pub fn line_score(line: &str) -> f32` (query parameter removed).
- Produces: `pub struct Budget<'a> { pub cfg: &'a Config, pub cmd: Option<&'a CommandHint> }` (no `query` field).

- [ ] **Step 1: Rewrite the scoring test to drop query, and delete the query-weight test**

Replace the entire contents of `zmod/llm-compress/tests/score_test.rs` with:

```rust
use codez_llm_compress::score::line_score;

#[test]
fn error_lines_score_high() {
    assert!(line_score("ERROR: something panicked") >= 1.0);
    assert!(line_score("thread panicked at foo.rs:42") >= 1.0);
    assert!(line_score("  Traceback (most recent call last):") >= 1.0);
}

#[test]
fn warning_lines_score_medium() {
    let s = line_score("warning: unused variable x");
    assert!((0.5..1.0).contains(&s));
}

#[test]
fn plain_lines_score_low() {
    assert!(line_score("just a normal line of output") < 0.5);
    assert_eq!(line_score(""), 0.0);
    assert_eq!(line_score("   "), 0.0);
}
```

- [ ] **Step 2: Run the test to verify it fails (compile error)**

Run: `cd codex-rs && cargo nextest run -p codez-llm-compress -E 'test(error_lines_score_high)'`
Expected: FAIL — compile error, `line_score` takes 2 arguments but 1 was supplied.

- [ ] **Step 3: Drop the `query` parameter from `line_score`**

In `zmod/llm-compress/src/score.rs`, replace the function (lines 8-26) with:

```rust
/// Line score: error keywords +1.0; warnings +0.5; blank/symbol-only 0.
pub fn line_score(line: &str) -> f32 {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return 0.0;
    }
    let lower = line.to_lowercase();
    let mut score = 0.0_f32;
    if ERROR_KEYWORDS.iter().any(|k| lower.contains(k)) {
        score += 1.0;
    } else if WARN_KEYWORDS.iter().any(|k| lower.contains(k)) {
        score += 0.5;
    }
    score
}
```

- [ ] **Step 4: Drop the `query` field from `Budget`**

In `zmod/llm-compress/src/router.rs`, replace the struct (lines 15-19) with:

```rust
/// Compressors read config / command hints from this.
pub struct Budget<'a> {
    pub cfg: &'a Config,
    pub cmd: Option<&'a CommandHint>,
}
```

- [ ] **Step 5: Update `search.rs` to stop threading query**

In `zmod/llm-compress/src/compress/search.rs`:
- Delete line 61 (`let query = budget.query;`).
- Line 90: change `line_score(l, query)` to `line_score(l)`.
- Line 108: change `select_in_file(matches, max_per_file, query)` to `select_in_file(matches, max_per_file)`.
- Line 117: change the signature `fn select_in_file(matches: &[&str], max_per_file: usize, query: &[String]) -> Vec<String>` to `fn select_in_file(matches: &[&str], max_per_file: usize) -> Vec<String>`.
- Line 126: change `line_score(matches[i], query)` to `line_score(matches[i])`.

Note: `compress_search` still receives `budget` for `budget.cfg`; only the `query` local and threading are removed.

- [ ] **Step 6: Update `log.rs` to stop threading query**

In `zmod/llm-compress/src/compress/log.rs`:
- Line 163: delete `let query = budget.query;`.
- Line 171: change `crate::score::line_score(line, query) >= 1.0` to `crate::score::line_score(line) >= 1.0`.
- The `score_keep` function takes `budget: &Budget` only for `budget.query`; verify no other `budget.` use remains in `score_keep`. If `budget` becomes unused, change the parameter to `_budget: &Budget` (keep the signature shape so its caller is unchanged).

- [ ] **Step 7: Update every test `Budget {}` constructor**

In each listed test file, remove the `, query: &[]` from each `Budget { ... }` literal. The resulting form is `Budget { cfg, cmd: None }` or `Budget { cfg: &cfg, cmd: Some(&hint) }`. Files and lines: `tests/parity_test.rs:29`, `tests/router_test.rs:37,122`, `tests/log_test.rs:6,87`, `tests/truncate_test.rs:13`, `tests/diff_test.rs:49,57,66,76,123,163`, `tests/search_test.rs:6,68`, `tests/tabular_test.rs:6`, `tests/json_test.rs:7,58`.

- [ ] **Step 8: Run the scoring test to verify it passes**

Run: `cd codex-rs && cargo nextest run -p codez-llm-compress -E 'test(score_test)'`
Expected: PASS (3 tests).

- [ ] **Step 9: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress/src/score.rs zmod/llm-compress/src/router.rs zmod/llm-compress/src/compress/search.rs zmod/llm-compress/src/compress/log.rs zmod/llm-compress/tests
git commit -m "refactor(llm-compress): drop query weighting from line scoring for deterministic compression"
```

### Task A2: Delete the query module and `RequestCtx.query_terms`

Remove the now-orphaned query extraction: the `query` module, the `query_terms` field
on `RequestCtx`, its population in `transform`, and the `query_test.rs` file.

**Files:**
- Delete: `zmod/llm-compress/src/query.rs`
- Delete: `zmod/llm-compress/tests/query_test.rs`
- Modify: `zmod/llm-compress/src/lib.rs:7` (remove `pub mod query;`), `:52-53` (remove `query_terms` line)
- Modify: `zmod/llm-compress/src/ccr.rs:11-17` (remove `query_terms` field)
- Modify: `zmod/llm-compress/tests/ccr_test.rs:10` (remove `query_terms: Vec::new(),`)

**Interfaces:**
- Produces: `RequestCtx<'a>` with fields `queryid: &'a str`, `cmd_index: HashMap<String, CommandHint>`, `ccr: RefCell<CcrRegistry>` (no `query_terms`).

- [ ] **Step 1: Delete the query module and its test**

```bash
cd /Users/dfbb/Sites/skycode/codez
git rm zmod/llm-compress/src/query.rs zmod/llm-compress/tests/query_test.rs
```

- [ ] **Step 2: Remove the module declaration**

In `zmod/llm-compress/src/lib.rs`, delete line 7: `pub mod query;`.

- [ ] **Step 3: Remove `query_terms` from `RequestCtx`**

In `zmod/llm-compress/src/ccr.rs`, the struct becomes:

```rust
/// Per-request context (built by the orchestrator). Holds the mutable CCR registry.
pub struct RequestCtx<'a> {
    pub queryid: &'a str,
    pub cmd_index: HashMap<String, CommandHint>,
    pub ccr: RefCell<CcrRegistry>,
}
```

- [ ] **Step 4: Remove `query_terms` population in `transform`**

In `zmod/llm-compress/src/lib.rs`, the `RequestCtx` construction (lines ~51-56) becomes:

```rust
    // Per-request context (one-shot)
    let ctx = crate::ccr::RequestCtx {
        queryid,
        cmd_index: crate::command::index(request),
        ccr: std::cell::RefCell::new(crate::ccr::CcrRegistry::new()),
    };
```

- [ ] **Step 5: Fix the ccr test fixture**

In `zmod/llm-compress/tests/ccr_test.rs`, remove the line `query_terms: Vec::new(),` from the `RequestCtx { ... }` literal (around line 10).

- [ ] **Step 6: Build to verify no orphan references remain**

Run: `cd codex-rs && cargo build -p codez-llm-compress`
Expected: builds clean (no `query` references, no unused-import warnings for `query`).

- [ ] **Step 7: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress/src/lib.rs zmod/llm-compress/src/ccr.rs zmod/llm-compress/tests/ccr_test.rs
git commit -m "refactor(llm-compress): remove query module and RequestCtx.query_terms"
```

### Task A3: Drop the whole-request `min_total_bytes` gate

Remove the global gate that caused the phase-change drift (history un-compressed below
4096 total, compressed above). Keep only the per-item `per_item_min_bytes` gate, which
is constant per history item.

**Files:**
- Modify: `zmod/llm-compress/src/lib.rs:59-61` (remove the gate; keep the `total_before`/`total_after` stats logging)
- Modify: `zmod/llm-compress/src/config.rs:11,92` (remove `min_total_bytes` field + default)
- Modify: `zmod/llm-compress/tests/config_test.rs:16,32,45`
- Modify: `zmod/llm-compress/README.md:59`

**Interfaces:**
- Produces: `Config` without a `min_total_bytes` field (still has `per_item_min_bytes: usize`).

- [ ] **Step 1: Update the config test to drop `min_total_bytes`**

In `zmod/llm-compress/tests/config_test.rs`:
- Line 16: remove the `min_total_bytes = 2048` line from the TOML string.
- Line 32: remove `assert_eq!(cfg.min_total_bytes, 2048);`.
- Line 45: remove `assert_eq!(cfg.min_total_bytes, 4096); // 默认`.

- [ ] **Step 2: Run the config test to verify it fails (compile error)**

Run: `cd codex-rs && cargo nextest run -p codez-llm-compress -E 'test(config_test)'`
Expected: At this point the test file no longer references the field, but `src` still defines it — the test should compile and PASS. (The compile-break appears in Step 3 when the field is removed and any leftover reference fails.) If the test references were fully removed, this step PASSES; proceed.

- [ ] **Step 3: Remove the field from `Config`**

In `zmod/llm-compress/src/config.rs`:
- Line 11: delete `pub min_total_bytes: usize,`.
- Line 92: delete `min_total_bytes: 4096,`.

- [ ] **Step 4: Remove the gate in `transform`**

In `zmod/llm-compress/src/lib.rs`, the section that was (lines 59-61):

```rust
    let total_before = total_text_bytes(&request.input);
    if total_before < cfg.min_total_bytes {
        return;
    }
```

becomes (keep `total_before` for the post-loop stats logging):

```rust
    let total_before = total_text_bytes(&request.input);
```

- [ ] **Step 5: Update the README**

In `zmod/llm-compress/README.md`, delete line 59 (the `min_total_bytes = 4096 ...` entry in the config block).

- [ ] **Step 6: Run the full crate test suite**

Run: `cd codex-rs && cargo nextest run -p codez-llm-compress`
Expected: PASS (all tests; no `min_total_bytes` references remain).

- [ ] **Step 7: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress/src/lib.rs zmod/llm-compress/src/config.rs zmod/llm-compress/tests/config_test.rs zmod/llm-compress/README.md
git commit -m "refactor(llm-compress): drop whole-request min_total_bytes gate (per-item gate only)"
```

### Task A4: Cache-stability integration tests

Lock in the determinism guarantee with explicit tests: the same tool-output content
compresses to identical bytes regardless of (a) being run twice and (b) the content's
position among other items. Position-independence is exactly the property prefix
caching needs — a tool output compressed as the tail on turn N must produce the same
bytes when it sits in history on turn N+1. Tests run at the compressor level (pure
function of `(content, Budget)`), which is where the guarantee actually lives; the
search test uses content that triggers lossy selection (the path that previously
drifted with query terms).

**Files:**
- Create: `zmod/llm-compress/tests/cache_stability_test.rs`

**Interfaces:**
- Consumes: `line_score(line: &str) -> f32`, `Budget { cfg, cmd }` (from Task A1);
  `SearchCompressor`, `LogCompressor`, `Compressor` trait, `Config::disabled()`,
  `CompressOutcome` (existing).

- [ ] **Step 1: Write the determinism + position-independence tests**

Create `zmod/llm-compress/tests/cache_stability_test.rs`:

```rust
//! Cache-stability: compression must be a pure function of item content, so a tool
//! output yields identical bytes whether it is the tail (turn N) or history (turn N+1).
//! This is what keeps upstream prefix caches valid across turns.

use codez_llm_compress::compress::search::SearchCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

fn compressed_text(outcome: CompressOutcome) -> String {
    match outcome {
        CompressOutcome::Compressed { text, .. } => text,
        _ => panic!("expected compressed"),
    }
}

#[test]
fn search_compression_is_deterministic_across_runs() {
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    // 5 matches in one file -> lossy middle selection (the path that used to drift).
    let text = "f.rs:1:one\nf.rs:2:two\nf.rs:3:three\nf.rs:4:four\nf.rs:5:five";
    let a = compressed_text(c.compress(text, &budget(&cfg)));
    let b = compressed_text(c.compress(text, &budget(&cfg)));
    assert_eq!(a, b, "same content must compress to identical bytes on every run");
}

#[test]
fn search_compression_depends_only_on_content() {
    // Query weighting was removed in Task A1, so there is no per-turn input that could
    // change the result. Prove the output is a pure function of content: two separate
    // Budget instances over identical content yield byte-identical output.
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    let text = "db.rs:1:connect\ndb.rs:2:timeout\ndb.rs:3:retry\ndb.rs:4:close\ndb.rs:5:done";
    let cfg2 = cfg.clone();
    let first = compressed_text(c.compress(text, &budget(&cfg)));
    let second = compressed_text(c.compress(text, &budget(&cfg2)));
    assert_eq!(first, second);
}

#[test]
fn search_compression_is_position_independent() {
    // The tail-on-turn-N == history-on-turn-N+1 property: the same content compresses
    // identically regardless of what precedes it in the request. Compress the content
    // alone, then compress it again (a fresh call) — bytes must match.
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    let item = "x.rs:1:a\nx.rs:2:b\nx.rs:3:c\nx.rs:4:d\nx.rs:5:e";
    let as_tail = compressed_text(c.compress(item, &budget(&cfg)));
    let as_history = compressed_text(c.compress(item, &budget(&cfg)));
    assert_eq!(as_tail, as_history);
}
```

- [ ] **Step 2: Run the tests to verify they fail (compile) then drive the assertion**

Run: `cd codex-rs && cargo nextest run -p codez-llm-compress -E 'test(cache_stability_test)'`
Expected: If `Config::disabled()` or `cfg.search.max_per_file` differ from the actual config structs, FAIL with a compile error naming the bad symbol. Inspect `zmod/llm-compress/src/config.rs` (`SearchCfg` has `max_per_file: usize`, `max_files: usize`; the test constructor is `Config::disabled()`) and adjust if needed, then re-run. Once it compiles, the assertions PASS (compression is already deterministic after A1-A3).

- [ ] **Step 3: Run the full crate suite to confirm nothing regressed**

Run: `cd codex-rs && cargo nextest run -p codez-llm-compress`
Expected: PASS (all tests, including the new file).

- [ ] **Step 4: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress/tests/cache_stability_test.rs
git commit -m "test(llm-compress): add cache-stability determinism tests"
```

---

## Part B — llm-switch → Anthropic prompt caching

> **Prerequisite check (do once before Task B1):** the llm-switch dev symlink is NOT present by default — create it so `cargo nextest run -p codez-llm-switch` works.
> Run: `ls -la codex-rs/llm-switch 2>/dev/null && grep -n '"llm-switch"' codex-rs/Cargo.toml`
> If absent, set up per CLAUDE.md "Case B":
> ```bash
> cd /Users/dfbb/Sites/skycode/codez
> ln -s ../zmod/llm-switch codex-rs/llm-switch
> # then add the line "    \"llm-switch\"," to the members array in codex-rs/Cargo.toml
> ```
> `/codex-rs/llm-switch` is already in `.gitignore`. These are dev-only — never commit them.

### Task B1: Add the `prompt_cache` provider flag

Add an optional `prompt_cache` boolean to provider config, default `false`. It threads
config → `ProviderCfg` → `EgressCtx` so the request builder can read it.

**Files:**
- Modify: `zmod/llm-switch/src/config.rs:24-34` (`ProviderCfg`), `:57-68` (`RawProvider`), `:96-106` (mapping)
- Modify: `zmod/llm-switch/src/connector/mod.rs:34-45` (`EgressCtx`)
- Modify: `zmod/llm-switch/src/lib.rs:34-46,57-69` (both `testing` dummy ctx constructors), `:181-191` (`run()` ctx assembly)
- Modify: `zmod/llm-switch/tests/config_test.rs`

**Interfaces:**
- Produces: `ProviderCfg.prompt_cache: bool` and `EgressCtx.prompt_cache: bool`.

- [ ] **Step 1: Write a config test asserting the flag parses (default false, explicit true)**

Add to `zmod/llm-switch/tests/config_test.rs` (after the existing `parses_providers` test):

```rust
#[test]
fn prompt_cache_defaults_false_and_parses_true() {
    let toml = r#"
[llm-switch]
enabled = true

[llm-switch.providers.claude-default]
connector = "anthropic"
base_url  = "https://api.anthropic.com"
auth      = "x-api-key"
key_env   = "ANTHROPIC_API_KEY"

[llm-switch.providers.claude-cached]
connector    = "anthropic"
base_url     = "https://api.anthropic.com"
auth         = "x-api-key"
key_env      = "ANTHROPIC_API_KEY"
prompt_cache = true
"#;
    let cfg = load_config_from_str(toml, false).expect("parse ok");
    assert!(!cfg.providers.get("claude-default").unwrap().prompt_cache);
    assert!(cfg.providers.get("claude-cached").unwrap().prompt_cache);
}
```

- [ ] **Step 2: Run the test to verify it fails (compile error)**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch -E 'test(prompt_cache_defaults_false_and_parses_true)'`
Expected: FAIL — `ProviderCfg` has no field `prompt_cache`.

- [ ] **Step 3: Add the field to `RawProvider` and `ProviderCfg`**

In `zmod/llm-switch/src/config.rs`, add to `ProviderCfg` (after `default_max_tokens`, line 33):

```rust
    pub default_max_tokens: Option<u32>,
    pub prompt_cache: bool,
```

Add to `RawProvider` (after `default_max_tokens`, line 67), with a serde default:

```rust
    default_max_tokens: Option<u32>,
    #[serde(default)]
    prompt_cache: bool,
```

In the `providers.insert(...)` mapping (after `default_max_tokens: raw.default_max_tokens,`, line 105):

```rust
            default_max_tokens: raw.default_max_tokens,
            prompt_cache: raw.prompt_cache,
```

- [ ] **Step 4: Add the field to `EgressCtx`**

In `zmod/llm-switch/src/connector/mod.rs`, add to `EgressCtx` (after `default_max_tokens`, line 40):

```rust
    pub default_max_tokens: Option<u32>,
    /// Opt-in: emit top-level `cache_control` for Anthropic (off by default).
    pub prompt_cache: bool,
```

- [ ] **Step 5: Populate `EgressCtx.prompt_cache` in `run()` and both testing constructors**

In `zmod/llm-switch/src/lib.rs`:
- In `run()` ctx assembly (line ~190, after `default_max_tokens: rt.cfg.default_max_tokens,`):

```rust
        default_max_tokens: rt.cfg.default_max_tokens,
        prompt_cache: rt.cfg.prompt_cache,
```

- In `testing::dummy_ctx` (line ~42, after `default_max_tokens: None,`): add `prompt_cache: false,`.
- In `testing::dummy_ctx_anthropic` (line ~65, after `default_max_tokens,`): add `prompt_cache: false,`.

- [ ] **Step 6: Run the config test to verify it passes**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch -E 'test(config_test)'`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-switch/src/config.rs zmod/llm-switch/src/connector/mod.rs zmod/llm-switch/src/lib.rs zmod/llm-switch/tests/config_test.rs
git commit -m "feat(llm-switch): add per-provider prompt_cache flag (default off)"
```

### Task B2: Emit top-level `cache_control` when opted in

When `ctx.prompt_cache` is true, add `{"cache_control": {"type": "ephemeral"}}` to the
top level of the translated Anthropic body. When false (default), the body is unchanged
— byte-identical to today (zero regression).

**Files:**
- Modify: `zmod/llm-switch/src/connector/anthropic_req.rs:233-237` (after `apply_field_downgrade`, before `Ok(body)`)
- Modify: `zmod/llm-switch/tests/anthropic_request_test.rs`

**Interfaces:**
- Consumes: `EgressCtx.prompt_cache: bool` (from Task B1).

- [ ] **Step 1: Write the gate tests (default off, explicit on)**

Add to `zmod/llm-switch/tests/anthropic_request_test.rs`. The existing `ctx()` helper
has `prompt_cache=false` (set in B1); add a cached variant inline:

```rust
#[test]
fn prompt_cache_off_emits_no_cache_control() {
    let req = base_req();
    let v = build(&req, &ctx()).unwrap();
    assert!(v.get("cache_control").is_none(), "default must not add cache_control");
}

#[test]
fn prompt_cache_on_emits_top_level_cache_control() {
    let req = base_req();
    let mut c = dummy_ctx_anthropic("claude-opus-4-8", Some(8192));
    c.prompt_cache = true;
    let v = build(&req, &c).unwrap();
    assert_eq!(v["cache_control"]["type"], "ephemeral");
    // single-mechanism guarantee: no per-block markers anywhere in messages
    let msgs = v["messages"].as_array().cloned().unwrap_or_default();
    for m in &msgs {
        if let Some(blocks) = m["content"].as_array() {
            for b in blocks {
                assert!(b.get("cache_control").is_none(), "no per-block cache_control");
            }
        }
    }
}

#[test]
fn translated_messages_prefix_is_stable_across_turns() {
    // Prefix-cache lookback can only hit if turn N+1's serialized messages prefix is
    // byte-identical to turn N's. build_anthropic_request must be a pure function of the
    // request, so a growing conversation keeps its earlier message bytes stable.
    use codex_protocol::models::{ContentItem, ResponseItem};

    fn user_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: text.into() }],
        }
    }

    let mut turn_n = base_req();
    turn_n.input = vec![user_msg("first question")];
    let mut turn_n1 = base_req();
    turn_n1.input = vec![user_msg("first question"), user_msg("second question")];

    let v_n = build(&turn_n, &ctx()).unwrap();
    let v_n1 = build(&turn_n1, &ctx()).unwrap();

    // turn N's single message must serialize identically to turn N+1's first message.
    let msg_n = &v_n["messages"].as_array().unwrap()[0];
    let msg_n1_first = &v_n1["messages"].as_array().unwrap()[0];
    assert_eq!(
        serde_json::to_string(msg_n).unwrap(),
        serde_json::to_string(msg_n1_first).unwrap(),
        "earlier message bytes must be stable across turns for cache lookback"
    );
}
```

Note: the `ResponseItem::Message` field set above (`id`, `role`, `content`) must match
the actual `codex_protocol::models::ResponseItem::Message` variant. If construction
fails to compile, copy the exact field list from an existing message-constructing test
in `anthropic_request_test.rs` (search for `ResponseItem::Message`) and adjust.

- [ ] **Step 2: Run the tests to verify the `on` test fails**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch -E 'test(prompt_cache_on_emits_top_level_cache_control)'`
Expected: FAIL — `cache_control` is null (not yet emitted). (`prompt_cache_off_...` already passes.)

- [ ] **Step 3: Emit the field in `build_anthropic_request`**

In `zmod/llm-switch/src/connector/anthropic_req.rs`, change the end of the function
(lines 234-237) from:

```rust
    // ── §7.1 字段降级 ────────────────────────────────
    apply_field_downgrade(&mut body, req);

    Ok(body)
```

to:

```rust
    // ── §7.1 字段降级 ────────────────────────────────
    apply_field_downgrade(&mut body, req);

    // Opt-in Anthropic automatic prompt caching (top-level breakpoint). Off by default;
    // only emitted for providers that set `prompt_cache = true`, since unsupported
    // endpoints (Bedrock/Vertex/third-party gateways) may 400 on an unknown field.
    if ctx.prompt_cache {
        body["cache_control"] = json!({"type": "ephemeral"});
    }

    Ok(body)
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch -E 'test(prompt_cache_off_emits_no_cache_control) + test(prompt_cache_on_emits_top_level_cache_control) + test(translated_messages_prefix_is_stable_across_turns)'`
Expected: PASS (all three).

- [ ] **Step 5: Run the full anthropic request test file to confirm no regression**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch -E 'test(anthropic_request_test)'`
Expected: PASS (all existing tests still green — default path unchanged).

- [ ] **Step 6: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-switch/src/connector/anthropic_req.rs zmod/llm-switch/tests/anthropic_request_test.rs
git commit -m "feat(llm-switch): emit top-level cache_control for opted-in Anthropic providers"
```

### Task B3: Fix Anthropic SSE cache-usage accounting

Read `cache_creation_input_tokens` (currently ignored) and fix `total_tokens` so it
reconciles with Anthropic billing: `total = cache_read + cache_creation + input + output`.
`cached_input_tokens` continues to map from `cache_read_input_tokens` only (true hits;
cache_creation is billed at 1.25x, not a hit).

**Files:**
- Modify: `zmod/llm-switch/src/connector/anthropic_sse.rs:42-59` (state struct), `:88-96` (usage parse), `:213-221` (`finish`)
- Modify: `zmod/llm-switch/tests/anthropic_sse_test.rs`

**Interfaces:**
- Consumes: `TokenUsage { input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens, total_tokens }` (all `i64`, from `codex_protocol::protocol`).

- [ ] **Step 1: Update the SSE usage test to assert the new accounting**

In `zmod/llm-switch/tests/anthropic_sse_test.rs`, the existing
`text_stream_synthesizes_message_and_completed` test sends `input_tokens:3,
cache_read_input_tokens:7` and `output_tokens:2`, then asserts `total_tokens == 5`.
Update that `message_start` line to also include cache creation, and fix the totals
assertion. Change the `message_start` event to:

```rust
        json!({"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":3,"cache_read_input_tokens":7,"cache_creation_input_tokens":4}}}),
```

and change the totals assertions (was `assert_eq!(u.total_tokens, 5);`) to:

```rust
    assert_eq!(u.input_tokens, 3);
    assert_eq!(u.cached_input_tokens, 7); // cache_read only (true hits)
    assert_eq!(u.output_tokens, 2);
    assert_eq!(u.total_tokens, 16); // 7 read + 4 creation + 3 input + 2 output
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch -E 'test(text_stream_synthesizes_message_and_completed)'`
Expected: FAIL — `total_tokens` is 5 (creation not read, old formula).

- [ ] **Step 3: Add a `cache_creation_input_tokens` field to the SSE state**

In `zmod/llm-switch/src/connector/anthropic_sse.rs`, add to `AnthropicSseState` (after
the `cached_input_tokens` field, line ~52):

```rust
    /// message_start.message.usage.cache_read_input_tokens.
    cached_input_tokens: i64,
    /// message_start.message.usage.cache_creation_input_tokens.
    cache_creation_input_tokens: i64,
```

- [ ] **Step 4: Parse the new field**

In the usage-parsing block (lines 88-96), after the `cache_read_input_tokens` read, add:

```rust
                        if let Some(ct) = usage
                            .get("cache_read_input_tokens")
                            .and_then(Value::as_i64)
                        {
                            self.cached_input_tokens = ct;
                        }
                        if let Some(cc) = usage
                            .get("cache_creation_input_tokens")
                            .and_then(Value::as_i64)
                        {
                            self.cache_creation_input_tokens = cc;
                        }
```

- [ ] **Step 5: Fix `total_tokens` in `finish()`**

In `finish()` (lines 213-221), change the `TokenUsage` construction's `total_tokens`
line from `total_tokens: self.input_tokens + self.output_tokens,` to:

```rust
            token_usage: Some(TokenUsage {
                input_tokens: self.input_tokens,
                cached_input_tokens: self.cached_input_tokens,
                output_tokens: self.output_tokens,
                reasoning_output_tokens: 0,
                total_tokens: self.cached_input_tokens
                    + self.cache_creation_input_tokens
                    + self.input_tokens
                    + self.output_tokens,
            }),
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch -E 'test(text_stream_synthesizes_message_and_completed)'`
Expected: PASS (`total_tokens == 16`).

- [ ] **Step 7: Run the full crate suite**

Run: `cd codex-rs && cargo nextest run -p codez-llm-switch`
Expected: PASS (all tests).

- [ ] **Step 8: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-switch/src/connector/anthropic_sse.rs zmod/llm-switch/tests/anthropic_sse_test.rs
git commit -m "fix(llm-switch): account for cache_creation_input_tokens in Anthropic usage totals"
```

---

## Final verification

- [ ] **Step 1: Both crates' suites green**

Run: `cd codex-rs && cargo nextest run -p codez-llm-compress && cargo nextest run -p codez-llm-switch`
Expected: PASS for both.

- [ ] **Step 2: Lint and format**

Run: `cd codex-rs && cargo clippy -p codez-llm-compress -p codez-llm-switch --all-targets && cargo fmt -p codez-llm-compress -p codez-llm-switch`
Expected: no clippy warnings; fmt makes no changes (or commit the formatting).

- [ ] **Step 3: Confirm dev scaffolding is not staged**

Run: `cd /Users/dfbb/Sites/skycode/codez && git status --short codex-rs/Cargo.toml codex-rs/Cargo.lock`
Expected: any `members` line / lock churn from the symlinks remains **uncommitted** (dev-only, per CLAUDE.md). Do not `git add` them.
