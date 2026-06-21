# Task 07 — tabular.rs:TabularCompressor

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §5④。依赖 Task 01(schema.rs/签名)。可与 05/06/08/09 并行。

**Goal:** 新增 `TabularCompressor`:识别 CSV/TSV/Markdown 表格,满足**严格前提**(有 header、列名唯一非空、列数一致、无转义分隔符/单元格换行)时,解析成对象数组 → 调 `schema::to_schema_form` → 输出合法 JSON。`kind=Json, lossy=false`,不挂 CCR。前提判定**在 detect 内**(适配 router first-match,不满足即 detect false 让 Truncate)。

## Files
- Create: `zmod/llm-compress/src/compress/tabular.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs`(加 `pub mod tabular;`)
- Test: `zmod/llm-compress/tests/tabular_test.rs`

**Interfaces:**
- Consumes: Task 01 的 `schema::to_schema_form`、`Budget`/`CompressOutcome`/`ContentKind`/`Compressor`、`config.tabular.enabled`。
- Produces: `pub struct TabularCompressor;`(impl Compressor)。

> **算法(spec §5④)**:`parse_table(text) -> Option<Vec<Vec<String>>>`(首行 header + 数据行,列数一致;否则 None)。detect:`cfg.tabular.enabled` 且 `parse_table` 成功且 header 唯一非空 → true。compress:转 `Vec<serde_json::Value::Object>` → `Value::Array` → `schema::to_schema_form`。CSV 用逗号分隔(简单解析:无引号转义则按 `,` 切;**含引号或换行的单元格 → parse_table 返回 None**);Markdown 用 `|` 分隔,跳过 `|---|` 分隔行。

---

- [ ] **Step 1: 写失败测试**

创建 `zmod/llm-compress/tests/tabular_test.rs`:

```rust
use codez_llm_compress::compress::tabular::TabularCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn csv_with_header_to_schema() {
    let cfg = Config::disabled(); // tabular.enabled 默认 true
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob\n3,carol";
    assert!(c.detect(text, &budget(&cfg)));
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &budget(&cfg)) {
        assert!(!lossy, "格式重构不删内容");
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
    assert!(!c.detect(text, &budget(&cfg)), "重复列名 → detect false 让 Truncate");
}

#[test]
fn ragged_columns_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b,c\n1,2\n3,4,5";
    assert!(!c.detect(text, &budget(&cfg)), "列数不齐 → detect false");
}

#[test]
fn quoted_cell_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b\n\"x,y\",z\n1,2";
    assert!(!c.detect(text, &budget(&cfg)), "含引号转义单元格 → detect false");
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

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test tabular_test 2>&1 | head`
Expected: FAIL(`tabular` 模块不存在)

- [ ] **Step 3: 实现 tabular.rs**

创建 `zmod/llm-compress/src/compress/tabular.rs`:

```rust
//! TabularCompressor —— CSV/TSV/Markdown 表格 → csv-schema(spec §5④)。
//! 严格前提在 detect 内判定;满足才认领。kind=Json, lossy=false,不挂 CCR。

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
        // 转对象数组
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

/// 解析 CSV/TSV/Markdown 表格为 Vec<行>(行 = Vec<单元格>),含 header。
/// 严格:有 header、列名唯一非空、所有行列数一致、无引号转义/单元格换行;否则 None。
fn parse_table(text: &str) -> Option<Vec<Vec<String>>> {
    let raw_lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if raw_lines.len() < 2 {
        return None;
    }
    let is_md = raw_lines[0].trim_start().starts_with('|');
    // 含引号的单元格 → 放弃(简单解析器无法稳定还原)
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
        // Markdown 分隔行 |---|---| 跳过
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
    // header 唯一非空
    let header = &rows[0];
    if header.iter().any(|h| h.is_empty()) {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for h in header {
        if !seen.insert(h) {
            return None; // 重复列名
        }
    }
    // 列数一致
    if rows.iter().any(|r| r.len() != ncol) {
        return None;
    }
    Some(rows)
}
```

在 `zmod/llm-compress/src/compress/mod.rs` 加 `pub mod tabular;`。

- [ ] **Step 4: 运行测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test tabular_test`
Expected: PASS(6 个)

- [ ] **Step 5: clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/compress/tabular.rs zmod/llm-compress/src/compress/mod.rs \
  zmod/llm-compress/tests/tabular_test.rs
git commit -m "feat(llm-compress-v2): Task07 tabular.rs TabularCompressor(严格前提 + csv-schema)"
```

