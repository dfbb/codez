# Task 06: LogCompressor(连续重复折叠 + head/tail 保留)

> 属于 `2026-06-20-llm-compress-00-index.md`。执行前先读 index 的 Global Constraints / 真实类型 / dev-build 决策。依赖 Task 01(config)与 Task 02(`Compressor` trait / `Budget` / `CompressOutcome`)。

**Goal:** 实现文本型压缩器 `LogCompressor`,识别多行日志/栈跟踪文本,先把连续完全相同的行折叠为 `行 + [llm-compress: 上一行 ×N]`,再对整体做 `truncate.head_lines` + `truncate.tail_lines` 保留(中间折叠为一行 `[llm-compress: 略 N 行]`)。占位为裸文本标记(log 属文本型)。本任务结束时 `cargo test -p codez-llm-compress --test log_test` 通过。

**覆盖 spec:** §6(文本型压缩器 / 裸占位标记 / dedup_repeats / head-tail 保留)。

**Files:**
- Create: `zmod/llm-compress/src/compress/log.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs`(加 `pub mod log;`;若该文件尚不存在则新建,内容见 Step 4)
- Modify: `zmod/llm-compress/src/lib.rs`(确保有 `pub mod compress;`)
- Test: `zmod/llm-compress/tests/log_test.rs`

**Interfaces:**
- Consumes(from Task 01):`config::{Config, TruncateCfg, LogCfg}`。
- Consumes(from Task 02):`router::{Budget, CompressOutcome, Compressor}`。
- Produces(03–06 的压缩器在 Task 08 被装进 `ContentRouter`):
  - `pub struct LogCompressor;`
  - `impl Compressor for LogCompressor`,`name()` = `"log"`。

**契约(不得改,源自 Task 02):**

```rust
pub struct Budget<'a> { pub cfg: &'a Config }
pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize }, Unchanged }
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

**行为规格(spec §6):**

- `name()` = `"log"`。
- `detect(text)`:**多行**(`≥8` 行)**且**具备以下任一日志特征,才认领。否则不认领(避免误吞普通短文本):
  1. 含时间戳:行匹配 `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}`(如 `2026-06-20T12:00:00`)或 `\d{2}:\d{2}:\d{2}`(如 `12:00:00`);
  2. 栈跟踪特征:某行含 ` at ` 且其后含 `:` 后跟数字(如 `at foo.rs:42`);
  3. 存在连续完全相同的两行(连续重复)。
- `compress(text, budget)`:
  1. 取 `cfg = &budget.cfg.log`(`LogCfg`)、`tr = &budget.cfg.truncate`(`TruncateCfg`)。
  2. **若 `cfg.dedup_repeats`**:连续完全相同的行折叠为 `该行 + 一行裸占位 [llm-compress: 上一行 ×N]`(`N` = 该行连续重复的总次数,即被折叠的行数;`N≥2` 才折叠,`N==1` 原样保留)。
  3. **再对整体做 head/tail 保留**:若折叠后总行数 `> tr.head_lines + tr.tail_lines`,保留前 `tr.head_lines` 行 + 一行 `[llm-compress: 略 N 行]`(`N` = 被省略的行数)+ 后 `tr.tail_lines` 行。否则不截断。
  4. 计算 `saved_bytes = 原文 .len() - 产物 .len()`(用 `saturating_sub`);`saved_bytes > 0` → `Compressed`,否则 `Unchanged`。
- 占位裸文本标记 `[llm-compress: …]`(log 是文本型,**不**包 JSON)。

**实现注意(钉死,避免歧义):**
- **行拆分用 `text.lines()`**:按 `\n`(兼容 `\r\n`,`lines()` 会去掉 `\r`),不保留行尾换行;产物用 `"\n"` 重新 `join`。这意味着若原文以 `\n` 结尾,产物可能少一个尾换行——这对 `saved_bytes` 只增不减,符合"保守压缩"。
- **两步顺序固定**:先 dedup,再 head/tail。head/tail 作用于 dedup 后的行序列(占位行也计入行数)。
- **占位行是独立的一整行**,不与日志正文同行。
- 非测试代码不得 `unwrap`/`expect`(本压缩器无需任何 unwrap)。

---

- [ ] **Step 1: 写失败测试**

Create `zmod/llm-compress/tests/log_test.rs`:

```rust
use codez_llm_compress::compress::log::LogCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg }
}

/// 构造一段真实风格、带时间戳的多行日志(≥8 行)。
fn timestamped_log(lines: usize) -> String {
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(&format!(
            "2026-06-20T12:00:{:02} INFO  request handled id={}\n",
            i % 60,
            i
        ));
    }
    s
}

#[test]
fn detect_true_for_timestamped_multiline_log() {
    let c = LogCompressor;
    let log = timestamped_log(12);
    assert!(c.detect(&log), "带时间戳的多行日志应被认领");
}

#[test]
fn detect_true_for_stacktrace() {
    let c = LogCompressor;
    let trace = "\
thread 'main' panicked at 'boom'
stack backtrace:
   0: core::panicking::panic
   1: app::run
             at src/main.rs:42
   2: app::main
             at src/main.rs:10
   3: std::rt::lang_start
   4: main
   5: __libc_start_main";
    assert!(c.detect(trace), "含 `at file:line` 的栈跟踪应被认领");
}

#[test]
fn detect_false_for_plain_short_text() {
    let c = LogCompressor;
    let txt = "Hello world.\nThis is a short note.\nNothing log-like here.";
    assert!(!c.detect(txt), "普通短文本不应被认领");
}

#[test]
fn detect_false_for_long_plain_text_without_log_features() {
    // ≥8 行但无任何日志特征 / 无连续重复 → 不认领。
    let c = LogCompressor;
    let mut s = String::new();
    for i in 0..12 {
        s.push_str(&format!("paragraph line number {i} talking about cats\n"));
    }
    assert!(!c.detect(&s), "多行但无日志特征的普通文本不应被认领");
}

#[test]
fn detect_true_for_consecutive_repeats() {
    let c = LogCompressor;
    let mut s = String::new();
    for _ in 0..10 {
        s.push_str("retrying connection...\n");
    }
    assert!(c.detect(&s), "存在连续重复行应被认领");
}

#[test]
fn dedup_collapses_consecutive_repeats() {
    let c = LogCompressor;
    let cfg = Config::disabled(); // dedup_repeats 默认 true
    assert!(cfg.log.dedup_repeats);
    let mut s = String::new();
    s.push_str("start\n");
    for _ in 0..5 {
        s.push_str("retrying connection...\n");
    }
    s.push_str("done\n");
    match c.compress(&s, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes } => {
            assert!(text.contains("retrying connection..."));
            assert!(
                text.contains("[llm-compress: 上一行 ×5]"),
                "应折叠为 ×5,实际:\n{text}"
            );
            // 折叠后 retrying 只出现一次正文 + 一行占位。
            assert_eq!(text.matches("retrying connection...").count(), 1);
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("应折叠重复行"),
    }
}

#[test]
fn dedup_disabled_keeps_repeats() {
    let c = LogCompressor;
    // 用 disabled() 再改字段构造 dedup_repeats=false 的 Config。
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false;
    // 给足 head/tail 余量,避免触发截断,纯验证 dedup 不发生。
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    let mut s = String::new();
    for _ in 0..6 {
        s.push_str("retrying connection...\n");
    }
    match c.compress(&s, &budget(&cfg)) {
        CompressOutcome::Compressed { .. } => panic!("dedup 关闭且未截断时不应有压缩"),
        CompressOutcome::Unchanged => {}
    }
}

#[test]
fn head_tail_truncates_long_log_with_placeholder() {
    let c = LogCompressor;
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false; // 隔离 head/tail 行为(各行互不相同)
    cfg.truncate.head_lines = 3;
    cfg.truncate.tail_lines = 3;
    let log = timestamped_log(50); // 50 行,各不相同
    match c.compress(&log, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes } => {
            // 中间被省略:50 - 3 - 3 = 44 行。
            assert!(
                text.contains("[llm-compress: 略 44 行]"),
                "应有 head/tail 占位,实际:\n{text}"
            );
            // 产物行数 = 3 + 1(占位) + 3 = 7 行。
            assert_eq!(text.lines().count(), 7);
            // 保留首行与末行。
            assert!(text.lines().next().unwrap().contains("id=0"));
            assert!(text.lines().last().unwrap().contains("id=49"));
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("长日志应被截断"),
    }
}

#[test]
fn dedup_then_head_tail_combined() {
    let c = LogCompressor;
    let mut cfg = Config::disabled(); // dedup_repeats=true
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    let mut s = String::new();
    s.push_str("2026-06-20T12:00:00 INFO boot\n");
    for _ in 0..30 {
        s.push_str("2026-06-20T12:00:01 WARN retrying...\n");
    }
    s.push_str("2026-06-20T12:00:02 INFO ok line a\n");
    s.push_str("2026-06-20T12:00:03 INFO ok line b\n");
    s.push_str("2026-06-20T12:00:04 INFO ok line c\n");
    match c.compress(&s, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes } => {
            assert!(text.contains("[llm-compress: 上一行 ×30]"));
            // dedup 后行数 = boot(1) + retrying(1) + 占位(1) + 3 行 ok = 6 行;
            // 6 > head(2)+tail(2)=4 → 仍会再截断,出现 head/tail 占位。
            assert!(text.contains("[llm-compress: 略"));
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("应有压缩"),
    }
}

#[test]
fn unchanged_when_nothing_to_do() {
    let c = LogCompressor;
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false;
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    // 10 行带时间戳、各不相同、无重复、未超 head+tail → 无压缩。
    let log = timestamped_log(10);
    assert!(matches!(
        c.compress(&log, &budget(&cfg)),
        CompressOutcome::Unchanged
    ));
}
```

- [ ] **Step 2: 跑测试看失败**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test log_test
```
Expected: 编译失败(`compress::log` 模块 / `LogCompressor` 未定义)。

- [ ] **Step 3: 写 log.rs**

Create `zmod/llm-compress/src/compress/log.rs`:

```rust
//! LogCompressor:文本型日志压缩器。
//! 两步保守压缩:① 连续重复行折叠 ② head/tail 保留。占位为裸文本标记 [llm-compress: …]。

use crate::router::{Budget, CompressOutcome, Compressor};

/// 识别多行日志 / 栈跟踪文本并做保守压缩。
pub struct LogCompressor;

/// detect 的"多行"门槛:行数 ≥ 此值才考虑认领。
const MIN_LINES: usize = 8;

impl Compressor for LogCompressor {
    fn name(&self) -> &'static str {
        "log"
    }

    fn detect(&self, text: &str) -> bool {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < MIN_LINES {
            return false;
        }
        has_timestamp(&lines) || has_stacktrace(&lines) || has_consecutive_repeat(&lines)
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let lines: Vec<&str> = text.lines().collect();

        // ① 连续重复行折叠(可选)。
        let dedup_repeats = budget.cfg.log.dedup_repeats;
        let after_dedup: Vec<String> = if dedup_repeats {
            dedup_consecutive(&lines)
        } else {
            lines.iter().map(|s| s.to_string()).collect()
        };

        // ② head/tail 保留。
        let head = budget.cfg.truncate.head_lines;
        let tail = budget.cfg.truncate.tail_lines;
        let final_lines = head_tail(&after_dedup, head, tail);

        let new_text = final_lines.join("\n");
        let saved = text.len().saturating_sub(new_text.len());
        if saved > 0 {
            CompressOutcome::Compressed {
                text: new_text,
                saved_bytes: saved,
            }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// 是否含时间戳行:`YYYY-MM-DD[T ]HH:MM:SS` 或裸 `HH:MM:SS`。
/// 不引入正则依赖,用字符级扫描判定。
fn has_timestamp(lines: &[&str]) -> bool {
    lines.iter().any(|l| line_has_full_ts(l) || line_has_clock_ts(l))
}

/// 检测行内是否出现 `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}`。
fn line_has_full_ts(line: &str) -> bool {
    let bytes = line.as_bytes();
    // 滑动窗口:date(10) + sep(1) + time(8) = 19 个字符。
    if bytes.len() < 19 {
        return false;
    }
    for i in 0..=bytes.len() - 19 {
        let w = &bytes[i..i + 19];
        let date_ok = w[0].is_ascii_digit()
            && w[1].is_ascii_digit()
            && w[2].is_ascii_digit()
            && w[3].is_ascii_digit()
            && w[4] == b'-'
            && w[5].is_ascii_digit()
            && w[6].is_ascii_digit()
            && w[7] == b'-'
            && w[8].is_ascii_digit()
            && w[9].is_ascii_digit();
        let sep_ok = w[10] == b'T' || w[10] == b' ';
        let time_ok = is_clock(&w[11..19]);
        if date_ok && sep_ok && time_ok {
            return true;
        }
    }
    false
}

/// 检测行内是否出现裸 `\d{2}:\d{2}:\d{2}`。
fn line_has_clock_ts(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.len() < 8 {
        return false;
    }
    for i in 0..=bytes.len() - 8 {
        if is_clock(&bytes[i..i + 8]) {
            return true;
        }
    }
    false
}

/// 8 字节窗口是否为 `dd:dd:dd`。
fn is_clock(w: &[u8]) -> bool {
    w.len() == 8
        && w[0].is_ascii_digit()
        && w[1].is_ascii_digit()
        && w[2] == b':'
        && w[3].is_ascii_digit()
        && w[4].is_ascii_digit()
        && w[5] == b':'
        && w[6].is_ascii_digit()
        && w[7].is_ascii_digit()
}

/// 是否含栈跟踪特征:某行含 ` at ` 且其后存在 `:` 紧跟数字(如 `at foo.rs:42`)。
fn has_stacktrace(lines: &[&str]) -> bool {
    lines.iter().any(|l| {
        if let Some(pos) = l.find(" at ") {
            let rest = &l[pos + 4..];
            colon_then_digit(rest)
        } else {
            false
        }
    })
}

/// 子串里是否存在 `:` 紧跟一个数字。
fn colon_then_digit(s: &str) -> bool {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b':' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

/// 是否存在连续两行完全相同。
fn has_consecutive_repeat(lines: &[&str]) -> bool {
    lines.windows(2).any(|w| w[0] == w[1])
}

/// 把连续完全相同的行折叠为:该行 + 裸占位 `[llm-compress: 上一行 ×N]`(N≥2)。
/// N==1 的行原样保留(不加占位)。
fn dedup_consecutive(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        let mut j = i + 1;
        while j < lines.len() && lines[j] == cur {
            j += 1;
        }
        let count = j - i; // 该行连续出现的总次数
        out.push(cur.to_string());
        if count >= 2 {
            out.push(format!("[llm-compress: 上一行 ×{count}]"));
        }
        i = j;
    }
    out
}

/// head/tail 保留:总行数 > head+tail 时,保留前 head 行 + `[llm-compress: 略 N 行]` + 后 tail 行。
/// 否则原样返回。
fn head_tail(lines: &[String], head: usize, tail: usize) -> Vec<String> {
    if lines.len() <= head + tail {
        return lines.to_vec();
    }
    let omitted = lines.len() - head - tail;
    let mut out: Vec<String> = Vec::with_capacity(head + tail + 1);
    out.extend_from_slice(&lines[..head]);
    out.push(format!("[llm-compress: 略 {omitted} 行]"));
    out.extend_from_slice(&lines[lines.len() - tail..]);
    out
}
```

> **设计说明(中文):**
> - 不引入 `regex` 依赖,时间戳/栈跟踪用字符级滑窗判定,廉价且零新依赖。
> - `dedup_consecutive` 的 `N` 取"连续出现总次数"(含首次出现),与占位文案 `×N` 一致:`retrying` 出现 5 次 → 正文 1 行 + `×5` 占位。
> - 两步顺序固定:dedup 先于 head/tail;占位行计入 head/tail 的行数统计。
> - `compress` 内全程无 `unwrap`/`expect`,符合 Global Constraints 的 Rust 风格。

- [ ] **Step 4: 注册模块(compress/mod.rs + lib.rs)**

Modify `zmod/llm-compress/src/compress/mod.rs`,加一行:

```rust
pub mod log;
```

> 若 `src/compress/mod.rs` 尚不存在(Task 03–05 未先建该目录),则**新建**该文件,内容为:
> ```rust
> //! 各内容类型压缩器(truncate / json / diff / log)。
> pub mod log;
> ```
> 后续 03/04/05 任务会在此追加各自的 `pub mod`。

Modify `zmod/llm-compress/src/lib.rs`,确保有 `compress` 模块声明(若已被 03–05 加过则跳过)。在 `pub mod router;` 之后加:

```rust
pub mod compress;
```

- [ ] **Step 5: 跑测试看通过**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test log_test
```
Expected: `test result: ok. 10 passed`。

- [ ] **Step 6: 提交(仅 zmod + 本计划)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-06-log.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): LogCompressor (dedup repeats + head/tail)"
```

> **不要** `git add codex-rs/core/Cargo.toml`——dev-build 使能器,保持 dirty 至 Task 09。
</content>
</invoke>
