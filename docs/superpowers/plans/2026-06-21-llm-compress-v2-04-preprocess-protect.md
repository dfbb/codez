# Task 04 — preprocess.rs(含 blob_fold)+ protect.rs

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §4.5 / §4.6。依赖 Task 01(config 子表)。可与 02/03 并行。

**Goal:** 实现 rtk 风格通用预处理层 `preprocess::run`(strip_progress / blob_fold / collapse_blank / truncate_line_bytes / dedup_consecutive,返回是否删了实质内容)与错误输出保护门 `protect::should_protect`。base64/blob 折叠唯一执行位置在此(spec §4.6 #6)。

## Files
- Create: `zmod/llm-compress/src/preprocess.rs`
- Create: `zmod/llm-compress/src/protect.rs`
- Modify: `zmod/llm-compress/src/lib.rs`(加 `pub mod preprocess; pub mod protect;`)
- Test: `zmod/llm-compress/tests/preprocess_test.rs`、`zmod/llm-compress/tests/protect_test.rs`

**Interfaces:**
- Consumes: Task 01 的 `config::{PreprocessCfg, ProtectCfg, Config}`、`command::CommandHint`。
- Produces:
  - `pub fn preprocess::run(text: &str, cfg: &PreprocessCfg) -> (String, bool)`(处理后文本, 是否删了实质内容)
  - `pub fn protect::should_protect(text: &str, cmd: Option<&CommandHint>, cfg: &Config) -> bool`

---

- [ ] **Step 1: 写 protect 失败测试**

创建 `zmod/llm-compress/tests/protect_test.rs`:

```rust
use codez_llm_compress::config::Config;
use codez_llm_compress::protect::should_protect;

#[test]
fn small_error_output_is_protected() {
    let cfg = Config::disabled(); // protect.error_max_bytes 默认 8192
    let text = "Traceback (most recent call last):\n  File x\nValueError: boom";
    assert!(should_protect(text, None, &cfg));
}

#[test]
fn large_error_output_not_protected() {
    let cfg = Config::disabled();
    let big = format!("error: x\n{}", "padding line\n".repeat(2000)); // > 8192 bytes
    assert!(!should_protect(&big, None, &cfg));
}

#[test]
fn non_error_output_not_protected() {
    let cfg = Config::disabled();
    assert!(!should_protect("just normal output\nline two", None, &cfg));
}

#[test]
fn zero_threshold_disables_protection() {
    let mut cfg = Config::disabled();
    cfg.protect.error_max_bytes = 0;
    assert!(!should_protect("error: boom", None, &cfg));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test protect_test 2>&1 | head`
Expected: FAIL(`protect` 模块不存在)

- [ ] **Step 3: 实现 protect.rs**

创建 `zmod/llm-compress/src/protect.rs`:

```rust
//! 错误输出保护门(spec §4.5/C2):错误/异常且 < 阈值 → 整段不压。
//! 优先级最高:在所有预处理之前判定,命中即整段逐字节不变(spec §2/§4.5 #7)。

use crate::command::CommandHint;
use crate::config::Config;

const STRONG_ERROR_MARKERS: &[&str] = &[
    "traceback (most recent call last)",
    "panic",
    "error:",
    "exception",
    "fatal:",
    "segmentation fault",
];

/// 文本含强错误指示符且 len < cfg.protect.error_max_bytes → true(整段不压)。
/// error_max_bytes==0 → 关闭保护,恒 false。
pub fn should_protect(text: &str, cmd: Option<&CommandHint>, cfg: &Config) -> bool {
    let limit = cfg.protect.error_max_bytes;
    if limit == 0 {
        return false;
    }
    if text.len() >= limit {
        return false;
    }
    let lower = text.to_lowercase();
    let has_error = STRONG_ERROR_MARKERS.iter().any(|m| lower.contains(m));
    // 命令提示辅助:test runner 输出含 fail/error 时提高保护倾向。
    let test_failed = cmd.is_some_and(|c| c.is_test_runner())
        && (lower.contains("fail") || lower.contains("error"));
    has_error || test_failed
}
```

在 `lib.rs` 加 `pub mod protect;`。

- [ ] **Step 4: 运行 protect 测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test protect_test`
Expected: PASS(4 个)

- [ ] **Step 5: 写 preprocess 失败测试**

创建 `zmod/llm-compress/tests/preprocess_test.rs`:

```rust
use codez_llm_compress::config::PreprocessCfg;
use codez_llm_compress::preprocess::run;

fn cfg() -> PreprocessCfg {
    PreprocessCfg::default()
}

#[test]
fn strip_progress_removes_download_lines_and_marks_lossy() {
    let input = "Downloading foo\nreal line\nDownloading bar\nanother";
    let (out, lossy) = run(input, &cfg());
    assert!(!out.contains("Downloading"));
    assert!(out.contains("real line"));
    assert!(lossy, "删进度条 → lossy=true");
}

#[test]
fn collapse_blank_is_not_lossy() {
    let input = "a\n\n\n\nb";
    let (out, lossy) = run(input, &cfg());
    // 连续空行归一为一个
    assert_eq!(out, "a\n\nb");
    assert!(!lossy, "空行归一是格式重构 → lossy=false");
}

#[test]
fn blob_fold_replaces_long_base64_and_marks_lossy() {
    let blob = "A".repeat(400); // > blob_min_bytes 256
    let input = format!("prefix\n{blob}\nsuffix");
    let (out, lossy) = run(&input, &cfg());
    assert!(!out.contains(&blob));
    assert!(out.contains("[llm-compress: base64"));
    assert!(lossy);
}

#[test]
fn truncate_line_bytes_marks_lossy_utf8_safe() {
    let mut c = cfg();
    c.truncate_line_bytes = 10;
    let input = "中文字符串很长很长很长很长".to_string(); // 多字节
    let (out, lossy) = run(&input, &c);
    assert!(lossy);
    // 产物仍是合法 UTF-8(能正常作为 String 存在即合法)
    assert!(out.len() <= input.len());
}

#[test]
fn dedup_consecutive_not_lossy_and_skips_marker_lines() {
    let input = "x\nx\nx\n[llm-compress: 已有占位]\n[llm-compress: 已有占位]";
    let (out, lossy) = run(input, &cfg());
    assert!(!lossy, "连续重复折叠是格式重构");
    assert!(out.contains("[llm-compress: 上一行 ×3]"));
    // 原文已含 [llm-compress: 前缀的行不参与折叠,原样保留两行
    assert_eq!(out.matches("[llm-compress: 已有占位]").count(), 2);
}

#[test]
fn all_disabled_returns_unchanged() {
    let c = PreprocessCfg { strip_progress: false, collapse_blank: false, truncate_line_bytes: 0, dedup_consecutive: false, blob_min_bytes: 256 };
    let input = "Downloading x\n\n\ny";
    let (out, lossy) = run(input, &c);
    assert_eq!(out, input);
    assert!(!lossy);
}
```

- [ ] **Step 6: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test preprocess_test 2>&1 | head`
Expected: FAIL(`preprocess` 模块不存在)

- [ ] **Step 7: 实现 preprocess.rs(骨架 + run + strip_progress + blob_fold)**

创建 `zmod/llm-compress/src/preprocess.rs`,先写顶部到 blob_fold:

```rust
//! rtk 风格通用预处理层(spec §4.6/D1)。返回 (处理后文本, 是否删了实质内容)。
//! 顺序:strip_progress → blob_fold → collapse_blank → truncate_line_bytes → dedup_consecutive。
//! 删内容段(strip_progress/blob_fold/truncate_line_bytes)置 lossy=true;格式重构段不置。
//! base64/blob 折叠唯一执行位置(#6),Truncate 不再折叠。

use crate::config::PreprocessCfg;

const MARKER_PREFIX: &str = "[llm-compress: ";

/// 主入口:按顺序跑各段。返回 (文本, 是否删实质内容)。
pub fn run(text: &str, cfg: &PreprocessCfg) -> (String, bool) {
    let mut s = text.to_string();
    let mut lossy = false;

    if cfg.strip_progress {
        let (ns, changed) = strip_progress(&s);
        s = ns;
        lossy |= changed;
    }
    if cfg.blob_min_bytes > 0 {
        let (ns, changed) = blob_fold(&s, cfg.blob_min_bytes);
        s = ns;
        lossy |= changed;
    }
    if cfg.collapse_blank {
        s = collapse_blank(&s); // 格式重构,不置 lossy
    }
    if cfg.truncate_line_bytes > 0 {
        let (ns, changed) = truncate_lines(&s, cfg.truncate_line_bytes);
        s = ns;
        lossy |= changed;
    }
    if cfg.dedup_consecutive {
        s = dedup_consecutive(&s); // 格式重构,不置 lossy
    }
    (s, lossy)
}

/// 删进度条/下载行(删内容)。返回 (文本, 是否删了行)。
fn strip_progress(text: &str) -> (String, bool) {
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = false;
    for line in text.split('\n') {
        if is_progress_line(line) {
            removed = true;
        } else {
            kept.push(line);
        }
    }
    (kept.join("\n"), removed)
}

fn is_progress_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("Downloading") || t.starts_with("Downloaded") || t.starts_with("Fetching") {
        return true;
    }
    // 含回车覆写(\r)或百分比进度
    if line.contains('\r') {
        return true;
    }
    // 形如 " 45%" / "[####    ] 80%"
    let has_pct = t.split_whitespace().any(|w| w.ends_with('%') && w.trim_end_matches('%').parse::<f64>().is_ok());
    has_pct && (t.contains('[') || t.contains('#') || t.contains('='))
}

/// 折叠超长 base64/data-uri 段(删内容,#6 唯一位置)。返回 (文本, 是否折叠)。
fn blob_fold(text: &str, min_bytes: usize) -> (String, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut folded = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.len() >= min_bytes && is_blobish(trimmed) {
            out.push(format!("[llm-compress: base64 {} 字节]", trimmed.len()));
            folded = true;
        } else {
            out.push(line.to_string());
        }
    }
    (out.join("\n"), folded)
}

/// 判定一行是否像 base64/data-uri:data: 前缀,或长串且字符集限于 base64 字母表。
fn is_blobish(s: &str) -> bool {
    if s.starts_with("data:") {
        return true;
    }
    let body = s.strip_prefix("data:").unwrap_or(s);
    let b64_ratio = body.chars().filter(|c| {
        c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=' || *c == '-' || *c == '_'
    }).count();
    body.len() > 0 && b64_ratio == body.len()
}
```

- [ ] **Step 8: 续写 preprocess.rs(collapse_blank + truncate_lines + dedup_consecutive)**

接着在 `preprocess.rs` 末尾追加:

```rust
/// 连续空行归一为一个空行(格式重构,不删实质内容)。
fn collapse_blank(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in text.split('\n') {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue; // 跳过多余空行
        }
        out.push(line);
        prev_blank = blank;
    }
    out.join("\n")
}

/// 超长单行按字节截断(UTF-8 边界安全,删内容)。返回 (文本, 是否截断)。
fn truncate_lines(text: &str, max_bytes: usize) -> (String, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut truncated = false;
    for line in text.split('\n') {
        if line.len() > max_bytes {
            // 在 ≤ max_bytes 的最大字符边界截断
            let mut cut = 0;
            for (idx, ch) in line.char_indices() {
                let end = idx + ch.len_utf8();
                if end <= max_bytes {
                    cut = end;
                } else {
                    break;
                }
            }
            out.push(format!("{}[llm-compress: 行截断]", &line[..cut]));
            truncated = true;
        } else {
            out.push(line.to_string());
        }
    }
    (out.join("\n"), truncated)
}

/// 连续完全相同行折叠为 行 + [llm-compress: 上一行 ×N](格式重构,不删内容)。
/// #6:本身即 [llm-compress: 前缀的行不参与折叠(原样保留),避免占位混淆。
fn dedup_consecutive(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        if cur.starts_with(MARKER_PREFIX) {
            out.push(cur.to_string()); // 占位行不折叠
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && lines[j] == cur && !lines[j].starts_with(MARKER_PREFIX) {
            j += 1;
        }
        let count = j - i;
        out.push(cur.to_string());
        if count >= 2 {
            out.push(format!("[llm-compress: 上一行 ×{count}]"));
        }
        i = j;
    }
    out.join("\n")
}
```

在 `lib.rs` 加 `pub mod preprocess;`。

- [ ] **Step 9: 运行 preprocess 测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test preprocess_test`
Expected: PASS(6 个)

> 若 `dedup_consecutive` 测试因占位行计数偏差失败,核对:`x\nx\nx` 三行 → `x` + `[llm-compress: 上一行 ×3]`;两行占位各自原样保留(count 行被 `starts_with(MARKER_PREFIX)` 提前 push,不进折叠)。

- [ ] **Step 10: clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/preprocess.rs zmod/llm-compress/src/protect.rs \
  zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/preprocess_test.rs zmod/llm-compress/tests/protect_test.rs
git commit -m "feat(llm-compress-v2): Task04 preprocess.rs(含 blob_fold)+ protect.rs"
```



