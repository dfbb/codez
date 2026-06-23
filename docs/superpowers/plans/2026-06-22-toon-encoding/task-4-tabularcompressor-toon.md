# Task 4: Rewrite `TabularCompressor` to emit TOON + orchestrator CCR isolation

> Self-contained task. Read `00-overview.md` for global context and constraints.
> Build/test inside `codex-rs/`; test command `cargo nextest run -p codez-llm-compress`.

**Goal:** (a) Make `TabularCompressor` build a `Value` from the parsed table and
encode it to TOON via the Task 1 helper, gated by `use_toon` and the five claim
conditions. (b) Teach the orchestrator (`lib.rs`) to treat `ContentKind::Toon`
like `Json`: never attach a CCR pointer, never re-parse as JSON. This closes the
hole where lossy preprocessing would decorate a TOON product with
`[llm-compress: 原文 …]`.

**Files:**
- Modify (full rewrite of the impl): `zmod/llm-compress/src/compress/tabular.rs`
- Modify: `zmod/llm-compress/src/lib.rs:108-145` (`compress_in_place` candidate handling)
- Test (rewrite): `zmod/llm-compress/tests/tabular_test.rs`
- Test (add one case): `zmod/llm-compress/tests/orchestration_test.rs`

**Interfaces:**
- Consumes (from Task 1): `crate::compress::toon::encode_checked(&Value) -> Option<String>`,
  `crate::router::ContentKind::Toon`.
- Consumes (from Task 2): `budget.cfg.json.use_toon`.
- Note: `TabularCompressor` is gated by `json.use_toon` (NOT `tabular.enabled`;
  that field is removed in Task 5). Its existing table-shape rules (header,
  unique non-empty columns, equal column counts, no quoted cells) are preserved
  via the existing `parse_table`.

---

- [ ] **Step 1: Rewrite the tabular test file**

Replace the entire contents of `zmod/llm-compress/tests/tabular_test.rs` with:

```rust
use codez_llm_compress::compress::tabular::TabularCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

/// A padded Markdown table shrinks under TOON.
#[test]
fn padded_markdown_shrinks_to_toon() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "\
| id    | name       | status   |
| ----- | ---------- | -------- |
| 1     | alice      | active   |
| 2     | bob        | inactive |
| 3     | carol      | active   |
| 4     | dave       | active   |
| 5     | erin       | inactive |";

    assert!(c.detect(text, &budget(&cfg)), "padded markdown should be claimed");
    let CompressOutcome::Compressed { text: new, lossy, kind, saved_bytes } =
        c.compress(text, &budget(&cfg))
    else {
        panic!("expected Compressed");
    };
    assert!(!lossy);
    assert_eq!(kind, ContentKind::Toon);
    assert!(new.len() < text.len(), "{} vs {}", new.len(), text.len());
    assert_eq!(saved_bytes, text.len() - new.len());
    // TOON round-trips to an array of {id,name,status} objects.
    let back: serde_json::Value = toon_format::decode_default(&new).unwrap();
    assert_eq!(back[0]["id"], serde_json::json!("1"));
    assert_eq!(back[0]["name"], serde_json::json!("alice"));
    assert_eq!(back[4]["status"], serde_json::json!("inactive"));
}

/// Compact CSV whose TOON form is not smaller → yield to Truncate.
#[test]
fn compact_csv_yields_to_truncate() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob\n3,carol";
    assert!(!c.detect(text, &budget(&cfg)), "no benefit → yield");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

/// Duplicate column names → not a valid table → not claimed.
#[test]
fn duplicate_column_names_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "id,id\n1,2\n3,4";
    assert!(!c.detect(text, &budget(&cfg)));
}

/// Ragged columns → not claimed.
#[test]
fn ragged_columns_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "a,b,c\n1,2\n3,4,5";
    assert!(!c.detect(text, &budget(&cfg)));
}

/// Quoted cells → not claimed (parser can't safely round-trip).
#[test]
fn quoted_cell_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "a,b\n\"x,y\",z\n1,2";
    assert!(!c.detect(text, &budget(&cfg)));
}

/// use_toon=false → not claimed.
#[test]
fn use_toon_false_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    cfg.json.use_toon = false;
    let c = TabularCompressor;
    let text = "\
| id    | name       | status   |
| ----- | ---------- | -------- |
| 1     | alice      | active   |
| 2     | bob        | inactive |
| 3     | carol      | active   |";
    assert!(!c.detect(text, &budget(&cfg)));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(/tabular_test/)'
```

Expected: FAIL — old `TabularCompressor` emits `ContentKind::Json` (`_schema`/
`_rows`) and is gated by `tabular.enabled`; new assertions don't hold.

- [ ] **Step 3: Rewrite `tabular.rs`**

Replace the entire contents of `zmod/llm-compress/src/compress/tabular.rs` with
the following. Keep the existing `parse_table` function exactly as-is (copy it
unchanged from the current file — it handles CSV/TSV/Markdown shape validation);
only the trait impl and product construction change.

```rust
//! TabularCompressor — CSV/TSV/Markdown table → TOON.
//! Parses a strict table (header, unique non-empty columns, equal column
//! counts, no quoted cells), builds an array-of-objects Value, and encodes it
//! to TOON. Product is kind=Toon, lossy=false. Gated by json.use_toon; claims
//! only when TOON round-trips, is strictly smaller, and fits truncate.max_bytes.

use crate::compress::toon::encode_checked;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::{Map, Value};

pub struct TabularCompressor;

/// Parse + encode + claim pipeline shared by detect and compress.
fn try_toon(text: &str, budget: &Budget) -> Option<String> {
    if !budget.cfg.json.use_toon {
        return None;
    }
    let table = parse_table(text)?;
    let header = &table[0];
    let mut arr: Vec<Value> = Vec::with_capacity(table.len() - 1);
    for row in &table[1..] {
        let mut m = Map::new();
        for (i, key) in header.iter().enumerate() {
            m.insert(key.clone(), Value::String(row[i].clone()));
        }
        arr.push(Value::Object(m));
    }
    let value = Value::Array(arr);
    let toon = encode_checked(&value)?;
    if toon.len() < text.len() && toon.len() <= budget.cfg.truncate.max_bytes {
        Some(toon)
    } else {
        None
    }
}

impl Compressor for TabularCompressor {
    fn name(&self) -> &'static str {
        "tabular"
    }

    fn detect(&self, text: &str, budget: &Budget) -> bool {
        try_toon(text, budget).is_some()
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        match try_toon(text, budget) {
            Some(toon) => {
                let saved = text.len().saturating_sub(toon.len());
                if saved == 0 {
                    return CompressOutcome::Unchanged;
                }
                CompressOutcome::Compressed {
                    text: toon,
                    saved_bytes: saved,
                    lossy: false,
                    kind: ContentKind::Toon,
                }
            }
            None => CompressOutcome::Unchanged,
        }
    }
}

// ===== parse_table: COPY UNCHANGED FROM THE CURRENT FILE =====
// Paste the existing `fn parse_table(text: &str) -> Option<Vec<Vec<String>>>`
// here verbatim (CSV/TSV/Markdown parsing + strict shape validation).
```

IMPORTANT: do not reimplement `parse_table` from memory — copy the existing
function body from the current `tabular.rs` so the shape rules stay identical.

- [ ] **Step 4: Run the tabular test to verify it passes**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(/tabular_test/)'
```

Expected: PASS.

- [ ] **Step 5: Update the orchestrator to isolate Toon from CCR**

In `zmod/llm-compress/src/lib.rs`, the `compress_in_place` match currently is:

```rust
    let candidate = match router.compress_text(&pre, &budget) {
        Some((new, comp_lossy, kind)) => {
            // kind=Json ⟹ 产物是合法 JSON,绝不追加 CCR(§4.0/§4.7 铁律)。
            // pre_lossy 仅表示预处理删了内容;但若路由器产出 JSON,写回 JSON
            // 不可再追加"[llm-compress: 原文 /path]"——那会破坏 JSON 合法性。
            // 规则:只有 kind==Text 且(pre_lossy 或 comp_lossy)才 attach CCR。
            if kind == ContentKind::Json {
                candidate_is_json = true;
                new
            } else if pre_lossy || comp_lossy {
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
```

Replace the inner `if kind == ContentKind::Json { … } else if … { … } else { … }`
with a `match` covering the new variant. Structured products (Json AND Toon)
never get a CCR pointer; only `Text` may. The `candidate_is_json` flag (which
drives the JSON-reparse write-back gate below) stays true ONLY for Json, since
TOON must not be re-parsed as JSON:

```rust
    let candidate = match router.compress_text(&pre, &budget) {
        Some((new, comp_lossy, kind)) => {
            // Structured products (Json/Toon) are NEVER decorated with a CCR
            // pointer: appending "[llm-compress: 原文 …]" would corrupt the
            // JSON, or break TOON's decodability (TOON is the model's only
            // view of this output). Only kind==Text may carry a CCR pointer,
            // and only when content was actually dropped (pre/comp lossy).
            match kind {
                ContentKind::Json => {
                    candidate_is_json = true;
                    new
                }
                ContentKind::Toon => new,
                ContentKind::Text => {
                    if pre_lossy || comp_lossy {
                        crate::ccr::attach(new, s, ctx, call_id, &cfg.ccr)
                    } else {
                        new
                    }
                }
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
```

(The `let json_valid = !candidate_is_json || …` gate and the
`if candidate.len() <= s.len() && json_valid` write-back below stay unchanged.
For a Toon candidate, `candidate_is_json` is false, so `json_valid` is true and
the candidate is written back if it is not larger.)

- [ ] **Step 6: Add the end-to-end CCR-isolation test**

This is the regression test for Step 5. It must construct an input that
*genuinely* makes a Toon compressor claim AND triggers lossy preprocessing — so
the bug (Toon + CCR pointer) would fire if Step 5 were absent.

Append to `zmod/llm-compress/tests/orchestration_test.rs`:

```rust
/// End-to-end iron law: a TOON product is written back as BARE TOON, never
/// decorated with a "[llm-compress: 原文 …]" pointer — even when preprocessing
/// was lossy (a leading progress line is stripped → pre_lossy=true).
#[test]
#[serial]
fn toon_kind_never_gets_ccr_attached() {
    let _home = setup_enabled_home();

    // A leading progress line is removed by strip_progress → pre_lossy=true.
    // The remaining single-line JSON array is parsed by JsonCompressor and
    // encoded to TOON. Keep the JSON line < 2000 bytes (preprocess
    // truncate_line_bytes) and the total >= 1024 bytes (per_item_min_bytes).
    let mut objs = String::from("[");
    for i in 0..40u32 {
        if i > 0 {
            objs.push(',');
        }
        objs.push_str(&format!(r#"{{"id":{i},"name":"item_{i}"}}"#));
    }
    objs.push(']');
    assert!(objs.len() < 2000 && objs.len() > 600, "json line len = {}", objs.len());
    let input_text = format!("Downloading crates.io index\n{objs}");
    assert!(input_text.len() >= 1024, "input must exceed per_item_min_bytes: {}", input_text.len());

    let mut r = req(vec![fco("call-toon", &input_text)]);
    transform(&mut r, &provider(), "qid-toon-ccr");

    let out = get_text(&r.input[0]);

    // The output must NOT carry a CCR pointer.
    assert!(
        !out.contains("[llm-compress: 原文 "),
        "TOON product must not carry a CCR pointer!\nout: {:?}",
        &out[..out.len().min(300)]
    );
    // And it must have actually been compressed (smaller than input) and be
    // valid TOON that round-trips. (If for some reason it wasn't claimed, the
    // output would equal pre-processed text; assert the compression happened.)
    assert!(out.len() < input_text.len(), "expected compression: {} vs {}", out.len(), input_text.len());
    let back: serde_json::Value = toon_format::decode_default(out)
        .expect("output must be decodable TOON");
    assert!(back.is_array(), "decoded TOON should be the original array");
}
```

Note: `get_text` is the existing helper in this test file (used by the JSON CCR
test). If it is not yet defined as a free fn, reuse the same extraction the
neighboring tests use to pull the `FunctionCallOutputBody::Text` out of
`r.input[0]`.

- [ ] **Step 7: Run the orchestration test to verify it passes**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(toon_kind_never_gets_ccr_attached)'
```

Expected: PASS.

- [ ] **Step 8: Run the full suite**

Run:

```bash
cargo nextest run -p codez-llm-compress
```

Expected: all PASS except `schema_test.rs` and the `csv_schema` reference in
`config_test.rs`, which Task 5 removes. If the old
`json_kind_never_gets_ccr_attached` test still passes, leave it (it asserts the
Json path, still valid). Do not delete it here.

- [ ] **Step 9: Commit**

```bash
git add zmod/llm-compress/src/compress/tabular.rs \
        zmod/llm-compress/src/lib.rs \
        zmod/llm-compress/tests/tabular_test.rs \
        zmod/llm-compress/tests/orchestration_test.rs
git commit -m "feat(llm-compress): TabularCompressor emits TOON; orchestrator never attaches CCR to Toon"
```
