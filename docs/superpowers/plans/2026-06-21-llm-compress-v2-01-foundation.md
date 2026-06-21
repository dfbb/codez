# Task 01 — 接口地基:router 类型 + config 扩展 + schema.rs + 现有压缩器签名同步

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。REQUIRED SUB-SKILL 见 index。
> 覆盖 spec §4.0 / §4.1 / §4.8 / §6。这是接口地基,后续所有任务依赖本任务定义的类型。

**Goal:** 改造 `router.rs` 的核心类型(`ContentKind` / `CompressOutcome` 增 `lossy`+`kind` / `Compressor::detect` 吃 budget / `Budget` 增 cmd+ctx / `compress_text` 返回三元组),扩展 `config.rs` 全部新子表,新建 `compress/schema.rs`,并同步现有 4 个压缩器(truncate/json/diff/log)签名使 crate 重新编译通过。

**本任务结束时**:crate 编译通过、现有测试调整后全绿,但新行为(让位/CCR/预处理)尚未接入——那是后续任务。

## Files
- Modify: `zmod/llm-compress/src/router.rs`(全量改 trait 与 outcome)
- Modify: `zmod/llm-compress/src/config.rs`(加 5 个新子表 + 字段)
- Create: `zmod/llm-compress/src/compress/schema.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs`(加 `pub mod schema;`)
- Modify: `zmod/llm-compress/src/compress/truncate.rs`(detect 加 budget;Compressed 加 lossy+kind)
- Modify: `zmod/llm-compress/src/compress/json.rs`(同上;本任务仅签名同步,算法 Task 05 改)
- Modify: `zmod/llm-compress/src/compress/diff.rs`(同上)
- Modify: `zmod/llm-compress/src/compress/log.rs`(同上;本任务仅签名同步,算法 Task 08 改)
- Modify: `zmod/llm-compress/src/lib.rs`(`compress_in_place` 适配新返回类型,过渡接线)
- Modify: `zmod/llm-compress/Cargo.toml`(加 `sha2` 依赖,供后续 ccr/schema 用)
- Test: 现有 `tests/router_test.rs`、`tests/truncate_test.rs`、`tests/json_test.rs`、`tests/diff_test.rs`、`tests/log_test.rs` 断言同步

**Interfaces:**
- Produces(后续任务消费):
  - `pub enum ContentKind { Text, Json }`(derive `Clone, Copy, Debug, PartialEq, Eq`)
  - `pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize, lossy: bool, kind: ContentKind }, Unchanged }`
  - `pub trait Compressor { fn name(&self)->&'static str; fn detect(&self,text:&str,budget:&Budget)->bool; fn compress(&self,text:&str,budget:&Budget)->CompressOutcome; }`
  - `pub struct Budget<'a> { pub cfg: &'a Config, pub cmd: Option<&'a CommandHint>, pub ctx: Option<&'a RequestCtx> }`(Task 03 定义 `CommandHint`,Task 11 定义 `RequestCtx`;本任务用前向声明占位,见 Step 5)
  - `pub fn compress_text(&self, text:&str, budget:&Budget) -> Option<(String, bool, ContentKind)>`(text, lossy, kind)
  - `config::Config` 新增子表:`preprocess: PreprocessCfg`、`search: SearchCfg`、`tabular: TabularCfg`、`protect: ProtectCfg`、`ccr: CcrCfg`;`json` 增 `csv_schema: bool`;`log` 增 `template_min_run: usize`、`keep_levels: Vec<String>`
  - `schema::to_schema_form(value: &serde_json::Value) -> Option<serde_json::Value>`

> **设计决策(实现者必读)**:`Budget` 携带 `cfg` + `cmd: Option<&CommandHint>` + `query: &[String]`——压缩器只需这三者(查询词供 score 加权,命令提示供 detect)。**不把 `RequestCtx`/CCR registry 塞进 Budget**:CCR 是编排层(Task 11)的事,压缩器不碰落盘。`CommandHint` 的**类型定义 + is_* 方法**在本任务建立(供 Budget 编译),其 `index()` 解析函数在 Task 03 实现。

- [ ] **Step 1: 加 sha2 依赖**

编辑 `zmod/llm-compress/Cargo.toml`,在 `[dependencies]` 末尾(`tracing = "0.1"` 行后)加:

```toml
sha2 = "0.10"
```

- [ ] **Step 2: 运行确认 crate 当前能编译(基线)**

Run: `cd codex-rs && cargo build -p codez-llm-compress`
Expected: PASS(改动前基线;若失败先确认软链 member 就位,见 index 开发期构建)

- [ ] **Step 3: 创建 command.rs 的 CommandHint 类型骨架(解析留给 Task 03)**

创建 `zmod/llm-compress/src/command.rs`:

```rust
//! call_id → 命令提示。类型与判别在此(Task 01 地基);index() 解析在 Task 03。
use std::collections::HashMap;

/// 从 FunctionCall 解析出的命令提示,仅作路由提示用(spec §4.3)。
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

/// Task 03 实现:遍历 request.input 的 FunctionCall 建 call_id→CommandHint 索引。
/// 本任务先给空实现占位,使 lib 可编译;Task 03 替换。
pub fn index(_request: &codex_api::ResponsesApiRequest) -> HashMap<String, CommandHint> {
    HashMap::new()
}
```

- [ ] **Step 4: lib.rs 注册 command 模块**

在 `zmod/llm-compress/src/lib.rs` 的模块声明区(现有 `pub mod config;` 等附近)加:

```rust
pub mod command;
```

- [ ] **Step 5: 改写 router.rs**

把 `zmod/llm-compress/src/router.rs` 全量替换为(在现有 `Budget`/`CompressOutcome`/`Compressor`/`ContentRouter` 基础上扩展):

```rust
//! 压缩器公共契约 + ContentRouter(命令感知重排 + first-match + fail-open)。

use crate::command::CommandHint;
use crate::config::Config;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// 产物形态。Json 恒配 lossy=false(spec §4.0 铁律)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Json,
}

/// 压缩器从中取配置/命令提示/查询词。
pub struct Budget<'a> {
    pub cfg: &'a Config,
    pub cmd: Option<&'a CommandHint>,
    pub query: &'a [String],
}

/// 单个压缩器对一段文本的处理结果。
pub enum CompressOutcome {
    Compressed {
        text: String,
        saved_bytes: usize,
        lossy: bool,
        kind: ContentKind,
    },
    Unchanged,
}

/// 内容识别 + 压缩。detect 也吃 budget(拿 cmd 做命令感知认领)。
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str, budget: &Budget) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}

/// 固定优先级 + 命令感知重排;首个 detect 命中者压缩;detect/compress 均 catch_unwind。
pub struct ContentRouter {
    compressors: Vec<Box<dyn Compressor>>,
}

impl ContentRouter {
    pub fn new(compressors: Vec<Box<dyn Compressor>>) -> Self {
        Self { compressors }
    }

    /// 返回 Some((new, lossy, kind)) 仅当确有压缩(Compressed 且 saved_bytes>0);
    /// Unchanged / 无命中 / panic → None。命令提示命中时把对应压缩器提到候选最前。
    pub fn compress_text(&self, text: &str, budget: &Budget) -> Option<(String, bool, ContentKind)> {
        // 命令感知重排:命中的命令把对应 name 的压缩器排到最前(稳定,仅前移一个)。
        let preferred: Option<&'static str> = budget.cmd.and_then(|c| {
            if c.is_git_diff() {
                Some("diff")
            } else if c.is_grep() {
                Some("search")
            } else {
                None
            }
        });

        // 构造候选迭代顺序(索引列表):preferred 命中的先排,其余按原序。
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

- [ ] **Step 6: 编译验证 router(预期现有压缩器报错)**

Run: `cd codex-rs && cargo build -p codez-llm-compress 2>&1 | head -30`
Expected: FAIL,报现有 truncate/json/diff/log 的 `detect` 签名不匹配、`Compressed{..}` 缺字段。这是下一步要修的。

- [ ] **Step 7: 同步 truncate.rs 签名**

`zmod/llm-compress/src/compress/truncate.rs`:

改 detect 签名(原 `fn detect(&self, _text: &str) -> bool`)为:

```rust
    fn detect(&self, _text: &str, _budget: &Budget) -> bool {
        true
    }
```

改 compress 内唯一的 `Compressed` 构造(原 `CompressOutcome::Compressed { text: result, saved_bytes }`)为:

```rust
            CompressOutcome::Compressed { text: result, saved_bytes, lossy: true, kind: ContentKind::Text }
```

并确保文件顶部 `use` 引入 `ContentKind`(原 `use crate::router::{Budget, CompressOutcome, Compressor};` 改为):

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

- [ ] **Step 8: 同步 json.rs 签名(仅签名,算法 Task 05 改)**

`zmod/llm-compress/src/compress/json.rs`:

顶部 use 加 `ContentKind`:

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

detect 改(原 `fn detect(&self, text: &str) -> bool`):

```rust
    fn detect(&self, text: &str, _budget: &Budget) -> bool {
        matches!(
            serde_json::from_str::<Value>(text),
            Ok(Value::Object(_)) | Ok(Value::Array(_))
        )
    }
```

compress 内唯一 `Compressed` 构造(原 `CompressOutcome::Compressed { text: new, saved_bytes }`)改为(JSON 恒不删内容 → `lossy: false, kind: Json`):

```rust
            CompressOutcome::Compressed { text: new, saved_bytes, lossy: false, kind: ContentKind::Json }
```

- [ ] **Step 9: 同步 diff.rs 签名**

`zmod/llm-compress/src/compress/diff.rs`:

顶部 use 加 `ContentKind`:

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

detect 改(原 `fn detect(&self, text: &str) -> bool`,保留函数体不变,只加参数):

```rust
    fn detect(&self, text: &str, _budget: &Budget) -> bool {
```

compress 内唯一 `Compressed` 构造(原 `CompressOutcome::Compressed { text: result, saved_bytes }`)改为(diff 折叠删上下文 → `lossy: true, kind: Text`):

```rust
        CompressOutcome::Compressed { text: result, saved_bytes, lossy: true, kind: ContentKind::Text }
```

- [ ] **Step 10: 同步 log.rs 签名(仅签名,算法 Task 08 改)**

`zmod/llm-compress/src/compress/log.rs`:

顶部 use 加 `ContentKind`:

```rust
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
```

detect 改(原 `fn detect(&self, text: &str) -> bool`,函数体不变,只加参数):

```rust
    fn detect(&self, text: &str, _budget: &Budget) -> bool {
```

compress 内唯一 `Compressed` 构造(原 `CompressOutcome::Compressed { text: new_text, saved_bytes: saved }`)改为(v1 log 做 head/tail 截断 → 暂标 `lossy: true, kind: Text`;Task 08 重写时再细分):

```rust
                CompressOutcome::Compressed { text: new_text, saved_bytes: saved, lossy: true, kind: ContentKind::Text }
```

- [ ] **Step 11: 适配 lib.rs(过渡接线,完整编排留 Task 11)**

`zmod/llm-compress/src/lib.rs` 现有 `Budget { cfg: &cfg }` 构造与 `compress_in_place` 用的是旧签名。本任务做**最小过渡适配**让其编译,完整 RequestCtx 编排在 Task 11。

改 `transform` 里构造 router/budget 处与 `compress_in_place`:

现有(v1):
```rust
    let router = build_router();
    let budget = Budget { cfg: &cfg };

    for item in request.input.iter_mut() {
        compress_item(item, &router, &budget, cfg.per_item_min_bytes);
    }
```

改为(过渡:query/cmd 暂空,Task 11 接真值):
```rust
    let router = build_router();
    let empty_query: Vec<String> = Vec::new();
    let budget = Budget { cfg: &cfg, cmd: None, query: &empty_query };

    for item in request.input.iter_mut() {
        compress_item(item, &router, &budget, cfg.per_item_min_bytes);
    }
```

改 `compress_in_place`(现有 `if let Some(new) = router.compress_text(s, budget) { *s = new; }`)为(解构三元组,本任务先忽略 lossy/kind,体积闸门保留):
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

- [ ] **Step 12: 扩展 config.rs —— 新子表结构体定义**

`zmod/llm-compress/src/config.rs`:在现有 `LogCfg` 定义后、`impl Default for Config` 前,加新子表结构体(每个都 `#[derive(Debug, Clone, Deserialize)]` + `#[serde(default)]`):

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

- [ ] **Step 13: config.rs —— Config 结构体加新字段 + JsonCfg/LogCfg 扩展**

在 `pub struct Config` 内(现有 `truncate/json/diff/log` 字段后)追加:

```rust
    pub preprocess: PreprocessCfg,
    pub search: SearchCfg,
    pub tabular: TabularCfg,
    pub protect: ProtectCfg,
    pub ccr: CcrCfg,
```

在 `pub struct JsonCfg` 内(现有 `max_array_items`、`max_depth` 后)追加:

```rust
    pub csv_schema: bool,
```

在 `pub struct LogCfg` 内(现有 `dedup_repeats` 后)追加:

```rust
    pub template_min_run: usize,
    pub keep_levels: Vec<String>,
```

- [ ] **Step 14: config.rs —— Default 实现**

在 `impl Default for Config` 的 `Self { ... }` 内(现有字段后)追加(值取 spec §6):

```rust
            preprocess: PreprocessCfg::default(),
            search: SearchCfg::default(),
            tabular: TabularCfg::default(),
            protect: ProtectCfg::default(),
            ccr: CcrCfg::default(),
```

在 `impl Default for JsonCfg` 的 `Self { ... }` 内追加 `csv_schema: true`(整体变为 `Self { max_array_items: 20, max_depth: 6, csv_schema: true }`)。

在 `impl Default for LogCfg` 的 `Self { ... }` 内追加(整体变为):

```rust
        Self { dedup_repeats: true, template_min_run: 3, keep_levels: vec!["error".to_string(), "warn".to_string()] }
```

文件末尾追加新子表的 Default 实现:

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

- [ ] **Step 15: 创建 compress/schema.rs**

创建 `zmod/llm-compress/src/compress/schema.rs`:

```rust
//! csv-schema 内表达公共模块(JSON §5① + Tabular §5④ 共用)。
//! 对象数组(各项同构)→ {"_schema":[...],"_rows":[[...]]}。纯格式重构、内容全保留 → lossy=false。

use serde_json::{Map, Value};

/// 对象数组(各项同构)→ {"_schema":[...],"_rows":[[...]]}。非同构/非数组/空 → None。
/// 同构判定:所有元素都是 object 且键集合相同(键顺序以首元素为准)。
/// 标量与嵌套值都按 schema 列顺序放进 _rows 的行(嵌套值保留原样,不扁平)。
pub fn to_schema_form(value: &Value) -> Option<Value> {
    let arr = value.as_array()?;
    if arr.len() < 2 {
        return None; // 单元素无收益
    }
    let first = arr.first()?.as_object()?;
    if first.is_empty() {
        return None;
    }
    // schema = 首元素键序
    let schema: Vec<String> = first.keys().cloned().collect();
    let key_set: std::collections::BTreeSet<&String> = first.keys().collect();

    let mut rows: Vec<Value> = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object()?; // 任一非 object → None
        // 键集合必须完全一致
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

在 `zmod/llm-compress/src/compress/mod.rs` 加:

```rust
pub mod schema;
```

- [ ] **Step 16: 同步现有测试的接口调用**

新接口改了 3 处签名,现有测试需同步。逐文件改:

**`tests/router_test.rs`**:三个假压缩器(`HalfCompressor`/`NeverCompressor`/`PanicCompressor` 及 `unchanged_outcome_returns_none` 内的 `Claims`)实现了 `Compressor` trait。每个:
- `fn detect(&self, _t: &str) -> bool` → `fn detect(&self, _t: &str, _b: &Budget) -> bool`
- 返回 `Compressed { text: new, saved_bytes: saved }` → `Compressed { text: new, saved_bytes: saved, lossy: true, kind: ContentKind::Text }`
- 顶部 `use` 加 `ContentKind`:`use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentRouter, ContentKind};`
- `Budget { cfg }` 构造 → `Budget { cfg, cmd: None, query: &[] }`
- `compress_text` 断言:原 `out.unwrap().starts_with("[half]")` → 解构三元组:`let (text, _lossy, _kind) = out.unwrap(); assert!(text.starts_with("[half]"));`;`assert!(r.compress_text(..).is_none())` 不变(仍是 Option)。

**`tests/truncate_test.rs`**:
- 顶部 use 加 `ContentKind`。
- 所有 `TruncateCompressor.detect(input)` → `.detect(input, &budget(&cfg))`(detect 现需 budget;`budget()` helper 已存在)。`detect_is_always_true` 同改。
- 所有 `match`/解构 `CompressOutcome::Compressed { text, saved_bytes }` → 补 `, lossy: _, kind: _`(或 `..`):`CompressOutcome::Compressed { text, saved_bytes, .. }`。
- `budget()` helper(`Budget { cfg }`)→ `Budget { cfg, cmd: None, query: &[] }`。

**`tests/json_test.rs`**:
- 顶部 use 加 `ContentKind`(若解构需要)。
- 所有 `c.detect(...)` → `c.detect(..., &budget(&cfg))`(detect 加 budget;注意 `detect_accepts_objects_arrays_rejects_scalars_and_garbage` 用例无 budget 时构造一个:`let cfg = Config::disabled(); let b = budget(&cfg);` 然后 `c.detect(x, &b)`)。
- 所有解构 `CompressOutcome::Compressed { text, saved_bytes }` / `{ text, .. }` → 补 `..`。
- `budget()` helper → `Budget { cfg, cmd: None, query: &[] }`。

**`tests/diff_test.rs`**、**`tests/log_test.rs`**:同样把 `.detect(x)` → `.detect(x, &budget(&cfg))`、`Budget{cfg}` → `Budget{cfg, cmd:None, query:&[]}`、解构 `Compressed{..}` 补 `..`、顶部 use 按需加 `ContentKind`。

> 提示:用 `cargo build -p codez-llm-compress --tests 2>&1 | head -40` 看编译错误逐个定位,比手工通读快。

- [ ] **Step 17: 写 schema.rs 单测**

创建 `zmod/llm-compress/tests/schema_test.rs`:

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
    // 嵌套对象原样放进行,不扁平
    assert_eq!(out["_rows"][0][1], json!({"x":9}));
}

#[test]
fn single_element_returns_none() {
    let v = json!([{"id":1}]);
    assert!(to_schema_form(&v).is_none());
}
```

- [ ] **Step 18: 编译 + 跑全部测试**

Run: `cd codex-rs && cargo build -p codez-llm-compress --tests`
Expected: PASS(无编译错误)

Run: `cd codex-rs && cargo test -p codez-llm-compress`
Expected: 全绿(现有测试 + 新 schema_test 5 个)

Run: `cd codex-rs && cargo clippy -p codez-llm-compress --all-targets`
Expected: 无 warning

- [ ] **Step 19: 提交**

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
git commit -m "feat(llm-compress-v2): Task01 接口地基 — router lossy+kind/detect budget + config 扩展 + schema.rs + 压缩器签名同步"
```






