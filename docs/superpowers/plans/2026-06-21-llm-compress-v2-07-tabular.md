# Task 07 — tabular.rs:TabularCompressor

> Part of `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §5④. Depends on Task 01 (schema.rs / signatures). Can run in parallel with 05/06/08/09.

**Goal:** Add `TabularCompressor`: recognize CSV/TSV/Markdown tables. When the **strict preconditions** are met (has a header, column names unique and non-empty, consistent column count, no escaped separators / no cell line breaks), parse into an array of objects → call `schema::to_schema_form` → emit valid JSON. `kind=Json, lossy=false`, no CCR attached. The precondition check happens **inside detect** (to fit the router's first-match behavior: if not satisfied, detect returns false so Truncate takes over).

## Files
- Create: `zmod/llm-compress/src/compress/tabular.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs` (add `pub mod tabular;`)
- Test: `zmod/llm-compress/tests/tabular_test.rs`

**Interfaces:**
- Consumes: Task 01's `schema::to_schema_form`, `Budget`/`CompressOutcome`/`ContentKind`/`Compressor`, `config.tabular.enabled`.
- Produces: `pub struct TabularCompressor;` (impl Compressor).

> **Algorithm (spec §5④)**: `parse_table(text) -> Option<Vec<Vec<String>>>` (first line is the header + data rows, consistent column count; otherwise None). detect: `cfg.tabular.enabled` AND `parse_table` succeeds AND the header is unique and non-empty → true. compress: convert to `Vec<serde_json::Value::Object>` → `Value::Array` → `schema::to_schema_form`. CSV is comma-separated (simple parsing: if there's no quote escaping, split on `,`; **a cell containing quotes or line breaks → parse_table returns None**); Markdown is `|`-separated, skipping the `|---|` separator row.

---

- [ ] **Step 1: Write the failing test**

Create `zmod/llm-compress/tests/tabular_test.rs`:

```rust
use codez_llm_compress::compress::tabular::TabularCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn csv_with_header_to_schema() {
    let cfg = Config::disabled(); // tabular.enabled defaults to true
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob\n3,carol";
    assert!(c.detect(text, &budget(&cfg)));
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &budget(&cfg)) {
        assert!(!lossy, "format restructuring does not drop content");
        assert_eq!(kind, ContentKind::Json);
        let v: serde_json::Value = serde_json::from_str(&new).expect("valid json");
        assert_eq!(v["_schema"], serde_json::json!(["id","name"]));
        assert_eq!(v["_rows"][0], serde_json::json!(["1","alice"]));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn markdown_table_to_schema() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "| id | name |\n|----|------|\n| 1 | alice |\n| 2 | bob |";
    assert!(c.detect(text, &budget(&cfg)));
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &budget(&cfg)) {
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["_schema"], serde_json::json!(["id","name"]));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn duplicate_column_names_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "id,id\n1,2\n3,4";
    assert!(!c.detect(text, &budget(&cfg)), "duplicate column names → detect false so Truncate takes over");
}

#[test]
fn ragged_columns_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b,c\n1,2\n3,4,5";
    assert!(!c.detect(text, &budget(&cfg)), "inconsistent column count → detect false");
}

#[test]
fn quoted_cell_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b\n\"x,y\",z\n1,2";
    assert!(!c.detect(text, &budget(&cfg)), "cell with quote escaping → detect false");
}

#[test]
fn disabled_config_detect_false() {
    let mut cfg = Config::disabled();
    cfg.tabular.enabled = false;
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob";
    assert!(!c.detect(text, &budget(&cfg)));
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test tabular_test 2>&1 | head`
Expected: FAIL (the `tabular` module does not exist)

- [ ] **Step 3: Implement tabular.rs**

Create `zmod/llm-compress/src/compress/tabular.rs`:

```rust
//! TabularCompressor — CSV/TSV/Markdown tables → csv-schema (spec §5④).
//! The strict preconditions are checked inside detect; only claimed when satisfied. kind=Json, lossy=false, no CCR attached.

use crate::compress::schema::to_schema_form;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::{Map, Value};

pub struct TabularCompressor;

impl Compressor for TabularCompressor {
    fn name(&self) -> &'static str {
        "tabular"
    }

    fn detect(&self, text: &str, budget: &Budget) -> bool {
        if !budget.cfg.tabular.enabled {
            return false;
        }
        parse_table(text).is_some()
    }

    fn compress(&self, text: &str, _budget: &Budget) -> CompressOutcome {
        let table = match parse_table(text) {
            Some(t) => t,
            None => return CompressOutcome::Unchanged,
        };
        // Convert to an array of objects
        let header = &table[0];
        let mut arr: Vec<Value> = Vec::with_capacity(table.len() - 1);
        for row in &table[1..] {
            let mut m = Map::new();
            for (i, key) in header.iter().enumerate() {
                m.insert(key.clone(), Value::String(row[i].clone()));
            }
            arr.push(Value::Object(m));
        }
        let snapshot = Value::Array(arr);
        let schema_form = match to_schema_form(&snapshot) {
            Some(v) => v,
            None => return CompressOutcome::Unchanged,
        };
        let new = match serde_json::to_string(&schema_form) {
            Ok(s) => s,
            Err(_) => return CompressOutcome::Unchanged,
        };
        let saved = text.len().saturating_sub(new.len());
        if saved > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes: saved, lossy: false, kind: ContentKind::Json }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// Parse a CSV/TSV/Markdown table into Vec<row> (row = Vec<cell>), including the header.
/// Strict: must have a header, column names unique and non-empty, all rows with consistent column count, no quote escaping / no cell line breaks; otherwise None.
fn parse_table(text: &str) -> Option<Vec<Vec<String>>> {
    let raw_lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if raw_lines.len() < 2 {
        return None;
    }
    let is_md = raw_lines[0].trim_start().starts_with('|');
    // Cell containing a quote → give up (the simple parser cannot reliably reconstruct it)
    if text.contains('"') {
        return None;
    }
    let split = |line: &str| -> Vec<String> {
        if is_md {
            let t = line.trim().trim_start_matches('|').trim_end_matches('|');
            t.split('|').map(|c| c.trim().to_string()).collect()
        } else if line.contains('\t') {
            line.split('\t').map(|c| c.trim().to_string()).collect()
        } else {
            line.split(',').map(|c| c.trim().to_string()).collect()
        }
    };

    let mut rows: Vec<Vec<String>> = Vec::new();
    for (i, line) in raw_lines.iter().enumerate() {
        // Skip the Markdown separator row |---|---|
        if is_md && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            continue;
        }
        let cells = split(line);
        let _ = i;
        rows.push(cells);
    }
    if rows.len() < 2 {
        return None;
    }
    let ncol = rows[0].len();
    if ncol == 0 {
        return None;
    }
    // header unique and non-empty
    let header = &rows[0];
    if header.iter().any(|h| h.is_empty()) {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for h in header {
        if !seen.insert(h) {
            return None; // duplicate column name
        }
    }
    // consistent column count
    if rows.iter().any(|r| r.len() != ncol) {
        return None;
    }
    Some(rows)
}
```

In `zmod/llm-compress/src/compress/mod.rs`, add `pub mod tabular;`.

- [ ] **Step 4: Run the test to pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test tabular_test`
Expected: PASS (6 tests)

- [ ] **Step 5: clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/compress/tabular.rs zmod/llm-compress/src/compress/mod.rs \
  zmod/llm-compress/tests/tabular_test.rs
git commit -m "feat(llm-compress-v2): Task07 tabular.rs TabularCompressor(strict preconditions + csv-schema)"
```
