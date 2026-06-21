# Task 08 — Log 改写:模板挖掘 + 级别评分保留

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §5⑤。依赖 Task 01(签名)、Task 02(score)。可与 05/06/07/09 并行。

**Goal:** 把 Log 压缩器从 v1 的"连续重复行折叠 + 位置截断 head/tail"改为:**① 模板挖掘(RLE,不删内容)** 连续同模板行折叠为模板头 + 变量表;**② 级别评分保留(删行)** 用 `score::line_score` 保 error/warn/栈帧,DEBUG/INFO 超量丢弃。解决"中段 ERROR/栈帧被无差别折叠"。模板折叠 `lossy=false`;删行 `lossy=true`。

## Files
- Modify: `zmod/llm-compress/src/compress/log.rs`(重写 compress;detect 保留 + 加 budget,Task 01 已同步签名)
- Test: `zmod/llm-compress/tests/log_test.rs`(改写,Task 01 已同步旧签名)

**Interfaces:**
- Consumes: Task 01 的 `Budget`/`CompressOutcome`/`ContentKind`、`config.log.{dedup_repeats,template_min_run,keep_levels}`、`config.truncate.{head_lines,tail_lines}`;Task 02 的 `score::line_score`。
- Produces: 升级后的 `LogCompressor`(detect 保持 v1 多行/时间戳/栈/重复判定 + 加 budget 参数)。

> **算法(spec §5⑤)**:
> - **模板挖掘(不删)**:连续 ≥ `template_min_run`(默认 3)行,若"同模板"(把行内数字/十六进制替换成占位后相等)→ 折叠为 `[llm-compress: 模板] <模板>` + 变量表 `[llm-compress: 变量] v1, v2, ...`。变量全保留 → lossy=false。
> - **级别评分保留(删行)**:对剩余行,error/warn(`keep_levels`)+ 栈帧(`at ...:数字`)+ `score::line_score ≥ 1.0` 的行**必留**;其余按"保 head/tail + 中段高分"选取,丢弃段折叠为 `[llm-compress: 略 N 行]` → lossy=true。
> - lossy:发生删行→true;仅模板折叠→false。kind 恒 Text。

---

- [ ] **Step 1: 写失败测试(改写 log_test.rs)**

`zmod/llm-compress/tests/log_test.rs` 现有 v1 用例(测连续重复 + head/tail)多数已不适用。**保留 Task 01 同步过签名的 detect 类用例**,删除测 v1 位置截断的用例,**追加** Task 08 新用例:

```rust
// ========== Task 08 新增(放文件末尾) ==========
use codez_llm_compress::compress::log::LogCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget_t08(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn middle_error_is_kept_not_folded() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    let c = LogCompressor;
    // 中段一条 ERROR,被大量 INFO 包围
    let mut lines = Vec::new();
    for i in 0..10 {
        lines.push(format!("INFO step {i}"));
    }
    lines.push("ERROR critical failure at core".to_string());
    for i in 0..10 {
        lines.push(format!("INFO step {}", 10 + i));
    }
    let text = lines.join("\n");
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(&text, &budget_t08(&cfg)) {
        assert!(lossy, "删了 INFO 行");
        assert_eq!(kind, ContentKind::Text);
        assert!(new.contains("ERROR critical failure"), "中段 ERROR 必须保留");
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn template_mining_folds_similar_lines_lossless() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100; // 不触发评分删行,只看模板折叠
    cfg.log.template_min_run = 3;
    let c = LogCompressor;
    // 连续同模板行(仅数字不同)
    let text = "worker 1 done\nworker 2 done\nworker 3 done\nworker 4 done\nworker 5 done";
    if let CompressOutcome::Compressed { text: new, lossy, .. } = c.compress(text, &budget_t08(&cfg)) {
        // 模板折叠不删内容
        assert!(!lossy, "纯模板折叠 → lossy=false");
        assert!(new.contains("[llm-compress: 模板]") || new.contains("模板"));
    } else {
        // 也可能因无收益 Unchanged;但 5 行同模板应有收益
        panic!("expected template fold");
    }
}

#[test]
fn detect_still_recognizes_multiline_logs() {
    let cfg = Config::disabled();
    let c = LogCompressor;
    let text = "2026-06-21T08:15:30 INFO a\n2026-06-21T08:15:31 INFO b\n2026-06-21T08:15:32 INFO c\n2026-06-21T08:15:33 INFO d\n2026-06-21T08:15:34 INFO e\n2026-06-21T08:15:35 INFO f\n2026-06-21T08:15:36 INFO g\n2026-06-21T08:15:37 INFO h";
    assert!(c.detect(text, &budget_t08(&cfg)));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test log_test 2>&1 | head -20`
Expected: FAIL(新行为未实现)

- [ ] **Step 3: 重写 log.rs 的 Compressor impl(detect 加 budget + 新 compress)**

`zmod/llm-compress/src/compress/log.rs`:**保留**文件中现有的辅助函数 `has_timestamp` / `line_has_full_ts` / `line_has_clock_ts` / `is_clock` / `has_stacktrace` / `colon_then_digit` / `has_consecutive_repeat`(detect 仍用)。**替换** `impl Compressor for LogCompressor` 整块为:

```rust
impl Compressor for LogCompressor {
    fn name(&self) -> &'static str {
        "log"
    }

    fn detect(&self, text: &str, _budget: &Budget) -> bool {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < MIN_LINES {
            return false;
        }
        has_timestamp(&lines) || has_stacktrace(&lines) || has_consecutive_repeat(&lines)
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let lines: Vec<&str> = text.lines().collect();

        // ① 模板挖掘(不删内容)
        let min_run = budget.cfg.log.template_min_run.max(2);
        let (after_tpl, _tpl_changed) = template_mine(&lines, min_run);

        // ② 级别评分保留(删行)
        let head = budget.cfg.truncate.head_lines;
        let tail = budget.cfg.truncate.tail_lines;
        let (final_lines, dropped) = score_keep(&after_tpl, head, tail, budget);

        let new_text = final_lines.join("\n");
        let saved = text.len().saturating_sub(new_text.len());
        if saved == 0 {
            return CompressOutcome::Unchanged;
        }
        // 删行 → lossy=true;仅模板折叠(无删行)→ lossy=false
        let lossy = dropped;
        CompressOutcome::Compressed {
            text: new_text,
            saved_bytes: saved,
            lossy,
            kind: ContentKind::Text,
        }
    }
}
```

> 注意:删除现有 compress 内 v1 的 `dedup_consecutive` / `head_tail` 调用与相关旧辅助函数(`dedup_consecutive`、`head_tail`)——它们被 `template_mine` / `score_keep` 取代。若 `MIN_LINES` 常量已存在则保留。

- [ ] **Step 4: 追加 template_mine 实现**

在 `log.rs` 末尾追加:

```rust
/// 模板挖掘:连续 ≥ min_run 行,规范化(数字/hex→占位)后相等则折叠为模板头 + 变量表。
/// 不删内容(变量全保留)。返回 (新行序列, 是否折叠过)。
fn template_mine(lines: &[&str], min_run: usize) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        let tpl = normalize_template(lines[i]);
        let mut j = i + 1;
        while j < lines.len() && normalize_template(lines[j]) == tpl {
            j += 1;
        }
        let run = j - i;
        if run >= min_run {
            out.push(format!("[llm-compress: 模板] {tpl}"));
            let vars: Vec<String> = lines[i..j].iter().map(|l| l.to_string()).collect();
            out.push(format!("[llm-compress: 变量 ×{run}] {}", vars.join(" | ")));
            changed = true;
        } else {
            for k in i..j {
                out.push(lines[k].to_string());
            }
        }
        i = j;
    }
    (out, changed)
}

/// 把行内数字串、十六进制串替换成占位,用于"同模板"判定。
fn normalize_template(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            while chars.peek().is_some_and(|c| c.is_ascii_digit() || *c == 'x' || c.is_ascii_hexdigit()) {
                chars.next();
            }
            out.push('#');
        } else {
            out.push(c);
            chars.next();
        }
    }
    out
}
```

- [ ] **Step 5: 追加 score_keep 实现**

在 `log.rs` 末尾追加:

```rust
/// 级别评分保留:保 head/tail + 必留行(error/warn/栈帧/高分)+ 中段按分;
/// 丢弃的连续段折叠为 [llm-compress: 略 N 行]。返回 (新行, 是否删了行)。
fn score_keep(lines: &[String], head: usize, tail: usize, budget: &Budget) -> (Vec<String>, bool) {
    let n = lines.len();
    if n <= head + tail {
        return (lines.to_vec(), false);
    }
    let query = budget.query;
    let mut keep = vec![false; n];
    // head/tail 必留
    for i in 0..head.min(n) {
        keep[i] = true;
    }
    for i in n.saturating_sub(tail)..n {
        keep[i] = true;
    }
    // 必留:模板行/变量行(以 [llm-compress: 开头)+ 高分行 + 栈帧
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("[llm-compress: ") {
            keep[i] = true;
        } else if crate::score::line_score(line, query) >= 1.0 {
            keep[i] = true;
        } else if is_stack_frame(line) {
            keep[i] = true;
        }
    }
    // 输出,丢弃连续段折叠
    let mut out: Vec<String> = Vec::new();
    let mut dropped = false;
    let mut i = 0;
    while i < n {
        if keep[i] {
            out.push(lines[i].clone());
            i += 1;
        } else {
            let start = i;
            while i < n && !keep[i] {
                i += 1;
            }
            out.push(format!("[llm-compress: 略 {} 行]", i - start));
            dropped = true;
        }
    }
    (out, dropped)
}

/// 栈帧行特征:含 " at " 且其后有 :数字(复用 colon_then_digit)。
fn is_stack_frame(line: &str) -> bool {
    if let Some(pos) = line.find(" at ") {
        colon_then_digit(&line[pos + 4..])
    } else {
        false
    }
}
```

> 注意:确保 `log.rs` 顶部 use 含 `use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};`(Task 01 已加)。`colon_then_digit` 是文件内现有函数,`is_stack_frame` 复用它。

- [ ] **Step 6: 运行测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test log_test`
Expected: PASS(保留的 detect 用例 + Task 08 新用例 3 个)

> 若现有 v1 用例(如测 `dedup_consecutive`/`head_tail` 旧产物)编译失败或断言失败:删除它们——它们测的是被本任务取代的 v1 行为。

- [ ] **Step 7: clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/compress/log.rs zmod/llm-compress/tests/log_test.rs
git commit -m "feat(llm-compress-v2): Task08 Log 改写 — 模板挖掘(不删)+ 级别评分保留(删行)"
```


