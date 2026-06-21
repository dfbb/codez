# Task 04 — preprocess.rs (incl. blob_fold) + protect.rs

> Belongs to `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §4.5 / §4.6. Depends on Task 01 (config subtables). Can run in parallel with 02/03.

**Goal:** Implement the rtk-style general preprocessing layer `preprocess::run` (strip_progress / blob_fold / collapse_blank / truncate_line_bytes / dedup_consecutive, returning whether substantive content was removed) and the error-output protection gate `protect::should_protect`. This is the sole location where base64/blob folding is performed (spec §4.6 #6).

## Files
- Create: `zmod/llm-compress/src/preprocess.rs`
- Create: `zmod/llm-compress/src/protect.rs`
- Modify: `zmod/llm-compress/src/lib.rs` (add `pub mod preprocess; pub mod protect;`)
- Test: `zmod/llm-compress/tests/preprocess_test.rs`, `zmod/llm-compress/tests/protect_test.rs`

**Interfaces:**
- Consumes: Task 01's `config::{PreprocessCfg, ProtectCfg, Config}`, `command::CommandHint`.
- Produces:
  - `pub fn preprocess::run(text: &str, cfg: &PreprocessCfg) -> (String, bool)` (processed text, whether substantive content was removed)
  - `pub fn protect::should_protect(text: &str, cmd: Option<&CommandHint>, cfg: &Config) -> bool`

---

- [ ] **Step 1: Write failing protect tests**

Create `zmod/llm-compress/tests/protect_test.rs`:

```rust
use codez_llm_compress::config::Config;
use codez_llm_compress::protect::should_protect;

#[test]
fn small_error_output_is_protected() {
    let cfg = Config::disabled(); // protect.error_max_bytes defaults to 8192
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

- [ ] **Step 2: Run to confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test protect_test 2>&1 | head`
Expected: FAIL (`protect` module does not exist)

- [ ] **Step 3: Implement protect.rs**

Create `zmod/llm-compress/src/protect.rs`:

```rust
//! Error-output protection gate (spec §4.5/C2): error/exception below the threshold → leave the whole block uncompressed.
//! Highest priority: decided before any preprocessing; once it matches, the whole block is preserved byte-for-byte (spec §2/§4.5 #7).

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

/// Text contains a strong error marker and len < cfg.protect.error_max_bytes → true (whole block uncompressed).
/// error_max_bytes==0 → protection disabled, always false.
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
    // Command-hint assist: raise the protection bias when test-runner output contains fail/error.
    let test_failed = cmd.is_some_and(|c| c.is_test_runner())
        && (lower.contains("fail") || lower.contains("error"));
    has_error || test_failed
}
```

Add `pub mod protect;` to `lib.rs`.

- [ ] **Step 4: Run protect tests to pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test protect_test`
Expected: PASS (4 tests)

- [ ] **Step 5: Write failing preprocess tests**

Create `zmod/llm-compress/tests/preprocess_test.rs`:

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
    assert!(lossy, "removing progress bars → lossy=true");
}

#[test]
fn collapse_blank_is_not_lossy() {
    let input = "a\n\n\n\nb";
    let (out, lossy) = run(input, &cfg());
    // consecutive blank lines collapse into one
    assert_eq!(out, "a\n\nb");
    assert!(!lossy, "blank-line collapse is a formatting reshape → lossy=false");
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
    let input = "中文字符串很长很长很长很长".to_string(); // multi-byte
    let (out, lossy) = run(&input, &c);
    assert!(lossy);
    // the result is still valid UTF-8 (being a valid String already proves it)
    assert!(out.len() <= input.len());
}

#[test]
fn dedup_consecutive_not_lossy_and_skips_marker_lines() {
    let input = "x\nx\nx\n[llm-compress: 已有占位]\n[llm-compress: 已有占位]";
    let (out, lossy) = run(input, &cfg());
    assert!(!lossy, "folding consecutive duplicates is a formatting reshape");
    assert!(out.contains("[llm-compress: 上一行 ×3]"));
    // lines already starting with [llm-compress: are excluded from folding; both lines are kept verbatim
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

- [ ] **Step 6: Run to confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test preprocess_test 2>&1 | head`
Expected: FAIL (`preprocess` module does not exist)

- [ ] **Step 7: Implement preprocess.rs (skeleton + run + strip_progress + blob_fold)**

Create `zmod/llm-compress/src/preprocess.rs`; start with the top down through blob_fold:

```rust
//! rtk-style general preprocessing layer (spec §4.6/D1). Returns (processed text, whether substantive content was removed).
//! Order: strip_progress → blob_fold → collapse_blank → truncate_line_bytes → dedup_consecutive.
//! Content-removing stages (strip_progress/blob_fold/truncate_line_bytes) set lossy=true; formatting-reshape stages do not.
//! Sole location for base64/blob folding (#6); Truncate no longer folds.

use crate::config::PreprocessCfg;

const MARKER_PREFIX: &str = "[llm-compress: ";

/// Main entry: run each stage in order. Returns (text, whether substantive content was removed).
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
        s = collapse_blank(&s); // formatting reshape, does not set lossy
    }
    if cfg.truncate_line_bytes > 0 {
        let (ns, changed) = truncate_lines(&s, cfg.truncate_line_bytes);
        s = ns;
        lossy |= changed;
    }
    if cfg.dedup_consecutive {
        s = dedup_consecutive(&s); // formatting reshape, does not set lossy
    }
    (s, lossy)
}

/// Remove progress-bar/download lines (content removal). Returns (text, whether any line was removed).
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
    // contains carriage-return overwrite (\r) or a percentage progress indicator
    if line.contains('\r') {
        return true;
    }
    // shapes like " 45%" / "[####    ] 80%"
    let has_pct = t.split_whitespace().any(|w| w.ends_with('%') && w.trim_end_matches('%').parse::<f64>().is_ok());
    has_pct && (t.contains('[') || t.contains('#') || t.contains('='))
}

/// Fold overly long base64/data-uri segments (content removal, #6 sole location). Returns (text, whether anything was folded).
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

/// Decide whether a line looks like base64/data-uri: a data: prefix, or a long run whose character set is limited to the base64 alphabet.
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

- [ ] **Step 8: Continue preprocess.rs (collapse_blank + truncate_lines + dedup_consecutive)**

Then append to the end of `preprocess.rs`:

```rust
/// Collapse consecutive blank lines into a single blank line (formatting reshape, does not remove substantive content).
fn collapse_blank(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in text.split('\n') {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue; // skip the redundant blank line
        }
        out.push(line);
        prev_blank = blank;
    }
    out.join("\n")
}

/// Truncate overly long single lines by byte count (UTF-8 boundary safe, content removal). Returns (text, whether anything was truncated).
fn truncate_lines(text: &str, max_bytes: usize) -> (String, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut truncated = false;
    for line in text.split('\n') {
        if line.len() > max_bytes {
            // cut at the largest character boundary ≤ max_bytes
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

/// Fold consecutive identical lines into line + [llm-compress: 上一行 ×N] (formatting reshape, no content removed).
/// #6: lines that themselves start with [llm-compress: are excluded from folding (kept verbatim) to avoid placeholder confusion.
fn dedup_consecutive(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        if cur.starts_with(MARKER_PREFIX) {
            out.push(cur.to_string()); // placeholder lines are not folded
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

Add `pub mod preprocess;` to `lib.rs`.

- [ ] **Step 9: Run preprocess tests to pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test preprocess_test`
Expected: PASS (6 tests)

> If the `dedup_consecutive` test fails due to a placeholder-line count discrepancy, double-check: the three lines `x\nx\nx` → `x` + `[llm-compress: 上一行 ×3]`; the two placeholder lines are each kept verbatim (the count lines are pushed early by `starts_with(MARKER_PREFIX)` and never enter folding).

- [ ] **Step 10: clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/preprocess.rs zmod/llm-compress/src/protect.rs \
  zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/preprocess_test.rs zmod/llm-compress/tests/protect_test.rs
git commit -m "feat(llm-compress-v2): Task04 preprocess.rs(含 blob_fold)+ protect.rs"
```


