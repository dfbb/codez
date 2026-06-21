# Task 04: JsonCompressor (in-structure JSON compression + parse validation)

> Part of `2026-06-20-llm-compress-00-index.md`. Read the index before starting. Depends on Task 01 (`config::{Config, JsonCfg}`) and Task 02 (`router::{Budget, CompressOutcome, Compressor}`).

**Goal:** Implement `JsonCompressor` — a structural compressor that **never breaks JSON**. It only claims text that `serde_json` can parse, recursively samples long arrays and trims over-deep subtrees **inside the JSON structure**, always represents placeholders with valid JSON values (an omitted span of a long array becomes a string element `"…(N more)"`, an over-deep subtree becomes the string `"…"`), serializes back to compact JSON, and **re-parses the result to validate it**; if validation fails or no bytes were saved, it falls back to the original text with `Unchanged`.

**Spec coverage:** §4 (JSON does not go through the text-level pipeline / in-structure compression / parse-validation fallback), §6 (conservative thresholds / placeholders expressed as valid JSON values).

**Key constraints (point-by-point alignment with the index's Global Constraints):**
- JSON **must not** go through text-level head/tail line truncation, and **must not** insert bare text markers `[llm-compress: …]`; placeholders use only valid JSON values (`"…(N more)"` / `"…"`).
- The result must be re-validated via `serde_json::from_str::<Value>()`; on failure → discard the compressed result and fall back to the original text.
- Avoid `unwrap`/`expect` in non-test code.

---

**Files:**
- Create: `zmod/llm-compress/src/compress/json.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs` (add `pub mod json;`)
- Test: `zmod/llm-compress/tests/json_test.rs`

> Note: all of this crate's compressors live under the `src/compress/` submodule. If `src/compress/mod.rs` does not yet exist (this task may run before the other compressors), Step 4 is responsible for creating it and registering `pub mod compress;` in `lib.rs`.

**Interfaces:**
- Consumes (from Task 01): `config::JsonCfg { pub max_array_items: usize, pub max_depth: usize }`, accessed via `budget.cfg.json`.
- Consumes (from Task 02): `router::{Budget, CompressOutcome, Compressor}` (signatures are pinned, must not change).
- Produces (registered into `ContentRouter` during the 08 orchestration):
  - `pub struct JsonCompressor;` (unit struct, stateless).
  - `impl Compressor for JsonCompressor`:
    - `name()` → `"json"`.
    - `detect(text)` → `serde_json::from_str::<serde_json::Value>(text).is_ok()`.
    - `compress(text, budget)` → in-structure recursive compression + parse validation, returning `Compressed`/`Unchanged`.

---

## Behavior spec (the `compress` implementation must follow this exactly)

Given input `text`, take `cfg = &budget.cfg.json` (i.e. `max_array_items` / `max_depth`).

1. Parse with `serde_json::from_str::<Value>(text)`; on failure → `Unchanged` (in theory `detect` already guarantees success, so this is a defensive fallback).
2. Compress the `Value` in place recursively (`compress_value(&mut value, depth=0, cfg)`):
   - **Over-deep trimming**: when `depth > max_depth` and the current node is an object or array (a container) → replace the entire subtree with the JSON string `Value::String("…")`. Scalars (numbers/strings/booleans/null) are kept as-is even when over-deep (trimming a scalar yields no savings and carries no risk of breakage, whereas replacing a scalar with `"…"` might actually make it longer, so only containers are trimmed).
   - **Long-array sampling**: when an array's element count `len > max_array_items`, keep the **first `ceil(max_array_items/2)`** and the **last `floor(max_array_items/2)`** elements, and insert **one** string element `Value::String("…(N more)")` in the middle, where `N = len - keep_head - keep_tail` (the number of omitted elements). The retained elements still need recursive processing (depth +1).
   - **Object**: keep all keys, recurse into each value (depth +1).
   - **Array (not over-length, or the remaining items after sampling)**: recurse into each element (depth +1).
3. Serialize back to a **compact** JSON string with `serde_json::to_string(&value)` (independent of the original's indentation/newline style).
4. **Validation**: `serde_json::from_str::<Value>(&new)`; on failure → discard `new` and return `Unchanged`.
5. `saved_bytes = text.len().saturating_sub(new.len())`; if `saved_bytes > 0` and validation passed → `Compressed { text: new, saved_bytes }`, otherwise `Unchanged`.

> Sampling rounding convention (aligned with the brief): `keep_head = (max_array_items + 1) / 2` (= ceil), `keep_tail = max_array_items / 2` (= floor). Their sum = `max_array_items`, so the sampled array length = `max_array_items + 1` (including the single placeholder string element in the middle). `max_array_items == 0` is treated as a degenerate case: here `keep_head=keep_tail=0`, and the whole array is replaced by a single `"…(N more)"` (still valid JSON, acceptable).

---

- [ ] **Step 1: Write the failing test**

Create `zmod/llm-compress/tests/json_test.rs`:

```rust
use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

/// Build an enabled config with small json thresholds, to make compression easy to trigger.
fn cfg_small() -> Config {
    let mut c = Config::disabled();
    c.json.max_array_items = 6; // ceil=3 head + floor=3 tail
    c.json.max_depth = 2;
    c
}

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg }
}

#[test]
fn detect_accepts_valid_json_rejects_garbage() {
    let c = JsonCompressor;
    assert!(c.detect(r#"{"a":1,"b":[1,2,3]}"#));
    assert!(c.detect("[1, 2, 3]"));
    assert!(c.detect("\"a quoted string is valid json\""));
    assert!(c.detect("123"));
    // Not JSON
    assert!(!c.detect("not json {"));
    assert!(!c.detect("{unquoted: key}"));
    assert!(!c.detect(""));
}

#[test]
fn long_array_is_sampled_with_placeholder_element() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // 20 elements, far above max_array_items=6
    let arr: Vec<i64> = (0..20).collect();
    let text = serde_json::to_string(&arr).unwrap();

    let out = c.compress(&text, &budget(&cfg));
    let new = match out {
        CompressOutcome::Compressed { text, saved_bytes } => {
            assert!(saved_bytes > 0);
            text
        }
        CompressOutcome::Unchanged => panic!("expected compression for a 20-element array"),
    };

    // The result must be valid JSON
    let v: Value = serde_json::from_str(&new).expect("compressed output must be valid JSON");
    let items = v.as_array().expect("top-level array");
    // 6 retained (3 head + 3 tail) + 1 placeholder = 7
    assert_eq!(items.len(), 7);
    // head 3: 0,1,2; tail 3: 17,18,19
    assert_eq!(items[0], serde_json::json!(0));
    assert_eq!(items[2], serde_json::json!(2));
    assert_eq!(items[4], serde_json::json!(18));
    assert_eq!(items[6], serde_json::json!(19));
    // The middle is the placeholder string "…(N more)", N = 20 - 6 = 14
    assert_eq!(items[3], serde_json::json!("…(14 more)"));
}

#[test]
fn output_is_always_valid_json() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // Mixed: long array + nested object + over-deep
    let text = r#"{
        "list": [1,2,3,4,5,6,7,8,9,10,11,12],
        "nested": {"a": {"b": {"c": {"d": 1}}}},
        "name": "keep me"
    }"#;
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &budget(&cfg)) {
        // Key assertion: no matter what it compresses to, the result can be re-parsed.
        serde_json::from_str::<Value>(&new).expect("compressed output must be valid JSON");
        // All keys must be retained
        let v: Value = serde_json::from_str(&new).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("list"));
        assert!(obj.contains_key("nested"));
        assert!(obj.contains_key("name"));
    }
    // Even if not compressed (Unchanged) there's nothing broken; the point of the test is "if it compresses, it must be valid".
}

#[test]
fn deep_subtree_replaced_by_ellipsis_string() {
    // max_depth=2: depth 0 top-level object, depth 1 value, depth 2 value; containers at depth 3 and beyond are replaced.
    let cfg = cfg_small();
    let c = JsonCompressor;
    let text = r#"{"a":{"b":{"c":{"d":{"e":1}}}}}"#;
    let out = c.compress(text, &budget(&cfg));
    let new = match out {
        CompressOutcome::Compressed { text, .. } => text,
        CompressOutcome::Unchanged => panic!("expected deep nesting to be trimmed"),
    };
    let v: Value = serde_json::from_str(&new).expect("valid JSON");
    // Walk down a/b to the replacement point; at some level we should hit the string "…"
    let mut found_ellipsis = false;
    let mut node = &v;
    for key in ["a", "b", "c", "d", "e"] {
        if node == &serde_json::json!("…") {
            found_ellipsis = true;
            break;
        }
        match node.get(key) {
            Some(child) => node = child,
            None => break,
        }
    }
    if node == &serde_json::json!("…") {
        found_ellipsis = true;
    }
    assert!(found_ellipsis, "expected a \"…\" placeholder for the over-deep subtree, got: {new}");
}

#[test]
fn small_json_is_unchanged() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // Short array (<=6), shallow nesting (<=2): nothing to compress.
    let text = r#"{"a":[1,2,3],"b":{"c":1}}"#;
    matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged)
        .then_some(())
        .expect("small json should be Unchanged");
}

#[test]
fn saved_bytes_equals_len_delta() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    let arr: Vec<i64> = (0..50).collect();
    let text = serde_json::to_string(&arr).unwrap();
    if let CompressOutcome::Compressed { text: new, saved_bytes } = c.compress(&text, &budget(&cfg)) {
        assert_eq!(saved_bytes, text.len() - new.len());
        assert!(new.len() < text.len());
    } else {
        panic!("expected compression");
    }
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run (from the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test json_test
```
Expected: compilation failure (the `compress::json` module / `JsonCompressor` is undefined).

- [ ] **Step 3: Write json.rs (full implementation, no TODOs)**

Create `zmod/llm-compress/src/compress/json.rs`:

```rust
//! JsonCompressor — in-structure JSON compression that never breaks JSON.
//!
//! Only claims text that serde_json can parse; recursively samples long arrays
//! and trims over-deep subtrees inside the Value structure, always representing
//! placeholders with valid JSON values; after serializing back to compact JSON
//! it re-parses to validate, falling back to the original on failure or no savings.

use crate::config::JsonCfg;
use crate::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

/// Stateless unit struct.
pub struct JsonCompressor;

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    /// Claimed if serde_json can parse it.
    fn detect(&self, text: &str) -> bool {
        serde_json::from_str::<Value>(text).is_ok()
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        // 1. parse (failure → Unchanged; detect already guarantees this, so this is a defensive fallback).
        let mut value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return CompressOutcome::Unchanged,
        };

        // 2. In-structure recursive compression.
        let cfg = &budget.cfg.json;
        compress_value(&mut value, 0, cfg);

        // 3. Serialize back to compact JSON.
        let new = match serde_json::to_string(&value) {
            Ok(s) => s,
            Err(_) => return CompressOutcome::Unchanged,
        };

        // 4. Validation: the result must be re-parseable, otherwise discard and fall back to the original.
        if serde_json::from_str::<Value>(&new).is_err() {
            return CompressOutcome::Unchanged;
        }

        // 5. Return Compressed only when there are real savings.
        let saved_bytes = text.len().saturating_sub(new.len());
        if saved_bytes > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// Recursively compress a Value in place.
///
/// - `depth`: the depth of the current node (top level is 0).
/// - Over-deep (`depth > max_depth`) containers (objects/arrays) are replaced wholesale with `"…"`.
/// - Long arrays (`len > max_array_items`) are sampled: keep ceil at the head, floor at the tail, with one `"…(N more)"` in between.
/// - Object keys are all retained, recursing into each value; arrays recurse element by element. Scalars are kept as-is.
fn compress_value(v: &mut Value, depth: usize, cfg: &JsonCfg) {
    // Over-deep: only trim containers, keep scalars (replacing a scalar with "…" yields no savings and may be longer).
    if depth > cfg.max_depth && (v.is_object() || v.is_array()) {
        *v = Value::String("…".to_string());
        return;
    }

    match v {
        Value::Array(items) => {
            // Sample first (if over-length), then recurse into the retained elements (depth +1).
            if items.len() > cfg.max_array_items {
                let len = items.len();
                let keep_head = (cfg.max_array_items + 1) / 2; // ceil
                let keep_tail = cfg.max_array_items / 2; // floor
                let omitted = len.saturating_sub(keep_head + keep_tail);

                // Take the tail segment (last keep_tail), then the head segment (first keep_head),
                // and reassemble as [head…] + placeholder + [tail…].
                let tail: Vec<Value> = items.split_off(len - keep_tail);
                items.truncate(keep_head);
                items.push(Value::String(format!("…({omitted} more)")));
                items.extend(tail);
            }

            for child in items.iter_mut() {
                // The placeholder string element is also visited by this recursion, but it's a scalar and unaffected.
                compress_value(child, depth + 1, cfg);
            }
        }
        Value::Object(map) => {
            for (_k, child) in map.iter_mut() {
                compress_value(child, depth + 1, cfg);
            }
        }
        // Scalar: kept as-is.
        _ => {}
    }
}
```

> Implementation notes (for review and for the executor to cross-check):
> - **Sampling order**: first `split_off(len - keep_tail)` grabs the tail segment and removes it from the original array, then `truncate(keep_head)` keeps only the head segment, then `push` the placeholder and `extend` the tail segment. This precisely preserves the original order and values of the head/tail elements, with a single placeholder string wedged in the middle. When `max_array_items == 0`, `keep_head=keep_tail=0`, and the array is left with only a single `"…(N more)"` — still valid JSON.
> - **The placeholder is also recursed**: after reassembly we recurse over the whole array via `iter_mut`; the placeholder is a `Value::String` (a scalar), and `compress_value`'s `_ => {}` arm skips it as-is, so it is not broken.
> - **The over-deep check is at the very front of the function**: this ensures that any container that crosses `max_depth` is replaced wholesale immediately, never descending further; scalars are harmless when over-deep, so they pass through.
> - **No `unwrap`/`expect`**: parse, serialize, and re-validate all use `match` / `is_err` to fall back to `Unchanged`.

- [ ] **Step 4: Register the module**

If `zmod/llm-compress/src/compress/mod.rs` **does not exist**, create it:

```rust
//! Compressor implementations for each content type.

pub mod json;
```

And in `zmod/llm-compress/src/lib.rs` (after `pub mod router;`) add a line (if `compress` is not yet registered):

```rust
pub mod compress;
```

If `compress/mod.rs` **already exists** (another compressor task ran first), modify it by appending:

```rust
pub mod json;
```

- [ ] **Step 5: Run the test and watch it pass**

Run (from the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test json_test
```
Expected: all cases pass (`test result: ok.`). In particular, confirm `output_is_always_valid_json`, `long_array_is_sampled_with_placeholder_element`, and `deep_subtree_replaced_by_ellipsis_string` are all green.

- [ ] **Step 6: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-04-json.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): JsonCompressor (in-structure compression with parse validation)"
```

> Note: commit only `zmod/llm-compress/**` and this plan file. **Do not** commit the codex-rs subtree's `core/Cargo.toml` / `Cargo.lock` changes (those are the dev-build enabler introduced in Task 01; keep them dirty, and Task 09 will restore them).
