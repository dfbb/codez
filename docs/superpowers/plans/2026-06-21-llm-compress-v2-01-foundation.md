# Task 01 — Interface Foundation: router types + config extension + schema.rs + sync existing compressor signatures

> Belongs to `2026-06-21-llm-compress-v2-00-index.md`. See index for the REQUIRED SUB-SKILL.
> Covers spec §4.0 / §4.1 / §4.8 / §6. This is the interface foundation; all subsequent tasks depend on the types defined here.

**Goal:** Rework the core types in `router.rs` (`ContentKind` / `CompressOutcome` gains `lossy`+`kind` / `Compressor::detect` takes a budget / `Budget` gains cmd+ctx / `compress_text` returns a triple), extend `config.rs` with all the new sub-tables, create `compress/schema.rs`, and sync the signatures of the 4 existing compressors (truncate/json/diff/log) so the crate compiles again.

**By the end of this task:** the crate compiles, existing tests are all green after adjustment, but the new behaviors (yielding/CCR/preprocessing) are not yet wired in — those belong to later tasks.

## Files
- Modify: `zmod/llm-compress/src/router.rs` (fully rework the trait and outcome)
- Modify: `zmod/llm-compress/src/config.rs` (add 5 new sub-tables + fields)
- Create: `zmod/llm-compress/src/compress/schema.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs` (add `pub mod schema;`)
- Modify: `zmod/llm-compress/src/compress/truncate.rs` (detect takes budget; Compressed gains lossy+kind)
- Modify: `zmod/llm-compress/src/compress/json.rs` (same; this task is signature-only, the algorithm changes in Task 05)
- Modify: `zmod/llm-compress/src/compress/diff.rs` (same)
- Modify: `zmod/llm-compress/src/compress/log.rs` (same; this task is signature-only, the algorithm changes in Task 08)
- Modify: `zmod/llm-compress/src/lib.rs` (`compress_in_place` adapts to the new return type, transitional wiring)
- Modify: `zmod/llm-compress/Cargo.toml` (add the `sha2` dependency, for later ccr/schema use)
- Test: sync assertions in existing `tests/router_test.rs`, `tests/truncate_test.rs`, `tests/json_test.rs`, `tests/diff_test.rs`, `tests/log_test.rs`

**Interfaces:**
- Produces (consumed by later tasks):
  - `pub enum ContentKind { Text, Json }` (derive `Clone, Copy, Debug, PartialEq, Eq`)
  - `pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize, lossy: bool, kind: ContentKind }, Unchanged }`
  - `pub trait Compressor { fn name(&self)->&'static str; fn detect(&self,text:&str,budget:&Budget)->bool; fn compress(&self,text:&str,budget:&Budget)->CompressOutcome; }`
  - `pub struct Budget<'a> { pub cfg: &'a Config, pub cmd: Option<&'a CommandHint>, pub ctx: Option<&'a RequestCtx> }` (Task 03 defines `CommandHint`, Task 11 defines `RequestCtx`; this task uses a forward declaration placeholder, see Step 5)
  - `pub fn compress_text(&self, text:&str, budget:&Budget) -> Option<(String, bool, ContentKind)>` (text, lossy, kind)
  - `config::Config` gains sub-tables: `preprocess: PreprocessCfg`, `search: SearchCfg`, `tabular: TabularCfg`, `protect: ProtectCfg`, `ccr: CcrCfg`; `json` gains `csv_schema: bool`; `log` gains `template_min_run: usize`, `keep_levels: Vec<String>`
  - `schema::to_schema_form(value: &serde_json::Value) -> Option<serde_json::Value>`

> **Design decision (required reading for implementers):** `Budget` carries `cfg` + `cmd: Option<&CommandHint>` + `query: &[String]` — a compressor only needs these three (query terms for score weighting, command hints for detect). **Do not stuff `RequestCtx`/the CCR registry into Budget**: CCR is the orchestration layer's concern (Task 11); compressors never touch persistence. The **type definition + is_* methods** of `CommandHint` are established in this task (so Budget can compile); its `index()` parsing function is implemented in Task 03.

- [ ] **Step 1: add the sha2 dependency**

Edit `zmod/llm-compress/Cargo.toml`, and at the end of `[dependencies]` (after the `tracing = "0.1"` line) add:

```toml
sha2 = "0.10"
```

- [ ] **Step 2: confirm the crate currently compiles (baseline)**

Run: `cd codex-rs && cargo build -p codez-llm-compress`
Expected: PASS (the pre-change baseline; if it fails, first confirm the symlink member is in place — see the index's dev-time build)

- [ ] **Step 3: create the CommandHint type skeleton in command.rs (parsing left to Task 03)**

Create `zmod/llm-compress/src/command.rs`:

```rust
//! call_id → command hint. The type and discriminators live here (Task 01 foundation); index() parsing is in Task 03.
use std::collections::HashMap;

/// Command hint parsed from a FunctionCall, used only as a routing hint (spec §4.3).
#[derive(Debug, Clone, Default)]
pub struct CommandHint {
    pub program: String,
    pub argv: Vec<String>,
}

impl CommandHint {
    pub fn is_git_diff(&self) -> bool {
        self.program.ends_with("git") && matches!(self.argv.first().map(String::as_str), Some("diff") | Some("show"))
    }
    pub fn is_grep(&self) -> bool {
        matches!(self.program.rsplit('/').next(), Some("grep") | Some("rg") | Some("ripgrep"))
    }
    pub fn is_ls_like(&self) -> bool {
        matches!(self.program.rsplit('/').next(), Some("ls") | Some("tree") | Some("find"))
    }
    pub fn is_test_runner(&self) -> bool {
        let p = self.program.rsplit('/').next().unwrap_or("");
        matches!(p, "pytest" | "jest" | "vitest" | "cargo" | "go" | "rspec")
    }
}

/// Implemented in Task 03: walk the FunctionCalls in request.input to build a call_id→CommandHint index.
/// For now this task provides an empty placeholder so lib compiles; Task 03 replaces it.
pub fn index(_request: &codex_api::ResponsesApiRequest) -> HashMap<String, CommandHint> {
    HashMap::new()
}
```

- [ ] **Step 4: register the command module in lib.rs**

In the module declaration area of `zmod/llm-compress/src/lib.rs` (near the existing `pub mod config;` etc.) add:

```rust
pub mod command;
```

- [ ] **Step 5: rewrite router.rs**

Fully replace `zmod/llm-compress/src/router.rs` with the following (extending the existing `Budget`/`CompressOutcome`/`Compressor`/`ContentRouter`):

```rust
//! Common compressor contract + ContentRouter (command-aware reordering + first-match + fail-open).

use crate::command::CommandHint;
use crate::config::Config;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Output shape. Json always has lossy=false (the iron rule of spec §4.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Json,
}

/// A compressor draws config/command hints/query terms from here.
pub struct Budget<'a> {
    pub cfg: &'a Config,
    pub cmd: Option<&'a CommandHint>,
    pub query: &'a [String],
}

/// The result of a single compressor processing one chunk of text.
pub enum CompressOutcome {
    Compressed {
        text: String,
        saved_bytes: usize,
        lossy: bool,
        kind: ContentKind,
    },
    Unchanged,
}

/// Content recognition + compression. detect also takes a budget (to read cmd for command-aware claiming).
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str, budget: &Budget) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}

/// Fixed priority + command-aware reordering; the first compressor whose detect hits compresses; both detect/compress are catch_unwind.
pub struct ContentRouter {
    compressors: Vec<Box<dyn Compressor>>,
}

impl ContentRouter {
    pub fn new(compressors: Vec<Box<dyn Compressor>>) -> Self {
        Self { compressors }
    }

    /// Returns Some((new, lossy, kind)) only when compression actually happened (Compressed with saved_bytes>0);
    /// Unchanged / no hit / panic → None. When a command hint hits, the corresponding compressor is moved to the front of the candidates.
    pub fn compress_text(&self, text: &str, budget: &Budget) -> Option<(String, bool, ContentKind)> {
        // Command-aware reordering: a matched command moves the compressor with the corresponding name to the front (stable, moves only one).
        let preferred: Option<&'static str> = budget.cmd.and_then(|c| {
            if c.is_git_diff() {
                Some("diff")
            } else if c.is_grep() {
                Some("search")
            } else {
                None
            }
        });

        // Build the candidate iteration order (index list): the preferred match goes first, the rest in original order.
        let mut order: Vec<usize> = (0..self.compressors.len()).collect();
        if let Some(name) = preferred {
            if let Some(pos) = self.compressors.iter().position(|c| c.name() == name) {
                let idx = order.remove(pos);
                order.insert(0, idx);
            }
        }

        let hit = order.into_iter().find(|&i| {
            let c = &self.compressors[i];
            catch_unwind(AssertUnwindSafe(|| c.detect(text, budget))).unwrap_or(false)
        })?;
        let c = &self.compressors[hit];

        let outcome = catch_unwind(AssertUnwindSafe(|| c.compress(text, budget)));
        match outcome {
            Ok(CompressOutcome::Compressed {
                text: new,
                saved_bytes,
                lossy,
                kind,
            }) if saved_bytes > 0 => Some((new, lossy, kind)),
            Ok(_) => None,
            Err(_) => {
                tracing::warn!("llm-compress: compressor '{}' panicked, passing through", c.name());
                None
            }
        }
    }
}
```

- [ ] **Step 6: compile-check router (existing compressors expected to error)**

Run: `cd codex-rs && cargo build -p codez-llm-compress 2>&1 | head -30`
Expected: FAIL, reporting that the `detect` signatures of the existing truncate/json/diff/log don't match and that `Compressed{..}` is missing fields. That's what the next step fixes.

- [ ] **Step 7: sync the truncate.rs signature**

`zmod/llm-compress/src/compress/truncate.rs`:

Change the detect signature (was `fn detect(&self, _text: &str) -> bool`) to:

```rust
    fn detect(&self, _text: &str, _budget: &Budget) -> bool {
        true
    }
```

Change the single `Compressed` constructor inside compress (was `CompressOutcome::Compressed { text: result, saved_bytes }`) to:

```rust
            CompressOutcome::Compressed { text: result, saved_bytes, lossy: true, kind: ContentKind::Text }
```

And make sure the `use` at the top of the file imports `ContentKind` (change `use crate::router::{Budget, CompressOutcome, Compressor};` to):

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

- [ ] **Step 8: sync the json.rs signature (signature only, algorithm changes in Task 05)**

`zmod/llm-compress/src/compress/json.rs`:

Add `ContentKind` to the top use:

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

Change detect (was `fn detect(&self, text: &str) -> bool`):

```rust
    fn detect(&self, text: &str, _budget: &Budget) -> bool {
        matches!(
            serde_json::from_str::<Value>(text),
            Ok(Value::Object(_)) | Ok(Value::Array(_))
        )
    }
```

Change the single `Compressed` constructor inside compress (was `CompressOutcome::Compressed { text: new, saved_bytes }`) to (JSON never deletes content → `lossy: false, kind: Json`):

```rust
            CompressOutcome::Compressed { text: new, saved_bytes, lossy: false, kind: ContentKind::Json }
```

- [ ] **Step 9: sync the diff.rs signature**

`zmod/llm-compress/src/compress/diff.rs`:

Add `ContentKind` to the top use:

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

Change detect (was `fn detect(&self, text: &str) -> bool`, keeping the body unchanged, only adding the parameter):

```rust
    fn detect(&self, text: &str, _budget: &Budget) -> bool {
```

Change the single `Compressed` constructor inside compress (was `CompressOutcome::Compressed { text: result, saved_bytes }`) to (diff folding deletes context → `lossy: true, kind: Text`):

```rust
        CompressOutcome::Compressed { text: result, saved_bytes, lossy: true, kind: ContentKind::Text }
```

- [ ] **Step 10: sync the log.rs signature (signature only, algorithm changes in Task 08)**

`zmod/llm-compress/src/compress/log.rs`:

Add `ContentKind` to the top use:

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

Change detect (was `fn detect(&self, text: &str) -> bool`, body unchanged, only adding the parameter):

```rust
    fn detect(&self, text: &str, _budget: &Budget) -> bool {
```

Change the single `Compressed` constructor inside compress (was `CompressOutcome::Compressed { text: new_text, saved_bytes: saved }`) to (v1 log does head/tail truncation → temporarily mark `lossy: true, kind: Text`; refine when rewriting in Task 08):

```rust
                CompressOutcome::Compressed { text: new_text, saved_bytes: saved, lossy: true, kind: ContentKind::Text }
```

- [ ] **Step 11: adapt lib.rs (transitional wiring, full orchestration left to Task 11)**

The existing `Budget { cfg: &cfg }` construction in `zmod/llm-compress/src/lib.rs` and the `compress_in_place` it uses are on the old signatures. This task does the **minimal transitional adaptation** to make it compile; the full RequestCtx orchestration is in Task 11.

Change where the router/budget is constructed in `transform`, and `compress_in_place`:

Current (v1):
```rust
    let router = build_router();
    let budget = Budget { cfg: &cfg };

    for item in request.input.iter_mut() {
        compress_item(item, &router, &budget, cfg.per_item_min_bytes);
    }
```

Change to (transitional: query/cmd are empty for now, Task 11 wires the real values):
```rust
    let router = build_router();
    let empty_query: Vec<String> = Vec::new();
    let budget = Budget { cfg: &cfg, cmd: None, query: &empty_query };

    for item in request.input.iter_mut() {
        compress_item(item, &router, &budget, cfg.per_item_min_bytes);
    }
```

Change `compress_in_place` (current `if let Some(new) = router.compress_text(s, budget) { *s = new; }`) to (destructure the triple; this task ignores lossy/kind for now, the size gate is retained):
```rust
fn compress_in_place(s: &mut String, router: &ContentRouter, budget: &Budget, min_bytes: usize) {
    if s.len() < min_bytes {
        return;
    }
    if let Some((new, _lossy, _kind)) = router.compress_text(s, budget) {
        if new.len() <= s.len() {
            *s = new;
        }
    }
}
```

- [ ] **Step 12: extend config.rs — new sub-table struct definitions**

`zmod/llm-compress/src/config.rs`: after the existing `LogCfg` definition and before `impl Default for Config`, add the new sub-table structs (each one `#[derive(Debug, Clone, Deserialize)]` + `#[serde(default)]`):

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PreprocessCfg {
    pub strip_progress: bool,
    pub collapse_blank: bool,
    pub truncate_line_bytes: usize,
    pub dedup_consecutive: bool,
    pub blob_min_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SearchCfg {
    pub max_per_file: usize,
    pub max_files: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TabularCfg {
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProtectCfg {
    pub error_max_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CcrCfg {
    pub enabled: bool,
    pub max_files_per_thread: usize,
    pub max_thread_bytes: u64,
    pub max_file_bytes: usize,
}
```

- [ ] **Step 13: config.rs — add new fields to the Config struct + extend JsonCfg/LogCfg**

Inside `pub struct Config` (after the existing `truncate/json/diff/log` fields) append:

```rust
    pub preprocess: PreprocessCfg,
    pub search: SearchCfg,
    pub tabular: TabularCfg,
    pub protect: ProtectCfg,
    pub ccr: CcrCfg,
```

Inside `pub struct JsonCfg` (after the existing `max_array_items`, `max_depth`) append:

```rust
    pub csv_schema: bool,
```

Inside `pub struct LogCfg` (after the existing `dedup_repeats`) append:

```rust
    pub template_min_run: usize,
    pub keep_levels: Vec<String>,
```

- [ ] **Step 14: config.rs — Default implementations**

Inside the `Self { ... }` of `impl Default for Config` (after the existing fields) append (values taken from spec §6):

```rust
            preprocess: PreprocessCfg::default(),
            search: SearchCfg::default(),
            tabular: TabularCfg::default(),
            protect: ProtectCfg::default(),
            ccr: CcrCfg::default(),
```

Inside the `Self { ... }` of `impl Default for JsonCfg` append `csv_schema: true` (so it becomes `Self { max_array_items: 20, max_depth: 6, csv_schema: true }` overall).

Inside the `Self { ... }` of `impl Default for LogCfg` append (so it becomes overall):

```rust
        Self { dedup_repeats: true, template_min_run: 3, keep_levels: vec!["error".to_string(), "warn".to_string()] }
```

Append the Default implementations of the new sub-tables at the end of the file:

```rust
impl Default for PreprocessCfg {
    fn default() -> Self {
        Self { strip_progress: true, collapse_blank: true, truncate_line_bytes: 2000, dedup_consecutive: true, blob_min_bytes: 256 }
    }
}
impl Default for SearchCfg {
    fn default() -> Self {
        Self { max_per_file: 5, max_files: 15 }
    }
}
impl Default for TabularCfg {
    fn default() -> Self {
        Self { enabled: true }
    }
}
impl Default for ProtectCfg {
    fn default() -> Self {
        Self { error_max_bytes: 8192 }
    }
}
impl Default for CcrCfg {
    fn default() -> Self {
        Self { enabled: true, max_files_per_thread: 200, max_thread_bytes: 67_108_864, max_file_bytes: 4_194_304 }
    }
}
```

- [ ] **Step 15: create compress/schema.rs**

Create `zmod/llm-compress/src/compress/schema.rs`:

```rust
//! Shared module for csv-schema inline representation (used by both JSON §5① and Tabular §5④).
//! Object array (items homogeneous) → {"_schema":[...],"_rows":[[...]]}. Pure structural reformatting, all content preserved → lossy=false.

use serde_json::{Map, Value};

/// Object array (items homogeneous) → {"_schema":[...],"_rows":[[...]]}. Non-homogeneous/non-array/empty → None.
/// Homogeneity check: all elements are objects with the same key set (key order taken from the first element).
/// Both scalar and nested values go into the _rows row in the schema column order (nested values are kept as-is, not flattened).
pub fn to_schema_form(value: &Value) -> Option<Value> {
    let arr = value.as_array()?;
    if arr.len() < 2 {
        return None; // single element yields no benefit
    }
    let first = arr.first()?.as_object()?;
    if first.is_empty() {
        return None;
    }
    // schema = the key order of the first element
    let schema: Vec<String> = first.keys().cloned().collect();
    let key_set: std::collections::BTreeSet<&String> = first.keys().collect();

    let mut rows: Vec<Value> = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object()?; // any non-object → None
        // the key set must match exactly
        let this_set: std::collections::BTreeSet<&String> = obj.keys().collect();
        if this_set != key_set {
            return None;
        }
        let row: Vec<Value> = schema.iter().map(|k| obj.get(k).cloned().unwrap_or(Value::Null)).collect();
        rows.push(Value::Array(row));
    }

    let mut out = Map::new();
    out.insert("_schema".to_string(), Value::Array(schema.into_iter().map(Value::String).collect()));
    out.insert("_rows".to_string(), Value::Array(rows));
    Some(Value::Object(out))
}
```

Add to `zmod/llm-compress/src/compress/mod.rs`:

```rust
pub mod schema;
```

- [ ] **Step 16: sync the interface calls in existing tests**

The new interface changed 3 signatures; existing tests need to be synced. Edit file by file:

**`tests/router_test.rs`**: the three fake compressors (`HalfCompressor`/`NeverCompressor`/`PanicCompressor` and the `Claims` inside `unchanged_outcome_returns_none`) implement the `Compressor` trait. For each:
- `fn detect(&self, _t: &str) -> bool` → `fn detect(&self, _t: &str, _b: &Budget) -> bool`
- return `Compressed { text: new, saved_bytes: saved }` → `Compressed { text: new, saved_bytes: saved, lossy: true, kind: ContentKind::Text }`
- add `ContentKind` to the top `use`: `use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentRouter, ContentKind};`
- `Budget { cfg }` construction → `Budget { cfg, cmd: None, query: &[] }`
- `compress_text` assertion: the original `out.unwrap().starts_with("[half]")` → destructure the triple: `let (text, _lossy, _kind) = out.unwrap(); assert!(text.starts_with("[half]"));`; `assert!(r.compress_text(..).is_none())` is unchanged (still an Option).

**`tests/truncate_test.rs`**:
- add `ContentKind` to the top use.
- all `TruncateCompressor.detect(input)` → `.detect(input, &budget(&cfg))` (detect now needs a budget; the `budget()` helper already exists). `detect_is_always_true` changes the same way.
- all `match`/destructure of `CompressOutcome::Compressed { text, saved_bytes }` → add `, lossy: _, kind: _` (or `..`): `CompressOutcome::Compressed { text, saved_bytes, .. }`.
- the `budget()` helper (`Budget { cfg }`) → `Budget { cfg, cmd: None, query: &[] }`.

**`tests/json_test.rs`**:
- add `ContentKind` to the top use (if destructuring needs it).
- all `c.detect(...)` → `c.detect(..., &budget(&cfg))` (detect takes a budget; note that the `detect_accepts_objects_arrays_rejects_scalars_and_garbage` case has no budget, so construct one: `let cfg = Config::disabled(); let b = budget(&cfg);` then `c.detect(x, &b)`).
- all destructures of `CompressOutcome::Compressed { text, saved_bytes }` / `{ text, .. }` → add `..`.
- the `budget()` helper → `Budget { cfg, cmd: None, query: &[] }`.

**`tests/diff_test.rs`**, **`tests/log_test.rs`**: likewise change `.detect(x)` → `.detect(x, &budget(&cfg))`, `Budget{cfg}` → `Budget{cfg, cmd:None, query:&[]}`, destructure `Compressed{..}` adding `..`, and add `ContentKind` to the top use as needed.

> Tip: use `cargo build -p codez-llm-compress --tests 2>&1 | head -40` to locate compile errors one by one — faster than reading through manually.

- [ ] **Step 17: write the schema.rs unit tests**

Create `zmod/llm-compress/tests/schema_test.rs`:

```rust
use codez_llm_compress::compress::schema::to_schema_form;
use serde_json::json;

#[test]
fn homogeneous_object_array_to_schema() {
    let v = json!([{"id":1,"name":"a"},{"id":2,"name":"b"}]);
    let out = to_schema_form(&v).expect("homogeneous array → schema");
    assert_eq!(out["_schema"], json!(["id","name"]));
    assert_eq!(out["_rows"], json!([[1,"a"],[2,"b"]]));
}

#[test]
fn heterogeneous_keys_return_none() {
    let v = json!([{"id":1},{"name":"b"}]);
    assert!(to_schema_form(&v).is_none());
}

#[test]
fn non_array_returns_none() {
    assert!(to_schema_form(&json!({"a":1})).is_none());
    assert!(to_schema_form(&json!("str")).is_none());
}

#[test]
fn nested_values_preserved_in_rows() {
    let v = json!([{"id":1,"meta":{"x":9}},{"id":2,"meta":{"x":8}}]);
    let out = to_schema_form(&v).unwrap();
    // nested objects are placed into the row as-is, not flattened
    assert_eq!(out["_rows"][0][1], json!({"x":9}));
}

#[test]
fn single_element_returns_none() {
    let v = json!([{"id":1}]);
    assert!(to_schema_form(&v).is_none());
}
```

- [ ] **Step 18: compile + run all tests**

Run: `cd codex-rs && cargo build -p codez-llm-compress --tests`
Expected: PASS (no compile errors)

Run: `cd codex-rs && cargo test -p codez-llm-compress`
Expected: all green (existing tests + the 5 new schema_test cases)

Run: `cd codex-rs && cargo clippy -p codez-llm-compress --all-targets`
Expected: no warnings

- [ ] **Step 19: commit**

```bash
git add zmod/llm-compress/src/router.rs zmod/llm-compress/src/config.rs \
  zmod/llm-compress/src/command.rs zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/src/compress/mod.rs zmod/llm-compress/src/compress/schema.rs \
  zmod/llm-compress/src/compress/truncate.rs zmod/llm-compress/src/compress/json.rs \
  zmod/llm-compress/src/compress/diff.rs zmod/llm-compress/src/compress/log.rs \
  zmod/llm-compress/Cargo.toml \
  zmod/llm-compress/tests/router_test.rs zmod/llm-compress/tests/truncate_test.rs \
  zmod/llm-compress/tests/json_test.rs zmod/llm-compress/tests/diff_test.rs \
  zmod/llm-compress/tests/log_test.rs zmod/llm-compress/tests/schema_test.rs
git commit -m "feat(llm-compress-v2): Task01 interface foundation — router lossy+kind/detect budget + config extension + schema.rs + compressor signature sync"
```
