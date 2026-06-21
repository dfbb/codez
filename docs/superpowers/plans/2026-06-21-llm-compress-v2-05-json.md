# Task 05 — JSON 升级:detect 让位 + 连续 RLE 去重 + csv-schema

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §5①。依赖 Task 01(schema.rs、新签名)。可与 06/07/08/09 并行。

**Goal:** 把 JSON 压缩器从 v1 的"有损抽样/超深裁剪"改为**只做不删内容步骤**(连续 RLE 去重 + csv-schema),`kind=Json` 恒 `lossy=false` 永不挂 CCR。`detect` 内预判"无损压缩后是否仍超 `truncate.max_bytes`",超则返回 false 让 Truncate 接管(适配 router first-match)。

## Files
- Modify: `zmod/llm-compress/src/compress/json.rs`(重写 detect + compress)
- Test: `zmod/llm-compress/tests/json_test.rs`(扩展,Task 01 已同步旧断言)

**Interfaces:**
- Consumes: Task 01 的 `schema::to_schema_form`、`Budget`、`CompressOutcome`、`ContentKind`、`config.json.{max_array_items,max_depth,csv_schema}`、`config.truncate.max_bytes`。
- Produces: 升级后的 `JsonCompressor`(行为变化:不再删元素;detect 会让位)。

> **核心算法(spec §5①)**:compress 内按序做两步无损变换 ——① 连续重复项 RLE 去重(相邻相等项折叠为首项 + `{"_llm_dup_prev":N}`;原数组已含 `_llm_dup_prev` 形态对象则跳过);② csv-schema(同构对象数组调 `schema::to_schema_form`)。两步都不删数据。detect 预判无损产物是否 ≤ `truncate.max_bytes`,是则认领,否则让位。

---

- [ ] **Step 1: 写失败测试(RLE + csv-schema + 让位)**

向 `zmod/llm-compress/tests/json_test.rs` **追加**(保留 Task 01 已同步的现有用例):

```rust
// ========== Task 05 新增 ==========
use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};
use codez_llm_compress::config::Config;

fn budget_t05(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn consecutive_rle_folds_adjacent_duplicates() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000; // 不让位
    let c = JsonCompressor;
    // 4 个相邻相同对象
    let text = r#"[{"a":1},{"a":1},{"a":1},{"a":1},{"b":2}]"#;
    let b = budget_t05(&cfg);
    assert!(c.detect(text, &b));
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &b) {
        assert!(!lossy, "RLE 不删内容");
        assert_eq!(kind, ContentKind::Json);
        let v: serde_json::Value = serde_json::from_str(&new).expect("valid json");
        // 首项保留 + 计数占位,首项 {"a":1} 仍在
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
    cfg.truncate.max_bytes = 50; // 很小
    let c = JsonCompressor;
    // 大数组、无相邻重复、非同构 → 无损压不下来 → 超 50 字节 → detect false
    let text = r#"[{"a":1,"x":"aaaa"},{"b":2,"y":"bbbb"},{"c":3,"z":"cccc"}]"#;
    let b = budget_t05(&cfg);
    assert!(!c.detect(text, &b), "无损压不到 50 字节 → 让位 Truncate");
}

#[test]
fn detect_accepts_when_small_enough() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    let text = r#"{"a":1}"#;
    let b = budget_t05(&cfg);
    assert!(c.detect(text, &b), "小 JSON 未超阈 → 认领");
}

#[test]
fn rle_skips_existing_marker_objects() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    // 原数组已含 _llm_dup_prev 形态对象,不应被折叠改写
    let text = r#"[{"_llm_dup_prev":5},{"_llm_dup_prev":5}]"#;
    let b = budget_t05(&cfg);
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &b) {
        // 不混淆:两个 _llm_dup_prev 对象不被当作 RLE 折叠产物
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert!(v.is_array() || v.is_object());
    }
    // 也允许 Unchanged(无收益);关键是不 panic、产物合法
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test json_test 2>&1 | head -20`
Expected: FAIL(新行为未实现;detect 仍是 Task 01 的纯 parse 判断,无让位)

- [ ] **Step 3: 重写 json.rs(顶部 + RLE + csv-schema 核心)**

把 `zmod/llm-compress/src/compress/json.rs` 全量替换为下面两步(本步先写顶部到 `lossless_compress`):

```rust
//! JsonCompressor —— 只做不删内容步骤(连续 RLE 去重 + csv-schema),kind=Json 恒 lossy=false。
//! detect 内预判:无损压缩后是否仍超 truncate.max_bytes;超则让位 Truncate(spec §5①)。

use crate::compress::schema::to_schema_form;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::Value;

const DUP_KEY: &str = "_llm_dup_prev";

pub struct JsonCompressor;

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    /// 让位判据:能 parse 为对象/数组,且无损压缩后体积 ≤ truncate.max_bytes 才认领。
    /// 否则(含 parse 失败 / 无损压不下来)返回 false,让 Truncate 兜底。
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
        // 校验产物可 parse(现有不变量)
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

/// 无损压缩:递归对每个数组先做连续 RLE 去重,再尝试 csv-schema。均不删数据。
fn lossless_compress(mut value: Value, budget: &Budget) -> Value {
    transform_value(&mut value, budget);
    value
}

fn transform_value(v: &mut Value, budget: &Budget) {
    match v {
        Value::Array(items) => {
            // 先递归子元素
            for child in items.iter_mut() {
                transform_value(child, budget);
            }
            // 连续 RLE 去重(就地)
            rle_dedup(items);
            // csv-schema:整段数组同构 → 替换为 schema 形态
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

/// 连续相邻相等项折叠为:首项 + {"_llm_dup_prev": N}(N=额外重复次数)。
/// 本身即含 _llm_dup_prev 键的对象不参与折叠(#6,避免占位混淆)。
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
        let extra = j - i - 1; // 额外重复次数
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

- [ ] **Step 4: 运行 json 测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test json_test`
Expected: PASS(Task 01 同步的旧用例 + Task 05 新用例)

> 若旧用例(v1 抽样相关,如 `long_array_is_sampled_with_placeholder_element`、`deep_subtree_replaced_by_ellipsis_string`)因移除抽样而失败:这些断言的是已删除的 v1 有损行为,**应删除或改写**为新行为断言。删除这些旧用例(它们测的是被 Task 05 移除的功能),保留 `detect_accepts_objects_arrays_rejects_scalars_and_garbage`(若与新 detect 让位冲突也需调整为带 budget 且阈值充足的版本)。

- [ ] **Step 5: clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/compress/json.rs zmod/llm-compress/tests/json_test.rs
git commit -m "feat(llm-compress-v2): Task05 JSON 升级 — detect 让位 + 连续RLE + csv-schema(不删内容)"
```

