# 实现计划:llm-compress 05 — DiffCompressor

> 本文件属于 `docs/superpowers/plans/` 索引下的 llm-compress 系列任务之一(第 05 篇)。
>
> **依赖**:
> - **Task 01**(配置):提供 `Config` 与 `pub struct DiffCfg { pub context_lines: usize }`,可经 `budget.cfg.diff` 访问。
> - **Task 02**(契约):提供 `Budget<'a>`、`CompressOutcome`、`Compressor` trait。本任务**严格遵循**该契约,不得修改。
>
> 本任务实现 `DiffCompressor`:识别 unified diff,保留全部变更行与结构头,仅折叠 hunk 内多余的上下文行。

---

## 契约(来自 Task 02,只读,不得改)

```rust
pub struct Budget<'a> { pub cfg: &'a Config }
pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize }, Unchanged }
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

配置(来自 Task 01):

```rust
pub struct DiffCfg { pub context_lines: usize }
```

从 `budget.cfg.diff` 读取。

---

## 行为规格(spec §6)

- `name()` 返回 `"diff"`。
- `detect(text)` 在满足以下**任一**条件时返回 `true`,否则 `false`:
  1. 任一行匹配 hunk 头正则 `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`;
  2. 任一行以 `diff --git ` 开头;
  3. **同时**存在以 `--- ` 开头的行**和**以 `+++ ` 开头的行。
- `compress(text, budget)`:解析 unified diff,逐 hunk 处理:
  - **完整保留**:变更行(以 `+` 或 `-` 开头,但不是文件头 `+++`/`---`)、hunk 头(`@@`)、文件头(`diff --git`、`index`、`--- `、`+++ `)。
  - **上下文行**(hunk 内以单个空格 ` ` 开头的未变更行):仅保留紧邻变更行**前后各 `context_lines` 行**;中间多余上下文折叠为**一行**裸占位 `[llm-compress: 略 N 行上下文]`,其中 `N` 为被折叠的上下文行数。
  - `saved_bytes > 0` → `Compressed { text, saved_bytes }`;否则 `Unchanged`。
  - 占位采用裸文本标记 `[llm-compress: …]`(diff 为文本型,允许裸标记)。

---

## Files

- **Create** `zmod/llm-compress/src/compress/diff.rs` — `DiffCompressor` 实现。
- **Create** `zmod/llm-compress/tests/diff_test.rs` — 集成测试。
- **Modify** `zmod/llm-compress/src/compress/mod.rs` — 新增 `pub mod diff;`。

## Interfaces

- **Produces**: `pub struct DiffCompressor;`(实现 `Compressor` trait)。
- **Consumes**: `Budget<'a>`、`CompressOutcome`、`Compressor`(Task 02);`DiffCfg`、`Config`(Task 01)。

---

## TDD 步骤

### ① 写失败测试

创建 `zmod/llm-compress/tests/diff_test.rs`:

```rust
//! DiffCompressor 集成测试。
//!
//! 覆盖:
//! - detect 对真实 git diff 为 true、对普通文本为 false;
//! - 大段上下文的 hunk 被折叠,变更行全保留;
//! - 占位标记存在;
//! - 小 diff(上下文本就少)→ Unchanged;
//! - saved_bytes 正确。

use codez_llm_compress::compress::diff::DiffCompressor;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use codez_llm_compress::config::Config;

/// 构造一个 context_lines=N 的 Config(借助 Task 01 的默认值再覆盖 diff 字段)。
fn cfg_with_context(n: usize) -> Config {
    let mut cfg = Config::default();
    cfg.diff.context_lines = n;
    cfg
}

/// 一段真实的多行 unified diff fixture:单文件、单 hunk,含大段未变更上下文。
/// hunk 内:6 行上文 + 1 行删除 + 1 行新增 + 6 行下文。
const REAL_DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,14 +1,14 @@
 line ctx 1
 line ctx 2
 line ctx 3
 line ctx 4
 line ctx 5
 line ctx 6
-old changed line
+new changed line
 line ctx 7
 line ctx 8
 line ctx 9
 line ctx 10
 line ctx 11
 line ctx 12
";

#[test]
fn detect_true_for_real_git_diff() {
    let c = DiffCompressor;
    assert!(c.detect(REAL_DIFF), "真实 git diff 应被识别");
}

#[test]
fn detect_true_for_bare_hunk_header() {
    let c = DiffCompressor;
    let text = "@@ -1,3 +1,4 @@\n a\n-b\n+c\n d\n";
    assert!(c.detect(text), "含 hunk 头应被识别");
}

#[test]
fn detect_false_for_plain_text() {
    let c = DiffCompressor;
    let text = "这是一段普通文本。\n没有任何 diff 特征。\n+ 这不是变更行只是个加号开头的句子\n";
    // 注意:仅靠单独的 '+' 开头一行不构成 diff(无 hunk 头、无 diff --git、无 '--- '+'+++ ' 配对)。
    assert!(!c.detect(text), "普通文本不应被识别");
}

#[test]
fn compress_folds_large_context_and_keeps_changes() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(2);
    let budget = Budget { cfg: &cfg };

    let outcome = c.compress(REAL_DIFF, &budget);
    let CompressOutcome::Compressed { text, saved_bytes } = outcome else {
        panic!("大段上下文应被压缩");
    };

    // 变更行必须完整保留。
    assert!(text.contains("-old changed line"), "删除行须保留");
    assert!(text.contains("+new changed line"), "新增行须保留");

    // 文件头与 hunk 头须保留。
    assert!(text.contains("diff --git a/src/lib.rs b/src/lib.rs"));
    assert!(text.contains("index 1234567..89abcde 100644"));
    assert!(text.contains("--- a/src/lib.rs"));
    assert!(text.contains("+++ b/src/lib.rs"));
    assert!(text.contains("@@ -1,14 +1,14 @@"));

    // 紧邻变更行前后各 2 行上下文须保留。
    assert!(text.contains(" line ctx 5"), "变更行前第 2 行须保留");
    assert!(text.contains(" line ctx 6"), "变更行前第 1 行须保留");
    assert!(text.contains(" line ctx 7"), "变更行后第 1 行须保留");
    assert!(text.contains(" line ctx 8"), "变更行后第 2 行须保留");

    // 被折叠的远端上下文不应出现。
    assert!(!text.contains(" line ctx 1"), "远端上文应被折叠");
    assert!(!text.contains(" line ctx 12"), "远端下文应被折叠");

    // 占位标记须存在。
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "上文 6-2=4 行应折叠为占位,实际输出:\n{text}"
    );
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "下文 6-2=4 行应折叠为占位"
    );

    // saved_bytes 应等于原文与压缩后文本的字节差。
    assert_eq!(saved_bytes, REAL_DIFF.len() - text.len(), "saved_bytes 须为字节差");
    assert!(saved_bytes > 0);
}

#[test]
fn compress_small_diff_unchanged() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(3);
    let budget = Budget { cfg: &cfg };

    // 上下文本就 ≤ context_lines,无可折叠。
    let small = "\
diff --git a/a.txt b/a.txt
index aaa..bbb 100644
--- a/a.txt
+++ b/a.txt
@@ -1,4 +1,4 @@
 ctx 1
 ctx 2
-old
+new
 ctx 3
";
    let outcome = c.compress(small, &budget);
    assert!(
        matches!(outcome, CompressOutcome::Unchanged),
        "无可折叠上下文时应 Unchanged"
    );
}
```

### ② 跑测试看失败

```bash
cd /Users/dfbb/Sites/skycode/codez/codex-rs
cargo test -p codez-llm-compress --test diff_test
```

预期:编译失败(`compress::diff` 模块尚不存在),即"红"。

### ③ 写 `diff.rs` 完整实现

创建 `zmod/llm-compress/src/compress/diff.rs`:

```rust
//! DiffCompressor:识别 unified diff,保留全部变更行与结构头,
//! 仅折叠 hunk 内多余的上下文行。
//!
//! 折叠规则(spec §6):
//! - 变更行(`+`/`-` 开头,但非文件头 `+++`/`---`)、hunk 头(`@@`)、
//!   文件头(`diff --git`/`index`/`--- `/`+++ `)全部保留。
//! - hunk 内上下文行(以单空格开头)仅保留紧邻变更行前后各 `context_lines` 行,
//!   中间折叠为一行裸占位 `[llm-compress: 略 N 行上下文]`。

use crate::router::{Budget, CompressOutcome, Compressor};

/// unified diff 压缩器。
pub struct DiffCompressor;

impl DiffCompressor {
    /// 判断一行是否为 hunk 头 `@@ -a,b +c,d @@`(b、d 可省略)。
    ///
    /// 不引入正则依赖,手写解析等价于 `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`。
    fn is_hunk_header(line: &str) -> bool {
        // 必须以 "@@ -" 起始。
        let rest = match line.strip_prefix("@@ -") {
            Some(r) => r,
            None => return false,
        };
        // 解析 "\d+(,\d+)? " —— 旧区间。
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // 紧跟一个空格。
        let rest = match rest.strip_prefix(' ') {
            Some(r) => r,
            None => return false,
        };
        // 紧跟 "+"。
        let rest = match rest.strip_prefix('+') {
            Some(r) => r,
            None => return false,
        };
        // 解析 "\d+(,\d+)?" —— 新区间。
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // 紧跟 " @@"。
        rest.starts_with(" @@")
    }

    /// 消费一个 `\d+(,\d+)?` 形式的区间,返回剩余切片;失败返回 None。
    fn consume_range(s: &str) -> Option<&str> {
        // 至少一位数字。
        let first_len = s.bytes().take_while(|b| b.is_ascii_digit()).count();
        if first_len == 0 {
            return None;
        }
        let s = &s[first_len..];
        // 可选 ",\d+"。
        if let Some(after_comma) = s.strip_prefix(',') {
            let len = after_comma.bytes().take_while(|b| b.is_ascii_digit()).count();
            if len == 0 {
                return None;
            }
            Some(&after_comma[len..])
        } else {
            Some(s)
        }
    }

    /// 是否为文件头(整体保留,不参与上下文折叠)。
    fn is_file_header(line: &str) -> bool {
        line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
    }

    /// 是否为变更行(`+`/`-` 开头,但排除文件头 `+++ `/`--- `)。
    fn is_change_line(line: &str) -> bool {
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            return false;
        }
        line.starts_with('+') || line.starts_with('-')
    }

    /// 是否为上下文行(hunk 内以单空格开头的未变更行)。
    fn is_context_line(line: &str) -> bool {
        line.starts_with(' ')
    }
}

impl Compressor for DiffCompressor {
    fn name(&self) -> &'static str {
        "diff"
    }

    fn detect(&self, text: &str) -> bool {
        let mut has_minus_header = false;
        let mut has_plus_header = false;
        for line in text.lines() {
            if Self::is_hunk_header(line) {
                return true;
            }
            if line.starts_with("diff --git ") {
                return true;
            }
            if line.starts_with("--- ") {
                has_minus_header = true;
            }
            if line.starts_with("+++ ") {
                has_plus_header = true;
            }
        }
        has_minus_header && has_plus_header
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let ctx = budget.cfg.diff.context_lines;

        // 第一步:把所有行收集为 Vec,便于做"前后窗口"判定。
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.len();

        // 标记每一行是否为上下文行。
        let is_ctx: Vec<bool> = lines.iter().map(|l| Self::is_context_line(l)).collect();
        // 标记每一行是否为"锚点"(变更行 / hunk 头 / 文件头) —— 上下文需围绕变更行保留。
        // 折叠窗口的依据是"与最近变更行的距离",因此先标记变更行位置。
        let is_change: Vec<bool> = lines.iter().map(|l| Self::is_change_line(l)).collect();

        // 计算每一上下文行到"最近变更行"的距离(只在同一连续上下文段内有意义,
        // 但用全局最近变更行距离即可正确实现"紧邻变更行前后各 ctx 行"的语义:
        // 上下文行若其上方 ctx 行内或下方 ctx 行内存在变更行,则保留)。
        let mut keep: Vec<bool> = vec![true; n];

        for i in 0..n {
            if !is_ctx[i] {
                // 非上下文行(变更行 / hunk 头 / 文件头 / 其它)一律保留。
                continue;
            }
            // 上下文行:检查上方 ctx 行内是否有变更行。
            let mut near_change = false;
            // 向上看 ctx 行。
            let lo = i.saturating_sub(ctx);
            for j in lo..i {
                if is_change[j] {
                    near_change = true;
                    break;
                }
            }
            // 向下看 ctx 行。
            if !near_change {
                let hi = (i + ctx + 1).min(n);
                for j in (i + 1)..hi {
                    if is_change[j] {
                        near_change = true;
                        break;
                    }
                }
            }
            keep[i] = near_change;
        }

        // 第二步:按顺序输出,遇到连续被丢弃的上下文段折叠为一行占位。
        let mut out_lines: Vec<String> = Vec::with_capacity(n);
        let mut i = 0;
        let mut any_folded = false;
        while i < n {
            if keep[i] {
                out_lines.push(lines[i].to_string());
                i += 1;
            } else {
                // 收集一段连续的被丢弃上下文行。
                let start = i;
                while i < n && !keep[i] {
                    i += 1;
                }
                let folded = i - start;
                out_lines.push(format!("[llm-compress: 略 {folded} 行上下文]"));
                any_folded = true;
            }
        }

        if !any_folded {
            return CompressOutcome::Unchanged;
        }

        // 重建文本:保留原文末尾换行习惯。原文以 '\n' 结尾则补一个。
        let mut result = out_lines.join("\n");
        if text.ends_with('\n') {
            result.push('\n');
        }

        // 若折叠后体积未减小(占位反而更长),视为 Unchanged。
        if result.len() >= text.len() {
            return CompressOutcome::Unchanged;
        }

        let saved_bytes = text.len() - result.len();
        CompressOutcome::Compressed {
            text: result,
            saved_bytes,
        }
    }
}
```

### ④ 注册模块

在 `zmod/llm-compress/src/compress/mod.rs` 中新增:

```rust
pub mod diff;
```

(置于现有 `pub mod ...;` 声明区,与既有模块同级。)

### ⑤ 跑测试看通过

```bash
cd /Users/dfbb/Sites/skycode/codez/codex-rs
cargo test -p codez-llm-compress --test diff_test
```

预期:全部用例通过,即"绿"。

### ⑥ 提交

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-05-diff.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): DiffCompressor (keep changes + bounded context)"
```

---

## 测试覆盖清单

| 用例 | 验证点 |
| --- | --- |
| `detect_true_for_real_git_diff` | detect 对真实 git diff 返回 `true` |
| `detect_true_for_bare_hunk_header` | detect 对裸 hunk 头返回 `true` |
| `detect_false_for_plain_text` | detect 对普通文本返回 `false` |
| `compress_folds_large_context_and_keeps_changes` | 大段上下文折叠 + 变更行/头部全保留 + 占位标记存在 + `saved_bytes` 为字节差 |
| `compress_small_diff_unchanged` | 上下文本就少 → `Unchanged` |

## 实现要点说明

- **不引入正则依赖**:`is_hunk_header` 手写解析等价于 `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`,避免为单一匹配引入 `regex` crate。
- **折叠语义**:"紧邻变更行前后各 `context_lines` 行"以"该上下文行的上/下 `ctx` 行窗口内是否存在变更行"判定,等价且简洁;连续被丢弃的上下文段折叠为单行占位,`N` 为该段行数。
- **末尾换行**:输出保留原文是否以 `\n` 结尾的习惯,保证 `saved_bytes` 与 `text.len() - result.len()` 严格一致。
- **保守回退**:若折叠后体积未减小则返回 `Unchanged`,符合 `saved_bytes > 0 → Compressed` 的规格。
