# llm-compress v2 设计文档 —— 压缩能力扩展

**日期**:2026-06-21
**crate**:`codez-llm-compress`(`zmod/llm-compress/`,已存在 v1)
**状态**:已批准设计,待写实现计划
**前序**:v1 设计见 `docs/superpowers/specs/2026-06-20-llm-compress-design.md`(已落地:Json/Diff/Log/Truncate 四压缩器 + ContentRouter + fail-open + CSV 统计日志)

> 语言规范:本仓库所有对话与文档统一使用中文。

---

## 1. 目标与边界

在 v1 基础上扩展 `codez-llm-compress` 的压缩能力,对标参考实现 `../3rd/compress/headroom`(内容路由 + 多压缩器)与 `../3rd/compress/rtk`(命令输出过滤管线),并新增 CCR(原文取回)机制。

**核心约束(继承自 v1,不变)**:

- 单一入口 `transform(&mut request, &api_provider, queryid)`,返回 `()`,**不改签名**。
- **不改集成点 patch**:所有新能力的输入(命令上下文、查询关键词)都从 transform 已持有的 `request` 内提取,不新增 `core/src/client.rs` 的触点,不碰 codex 工具系统。
- 只处理 `FunctionCallOutput` / `CustomToolCallOutput` 两个变体的 `output` 文本;文本提取规则不变(`Text(s)` 压 `s`;`ContentItems` 仅压 `InputText.text`,图片/加密内容不动,绝不 flatten)。
- **fail-open 贯穿**:任何环节出问题退回原文/跳过,绝不阻断请求。
- 占位标记统一 `[llm-compress: …]`。压后体积 ≤ 压前。UTF-8 安全。非测试代码不用 `unwrap`/`expect`(catch_unwind 边界除外)。

**范围(本次纳入,12 项 + 共享原语)**:

| 组 | 项 | 形态 | 可逆性 | 挂 CCR |
|---|---|---|---|---|
| A 新压缩器 | Search(grep/ripgrep) | 新 Compressor | 有损 | ✓ |
| | Tabular(CSV/TSV/MD 表格) | 新 Compressor | 无损 | ✗ |
| B 升级 | Log → 错误优先 + 模板挖掘(RLE) | 改写 log.rs | 有损+无损 | 有损部分 |
| | JSON +csv-schema +完全去重 | 增量 json.rs | 无损+有损抽样 | 有损部分 |
| C 新手段 | base64/blob 折叠 | 共享,Truncate 前置 | 有损 | ✓ |
| | 错误输出保护 | router 前置门 | —不压 | — |
| D rtk | 通用预处理(进度条/空行/超长行/连续重复) | 新 preprocess.rs | 多无损 | ✗ |
| | 命令感知路由(call_id→命令名,仅路由提示) | 新 command.rs | — | — |
| E CCR | 落盘+路径占位,LRU 清理 | 新 ccr.rs | — | — |
| 共享 | 评分(内容特征+最后user消息加权) | 新 score.rs | — | — |
| 共享 | 查询关键词提取 | 新 query.rs | — | — |

**范围外(有意排除,违背薄型/不可逆/热路径定位)**:Code(tree-sitter AST)、Kompress(ML 模型)、HTML 提取(trafilatura)、Magika ML 检测、消息级裁剪/滚动窗口、CacheAligner、Net-Cost Gate、按 auth 模式差异化策略、跨会话学习(TOIN)、token 计数。

---

## 2. 总体架构与数据流

沿用 v1 的 `ContentRouter` + `Compressor` trait + fail-open。新路由优先级(专用格式优先,Search 在 Log 前防 grep 被抢):

```
Json → Search → Diff → Tabular → Log → Truncate
```

**transform 编排(改写 lib.rs)**:先建一次性请求上下文,再逐项处理。

```
transform(request, _provider, queryid):
  cfg = config::load()                         // OnceLock 缓存(v1 已有)
  if !cfg.enabled { return }
  ctx = RequestCtx {
      queryid,                                 // CCR 目录名
      query_terms: query::extract(request),    // S2:最后一条 user 消息关键词(一次性)
      cmd_index:  command::index(request),     // D2:call_id → CommandHint(一次性)
  }
  total_before = total_text_bytes(&request.input)
  for item in request.input.iter_mut():        // 仍只碰两变体
      compress_item(item, &ctx, &cfg)
  total_after = total_text_bytes(&request.input)
  if total_after < total_before { stats::log_compression(queryid, before, after) }
```

**单个文本片段处理链(①–⑥)**:

```
compress_in_place(s, ctx, cfg, call_id):
  cmd = ctx.cmd_index.get(call_id)                      // ① 命令名(Option)
  if protect::should_protect(s, cmd, cfg) { return }    // ② 错误保护门,命中跳过
  pre = preprocess::run(s, &cfg.preprocess)             // ③ rtk 通用预处理(多无损)
  match router.compress_text(&pre, &Budget{cfg, ctx, cmd}):   // ④⑤ 路由+压缩
    Some((new, lossy)) =>
      *s = if lossy { ccr::attach(new, original=s, ctx, call_id) } else { new }  // ⑥ 有损挂 CCR
    None =>
      *s = pre        // 预处理结果即使没进一步压也保留(预处理多无损)
```

> **新增行为(需测试)**:预处理结果即使路由未压也保留——删进度条/空行不丢语义,算有效压缩。

**CCR 挂载统一原则**:任何压缩器产出 `lossy=true` 即挂 CCR(落盘原文 + 占位写路径);`lossy=false`(无损)不挂。由编排层 `ccr::attach` 统一处理,无压缩器特例。

**模块布局(新增)**:

```
src/
  lib.rs            # 改写:编排 + RequestCtx
  config.rs         # 扩展:新增子表 Default
  preprocess.rs     # 新:rtk 通用预处理段
  command.rs        # 新:call_id→CommandHint 索引 + 路由提示
  query.rs          # 新:提取最后一条 user 消息关键词
  score.rs          # 新:共享评分(内容特征 + 查询加权)
  protect.rs        # 新:错误输出保护判定
  ccr.rs            # 新:落盘 + 占位 + LRU 清理
  router.rs         # 改:CompressOutcome 增 lossy;compress_text 返回 (String, bool)
  compress/
    mod.rs
    schema.rs       # 新:csv-schema 内表达公共模块(JSON + Tabular 共用)
    search.rs       # 新:SearchCompressor
    tabular.rs      # 新:TabularCompressor
    json.rs         # 升级:csv-schema + 完全去重(前置无损)+ 现有抽样
    log.rs          # 改写:模板挖掘(无损)+ 级别评分保留(有损)
    diff.rs         # 不动(有损产物经编排挂 CCR)
    truncate.rs     # 改:base64 折叠前置;截断产物经编排挂 CCR
```

---

## 3. 特性 → 参考源文件映射

> 路径基准 `../3rd/compress/`(即 `/Users/dfbb/Sites/skycode/3rd/compress/`),均已实地核验存在。Rust 实现优先,Python 辅助说明算法。codez 侧类型读 `codex-rs`。

### A 组 · 新压缩器

**A1. Search 压缩器**(grep/ripgrep:按文件分组、保首尾匹配、评分选中段、错误加权)
- `headroom/crates/headroom-core/src/transforms/search_compressor.rs`(902 行,主实现)
- `headroom/headroom/transforms/search_compressor.py`(评分/分组算法说明)
- rtk 分组参考:`rtk/src/cmds/system/grep_cmd.rs`

**A2. Tabular 压缩器**(CSV/TSV/MD 表格 → csv-schema 无损重编码)
- `headroom/headroom/transforms/tabular_ingest.py`(表格→记录解析)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/formatter.rs:205`(csv-schema 格式器)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/compactor.rs`(逐行紧凑)

### B 组 · 升级现有

**B1. Log → 错误优先 + 模板挖掘(RLE)**
- `headroom/crates/headroom-core/src/transforms/log_compressor.rs`(1295 行,级别评分/栈保留/format 检测)
- `headroom/headroom/transforms/log_compressor.py`(权重 ERROR=1.0/WARN=0.7/INFO=0.3/DEBUG=0.1)
- 模板挖掘(RLE):`headroom/crates/headroom-core/src/transforms/pipeline/reformats/log_template.rs`(默认 min_run=3)

**B2. JSON +csv-schema +完全去重**
- csv-schema:`headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/formatter.rs`
- 完全去重:`headroom/crates/headroom-core/src/transforms/smart_crusher/orchestration.rs`、`headroom/headroom/transforms/smart_crusher.py`(`dedup_identical_items`)
- minify 参考:`headroom/crates/headroom-core/src/transforms/pipeline/reformats/json_minifier.rs`

### C 组 · 新增手段

**C1. base64/blob 折叠**(超长 base64/data-uri → 占位)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/walker.rs`(opaque blob 检测)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/classifier.rs`(单元格分类:opaque)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/statistics.rs`(熵/base64 特征)

**C2. 错误输出保护**(错误/异常且 <阈值 → 整段不压)
- `headroom/headroom/transforms/error_detection.py`(强错误指示符检测)
- `headroom/headroom/transforms/content_router.py`(`protect_error_outputs` / `error_protection_max_chars=8000`)

### D 组 · rtk 引入

**D1. 通用预处理层**(删进度条/塌缩空行/超长行截断/连续重复去重)
- `rtk/src/core/toml_filter.rs`(8 段管线主实现,1698 行)
- `rtk/src/core/utils.rs`(`strip_ansi()` 等行级工具)
- `rtk/src/core/truncate.rs`(CAP_* 容量常,64 行)
- 连续重复去重:`rtk/src/cmds/system/log_cmd.rs`

**D2. 命令感知路由**(call_id→命令名,仅路由提示)
- 命令匹配机制:`rtk/src/core/toml_filter.rs`(`match_command` 正则分派)
- git 摘要意图参考:`rtk/src/cmds/git/diff_cmd.rs`、`rtk/src/cmds/git/git.rs`
- codez 侧反查无第三方参考:读 `codex-rs/protocol/src/models.rs` 的 `FunctionCall`/`FunctionCallOutput`

### E 组 · CCR

- `headroom/headroom/ccr/batch_store.py`(原文存储,主参考)
- `headroom/headroom/ccr/context_tracker.py`(thread 维度跟踪)
- `headroom/crates/headroom-core/src/ccr/mod.rs`(Rust 结构,119 行)
- 占位标记格式自定义(headroom 用 `<<ccr:HASH>>`,codez 改为可读路径,无直接参考)

### 共享原语

**S1. 评分**(内容特征 + 最后一条 user 消息关键词加权)
- `headroom/crates/headroom-core/src/transforms/anchor_selector.rs`(锚点选择/权重归一)
- `headroom/headroom/transforms/anchor_selector.py`(查询词重叠加权)
- 自适应保留量参考:`headroom/crates/headroom-core/src/transforms/adaptive_sizer.rs`(Kneedle)

**S2. 查询提取**(无第三方参考)
- 读 `codex-rs/codex-api` 的 `ResponsesApiRequest` + `codex-protocol` 的 `ResponseItem::Message`

---

## 4. 模块接口与算法

### 4.1 router.rs:CompressOutcome 增 lossy 标记

```rust
pub enum CompressOutcome {
    Compressed { text: String, saved_bytes: usize, lossy: bool },  // 新增 lossy
    Unchanged,
}
```

- `lossy=true`:删了内容(Search 删匹配、Log 删行、Diff 折上下文、Truncate 截断、JSON 抽样/超深、base64 折叠)。
- `lossy=false`:无损(JSON csv-schema/完全去重、Tabular、Log 模板折叠)。
- `ContentRouter::compress_text` 返回由 `Option<String>` 改为 `Option<(String, bool)>`(text, lossy)。
- **混合片段**:一段文本经多步处理(如 JSON 先 csv-schema 无损、再抽样有损),压缩器内累积一个 lossy 标志,任一有损步骤置位,最终写进 `CompressOutcome.lossy`。CCR 存的是该片段进压缩器前的原文。
- 影响:现有 4 压缩器的 `Compressed{..}` 构造处补 `lossy`;现有测试断言同步。

### 4.2 query.rs(S2)

```rust
pub fn extract(request: &ResponsesApiRequest) -> Vec<String>
```

- 从 `request.input` **从后往前**找第一条 `ResponseItem::Message { role:"user", .. }`,取其 `InputText` 文本。
- 分词:非字母数字切分 → 小写 → 去停用词(内置小表)→ 去长度 ≤2 → 最多保留 N 个(默认 32)。
- 找不到 user 消息 → 空 Vec(评分退化为纯内容特征,不报错)。

### 4.3 command.rs(D2)

```rust
pub struct CommandHint { pub program: String, pub argv: Vec<String> }
pub fn index(request: &ResponsesApiRequest) -> HashMap<String, CommandHint>  // key=call_id
impl CommandHint { pub fn is_git_diff(&self)->bool; pub fn is_grep(&self)->bool;
                   pub fn is_ls_like(&self)->bool; pub fn is_test_runner(&self)->bool; }
```

- 遍历 `request.input` 的 `ResponseItem::FunctionCall { name, arguments, call_id, .. }`。
- codex shell 类工具(`name` 如 `"shell"`/`"local_shell"`):真正命令在 `arguments`(JSON 含 `command` 数组),解析取 argv;非 shell 工具则 `program=name`、argv 空。
- 解析失败/非 JSON → 该 call_id 不入索引(fail-open)。
- `CommandHint` 仅作**路由提示**:不另写命令专属压缩逻辑,只用于在 router 中优先把内容交给对应压缩器、或提高其激进度。

### 4.4 score.rs(S1)

```rust
pub fn line_score(line: &str, query: &[String]) -> f32
```

- 内容特征(参考 anchor_selector):含错误关键词(error/fail/panic/exception/traceback…)+1.0;含警告(warn/warning)+0.5;查询词命中每个 +0.3;空行/纯符号 0。
- 纯静态 + 查询加权,无 ML。供 Search/Log 决定保留哪些行。

### 4.5 protect.rs(C2)

```rust
pub fn should_protect(text: &str, cmd: Option<&CommandHint>, cfg: &Config) -> bool
```

- 参考 error_detection.py:文本含强错误指示符(traceback/panic/`error:`/非零 exit 提示等)**且** `text.len() < cfg.protect.error_max_bytes`(默认 8192)→ true(整段不压)。
- `error_max_bytes=0` → 关闭保护。命令提示辅助:cmd 是 test runner 且输出含失败 → 提高保护倾向。

### 4.6 preprocess.rs(D1)

```rust
pub fn run(text: &str, cfg: &PreprocessCfg) -> String
```

按顺序执行(各段可独立开关,任一段异常则跳过该段):
1. `strip_progress`:删进度条/下载行(`^Downloading`、百分比进度、`\r` 覆写行)。
2. `collapse_blank`:连续空行塌缩为一个。
3. `truncate_line_bytes`:超长单行按字节截断(UTF-8 边界安全),尾占位;0=关闭。
4. `dedup_consecutive`:连续完全相同行折叠 `(×N)`。

多为无损(删进度条/空行不丢语义)。注:此处 dedup 是**公共预处理层**,供所有压缩器复用;Log 压缩器内的模板挖掘是更强的变体。

### 4.7 ccr.rs(E)

```rust
pub fn attach(compressed: String, original: &str, ctx: &RequestCtx, call_id: &str) -> String
```

- 路径:`~/.codex/llm-compress/ccr/<thread_id>/<hash>.txt`;`hash`=原文 SHA256 前 12 hex(内容相同则同名,天然去重)。`thread_id`=ctx.queryid。
- **粒度:每工具输出一文件**(同一输出多处折叠都指向同一文件)。`attach` 对同一 call_id 首次落盘,占位写该文件路径 + 行号范围辅助定位。
- **LRU 清理**:写前扫该 thread 目录,按 mtime 排序,超 `cfg.ccr.max_files_per_thread`(默认 200)删最旧。
- 占位格式:`[llm-compress: 略 320 行/18KB,原文: ~/.codex/llm-compress/ccr/<thread>/<hash>.txt]`。
- **fail-open**:落盘失败(磁盘满/权限)仅 `tracing::warn`,占位**退化为不含路径的普通占位**,压缩照常生效。

### 4.8 compress/schema.rs(csv-schema 公共模块)

```rust
/// 对象数组(各项同构)→ {"_schema":[...],"_rows":[[...]]}。非同构 → None。无损。
pub fn to_schema_form(value: &Value) -> Option<Value>
```

- 同构判定:所有元素都是 object 且键集合相同(参考 formatter.rs 的 csv-schema 适用条件)。
- 标量值进 `_rows`;嵌套对象/数组的值保留原样放进行(不强制扁平,避免破坏)。
- 产物仍是合法 JSON(保住硬不变量)。JSON 压缩器对 `Value::Array` 调用;Tabular 压缩器先把表格 parse 成 `Value::Array<Object>` 再调用。

---

## 5. 六压缩器算法

路由顺序 `Json → Search → Diff → Tabular → Log → Truncate`。所有 `compress` 在 router 的 `catch_unwind` 内,失败回退原文。

### ① JSON(升级 json.rs)— 无损前置 + 有损抽样兜底

在现有"数组首尾抽样 + 超深截断"前,新增两个无损步骤:

1. **完全重复去重(无损)**:数组内 `Value` 完全相等的重复项折叠为一项 + 计数占位 `{"_dup":"×N"}`。
2. **csv-schema 内表达(无损)**:对象数组各项同构时,调 `schema::to_schema_form` 重写为 `{"_schema":[...],"_rows":[...]}`,去掉每行重复键名。产物仍合法 JSON。
3. **长数组抽样 + 超深截断(有损)**:现有逻辑保留为兜底,`cfg.json.lossy_sample=false` 可关掉只走无损。抽样占位 `…(N more)` 已显式。
- 任一有损步骤(抽样/超深)发生 → `lossy=true`,挂 CCR。无损步骤独立生效时 `lossy=false`。
- 产物必经 `serde_json` 重新 parse 校验,失败回退原文(现有不变量)。

### ② Search(新 search.rs)— 有损,挂 CCR

- **detect**:多行且多数行匹配 `路径:行号:内容` 或 `路径:行号:列:内容`(ripgrep);命令提示 `is_grep()` 为真则强认领。
- **compress**(参考 search_compressor.rs):按文件路径分组;组内每文件必留首+末匹配,中间按 `score::line_score`(查询加权)选 top-K(`cfg.search.max_per_file`,默认 5),回排序号;文件数超 `cfg.search.max_files`(默认 15)→ 按组总分留高分文件,其余折叠 `[llm-compress: 略 N 个文件]`;占位标省略匹配/文件数。
- `lossy=true`(删了匹配)。

### ③ Diff(diff.rs 不动)— 有损,经编排挂 CCR

- 算法保持 v1(保全变更行 + 折叠多余上下文)。唯一改动:`Compressed{..}` 补 `lossy=true`(产生折叠时),编排层据此挂 CCR。
- 命令提示 `is_git_diff()` 仅用于 router 中优先交给 Diff。

### ④ Tabular(新 tabular.rs)— 无损,不挂 CCR

- **detect**:CSV/TSV(多行、稳定分隔符、列数一致)或 Markdown 表格(`|---|` 分隔行)。`cfg.tabular.enabled=false` 则不认领。
- **compress**(参考 tabular_ingest.py):解析成记录数组 → 调 `schema::to_schema_form` → 输出合法 JSON 的 `{"_schema",_rows}`。无损。解析不确定(列数不齐)→ Unchanged,让给 Log/Truncate。
- `lossy=false`。

### ⑤ Log(改写 log.rs)— 无损模板 + 有损评分

- **模板挖掘(RLE,无损)先行**:连续同模板行(仅变量不同)→ 模板头 + 变量表。参考 log_template.rs(`cfg.log.template_min_run`,默认 3)。
- **级别评分保留(有损)**:剩余行用 `score::line_score`,`cfg.log.keep_levels`(默认 `["error","warn"]`)对应级别 + 栈帧必留,DEBUG/INFO 超量按分丢弃;保留首尾 + 高分中段错误。参考 log_compressor.rs。
- 发生有损删除 → `lossy=true` 挂 CCR;纯模板折叠(无损)`lossy=false`。
- 替代 v1 的"位置截断 head/tail",解决"中段 ERROR/栈帧被无差别折叠"问题。

### ⑥ Truncate(改 truncate.rs)— 有损,挂 CCR

- **base64/blob 折叠(C1)前置**:截断前先把超长 base64/data-uri 段(阈值 `cfg.preprocess` 或专项,默认 256B)替换为 `[llm-compress: base64 N 字节]`。参考 walker.rs/classifier.rs。
- 兜底:strip_ansi(已有)+ head/tail + 超 max_bytes 硬截断(UTF-8 安全,已有)。
- `lossy=true`(截断/折叠均有损),挂 CCR。

---

## 6. 配置全集

在 v1 字段上增量(现有字段全保留)。整节缺失或 `enabled=false` → 全关零改动;每个子表缺失 → 用默认值。所有新字段进 `config.rs` 的 `Default`,沿用 `#[serde(default)]`。

```toml
[llm_compress]
enabled = false
min_total_bytes = 4096
per_item_min_bytes = 1024

# ── 现有(保留)──
[llm_compress.truncate]
head_lines = 50
tail_lines = 50
max_bytes = 16384

[llm_compress.json]
max_array_items = 20
max_depth = 6
lossy_sample = true            # 新增:关掉则只做无损(csv-schema+去重)

[llm_compress.diff]
context_lines = 3

[llm_compress.log]
dedup_repeats = true           # 现有
template_min_run = 3           # 新增:RLE 模板挖掘最小连续行
keep_levels = ["error", "warn"]  # 新增:必留级别

# ── 新增子表 ──
[llm_compress.preprocess]      # D1
strip_progress = true
collapse_blank = true
truncate_line_bytes = 2000     # 0=关闭
dedup_consecutive = true
blob_min_bytes = 256           # base64/data-uri 折叠阈值

[llm_compress.search]          # A1
max_per_file = 5
max_files = 15

[llm_compress.tabular]         # A2
enabled = true

[llm_compress.protect]         # C2
error_max_bytes = 8192         # 0=关闭保护

[llm_compress.ccr]             # E
enabled = true
max_files_per_thread = 200
```

---

## 7. 错误处理(全程 fail-open)

继承 v1 全部 fail-open,新增模块的失败处置:

- `query.rs`/`command.rs` 解析失败 → 返回空/跳过该项,不报错。
- `preprocess.rs` 任一段异常 → 该段跳过,返回上一段结果。
- `protect.rs` 判定异常 → 视作不保护(继续压,保守)。
- `ccr.rs` 落盘失败 → 仅 warn,占位退化为不含路径的普通占位,压缩照常生效,不阻断请求。
- `schema.rs` 同构判定不成立 → 返回 None,JSON/Tabular 走各自兜底。
- 所有压缩器仍在 router 的 `catch_unwind` 内。
- `transform` 仍返回 `()`,从类型上杜绝压缩失败阻断请求。

---

## 8. 测试数据继承

### 8.1 来源与许可

`headroom` 与 `rtk` 均 **Apache-2.0**,允许拷贝,需保留版权。继承数据放:

```
zmod/llm-compress/tests/fixtures/inherited/
  LICENSE-headroom   # Apache-2.0 + © 2025 Headroom Contributors
  LICENSE-rtk        # Apache-2.0 + © 2024 rtk-ai Labs
  NOTICE.md          # 说明哪些文件改编自何处
  search/ log/ diff/ json/ tabular/ preprocess/   # 按压缩器分类
```

每个继承文件头注释:`// Adapted from headroom (Apache-2.0, © 2025 Headroom Contributors): <原路径>`。

### 8.2 继承清单(确切来源)

| 我方压缩器 | 继承的 input 来源 | expected 对照来源 |
|---|---|---|
| Search | `headroom/crates/headroom-core/src/transforms/search_compressor.rs:668-900`(16 内嵌例:标准 grep、ripgrep context、Windows 路径、文件名带 `-`、内容含 `:`) | 同处 + `headroom/tests/test_search_compressor.py`(49 例) |
| Log | `headroom/tests/parity/fixtures/log_compressor/*.json`(20 个,含 305→8 行真实日志) | 同 JSON 的 `output.compressed` |
| Diff | `headroom/tests/parity/fixtures/diff_compressor/*.json`(27 个) | 同 JSON `output` |
| JSON | `headroom/tests/parity/fixtures/smart_crusher/*.json`(17 个:30/100 项数组、嵌套、重复、unicode、空、passthrough) | 同 JSON `output` |
| 预处理 | `rtk/src/core/toml_filter.rs:712-1000`、`rtk/src/core/utils.rs:401-859` | 同处 expected |
| 命令规则参考 | `rtk/src/filters/*.toml` 的 `[[tests]]`(gcc/gradle/basedpyright 等 ~150 例) | 同处 expected |
| 真实命令输出 | `rtk/tests/fixtures/*.txt`(42 个:Maven/Gradle/GitLab CI/.NET) | — |
| Tabular | 无专用 fixture → 从 smart_crusher 对象数组反构 CSV/MD + 自造少量 | 自造,人工核对 |
| 错误保护 | `headroom/tests/parity/fixtures/content_detector/*.json`(21 个,含错误日志识别) | — |

### 8.3 对比断言形态(关键)

我方算法薄型、占位标记不同(`[llm-compress: …]` vs `<<ccr:HASH>>`)、默认值不同,**不做逐字节相等**。对每个继承的 `(input, ref_output)` 断言一组不变量:

1. **体积不劣于参考**:`our_output.len() ≤ ref_output.len() * 1.5`(薄型允许略宽松,不能离谱)。
2. **关键行全保留**:参考输出保留的"错误行/变更行/首末匹配",我方也必须含(用 `score::line_score` 高分行集合比对)。
3. **我方硬不变量**:压后 ≤ 压前、UTF-8 合法、JSON 产物可 parse、占位标记存在。
4. **CCR 可逆性**:有损项的 CCR 文件落盘且内容 == 原 input。

我方 expected 仍由实现产生 + 人工核对固化(insta 快照),参考输出只用于上面对比断言,不直接断言相等。

---

## 9. 测试组织与成功判据

### 9.1 测试组织

```
tests/
  fixtures/inherited/...                          # 继承 input + ref_output + LICENSE/NOTICE
  search_test.rs  tabular_test.rs                 # 新压缩器
  json_test.rs(扩展)  log_test.rs(改写)         # 升级
  preprocess_test.rs  command_test.rs  query_test.rs
  score_test.rs  protect_test.rs  ccr_test.rs  schema_test.rs
  parity_test.rs                                  # 遍历 inherited/ 跑 8.3 对比断言
  snapshots/                                      # insta 固化我方 expected
```

- 各新模块单测:query(提取/无 user)、command(解析 argv/非 JSON 跳过)、score(错误行高分/查询加权)、protect(错误且小→保护、大→不保护)、preprocess(各段独立+组合)、schema(同构→重写、异构→None、嵌套值保留)、ccr(落盘+占位含路径、LRU 删最旧、落盘失败退化)。
- 新压缩器:search(分组/保首尾/超文件折叠/查询加权)、tabular(CSV/TSV/MD→schema、列不齐→Unchanged)。
- 升级回归:json(csv-schema 无损+去重+产物合法 JSON)、log(模板折叠无损+级别评分+中段 ERROR 不丢)。
- lossy 标记:无损步骤 `lossy=false` 不挂 CCR、有损 `lossy=true` 挂 CCR。
- 编排:预处理无损也算有效压缩、保护门命中整段不变、混合有损+无损片段挂 CCR。
- 不变量(保留):`enabled=false` 逐字节不变、压后 ≤ 压前、UTF-8 安全、只碰两变体、ContentItems 图片不动。
- ccr/落盘测试用 `tempfile` + 注入 HOME(沿用 stats_test 模式)。dev-deps `insta`/`tempfile` 已有。
- 开发期走软链 member:`cd codex-rs && cargo test -p codez-llm-compress`。

### 9.2 成功判据

1. 六压缩器(Json/Search/Diff/Tabular/Log/Truncate)+ 预处理 + 保护门 + CCR 全部落地,路由优先级 `Json→Search→Diff→Tabular→Log→Truncate` 生效。
2. 继承 fixture 的 `parity_test` 全绿(8.3 四类不变量)。
3. 硬不变量满足:压后 ≤ 压前、UTF-8 合法、JSON 产物可 parse、只碰两变体、图片/加密内容不动、`enabled=false` 逐字节不变。
4. CCR 可逆:有损压缩落盘原文,占位含可读路径,模型用 shell/read 工具可取回;LRU 清理生效;落盘失败 fail-open。
5. **transform 签名与集成点 patch 不变**:命令上下文/查询关键词全从 request 内提取,无新增 core 触点,不碰工具系统。
6. fail-open 全覆盖:任何新模块失败都退回原文/跳过,不阻断请求。

---

## 10. 实现顺序建议

依赖关系(共享原语先行):

```
1 router.lossy + schema.rs ─┬─> 4 JSON 升级
2 query + command + score   ├─> 5 Search
3 preprocess + protect      ├─> 6 Tabular
                            ├─> 7 Log 改写
                            ├─> 8 Truncate+base64
9 ccr ──────────────────────┴─> 10 lib 编排接线
                                 11 继承 fixture + parity_test
```

- 1-3 是地基(类型 + 共享模块),无依赖可并行。
- 4-8 各压缩器依赖地基,互相独立可并行。
- 9 ccr 独立。10 编排把全部接线。11 数据继承与对比测试收尾。





