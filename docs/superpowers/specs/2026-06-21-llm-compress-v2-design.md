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

| 组 | 项 | 形态 | 是否删实质内容 | 挂 CCR |
|---|---|---|---|---|
| A 新压缩器 | Search(grep/ripgrep) | 新 Compressor | 删匹配(有损) | ✓ |
| | Tabular(CSV/TSV/MD 表格) | 新 Compressor | 否(格式重构,内容保留) | ✗ |
| B 升级 | Log → 错误优先 + 模板挖掘(RLE) | 改写 log.rs | 删行有损 / 模板折叠不删 | 删行部分 |
| | JSON +csv-schema +连续RLE去重 | 增量 json.rs | 否(只做不删内容步骤;需删则转 Truncate) | ✗ |
| C 新手段 | base64/blob 折叠 | 预处理层 blob_fold 段(唯一位置) | 删(替换为占位) | ✓ |
| | 错误输出保护 | router 前置门 | —不压 | — |
| D rtk | 通用预处理(进度条/blob/空行/超长行/连续重复) | 新 preprocess.rs | 删进度条/blob/超长行截断删,空行归一/连续折叠不删 | 删除段挂 |
| | 命令感知路由(call_id→命令名,仅路由提示) | 新 command.rs | — | — |
| E CCR | 落盘+Text路径占位,双限清理 | 新 ccr.rs | — | — |
| 共享 | 评分(内容特征+最后user消息加权) | 新 score.rs | — | — |
| 共享 | 查询关键词提取 | 新 query.rs | — | — |

> **`lossy` 口径(贯穿全文,#2 修正)= 是否删了实质内容(语义口径,非字节)**。纯格式重构(JSON minify、csv-schema、表格转 JSON、连续空行归一、连续重复/项 RLE)内容全保留 → `lossy=false` 不挂 CCR;抽样/删行/删匹配/截断/超深/base64 折叠删了内容 → `lossy=true` 挂 CCR。详见 §4.0。

**范围外(有意排除,违背薄型/不可逆/热路径定位)**:Code(tree-sitter AST)、Kompress(ML 模型)、HTML 提取(trafilatura)、Magika ML 检测、消息级裁剪/滚动窗口、CacheAligner、Net-Cost Gate、按 auth 模式差异化策略、跨会话学习(TOIN)、token 计数。

---

## 2. 总体架构与数据流

沿用 v1 的 `ContentRouter` + `Compressor` trait + fail-open。新路由优先级(专用格式优先,Search 在 Log 前防 grep 被抢):

```
Json → Search → Diff → Tabular → Log → Truncate
```

**transform 编排(改写 lib.rs)**:先建一次性请求上下文(含**可变 CCR registry**),再逐项处理。

```
transform(request, _provider, queryid):
  cfg = config::load()                         // OnceLock 缓存(v1 已有)
  if !cfg.enabled { return }
  ctx = RequestCtx {
      queryid,                                 // CCR 目录名
      query_terms: query::extract(request),    // S2:最后一条 user 消息关键词(一次性)
      cmd_index:  command::index(request),     // D2:call_id → CommandHint(一次性)
      ccr: RefCell<CcrRegistry>,               // #8:可变,记 (call_id,fragment_hash)→已落盘文件路径(每文本片段一文件)
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
  (pre, pre_lossy) = preprocess::run(s, &cfg.preprocess)  // ③ rtk 预处理,返回(文本, 是否删了实质内容)
  candidate =
    match router.compress_text(&pre, &Budget{cfg, ctx, cmd}):   // ④⑤ 路由+压缩(detect 也吃 budget)
      Some((new, comp_lossy, kind)) =>
        lossy = pre_lossy || comp_lossy
        // lossy=true ⟹ kind=Text(§4.0);JSON/Tabular 恒 lossy=false 不进此分支
        if lossy { ccr::attach(new, original=s, ctx, call_id, &cfg.ccr) } else { new }  // ⑥ attach 不收 kind
      None =>                                            // 路由未压,仅保留预处理结果
        if pre_lossy { ccr::attach(pre, original=s, ctx, call_id, &cfg.ccr) } else { pre }
  // #4:最终写回前【统一】二次体积检查——不止在 attach 内。
  // 预处理无损分支(空行归一/RLE 计数占位)对小输入也可能变长。
  if candidate.len() <= s.len() { *s = candidate }      // 否则保留原文,保住"压后 ≤ 压前"(§1)
```

> **新增行为(需测试)**:预处理结果即使路由未压也保留;**若预处理删了实质内容(删进度条/blob/截断)则同样挂 CCR**(#5)。纯格式重构的预处理段(连续空行归一、连续重复行折叠)不触发 lossy。**最终写回统一过 `candidate.len() <= original.len()` 闸门(#4)**,不满足则回退原文。
>
> **protect 优先级最高(#7,有意设计)**:`protect::should_protect` 在**所有处理之前**,命中即 `return`,该片段**整段逐字节不变**——包括 ANSI/空行/blob/重复行等无害预处理也**一律不做**。理由:错误/异常输出是诊断关键,任何改动(哪怕去 ANSI)都可能干扰模型判断,故命中保护门的小型错误输出完全原样透传。这是刻意取舍,优先于"预处理是通用层"。

**CCR 挂载统一原则**:任何处理(预处理或压缩器)删了**实质内容**(`lossy=true`)即挂 CCR;纯格式重构(`lossy=false`)不挂。`lossy` 口径见 §4.0(语义口径,非字节)。由编排层 `ccr::attach` 统一处理:因 **`lossy=true ⟹ kind=Text`**(§4.1 不变量),attach **只处理 Text 产物、只产 Text 裸占位**(不收 kind、无 JSON 注入)。体积闸门有两道:`attach` 内的占位拼接检查(§4.7)+ 编排层最终写回检查(#4,§2 处理链)。

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
  router.rs         # 改:CompressOutcome 增 lossy+kind(ContentKind);compress_text 返回 Option<(String,bool,ContentKind)>
  compress/
    mod.rs
    schema.rs       # 新:csv-schema 内表达公共模块(JSON + Tabular 共用)
    search.rs       # 新:SearchCompressor
    tabular.rs      # 新:TabularCompressor
    json.rs         # 升级:detect 内判定让渡;csv-schema + 连续RLE去重(均不删内容);移除 v1 抽样/超深裁剪
    log.rs          # 改写:模板挖掘(不删)+ 级别评分保留(删行)
    diff.rs         # 不动(有损产物经编排挂 CCR)
    truncate.rs     # 改:detect 吃 budget;截断产物经编排挂 CCR(blob 折叠已移至 preprocess)
```

---

## 3. 特性 → 参考源文件映射

> 路径基准 `../3rd/compress/`(即 `/Users/dfbb/Sites/skycode/3rd/compress/`),均已实地核验存在。Rust 实现优先,Python 辅助说明算法。codez 侧类型读 `codex-rs`。

### A 组 · 新压缩器

**A1. Search 压缩器**(grep/ripgrep:按文件分组、保首尾匹配、评分选中段、错误加权)
- `headroom/crates/headroom-core/src/transforms/search_compressor.rs`(902 行,主实现)
- `headroom/headroom/transforms/search_compressor.py`(评分/分组算法说明)
- rtk 分组参考:`rtk/src/cmds/system/grep_cmd.rs`

**A2. Tabular 压缩器**(CSV/TSV/MD 表格 → csv-schema 重编码;格式重构不删内容,不挂 CCR)
- `headroom/headroom/transforms/tabular_ingest.py`(表格→记录解析)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/formatter.rs:205`(csv-schema 格式器)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/compactor.rs`(逐行紧凑)

### B 组 · 升级现有

**B1. Log → 错误优先 + 模板挖掘(RLE)**
- `headroom/crates/headroom-core/src/transforms/log_compressor.rs`(1295 行,级别评分/栈保留/format 检测)
- `headroom/headroom/transforms/log_compressor.py`(权重 ERROR=1.0/WARN=0.7/INFO=0.3/DEBUG=0.1)
- 模板挖掘(RLE):`headroom/crates/headroom-core/src/transforms/pipeline/reformats/log_template.rs`(默认 min_run=3)

**B2. JSON +csv-schema +连续RLE去重**
- csv-schema:`headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/formatter.rs`
- 连续 RLE 去重:`headroom/crates/headroom-core/src/transforms/smart_crusher/orchestration.rs`、`headroom/headroom/transforms/smart_crusher.py`(`dedup_identical_items`,本设计收窄为**仅相邻**项 RLE)
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

### 4.0 lossy 口径(贯穿全文)

**`lossy = true` ⟺ 变换删除了实质内容(语义口径,#2 修正)。** 不以"字节可恢复"为判据——因为 JSON 经 `serde_json` parse→serialize 必然改变空白/键序/数字表现(见 `zmod/llm-compress/src/compress/json.rs:30,47`),字节口径会让一切 JSON 变换都成 lossy,失去区分意义。

- **无损(lossy=false)= 纯格式重构,内容全保留**:JSON minify、csv-schema 重编码(对象数组↔schema+rows)、表格转 JSON、连续空行归一、连续重复行折叠、JSON 连续重复项 RLE。这些**不删任何数据**,模型拿到的信息等价,无需取回 → **不挂 CCR**。
- **有损(lossy=true)= 删了实质内容,模型可能想看原文**:抽样删数组元素、删行/删匹配、head/tail 截断、超深子树裁剪、base64/blob 折叠。**挂 CCR**。

> **关键约束(#2/#3 修正):产生 `kind=Json` 产物的压缩器(JSON、Tabular)只做不删内容的步骤(连续 RLE、csv-schema),`lossy` 恒为 false,永不挂 CCR。** 于是 CCR 占位只需 Text 一种承载,**不存在向 JSON 注入 CCR 字段的情况**,彻底回避"数组变对象""保留字段碰撞"风险。JSON 需删内容(深度抽样/超深裁剪)时,**不在 JSON 压缩器内做**,而是 detect 不认领/产出 Unchanged,交由 Truncate 按文本处理(裸占位 + CCR,`kind=Text`)。
>
> 唯一剩余的 JSON 保留字段是 RLE 的 `_llm_dup_prev`:**原数组若已含 `{"_llm_dup_prev":...}` 形态对象,跳过对其折叠**(保留原样,不改名不 envelope),见 §5①。其余占位标记 `[llm-compress: …]` 仅出现在 Text 产物。

### 4.1 router.rs:CompressOutcome 增 lossy + content_kind + detect 吃 budget

```rust
pub enum ContentKind { Text, Json }   // 标识产物形态;Json 恒配 lossy=false(见 §4.0)

pub enum CompressOutcome {
    Compressed { text: String, saved_bytes: usize, lossy: bool, kind: ContentKind },
    Unchanged,
}

pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str, budget: &Budget) -> bool;          // 改:detect 也吃 budget(拿 cmd)
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

- `kind`(#1/#8):标识产物是 JSON 文本(JSON/Tabular 压缩器,恒 `lossy=false`)还是普通文本。`ccr::attach` 只对 `lossy=true` 的项执行,而 `lossy=true` ⟹ `kind=Text`(§4.0 约束),故 attach **只产出 Text 裸占位**;`kind` 仍保留在接口里供未来扩展与断言"Json 产物不挂 CCR"。

**命令感知路由(#2)**:现有 router 是 first-match 且 `detect` 无 cmd,无法"强认领"。改为:

- `Budget` 增 `cmd: Option<&CommandHint>`、`ctx: &RequestCtx`。`detect` 签名加 `budget`,使各压缩器能据命令名认领(如 `is_grep()` → Search.detect 直接返回 true)。
- `ContentRouter::compress_text(text, budget)`:**先按 `budget.cmd` 重排候选压缩器顺序**(如命中 `is_git_diff()` 把 Diff 提到最前;`is_grep()` 把 Search 提到最前),再走 first-match 的 `detect`。无命令提示时用默认顺序 `Json→Search→Diff→Tabular→Log→Truncate`。
- **返回类型(#1)**:`compress_text` 返回 `Option<(String, bool, ContentKind)>`(text, lossy, kind),与 §2 伪代码一致。
- **混合片段**:压缩器内累积 lossy 标志,任一删内容步骤置位写进 `CompressOutcome.lossy`。
- 影响:现有 4 压缩器的 `detect`(加 budget)/`Compressed{..}`(加 lossy+kind)签名与构造处同步;现有测试断言同步。

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

- 遍历 `request.input` 的 `ResponseItem::FunctionCall { name, arguments, call_id, .. }`(`codex-rs/protocol/src/models.rs:973`)。
- **codex 实际工具名与参数(已核实,#1 修正)**——命令是**单字符串命令行**,不是数组:
  - `shell_command`:`arguments` JSON = `{"command": "<shell 字符串>"}`(`codex-rs/core/tests/common/responses.rs:899`)。
  - `exec_command`:`arguments` JSON = `{"cmd": "<shell 字符串>", ...}`(`codex-rs/core/src/tools/handlers/unified_exec.rs:28`)。
  - 取字符串后做**轻量 shell 解析**:按空白分词(尊重引号),首 token = `program`,其余 = `argv`;遇 `git diff`/`rg`/`grep`/`ls`/`pytest` 等据 program+首参判别。解析参考 `codex-rs/core/src/shell.rs:22` 的 `derive_exec_args`,但本模块只读不执行,失败则 program 取整串、argv 空。
  - 其它(非 shell 的自定义函数工具):`program = name`、argv 空。
- 解析失败/非 JSON/取不到命令字段 → 该 call_id 不入索引(fail-open)。
- `CommandHint` 仅作**路由提示**(§4.1):不另写命令专属压缩逻辑,只用于 router 重排候选顺序/提高激进度。

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
- **优先级最高(#7)**:`should_protect` 在 ① 命令识别之后、③ 预处理**之前**判定(§2 处理链)。命中 → 整段逐字节不变,**所有预处理段(含 ANSI/空行/blob/重复行)都不执行**。错误输出的完整性优先于一切压缩/清理。

### 4.6 preprocess.rs(D1)

```rust
/// 返回 (处理后文本, 是否删了实质内容)。lossy 口径见 §4.0(语义口径)。
pub fn run(text: &str, cfg: &PreprocessCfg) -> (String, bool)
```

按顺序执行(各段可独立开关,任一段异常则跳过该段),**每段标注是否删实质内容(#5)**:

1. `strip_progress`(**删内容→lossy**):删进度条/下载行(`^Downloading`、百分比进度、`\r` 覆写行)。
2. `blob_fold`(**删内容→lossy,#6 唯一执行位置**):超长 base64/data-uri 段(阈值 `cfg.preprocess.blob_min_bytes`,默认 256B)替换为 `[llm-compress: base64 N 字节]`。参考 walker.rs/classifier.rs。**base64/blob 折叠只在此发生**,Truncate 不再重复折叠(§5⑥)。
3. `collapse_blank`(**格式重构→不删**):连续空行归一为一个空行(只去掉冗余空白,无实质内容,§4.0)。
4. `truncate_line_bytes`(**删内容→lossy**):超长单行按字节截断(UTF-8 边界安全),尾占位;0=关闭。
5. `dedup_consecutive`(**格式重构→不删**):连续完全相同行折叠为 `行内容` + `[llm-compress: 上一行 ×N]`,内容保留。

- `run` 返回的 `bool` = 是否触发了**删实质内容**的段(strip_progress / blob_fold / truncate_line_bytes)。若为 true,编排层对预处理结果挂 CCR(§2 处理链)。
- **标记碰撞(#6 硬规则)**:`dedup_consecutive` 折叠前先扫描:**若某行原文本身即匹配 `^\[llm-compress: ` 前缀,则不对其参与折叠**(原样保留该行,跳过 RLE),从而占位与原文不混淆。不改名、不 escape、不 envelope——**遇保留前缀即跳过**是全局统一规则(JSON 的 `_llm_dup_prev` 同理,见 §5①)。
- 此处 dedup 是**公共预处理层**,供所有压缩器复用;Log 压缩器内的模板挖掘(§5⑤)是更强变体。
- **注**:即便所有预处理段都"不删内容",其计数占位对极小输入仍可能让文本变长 → 由 §2 编排层最终 `candidate.len() <= original.len()` 闸门(#4)兜底回退,不在本模块判体积。

### 4.7 ccr.rs(E)

```rust
/// 落盘片段原文 + 在 compressed 后附 Text 取回占位。仅 lossy=true 项调用(⟹ kind=Text)。
pub fn attach(compressed: String, original: &str, ctx: &RequestCtx,
              call_id: &str, cfg: &CcrCfg) -> String
```

- **核心总则(#2/#6 写死,不可被实现弱化)**:在 `cfg.enabled=true` 下,**有损压缩与可取回是绑定的**——`attach` 的结果只有两种合法形态:① 成功落盘 → 返回"有损产物 + 含路径占位"(可取回);② **任何原因无法落盘**(磁盘失败/权限/超 `max_file_bytes`/路径异常)→ **返回原文(放弃本次有损压缩)**。**绝不允许出现"有损产物但无可取回路径"的第三种结果。** 这保证"`enabled=true` 下凡有损必可取回"(§9.2 判据 4)恒成立。仅 `cfg.enabled=false` 才允许"有损但不可取回"(下条)。
- **只对 Text 产物(#2/#3)**:`attach` 仅在 `lossy=true` 时调用,而 `lossy=true ⟹ kind=Text`(§4.0/§4.1)。故 `attach` **只产出 Text 裸占位,无 JSON 注入分支**,彻底回避数组变对象/保留字段覆盖。`kind` 不传入 attach。
- **cfg 门控(#5 统一语义,仅此路径允许"有损不可取回")**:`cfg.enabled=false` 时 `attach` 不落盘、**不追加 CCR 取回路径**,直接返回传入的 `compressed`——`compressed` 里压缩器**自身的省略占位**(如 `[llm-compress: 略 N 行]`)**保留**,只是没有"原文: <path>"那段。即 disabled = "有省略提示但不可取回"(等价 v1)。`enabled=true` 才落盘并在占位中追加路径。
- **粒度:每文本片段一文件(#3)**:registry key = `(call_id, fragment_hash)`,`fragment_hash`=该片段原文 SHA256 前 12 hex。ContentItems 内同一 call_id 的多个 InputText 各自落盘,不指错。`ctx.ccr`(`RefCell<CcrRegistry>`)避免同片段重复落盘。
- **路径组件 sanitize(#4)**:`thread_id`(=ctx.queryid)与 `call_id` **均不直接入路径**——二者都来自请求内容,不能假设是安全文件名(可能含 `/`、`..`、超长)。规则:`thread_dir = sanitize(queryid)`(非 `[A-Za-z0-9_-]` 字符替换为 `_`,超 64 字节则取其 SHA256 前 16 hex);文件名 = `<sanitize(call_id 截断 32)>-<fragment_hash>.txt`。最终路径 `~/.codex/llm-compress/ccr/<thread_dir>/<filename>`,保证无路径穿越、无超长。
- **清理双限(#5)**:写前对该 thread 目录:① 文件数超 `cfg.max_files_per_thread`(默认 200)→ 按 mtime 删最旧;② 目录总字节超 `cfg.max_thread_bytes`(默认 64 MiB)→ 继续按 mtime 删最旧直到达标。
- **单文件超限 = 放弃压缩(#6,保"有损必可取回")**:`enabled=true` 且原文超 `cfg.max_file_bytes`(默认 4 MiB)时,**不产生不可取回的有损压缩**——`attach` 返回**原文**(放弃本次有损压缩),而非落盘失败式的"无路径有损占位"。这样"`enabled=true` 下有损项必有可取回 CCR 文件"的不变量(§9.2 判据 4)始终成立。
- **二次体积检查**:`attach` 拼好占位后若 `attached.len() > original.len()`,降级为更短引用(`[llm-compress: 略,见 ccr/<fragment_hash>]`);仍超则放弃、返回原文。这是 attach 内局部检查,编排层(§2)还有最终统一闸门(#4),二者叠加。
- **fail-open**:落盘失败(磁盘满/权限/扫描失败)仅 `tracing::warn`,**放弃本次有损压缩、返回原文**(与 #6 同策略:不留下不可取回的有损产物)。注:这是 `enabled=true` 下的兜底;`enabled=false` 是另一条路径(上面 cfg 门控),disabled 时本就不承诺可取回,保留压缩器省略占位即可。

### 4.8 compress/schema.rs(csv-schema 公共模块)

```rust
/// 对象数组(各项同构)→ {"_schema":[...],"_rows":[[...]]}。非同构 → None。
/// 纯格式重构、内容全保留 → lossy=false,不挂 CCR(§4.0)。
pub fn to_schema_form(value: &Value) -> Option<Value>
```

- 同构判定:所有元素都是 object 且键集合相同(参考 formatter.rs 的 csv-schema 适用条件)。
- 标量值进 `_rows`;嵌套对象/数组的值保留原样放进行(不强制扁平,避免破坏)。
- 产物仍是合法 JSON(保住硬不变量)。JSON 压缩器对 `Value::Array` 调用(§5①步骤2);Tabular 先把表格 parse 成 `Value::Array<Object>` 再调用(§5④)。
- **内容全保留**(键名移到 `_schema`、值移到 `_rows`,数据无删减),按 §4.0 语义口径标 **lossy=false,不挂 CCR**。模型从 `_schema`+`_rows` 可读到全部数据。

---

## 5. 六压缩器算法

路由顺序 `Json → Search → Diff → Tabular → Log → Truncate`。所有 `compress` 在 router 的 `catch_unwind` 内,失败回退原文。

### ① JSON(升级 json.rs)— 只做不删内容步骤,永不挂 CCR

产物为 JSON 文本,`kind=Json`,**`lossy` 恒为 false**(§4.0/§4.1 约束)。JSON 压缩器**只做不删内容的两步**:

1. **连续重复项 RLE 去重(不删内容)**:仅折叠数组中相邻且 `Value` 完全相等的项,折叠为:保留首项 + 紧随占位 `{"_llm_dup_prev": N}`(语义 = 前一项再重复 N 次,连同首项共 N+1 项)。数据全保留 → **lossy=false 不挂 CCR**。仅连续 RLE,不做非连续去重(会丢顺序/位置)。参考 `smart_crusher/orchestration.rs`(收窄为连续 RLE)。
   - **保留字段碰撞(#3 统一规则)**:原数组若本就含 `{"_llm_dup_prev":...}` 形态对象,**跳过对其折叠**(原样保留),与 §4.6 dedup 的"遇保留前缀即跳过"同源——不改名、不 escape、不 envelope。
2. **csv-schema 内表达(不删内容)**:对象数组各项同构时,调 `schema::to_schema_form` 重写为 `{"_schema":[...],"_rows":[...]}`。数据全保留 → **lossy=false 不挂 CCR**(§4.8)。`cfg.json.csv_schema=false` 可关。

- **detect 决定让渡(#1/#4 修正,适配 router first-match)**:router 是 first-match——一旦 detect 命中,即便 compress 返回 Unchanged 也不再尝试后续压缩器(`zmod/llm-compress/src/router.rs:36`)。故 JSON **不能**靠"compress 返回 Unchanged 让 Truncate 接管"。改为在 **`JsonCompressor::detect(text, budget)` 内 parse 后预判**,判据是"**无损步骤(RLE/csv-schema)是否预计产生有效压缩**":
  - 能 parse 为 JSON,且 RLE/csv-schema 预计**有效压缩**(检测到相邻重复项可折叠,或存在同构对象数组可转 schema)→ detect 返回 **true**,JSON 压缩器处理(产 `kind=Json,lossy=false`)。
  - 能 parse,但无损步骤**预计无有效收益**(无相邻重复、无同构数组),**且**文本仍超 `cfg.truncate.max_bytes`(即放着不管会超限,需删内容)→ detect 返回 **false**,让位给后续 Truncate 按文本抽样/截断(裸占位 + CCR,`kind=Text`)。数组超 `max_array_items` / 嵌套超 `max_depth` 是"无损不够、需删内容"的典型情形,归入此分支。
  - 能 parse,无损步骤无收益,但文本**未超** `truncate.max_bytes`(小 JSON,无需压)→ detect 返回 **false**(无收益不认领,Truncate detect 永真但因未超阈也会 Unchanged,最终原样保留)。
  - 不能 parse → detect 返回 false(本就不认领)。
- **不在 JSON 压缩器内做任何删内容步骤**:v1 的"长数组抽样 / 超深截断"移除。`cfg.json.max_array_items`/`max_depth` 与 `truncate.max_bytes` 在 **detect 阶段**共同用于判定让渡,不在 compress 内删元素。
- 产物必经 `serde_json` 重新 parse 校验,失败回退原文(现有不变量)。RLE/csv-schema 产物天然合法 JSON。

### ② Search(新 search.rs)— 删匹配,挂 CCR

- **detect(&self, text, budget)**:多行且多数行匹配 `路径:行号:内容` 或 `路径:行号:列:内容`(ripgrep);`budget.cmd.is_grep()` 为真时直接认领(detect 现可读 budget,见 §4.1)。
- **compress**(参考 search_compressor.rs):按文件路径分组;组内每文件必留首+末匹配,中间按 `score::line_score`(查询加权)选 top-K(`cfg.search.max_per_file`,默认 5),回排序号;文件数超 `cfg.search.max_files`(默认 15)→ 按组总分留高分文件,其余折叠 `[llm-compress: 略 N 个文件]`;占位标省略匹配/文件数。
- `lossy=true`(删了匹配),`kind=Text`,挂 CCR。

### ③ Diff(diff.rs 不动)— 删上下文,经编排挂 CCR

- 算法保持 v1(保全变更行 + 折叠多余上下文)。改动:`detect` 加 `budget` 参数;`Compressed{..}` 补 `lossy=true`(产生折叠时)、`kind=Text`,编排层据此挂 CCR。
- `budget.cmd.is_git_diff()` 为真时,router 把 Diff 提到候选最前(§4.1 重排)。

### ④ Tabular(新 tabular.rs)— 格式重构,不删内容,不挂 CCR

- **detect(&self, text, budget)**:`cfg.tabular.enabled=false` 不认领。是 CSV/TSV(多行、稳定分隔符)或 Markdown 表格(`|---|` 分隔行),**且满足下列全部严格前提**才返回 true;**任一不满足即 detect 返回 false**(让位给后续 Log/Truncate——因 router first-match,退化判定必须在 detect 内,不能靠 compress 返回 Unchanged,#1):
  1. 有明确 header 行(CSV 首行 / Markdown 表头);无 header → false。
  2. 列名**唯一且非空**;有重复列名或空列名 → false(避免对象键覆盖、丢列)。
  3. 所有数据行列数与 header 一致;列数不齐 → false。
  4. 无转义分隔符、无单元格内换行(简单解析器无法稳定还原)→ 命中则 false。
- **compress**(参考 tabular_ingest.py):detect 已保证前提满足,解析成记录数组 → 调 `schema::to_schema_form` → 输出合法 JSON `{"_schema",_rows}`,`kind=Json`。(防御性:解析意外失败仍返回 `Unchanged`。)
- **lossy=false**:所有行列数据进入 `_schema`+`_rows`,数据全保留 → 不挂 CCR(§4.0)。原始分隔符/对齐等表现形式不保留,但非实质内容删除。

### ⑤ Log(改写 log.rs)— 模板折叠不删 + 评分删行

- **模板挖掘(RLE,不删内容)先行**:连续同模板行(仅变量不同)→ 模板头 + 变量表(变量全保留)。参考 log_template.rs(`cfg.log.template_min_run`,默认 3)。lossy=false。
  - **标记碰撞(#6)**:模板头/变量表用固定前缀(如 `[llm-compress: 模板]`),原文已含同前缀行则实现期转义,不影响 lossy 判定(格式重构)。
- **级别评分保留(删行)**:剩余行用 `score::line_score`,`cfg.log.keep_levels`(默认 `["error","warn"]`)对应级别 + 栈帧必留,DEBUG/INFO 超量按分丢弃;保留首尾 + 高分中段错误。参考 log_compressor.rs。
- 发生删行 → `lossy=true` 挂 CCR(`kind=Text`);纯模板折叠(不删)`lossy=false`。
- 替代 v1 的"位置截断 head/tail",解决"中段 ERROR/栈帧被无差别折叠"问题。

### ⑥ Truncate(改 truncate.rs)— 删内容,挂 CCR

- 兜底:strip_ansi(已有)+ head/tail + 超 max_bytes 硬截断(UTF-8 安全,已有)。
- **base64/blob 折叠不在此**:已上移到预处理层 `blob_fold` 段(§4.6,#6 唯一位置),Truncate 拿到的文本已折叠过 blob,不重复处理。
- `lossy=true`(截断删内容),`kind=Text`,挂 CCR。

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
max_array_items = 20           # detect 阶段:数组超此长度且无损步骤无收益、又超 truncate.max_bytes → detect false 让 Truncate 接管
max_depth = 6                  # detect 阶段:嵌套超此深度同上(判据见 §5①,统一以"无损无收益+超截断阈值"让位)
csv_schema = true              # 对象数组转 csv-schema(格式重构,不删内容,不挂 CCR)
# 注:JSON 压缩器只做不删内容步骤(RLE/csv-schema);需删内容时由 detect 让渡给 Truncate(§5①),JSON 内不删元素

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
max_files_per_thread = 200     # 每 thread 目录文件数上限(LRU)
max_thread_bytes = 67108864    # 每 thread 目录总字节上限 64 MiB(LRU 删至达标)
max_file_bytes = 4194304       # 单个 CCR 文件上限 4 MiB;enabled 下原文超此值→放弃压缩返回原文(保"有损必可取回")
```

---

## 7. 错误处理(全程 fail-open)

继承 v1 全部 fail-open,新增模块的失败处置:

- `query.rs`/`command.rs` 解析失败 → 返回空/跳过该项,不报错。
- `preprocess.rs` 任一段异常 → 该段跳过,返回上一段结果。
- `protect.rs` 判定异常 → 视作不保护(继续压,保守)。
- `ccr.rs` 落盘失败(或原文超 `max_file_bytes`)→ 仅 warn,**放弃本次有损压缩、返回原文**(不留下不可取回的有损产物,§4.7);不阻断请求。`ccr.enabled=false` 是另一条路径:保留压缩器自身省略占位、不追加取回路径。
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
  NOTICE.md          # 逐文件登记:本仓库相对路径 → 改编自哪个上游路径 + 协议
  manifest.toml      # sidecar:每个 fixture 的来源、对应压缩器、ref_output 路径、测哪条不变量
  search/ log/ diff/ json/ tabular/ preprocess/   # 按压缩器分类,内含纯原始数据文件
```

- **不在 fixture 文件内写任何来源/版权注释(#8)**:继承数据含 JSON 与真实命令输出,给 JSON 加 `//` 会使其非法、给 `.txt` 加头会污染输入并影响压缩/parity 结果。来源与版权**仅**记在 `NOTICE.md` + `manifest.toml`(sidecar),fixture 文件保持上游原始字节不变。
- `manifest.toml` 每条:`{ file = "search/grep_basic.txt", origin = "headroom/.../search_compressor.rs:680", compressor = "search", ref_output = "search/grep_basic.expected", invariants = ["关键行保留","体积不劣"] }`。测试加载时读 manifest 定位 input/ref_output,不依赖文件内注释。

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
4. **CCR 可逆性(#9,仅 `ccr.enabled=true` 下断言)**:有损项的 CCR 文件落盘且内容 == 该片段原文(注:CCR 存的是**片段原文**,非整 input;每片段一文件,§4.7)。`parity_test` 与所有 CCR 可逆性测试**固定在 `ccr.enabled=true` 配置下运行**;`ccr.enabled=false` 是独立用例,断言"有损压缩照常发生、无 CCR 文件、占位**保留压缩器自身省略提示但不含取回路径**(§4.7 #5)",**不**断言可取回。另:`enabled=true` 且原文超 `max_file_bytes` 的用例,断言**该项退回原文(未压缩)**而非产生不可取回有损产物(§4.7 #6)。

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

- 各新模块单测:query(提取/无 user)、command(解析 `shell_command.command`/`exec_command.cmd` 字符串→program/argv、非 JSON 跳过、is_* 判别)、score(错误行高分/查询加权)、protect(错误且小→保护、大→不保护)、preprocess(各段独立+组合、删内容段[strip_progress/blob_fold/truncate_line]返回 lossy=true、格式重构段 lossy=false、遇 `[llm-compress:` 前缀行跳过折叠)、schema(同构→重写、异构→None、嵌套值保留)、ccr(只产 Text 占位、路径组件 sanitize[含 `/`、`..`、超长]、每片段一文件 key=(call_id,fragment_hash)、文件数/总字节双限 LRU、**enabled 下单文件超 max_file_bytes → 返回原文**、**enabled 下落盘失败 → 返回原文/放弃本次有损压缩**、cfg.enabled=false → 不落盘且保留压缩器省略占位、二次体积检查降级/放弃)。
- 新压缩器:search(分组/保首尾/超文件折叠/查询加权/`is_grep` 命中 detect、lossy=true 挂 CCR)、tabular(满足严格前提→schema 产物可 parse、lossy=false 不挂 CCR;**无header/重复列名/空列名/列不齐/转义分隔符/单元格换行→detect 返回 false,Truncate 命中**[#1])。
- router:detect 吃 budget;`compress_text` 返回 `Option<(String,bool,ContentKind)>`;`is_git_diff`/`is_grep` 命中时候选重排到最前。
- 升级回归:json(连续 RLE 去重不删内容 + csv-schema 不删内容 + 产物合法 JSON + `kind=Json` 恒 `lossy=false` 不挂 CCR + **detect 让位:无损步骤预计有收益→认领;无损无收益且超 `truncate.max_bytes`(含数组超 max_array_items/嵌套超 max_depth)→ detect 返回 false 让 Truncate 命中;无损无收益且未超阈→ detect false 原样保留**[#1])、log(模板折叠不删+级别评分删行挂 CCR+中段 ERROR 不丢)。
- lossy 与 kind 不变量:`kind=Json ⟹ lossy=false`(JSON/Tabular 永不挂 CCR);`lossy=true ⟹ kind=Text`(attach 只产 Text 占位)。RLE/csv-schema/空行归一/连续折叠 lossy=false;删匹配/删行/截断/blob/strip_progress lossy=true。
- 编排:预处理删内容段也挂 CCR、纯格式重构预处理不挂但保留结果、**保护门命中整段逐字节不变(含 ANSI/空行/blob/重复行也不处理,#7)**、最终写回统一过 candidate≤original 闸门(#4),小输入计数占位变长则回退原文。
- 不变量(保留 + #3/#4/#5):`enabled=false` 逐字节不变、**压后 ≤ 压前(attach 内 + 编排层最终两道闸门)**、UTF-8 安全、JSON 产物可 parse(JSON 压缩器无 CCR 注入,产物天然合法)、只碰两变体、ContentItems 图片不动、CCR 双限不超磁盘。
- ccr/落盘测试用 `tempfile` + 注入 HOME(沿用 stats_test 模式)。dev-deps `insta`/`tempfile` 已有。
- 开发期走软链 member:`cd codex-rs && cargo test -p codez-llm-compress`。

### 9.2 成功判据

1. 六压缩器(Json/Search/Diff/Tabular/Log/Truncate)+ 预处理 + 保护门 + CCR 全部落地,路由优先级 `Json→Search→Diff→Tabular→Log→Truncate` 生效;命令提示能重排候选(detect 吃 budget)。
2. 继承 fixture 的 `parity_test` 全绿(8.3 四类不变量)。
3. 硬不变量满足:**压后 ≤ 压前(CCR 占位拼接后二次检查,超出则降级/放弃,#3)**、UTF-8 合法、JSON 产物可 parse、只碰两变体、图片/加密内容不动、`enabled=false` 逐字节不变。
4. CCR(#9):`ccr.enabled=true` 时,有损压缩(删行/删匹配/截断/blob)**必须**落盘片段原文且可取回,占位为 **Text 裸标记**含路径(JSON/Tabular 产物 `lossy=false` 不挂 CCR);每片段一文件(registry key=(call_id,fragment_hash));路径组件 sanitize;文件数/总字节双限;**原文超 max_file_bytes 或落盘失败 → 该项退回原文(不产生不可取回有损产物,#6)**。`ccr.enabled=false` 时:有损压缩照常发生但不落盘、占位保留压缩器自身省略提示但不含取回路径,**不要求可取回**。parity 与可逆性测试固定在 enabled 下跑。
5. **transform 签名与集成点 patch 不变**:命令上下文(`shell_command`/`exec_command` 字符串解析)/查询关键词全从 request 内提取,无新增 core 触点,不碰工具系统。
6. fail-open 全覆盖:任何新模块失败都退回原文/跳过,不阻断请求。

---

## 10. 实现顺序建议

依赖关系(共享原语先行):

```
1 router(CompressOutcome 增 lossy+kind:ContentKind + detect 吃 budget + compress_text 返回三元组 + 候选重排) + schema.rs ─┬─> 4 JSON 升级
2 query + command(shell_command/exec_command 解析) + score  ├─> 5 Search
3 preprocess(删内容/格式重构分类 + blob_fold) + protect      ├─> 6 Tabular
                                                            ├─> 7 Log 改写
                                                            ├─> 8 Truncate(不含 blob)
9 ccr(cfg 门控 + registry + sanitize + 双限 + 二次体积检查) ─┴─> 10 lib 编排接线
                                                              11 继承 fixture + parity_test
```

- 1 是接口地基:`CompressOutcome` 增 `lossy` **和 `kind: ContentKind`(#8)** + `detect(&self,text,budget)` + `compress_text` 返回 `Option<(String,bool,ContentKind)>` + router 候选重排——**先改它,4-8 各压缩器才有统一签名(每个都要补 detect budget 参数、`Compressed{..}` 的 lossy+kind 两个字段)**。schema.rs 同属地基(JSON/Tabular 共用)。
- 2-3 共享模块,无依赖可与 1 并行。blob_fold 归 preprocess(#6 唯一位置)。
- 4-8 各压缩器依赖 1-3,互相独立可并行;均需把 `detect` 改吃 budget、`Compressed{..}` 补 lossy+kind。JSON/Tabular 恒 `kind=Json,lossy=false`;Truncate 不再做 blob(已上移)。
- 9 ccr 依赖 RequestCtx 的可变 registry(#8)与 CcrCfg(#4/#5),只产 Text 占位,含路径 sanitize、双限清理、二次体积检查。
- 10 编排把全部接线(含预处理删内容段挂 CCR + 最终 candidate≤original 闸门)。11 数据继承(NOTICE+manifest sidecar)与对比测试收尾。





