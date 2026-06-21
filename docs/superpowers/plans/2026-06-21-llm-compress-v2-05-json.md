# Task 05 — JSON Upgrade: detect Yields + Consecutive RLE Dedup + csv-schema

> Part of `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §5①. Depends on Task 01 (schema.rs, new signatures). Can run in parallel with 06/07/08/09.

**Goal:** Change the JSON compressor from v1's "lossy sampling / over-deep pruning" to a **content-preserving-only** pipeline (consecutive RLE dedup + csv-schema). `kind=Json` is always `lossy=false` and never raises a CCR. `detect` predicts internally "whether the lossless-compressed result still exceeds `truncate.max_bytes`"; if so it returns false and hands off to Truncate (matching the router's first-match behavior).

## Files
- Modify: `zmod/llm-compress/src/compress/json.rs` (rewrite detect + compress)
- Test: `zmod/llm-compress/tests/json_test.rs` (extend; Task 01 already synced the old assertions)

**Interfaces:**
- Consumes: Task 01's `schema::to_schema_form`, `Budget`, `CompressOutcome`, `ContentKind`, `config.json.{max_array_items,max_depth,csv_schema}`, `config.truncate.max_bytes`.
- Produces: the upgraded `JsonCompressor` (behavior change: no longer deletes elements; detect will yield).

> **Core algorithm (spec §5①)**: inside compress, perform two ordered lossless transforms — ① consecutive duplicate RLE dedup (fold adjacent equal items into the first item + `{"_llm_dup_prev":N}`; skip if the source array already contains objects in `_llm_dup_prev` form); ② csv-schema (call `schema::to_schema_form` for homogeneous object arrays). Neither step deletes data. detect predicts whether the lossless product is ≤ `truncate.max_bytes`; if so it claims the input, otherwise it yields.

---

- [ ] **Step 1: Write failing tests (RLE + csv-schema + yielding)**

**Append** to `zmod/llm-compress/tests/json_test.rs` (keep the existing cases that Task 01 already synced):

```rust
// ========== Task 05 additions ==========
use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};
use codez_llm_compress::config::Config;

fn budget_t05(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn consecutive_rle_folds_adjacent_duplicates() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000; // do not yield
    let c = JsonCompressor;
    // 4 adjacent identical objects
    let text = r#"[{"a":1},{"a":1},{"a":1},{"a":1},{"b":2}]"#;
    let b = budget_t05(&cfg);
    assert!(c.detect(text, &b));
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &b) {
        assert!(!lossy, "RLE does not delete content");
        assert_eq!(kind, ContentKind::Json);
        let v: serde_json::Value = serde_json::from_str(&new).expect("valid json");
        // first item preserved + count placeholder; first item {"a":1} still present
        assert_eq!(v[0], serde_json::json!({"a":1}));
        assert!(new.contains("_llm_dup_prev"));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn csv_schema_applied_to_homogeneous_array() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    cfg.json.csv_schema = true;
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"},{"id":3,"name":"carol"}]"#;
    let b = budget_t05(&cfg);
    if let CompressOutcome::Compressed { text: new, lossy, .. } = c.compress(text, &b) {
        assert!(!lossy);
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["_schema"], serde_json::json!(["id","name"]));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn detect_yields_to_truncate_when_lossless_insufficient() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 50; // very small
    let c = JsonCompressor;
    // large array, no adjacent duplicates, non-homogeneous → lossless can't shrink it → exceeds 50 bytes → detect false
    let text = r#"[{"a":1,"x":"aaaa"},{"b":2,"y":"bbbb"},{"c":3,"z":"cccc"}]"#;
    let b = budget_t05(&cfg);
    assert!(!c.detect(text, &b), "lossless can't reach 50 bytes → yield to Truncate");
}

#[test]
fn detect_accepts_when_small_enough() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    let text = r#"{"a":1}"#;
    let b = budget_t05(&cfg);
    assert!(c.detect(text, &b), "small JSON under threshold → claim it");
}

#[test]
fn rle_skips_existing_marker_objects() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    // source array already contains objects in _llm_dup_prev form; they must not be folded/rewritten
    let text = r#"[{"_llm_dup_prev":5},{"_llm_dup_prev":5}]"#;
    let b = budget_t05(&cfg);
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &b) {
        // no confusion: the two _llm_dup_prev objects are not treated as RLE fold products
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert!(v.is_array() || v.is_object());
    }
    // Unchanged is also allowed (no gain); the key point is no panic and a valid product
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test json_test 2>&1 | head -20`
Expected: FAIL (new behavior not yet implemented; detect is still Task 01's pure-parse check with no yielding)

- [ ] **Step 3: Rewrite json.rs (top + RLE + csv-schema core)**

Replace `zmod/llm-compress/src/compress/json.rs` entirely with the following two steps (this step writes the top through `lossless_compress`):

```rust
//! JsonCompressor — content-preserving-only pipeline (consecutive RLE dedup + csv-schema); kind=Json is always lossy=false.
//! detect predicts internally: whether the lossless-compressed result still exceeds truncate.max_bytes; if so, yield to Truncate (spec §5①).

use crate::compress::schema::to_schema_form;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::Value;

const DUP_KEY: &str = "_llm_dup_prev";

pub struct JsonCompressor;

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    /// Yield criterion: claim the input only if it parses as an object/array and the lossless-compressed size is ≤ truncate.max_bytes.
    /// Otherwise (including parse failure / lossless can't shrink it) return false and let Truncate take over.
    fn detect(&self, text: &str, budget: &Budget) -> bool {
        let value: Value = match serde_json::from_str(text) {
            Ok(v @ Value::Object(_)) | Ok(v @ Value::Array(_)) => v,
            _ => return false,
        };
        let compressed = lossless_compress(value, budget);
        match serde_json::to_string(&compressed) {
            Ok(s) => s.len() <= budget.cfg.truncate.max_bytes,
            Err(_) => false,
        }
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return CompressOutcome::Unchanged,
        };
        let compressed = lossless_compress(value, budget);
        let new = match serde_json::to_string(&compressed) {
            Ok(s) => s,
            Err(_) => return CompressOutcome::Unchanged,
        };
        // verify the product parses (existing invariant)
        if serde_json::from_str::<Value>(&new).is_err() {
            return CompressOutcome::Unchanged;
        }
        let saved = text.len().saturating_sub(new.len());
        if saved > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes: saved, lossy: false, kind: ContentKind::Json }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// Lossless compression: recursively apply consecutive RLE dedup to each array first, then attempt csv-schema. Neither deletes data.
fn lossless_compress(mut value: Value, budget: &Budget) -> Value {
    transform_value(&mut value, budget);
    value
}

fn transform_value(v: &mut Value, budget: &Budget) {
    match v {
        Value::Array(items) => {
            // recurse into children first
            for child in items.iter_mut() {
                transform_value(child, budget);
            }
            // consecutive RLE dedup (in place)
            rle_dedup(items);
            // csv-schema: if the whole array is homogeneous → replace with schema form
            if budget.cfg.json.csv_schema {
                let snapshot = Value::Array(items.clone());
                if let Some(schema_form) = to_schema_form(&snapshot) {
                    *v = schema_form;
                }
            }
        }
        Value::Object(map) => {
            for (_k, child) in map.iter_mut() {
                transform_value(child, budget);
            }
        }
        _ => {}
    }
}

/// Fold consecutive adjacent equal items into: first item + {"_llm_dup_prev": N} (N = number of extra repeats).
/// Objects that already contain the _llm_dup_prev key do not participate in folding (#6, to avoid placeholder confusion).
fn rle_dedup(items: &mut Vec<Value>) {
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() {
        let cur = &items[i];
        if is_dup_marker(cur) {
            out.push(cur.clone());
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < items.len() && items[j] == *cur && !is_dup_marker(&items[j]) {
            j += 1;
        }
        let extra = j - i - 1; // number of extra repeats
        out.push(cur.clone());
        if extra >= 1 {
            let mut m = serde_json::Map::new();
            m.insert(DUP_KEY.to_string(), Value::from(extra));
            out.push(Value::Object(m));
        }
        i = j;
    }
    *items = out;
}

fn is_dup_marker(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.contains_key(DUP_KEY))
}
```

- [ ] **Step 4: Run json tests and pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test json_test`
Expected: PASS (Task 01's synced old cases + Task 05's new cases)

> If old cases (related to v1 sampling, e.g. `long_array_is_sampled_with_placeholder_element`, `deep_subtree_replaced_by_ellipsis_string`) fail because sampling was removed: these assert the deleted v1 lossy behavior and **should be removed or rewritten** to assert the new behavior. Delete these old cases (they test functionality Task 05 removed) and keep `detect_accepts_objects_arrays_rejects_scalars_and_garbage` (if it conflicts with the new detect yielding, also adjust it to a version that passes a budget with a sufficiently large threshold).

- [ ] **Step 5: clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/compress/json.rs zmod/llm-compress/tests/json_test.rs
git commit -m "feat(llm-compress-v2): Task05 JSON upgrade — detect yields + consecutive RLE + csv-schema (content-preserving)"
```
