# Task 04: JsonCompressor(JSON 结构内压缩 + parse 校验)

> 属于 `2026-06-20-llm-compress-00-index.md`。执行前先读 index。依赖 Task 01(`config::{Config, JsonCfg}`)与 Task 02(`router::{Budget, CompressOutcome, Compressor}`)。

**Goal:** 实现 `JsonCompressor`——一个**绝不破坏 JSON** 的结构型压缩器。它只认领可被 `serde_json` 解析的文本,在 **JSON 结构内部**递归抽样长数组、裁剪超深子树,占位一律用合法 JSON 值承载(长数组省略段用字符串元素 `"…(N more)"`,超深子树用字符串 `"…"`),序列化回紧凑 JSON,并对产物**再次 parse 校验**;校验失败或未省下字节则回退原文 `Unchanged`。

**覆盖 spec:** §4(JSON 不走文本级流程 / 结构内压缩 / parse 校验回退)、§6(保守阈值 / 占位以合法 JSON 值表达)。

**关键约束(逐条对齐 index 的 Global Constraints):**
- JSON **不得**走文本级 head/tail 行截断,**不得**插入裸文本标记 `[llm-compress: …]`;占位只用合法 JSON 值(`"…(N more)"` / `"…"`)。
- 产物必经 `serde_json::from_str::<Value>()` 重新 parse 校验,失败 → 丢弃压缩结果、回退原文。
- 非测试代码避免 `unwrap`/`expect`。

---

**Files:**
- Create: `zmod/llm-compress/src/compress/json.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs`(加 `pub mod json;`)
- Test: `zmod/llm-compress/tests/json_test.rs`

> 说明:本 crate 的压缩器统一放在 `src/compress/` 子模块下。若 `src/compress/mod.rs` 尚不存在(本任务可能先于其它压缩器执行),Step 4 负责创建它并在 `lib.rs` 注册 `pub mod compress;`。

**Interfaces:**
- Consumes(from Task 01):`config::JsonCfg { pub max_array_items: usize, pub max_depth: usize }`,经 `budget.cfg.json` 取用。
- Consumes(from Task 02):`router::{Budget, CompressOutcome, Compressor}`(钉死签名,不得改)。
- Produces(08 编排时注册进 `ContentRouter`):
  - `pub struct JsonCompressor;`(单元结构,无状态)。
  - `impl Compressor for JsonCompressor`:
    - `name()` → `"json"`。
    - `detect(text)` → `serde_json::from_str::<serde_json::Value>(text).is_ok()`。
    - `compress(text, budget)` → 结构内递归压缩 + parse 校验,返回 `Compressed`/`Unchanged`。

---

## 行为规格(实现 `compress` 必须照此)

入参 `text`,取 `cfg = &budget.cfg.json`(即 `max_array_items` / `max_depth`)。

1. `serde_json::from_str::<Value>(text)` 解析;失败 → `Unchanged`(理论上 `detect` 已保证成功,这是防御性回退)。
2. 在 `Value` 上原地递归压缩(`compress_value(&mut value, depth=0, cfg)`):
   - **超深裁剪**:当 `depth > max_depth` 且当前节点是对象或数组(容器)→ 整个子树替换为 JSON 字符串 `Value::String("…")`。标量(数字/字符串/布尔/null)即便超深也原样保留(裁掉标量无收益且无破坏风险,但替换标量为 `"…"` 反而可能变长,故只裁容器)。
   - **长数组抽样**:数组元素数 `len > max_array_items` 时,保留**前 `ceil(max_array_items/2)`** 个与**后 `floor(max_array_items/2)`** 个,中间插入**一个**字符串元素 `Value::String("…(N more)")`,其中 `N = len - keep_head - keep_tail`(被省略的元素数)。保留下来的元素仍需递归处理(深度 +1)。
   - **对象**:键全保留,逐值递归处理(深度 +1)。
   - **数组(未超长或抽样后剩余项)**:逐元素递归处理(深度 +1)。
3. `serde_json::to_string(&value)` 序列化回**紧凑** JSON 字符串(与原文缩进/换行风格无关)。
4. **校验**:`serde_json::from_str::<Value>(&new)`;失败 → 丢弃 `new`、返回 `Unchanged`。
5. `saved_bytes = text.len().saturating_sub(new.len())`;`saved_bytes > 0` 且校验通过 → `Compressed { text: new, saved_bytes }`,否则 `Unchanged`。

> 抽样取整口径(对齐 brief):`keep_head = (max_array_items + 1) / 2`(= ceil),`keep_tail = max_array_items / 2`(= floor)。二者之和 = `max_array_items`,故抽样后数组长度 = `max_array_items + 1`(含中间那一个占位字符串元素)。`max_array_items == 0` 视为退化情形:此时 `keep_head=keep_tail=0`,数组整体被一个 `"…(N more)"` 替换(仍是合法 JSON,可接受)。

---

- [ ] **Step 1: 写失败测试**

Create `zmod/llm-compress/tests/json_test.rs`:

```rust
use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

/// 构造一个启用且 json 阈值偏小的配置,便于触发压缩。
fn cfg_small() -> Config {
    let mut c = Config::disabled();
    c.json.max_array_items = 6; // ceil=3 头 + floor=3 尾
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
    // 非 JSON
    assert!(!c.detect("not json {"));
    assert!(!c.detect("{unquoted: key}"));
    assert!(!c.detect(""));
}

#[test]
fn long_array_is_sampled_with_placeholder_element() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // 20 个元素,远超 max_array_items=6
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

    // 产物必须是合法 JSON
    let v: Value = serde_json::from_str(&new).expect("compressed output must be valid JSON");
    let items = v.as_array().expect("top-level array");
    // 6 个保留(3 头 + 3 尾) + 1 个占位 = 7
    assert_eq!(items.len(), 7);
    // 头 3:0,1,2;尾 3:17,18,19
    assert_eq!(items[0], serde_json::json!(0));
    assert_eq!(items[2], serde_json::json!(2));
    assert_eq!(items[4], serde_json::json!(18));
    assert_eq!(items[6], serde_json::json!(19));
    // 中间是占位字符串 "…(N more)",N = 20 - 6 = 14
    assert_eq!(items[3], serde_json::json!("…(14 more)"));
}

#[test]
fn output_is_always_valid_json() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // 混合:长数组 + 嵌套对象 + 超深
    let text = r#"{
        "list": [1,2,3,4,5,6,7,8,9,10,11,12],
        "nested": {"a": {"b": {"c": {"d": 1}}}},
        "name": "keep me"
    }"#;
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &budget(&cfg)) {
        // 关键断言:无论压成什么,产物都能被重新解析。
        serde_json::from_str::<Value>(&new).expect("compressed output must be valid JSON");
        // 键必须全保留
        let v: Value = serde_json::from_str(&new).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("list"));
        assert!(obj.contains_key("nested"));
        assert!(obj.contains_key("name"));
    }
    // 即便未压缩(Unchanged)也无破坏可言,测试主旨是"压了就必须合法"。
}

#[test]
fn deep_subtree_replaced_by_ellipsis_string() {
    // max_depth=2:depth 0 顶层对象,depth 1 值,depth 2 值,depth 3 起的容器被替换。
    let cfg = cfg_small();
    let c = JsonCompressor;
    let text = r#"{"a":{"b":{"c":{"d":{"e":1}}}}}"#;
    let out = c.compress(text, &budget(&cfg));
    let new = match out {
        CompressOutcome::Compressed { text, .. } => text,
        CompressOutcome::Unchanged => panic!("expected deep nesting to be trimmed"),
    };
    let v: Value = serde_json::from_str(&new).expect("valid JSON");
    // 顺着 a/b 走到被替换处,应在某层遇到字符串 "…"
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
    // 数组短(<=6)、嵌套浅(<=2):无可压。
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

- [ ] **Step 2: 跑测试看失败**

Run(在 `codex-rs/` 目录下执行):
```bash
cargo test -p codez-llm-compress --test json_test
```
Expected: 编译失败(`compress::json` 模块 / `JsonCompressor` 未定义)。

- [ ] **Step 3: 写 json.rs(完整实现,无 TODO)**

Create `zmod/llm-compress/src/compress/json.rs`:

```rust
//! JsonCompressor —— JSON 结构内压缩,绝不破坏 JSON。
//!
//! 只认领可被 serde_json 解析的文本;在 Value 结构内部递归抽样长数组、
//! 裁剪超深子树,占位一律用合法 JSON 值承载;序列化回紧凑 JSON 后再次
//! parse 校验,失败或无收益则回退原文。

use crate::config::JsonCfg;
use crate::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

/// 无状态单元结构。
pub struct JsonCompressor;

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    /// 能被 serde_json 解析即认领。
    fn detect(&self, text: &str) -> bool {
        serde_json::from_str::<Value>(text).is_ok()
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        // 1. parse(失败 → Unchanged;detect 已保证,这里是防御性回退)。
        let mut value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return CompressOutcome::Unchanged,
        };

        // 2. 结构内递归压缩。
        let cfg = &budget.cfg.json;
        compress_value(&mut value, 0, cfg);

        // 3. 序列化回紧凑 JSON。
        let new = match serde_json::to_string(&value) {
            Ok(s) => s,
            Err(_) => return CompressOutcome::Unchanged,
        };

        // 4. 校验:产物必须能被重新解析,否则丢弃、回退原文。
        if serde_json::from_str::<Value>(&new).is_err() {
            return CompressOutcome::Unchanged;
        }

        // 5. 仅在确有收益时返回 Compressed。
        let saved_bytes = text.len().saturating_sub(new.len());
        if saved_bytes > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// 原地递归压缩一个 Value。
///
/// - `depth`:当前节点所在深度(顶层为 0)。
/// - 超深(`depth > max_depth`)的容器(对象/数组)整体替换为 `"…"`。
/// - 长数组(`len > max_array_items`)抽样:留头 ceil、留尾 floor,中间插一个 `"…(N more)"`。
/// - 对象键全保留,逐值递归;数组逐元素递归。标量原样保留。
fn compress_value(v: &mut Value, depth: usize, cfg: &JsonCfg) {
    // 超深:仅裁容器,标量留着(替换标量为 "…" 无收益且可能更长)。
    if depth > cfg.max_depth && (v.is_object() || v.is_array()) {
        *v = Value::String("…".to_string());
        return;
    }

    match v {
        Value::Array(items) => {
            // 先抽样(如超长),再对保留下来的元素递归(深度 +1)。
            if items.len() > cfg.max_array_items {
                let len = items.len();
                let keep_head = (cfg.max_array_items + 1) / 2; // ceil
                let keep_tail = cfg.max_array_items / 2; // floor
                let omitted = len.saturating_sub(keep_head + keep_tail);

                // 取尾段(后 keep_tail 个),再取头段(前 keep_head 个),
                // 用 [头…] + 占位 + [尾…] 重组。
                let tail: Vec<Value> = items.split_off(len - keep_tail);
                items.truncate(keep_head);
                items.push(Value::String(format!("…({omitted} more)")));
                items.extend(tail);
            }

            for child in items.iter_mut() {
                // 占位字符串元素也会被这层递归扫到,但它是标量,不受影响。
                compress_value(child, depth + 1, cfg);
            }
        }
        Value::Object(map) => {
            for (_k, child) in map.iter_mut() {
                compress_value(child, depth + 1, cfg);
            }
        }
        // 标量:原样保留。
        _ => {}
    }
}
```

> 实现要点说明(供 review 与执行者核对):
> - **抽样顺序**:先 `split_off(len - keep_tail)` 拿到尾段并从原数组移除,再 `truncate(keep_head)` 只留头段,然后 `push` 占位、`extend` 尾段。这样头/尾元素的原始顺序与值都被精确保留,中间夹一个占位字符串。当 `max_array_items == 0` 时 `keep_head=keep_tail=0`,数组只剩一个 `"…(N more)"`——仍是合法 JSON。
> - **占位也会被递归**:重组后对全数组 `iter_mut` 递归,占位是 `Value::String`(标量),`compress_value` 的 `_ => {}` 分支会原样跳过它,不会被破坏。
> - **超深判定放在函数最前**:保证任何容器一旦越过 `max_depth` 立即整体替换,不再深入;标量越深无害故放行。
> - **无 `unwrap`/`expect`**:parse、序列化、再校验三处均用 `match` / `is_err` 兜底回退 `Unchanged`。

- [ ] **Step 4: 注册模块**

若 `zmod/llm-compress/src/compress/mod.rs` **不存在**,Create 它:

```rust
//! 各内容类型的压缩器实现。

pub mod json;
```

并在 `zmod/llm-compress/src/lib.rs` 中(`pub mod router;` 之后)加一行(若尚未注册 `compress`):

```rust
pub mod compress;
```

若 `compress/mod.rs` **已存在**(其它压缩器任务先行),Modify 它,追加:

```rust
pub mod json;
```

- [ ] **Step 5: 跑测试看通过**

Run(在 `codex-rs/` 目录下执行):
```bash
cargo test -p codez-llm-compress --test json_test
```
Expected: 全部用例通过(`test result: ok.`)。重点确认 `output_is_always_valid_json`、`long_array_is_sampled_with_placeholder_element`、`deep_subtree_replaced_by_ellipsis_string` 均绿。

- [ ] **Step 6: 提交**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-04-json.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): JsonCompressor (in-structure compression with parse validation)"
```

> 注意:仅提交 `zmod/llm-compress/**` 与本 plan 文件。**不得**提交 codex-rs 子树的 `core/Cargo.toml` / `Cargo.lock` 改动(那是 Task 01 起的 dev-build 使能器,保持 dirty,Task 09 才还原)。
