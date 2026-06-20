# Task 03: TruncateCompressor(兜底文本截断器)

> 属于 `2026-06-20-llm-compress-00-index.md`。执行前先读 index 的 Global Constraints。依赖 Task 01(`config::TruncateCfg`)与 Task 02(`router::{Budget, CompressOutcome, Compressor}`)。

**Goal:** 实现兜底文本压缩器 `TruncateCompressor`:剥 ANSI 转义、按行 head/tail 保留、中间用裸占位标记 `[llm-compress: …]` 替换,必要时按 `max_bytes` 在 UTF-8 字符边界硬截断。`detect()` 永真(认领一切),作为 ContentRouter 链尾兜底。小输入保守 `Unchanged`。

**覆盖 spec:** §6(文本型压缩器:head/tail 截断 + 裸占位 + 不可逆但保守)。

**契约钉死(来自 Task 02,不得改):**

```rust
pub struct Budget<'a> { pub cfg: &'a Config }            // crate::router::Budget
pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize }, Unchanged }
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

配置切片(来自 Task 01 `config::TruncateCfg`,从 `budget.cfg.truncate` 取):

```rust
pub struct TruncateCfg { pub head_lines: usize, pub tail_lines: usize, pub max_bytes: usize }
```

**Files:**
- Create: `zmod/llm-compress/src/compress/truncate.rs`
- Create: `zmod/llm-compress/tests/truncate_test.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs`(新建该文件并 `pub mod truncate;`;若 04–06 已建则只追加该行)
- Modify: `zmod/llm-compress/src/lib.rs`(加 `pub mod compress;`)

**Interfaces:**
- Consumes(from Task 01): `config::{Config, TruncateCfg}`。
- Consumes(from Task 02): `router::{Budget, CompressOutcome, Compressor}`。
- Produces(08 注册进 ContentRouter 时依赖):
  - `pub struct TruncateCompressor;` —— 实现 `Compressor`,`name() == "truncate"`,`detect()` 永真。

**行为规格(spec §6,逐条):**
1. `name()` 返回 `"truncate"`。
2. `detect()` 永真(兜底,认领一切内容)。
3. `compress()`:
   - ① strip ANSI:去掉 `\x1b[ … <终止字节>` 形式的 CSI 序列(覆盖 `\x1b[0m`、`\x1b[1;31m`、`\x1b[2K` 等);手写扫描器,不引第三方正则依赖。
   - ② 按 `\n` 切行。
   - ③ 若(剥 ANSI 后)总行数 `≤ head_lines + tail_lines` **且** 字节数 `≤ max_bytes` → `Unchanged`。
   - ④ 否则保留前 `head_lines` 行 + 后 `tail_lines` 行,中间替换为**一行**裸占位标记 `[llm-compress: 略 N 行 / M 字节]`(N=省略行数,M=省略行的字节数,含被省略行之间的换行)。
   - ⑤ 若拼接后仍 `> max_bytes`,再按 `max_bytes` 硬截断(在 UTF-8 字符边界,不切坏多字节字符)并在末尾追加 `\n[llm-compress: 截断至 max_bytes]`。
   - ⑥ `saved_bytes = 原文字节长 - 新文本字节长`;`saved_bytes > 0` → `Compressed { text, saved_bytes }`,否则 `Unchanged`。
4. 占位标记格式严格用 `[llm-compress: …]`(裸文本,非 JSON)。
5. **不可逆但保守**:小输入(行数与字节都未超阈)直接 `Unchanged`,绝不无谓改写。

> 设计取舍(照 index Global Constraints):非测试代码不用 `unwrap`/`expect`;ANSI 剥离手写、零额外依赖;UTF-8 硬截断用 `char_indices` 找安全字节边界。`saved_bytes` 用字节口径(`len()`),与 spec §7 统计口径一致。`min_total_bytes` / `per_item_min_bytes` 门槛由 Task 08 的 transform 把控,本压缩器只看 `truncate.*` 三个阈值。

---

- [ ] **Step 1: 写失败测试**

Create `zmod/llm-compress/tests/truncate_test.rs`:

```rust
use codez_llm_compress::compress::truncate::TruncateCompressor;
use codez_llm_compress::config::{Config, TruncateCfg};
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

/// 用指定 truncate 阈值构造一个 Config(其余字段取默认)。
fn cfg_with(head_lines: usize, tail_lines: usize, max_bytes: usize) -> Config {
    let mut c = Config::disabled();
    c.truncate = TruncateCfg { head_lines, tail_lines, max_bytes };
    c
}

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg }
}

#[test]
fn name_is_truncate() {
    assert_eq!(TruncateCompressor.name(), "truncate");
}

#[test]
fn detect_is_always_true() {
    assert!(TruncateCompressor.detect(""));
    assert!(TruncateCompressor.detect("anything at all"));
}

#[test]
fn small_input_is_unchanged() {
    // 3 行,远低于 head+tail=100,字节也远低于 max_bytes。
    let cfg = cfg_with(50, 50, 16384);
    let input = "line1\nline2\nline3";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    assert!(matches!(out, CompressOutcome::Unchanged));
}

#[test]
fn large_input_keeps_head_and_tail_with_marker() {
    // head=2, tail=2;造 10 行 → 必然超 head+tail=4。
    let cfg = cfg_with(2, 2, 16384);
    let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
    let input = lines.join("\n");
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes } = out else {
        panic!("expected Compressed");
    };
    // 头 2 行在;
    assert!(text.contains("line0"));
    assert!(text.contains("line1"));
    // 尾 2 行在;
    assert!(text.contains("line8"));
    assert!(text.contains("line9"));
    // 中间某行被省略;
    assert!(!text.contains("line5"));
    // 裸占位标记存在,格式正确;
    assert!(text.contains("[llm-compress: 略 6 行 /"));
    // 确有压缩;
    assert!(saved_bytes > 0);
    assert_eq!(saved_bytes, input.len() - text.len());
}

#[test]
fn ansi_escapes_are_stripped() {
    // 含颜色码;阈值放大到不会触发行/字节截断,只验证 ANSI 被剥离。
    // 注意:剥 ANSI 后仍是小输入 → Unchanged,故这里用一行超 max_bytes 的方式逼出压缩,
    // 但为聚焦 ANSI,改用 head=0/tail=0 + 一个会被硬截断的长行也不便观察。
    // 采用:多行 + 低 head/tail,使其进入压缩路径,再断言输出里无 \x1b。
    let cfg = cfg_with(1, 1, 16384);
    let input = "\x1b[31mred0\x1b[0m\n\x1b[1;32mgreen1\x1b[0m\n\x1b[33myellow2\x1b[0m\n\x1b[34mblue3\x1b[0m";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, .. } = out else {
        panic!("expected Compressed");
    };
    // 输出中不得残留任何 ESC 字节;
    assert!(!text.contains('\x1b'));
    // 颜色码被剥离后,纯文本仍在;
    assert!(text.contains("red0"));
    assert!(text.contains("blue3"));
}

#[test]
fn hard_truncate_does_not_split_utf8() {
    // head=tail=0 → 全体进占位;占位标记本身含中文(多字节),
    // 设极小 max_bytes 逼出硬截断,断言输出仍是合法 UTF-8(能成功转回 &str)。
    let cfg = cfg_with(0, 0, 12);
    // 多行中文,字节数远超 12。
    let input = "甲乙丙丁\n戊己庚辛\n壬癸子丑\n寅卯辰巳";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes } = out else {
        panic!("expected Compressed");
    };
    // 关键:String 天然保证 UTF-8;若硬截断切坏边界,实现里会 panic 或产出非法字节。
    // 这里再显式校验:bytes 能无损解析回字符串(round-trip)。
    assert_eq!(std::str::from_utf8(text.as_bytes()).unwrap(), text);
    // 硬截断标记存在;
    assert!(text.contains("[llm-compress: 截断至 max_bytes]"));
    assert!(saved_bytes > 0);
}

#[test]
fn saved_bytes_is_original_minus_new() {
    let cfg = cfg_with(1, 1, 16384);
    let input = "a\nbbbbbbbbbb\ncccccccccc\ndddddddddd\ne";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes } = out else {
        panic!("expected Compressed");
    };
    assert_eq!(saved_bytes, input.len() - text.len());
    assert!(saved_bytes > 0);
}

#[test]
fn over_byte_limit_but_few_lines_compresses() {
    // 仅 2 行(≤ head+tail=4),但字节超 max_bytes → 仍须压缩(条件③是“且”,任一超即压)。
    let cfg = cfg_with(2, 2, 8);
    let input = "0123456789\nabcdefghij"; // 21 字节 > 8
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    // head=2/tail=2 已覆盖全部 2 行 → 无中间省略行 → 占位标记不出现,
    // 但拼接后仍 > max_bytes → 触发硬截断标记。
    let CompressOutcome::Compressed { text, saved_bytes } = out else {
        panic!("expected Compressed (byte limit exceeded)");
    };
    assert!(text.contains("[llm-compress: 截断至 max_bytes]"));
    assert!(saved_bytes > 0);
}
```

- [ ] **Step 2: 跑测试看失败**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test truncate_test
```
Expected: 编译失败(`compress` 模块 / `TruncateCompressor` 未定义)。

- [ ] **Step 3: 写 truncate.rs(完整实现,无 TODO)**

Create `zmod/llm-compress/src/compress/truncate.rs`:

```rust
//! 兜底文本截断器:剥 ANSI、按行 head/tail 保留、中间裸占位、必要时按 max_bytes
//! 在 UTF-8 字符边界硬截断。detect() 永真,作为 ContentRouter 链尾兜底。
//! 不可逆但保守:小输入直接 Unchanged。

use crate::router::{Budget, CompressOutcome, Compressor};

/// 兜底文本压缩器。无状态,单元结构体。
pub struct TruncateCompressor;

impl Compressor for TruncateCompressor {
    fn name(&self) -> &'static str {
        "truncate"
    }

    /// 永真:认领一切内容(链尾兜底)。
    fn detect(&self, _text: &str) -> bool {
        true
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let cfg = &budget.cfg.truncate;
        let original_len = text.len();

        // ① 剥 ANSI 转义序列。
        let cleaned = strip_ansi(text);

        // ② 按行切。
        let lines: Vec<&str> = cleaned.split('\n').collect();
        let line_count = lines.len();

        // ③ 行数与字节都未超阈 → 保守不动。
        //    注意:即使剥了 ANSI,只要未超阈也返回 Unchanged(不为“仅去色”而改写,
        //    避免无谓不可逆改动;saved_bytes 口径也要求 new < original 才算压缩)。
        if line_count <= cfg.head_lines + cfg.tail_lines && cleaned.len() <= cfg.max_bytes {
            return CompressOutcome::Unchanged;
        }

        // ④ 保留前 head_lines + 后 tail_lines,中间一行裸占位。
        let mut result = build_head_tail(&lines, cfg.head_lines, cfg.tail_lines);

        // ⑤ 若仍超 max_bytes,按字符边界硬截断 + 追加截断标记。
        if result.len() > cfg.max_bytes {
            result = hard_truncate(&result, cfg.max_bytes);
        }

        // ⑥ 计算 saved_bytes;须确有缩减。
        if result.len() < original_len {
            let saved_bytes = original_len - result.len();
            CompressOutcome::Compressed { text: result, saved_bytes }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// 手写 ANSI(CSI)剥离:跳过 `\x1b[` 起、到终止字节(`@`..=`~`,0x40..=0x7E)止的序列。
/// 覆盖 `\x1b[0m`、`\x1b[1;31m`、`\x1b[2K` 等;对孤立的 `\x1b` 也安全跳过。
/// 非 CSI 的其它 ESC 形式(如 `\x1bP`…)在工具输出里罕见,这里保守地丢弃紧跟的单字节,
/// 不追求覆盖全部 ECMA-48,够用即可(spec §6 仅要求剥常见颜色/控制码)。
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI:跳到终止字节(0x40..=0x7E)为止(含终止字节)。
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                // j 指向终止字节(若存在);整段连终止字节一并丢弃。
                i = if j < bytes.len() { j + 1 } else { j };
            } else {
                // 孤立 ESC 或非 CSI:丢弃 ESC 及其后一个字节(若有)。
                i = if i + 1 < bytes.len() { i + 2 } else { i + 1 };
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // strip_ansi 只删除完整 ASCII 控制序列(ESC=0x1b 与 0x40..=0x7e 均为单字节 ASCII),
    // 不会切断多字节 UTF-8 字符,故 from_utf8 必然成功;失败时保守回退原文。
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// 拼装“前 head 行 + 占位标记 + 后 tail 行”。
/// 若 head+tail 覆盖了全部行(无中间省略),则不插占位标记,原样拼回。
fn build_head_tail(lines: &[&str], head: usize, tail: usize) -> String {
    let n = lines.len();

    // head+tail 覆盖全部行 → 无省略,直接拼回(可能仍因字节超阈走硬截断)。
    if head + tail >= n {
        return lines.join("\n");
    }

    let head_part = &lines[..head];
    let tail_part = &lines[n - tail..];
    let omitted = &lines[head..n - tail];

    let omitted_lines = omitted.len();
    // 省略字节数:被省略各行的字节 + 行间换行(omitted_lines - 1 个,若 >0)。
    let mut omitted_bytes: usize = omitted.iter().map(|l| l.len()).sum();
    if omitted_lines > 0 {
        omitted_bytes += omitted_lines - 1;
    }

    let marker = format!("[llm-compress: 略 {omitted_lines} 行 / {omitted_bytes} 字节]");

    let mut parts: Vec<&str> = Vec::with_capacity(head + 1 + tail);
    parts.extend_from_slice(head_part);
    parts.push(&marker);
    parts.extend_from_slice(tail_part);
    parts.join("\n")
}

/// 在 UTF-8 字符边界把 `text` 截到 `max_bytes` 以内,追加截断标记。
/// 预留标记所需字节,确保最终结果整体 ≤ 一个合理上界(尽量贴近 max_bytes)。
fn hard_truncate(text: &str, max_bytes: usize) -> String {
    const SUFFIX: &str = "\n[llm-compress: 截断至 max_bytes]";

    // 预算:正文允许的字节上限 = max_bytes 减去标记长度(若 max_bytes 比标记还小,则正文取 0)。
    let budget = max_bytes.saturating_sub(SUFFIX.len());

    // 找 ≤ budget 的最大 UTF-8 字符边界。
    let mut cut = 0usize;
    for (idx, ch) in text.char_indices() {
        let end = idx + ch.len_utf8();
        if end <= budget {
            cut = end;
        } else {
            break;
        }
    }

    let mut result = String::with_capacity(cut + SUFFIX.len());
    result.push_str(&text[..cut]);
    result.push_str(SUFFIX);
    result
}
```

- [ ] **Step 4: 注册模块(compress/mod.rs + lib.rs)**

Create(或若 04–06 已建则改)`zmod/llm-compress/src/compress/mod.rs`:

```rust
//! 各内容类型压缩器集合。每个子模块实现一个 router::Compressor。
//! 03 truncate(兜底文本);04 json;05 diff;06 log —— 由各自任务追加 pub mod 行。

pub mod truncate;
```

> 若执行本任务时 `compress/mod.rs` 已由并行任务(04–06)创建,**只追加** `pub mod truncate;` 一行,勿覆盖其它 `pub mod`。

Modify `zmod/llm-compress/src/lib.rs`,在 `pub mod router;` 之后追加(若 04–06 已加过则跳过,避免重复):

```rust
pub mod compress;
```

- [ ] **Step 5: 跑测试看通过**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test truncate_test
```
Expected: `test result: ok. 9 passed`。

- [ ] **Step 6: 提交(仅 zmod + 本 plan,不含 codex-rs 的 dirty 改动)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-03-truncate.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): TruncateCompressor (fallback text truncation)"
```

> **不要** `git add codex-rs/**`——`core/Cargo.toml` 的 dev-build 使能改动保持 dirty 至 Task 09。
