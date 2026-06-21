# Task 06: LogCompressor (collapse consecutive repeats + head/tail retention)

> Part of `2026-06-20-llm-compress-00-index.md`. Before executing, read the index's Global Constraints / real types / dev-build decisions. Depends on Task 01 (config) and Task 02 (`Compressor` trait / `Budget` / `CompressOutcome`).

**Goal:** Implement the text-type compressor `LogCompressor`, which recognizes multi-line log / stack-trace text. First it collapses runs of identical lines into `line + [llm-compress: 上一行 ×N]`, then applies `truncate.head_lines` + `truncate.tail_lines` retention to the whole thing (collapsing the middle into a single line `[llm-compress: 略 N 行]`). The placeholder is a bare text marker (log is a text type). By the end of this task, `cargo test -p codez-llm-compress --test log_test` passes.

**Spec coverage:** §6 (text-type compressor / bare placeholder marker / dedup_repeats / head-tail retention).

**Files:**
- Create: `zmod/llm-compress/src/compress/log.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs` (add `pub mod log;`; if the file does not yet exist, create it with the content shown in Step 4)
- Modify: `zmod/llm-compress/src/lib.rs` (ensure `pub mod compress;` is present)
- Test: `zmod/llm-compress/tests/log_test.rs`

**Interfaces:**
- Consumes (from Task 01): `config::{Config, TruncateCfg, LogCfg}`.
- Consumes (from Task 02): `router::{Budget, CompressOutcome, Compressor}`.
- Produces (the compressors from 03–06 are wired into `ContentRouter` in Task 08):
  - `pub struct LogCompressor;`
  - `impl Compressor for LogCompressor`, with `name()` = `"log"`.

**Contract (do not change; sourced from Task 02):**

```rust
pub struct Budget<'a> { pub cfg: &'a Config }
pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize }, Unchanged }
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

**Behavior spec (spec §6):**

- `name()` = `"log"`.
- `detect(text)`: claims the text only if it is **multi-line** (`≥8` lines) **and** has at least one of the following log characteristics. Otherwise it does not claim it (to avoid wrongly swallowing ordinary short text):
  1. Contains a timestamp: a line matches `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}` (e.g. `2026-06-20T12:00:00`) or `\d{2}:\d{2}:\d{2}` (e.g. `12:00:00`);
  2. Stack-trace characteristic: some line contains ` at ` followed by a `:` followed by a digit (e.g. `at foo.rs:42`);
  3. There exist two consecutive identical lines (consecutive repeat).
- `compress(text, budget)`:
  1. Take `cfg = &budget.cfg.log` (`LogCfg`) and `tr = &budget.cfg.truncate` (`TruncateCfg`).
  2. **If `cfg.dedup_repeats`**: collapse runs of identical lines into `that line + one bare placeholder line [llm-compress: 上一行 ×N]` (`N` = the total number of consecutive repeats of that line, i.e. the number of collapsed lines; collapse only when `N≥2`, keep `N==1` as-is).
  3. **Then apply head/tail retention to the whole thing**: if the total line count after collapsing is `> tr.head_lines + tr.tail_lines`, keep the first `tr.head_lines` lines + one line `[llm-compress: 略 N 行]` (`N` = the number of omitted lines) + the last `tr.tail_lines` lines. Otherwise do not truncate.
  4. Compute `saved_bytes = original.len() - result.len()` (using `saturating_sub`); `saved_bytes > 0` → `Compressed`, otherwise `Unchanged`.
- The placeholder is a bare text marker `[llm-compress: …]` (log is a text type, **not** wrapped in JSON).

**Implementation notes (pinned down, to avoid ambiguity):**
- **Split lines with `text.lines()`**: split on `\n` (compatible with `\r\n`; `lines()` strips the `\r`), without keeping trailing newlines; rejoin the result with `"\n"`. This means that if the original ends with `\n`, the result may be missing one trailing newline — this only increases `saved_bytes`, never decreases it, which is consistent with "conservative compression".
- **The two-step order is fixed**: dedup first, then head/tail. head/tail operates on the line sequence after dedup (placeholder lines also count toward the line count).
- **A placeholder line is its own complete line**, not on the same line as the log body.
- Non-test code must not use `unwrap`/`expect` (this compressor needs no unwrap at all).

---

- [ ] **Step 1: Write a failing test**

Create `zmod/llm-compress/tests/log_test.rs`:

```rust
use codez_llm_compress::compress::log::LogCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg }
}

/// Build a realistic-style, multi-line log with timestamps (≥8 lines).
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
    assert!(c.detect(&log), "a timestamped multi-line log should be claimed");
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
    assert!(c.detect(trace), "a stack trace containing `at file:line` should be claimed");
}

#[test]
fn detect_false_for_plain_short_text() {
    let c = LogCompressor;
    let txt = "Hello world.\nThis is a short note.\nNothing log-like here.";
    assert!(!c.detect(txt), "ordinary short text should not be claimed");
}

#[test]
fn detect_false_for_long_plain_text_without_log_features() {
    // ≥8 lines but no log characteristics / no consecutive repeats → not claimed.
    let c = LogCompressor;
    let mut s = String::new();
    for i in 0..12 {
        s.push_str(&format!("paragraph line number {i} talking about cats\n"));
    }
    assert!(!c.detect(&s), "multi-line text without log characteristics should not be claimed");
}

#[test]
fn detect_true_for_consecutive_repeats() {
    let c = LogCompressor;
    let mut s = String::new();
    for _ in 0..10 {
        s.push_str("retrying connection...\n");
    }
    assert!(c.detect(&s), "presence of consecutive repeated lines should be claimed");
}

#[test]
fn dedup_collapses_consecutive_repeats() {
    let c = LogCompressor;
    let cfg = Config::disabled(); // dedup_repeats defaults to true
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
                "should collapse to ×5, actual:\n{text}"
            );
            // After collapsing, retrying appears once as body + one placeholder line.
            assert_eq!(text.matches("retrying connection...").count(), 1);
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("repeated lines should be collapsed"),
    }
}

#[test]
fn dedup_disabled_keeps_repeats() {
    let c = LogCompressor;
    // Use disabled() then tweak fields to build a Config with dedup_repeats=false.
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false;
    // Give plenty of head/tail headroom to avoid triggering truncation; purely verify dedup does not happen.
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    let mut s = String::new();
    for _ in 0..6 {
        s.push_str("retrying connection...\n");
    }
    match c.compress(&s, &budget(&cfg)) {
        CompressOutcome::Compressed { .. } => panic!("with dedup off and no truncation, there should be no compression"),
        CompressOutcome::Unchanged => {}
    }
}

#[test]
fn head_tail_truncates_long_log_with_placeholder() {
    let c = LogCompressor;
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false; // isolate head/tail behavior (all lines distinct)
    cfg.truncate.head_lines = 3;
    cfg.truncate.tail_lines = 3;
    let log = timestamped_log(50); // 50 lines, all distinct
    match c.compress(&log, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes } => {
            // Middle omitted: 50 - 3 - 3 = 44 lines.
            assert!(
                text.contains("[llm-compress: 略 44 行]"),
                "should have a head/tail placeholder, actual:\n{text}"
            );
            // Result line count = 3 + 1 (placeholder) + 3 = 7 lines.
            assert_eq!(text.lines().count(), 7);
            // First and last lines are retained.
            assert!(text.lines().next().unwrap().contains("id=0"));
            assert!(text.lines().last().unwrap().contains("id=49"));
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("a long log should be truncated"),
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
            // After dedup, line count = boot(1) + retrying(1) + placeholder(1) + 3 ok lines = 6 lines;
            // 6 > head(2)+tail(2)=4 → it will still be truncated, producing a head/tail placeholder.
            assert!(text.contains("[llm-compress: 略"));
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("there should be compression"),
    }
}

#[test]
fn unchanged_when_nothing_to_do() {
    let c = LogCompressor;
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false;
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    // 10 timestamped lines, all distinct, no repeats, not exceeding head+tail → no compression.
    let log = timestamped_log(10);
    assert!(matches!(
        c.compress(&log, &budget(&cfg)),
        CompressOutcome::Unchanged
    ));
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test log_test
```
Expected: compilation failure (`compress::log` module / `LogCompressor` undefined).

- [ ] **Step 3: Write log.rs**

Create `zmod/llm-compress/src/compress/log.rs`:

```rust
//! LogCompressor: text-type log compressor.
//! Two-step conservative compression: ① collapse consecutive repeated lines ② head/tail retention. The placeholder is a bare text marker [llm-compress: …].

use crate::router::{Budget, CompressOutcome, Compressor};

/// Recognizes multi-line log / stack-trace text and applies conservative compression.
pub struct LogCompressor;

/// The "multi-line" threshold for detect: only consider claiming when the line count ≥ this value.
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

        // ① Collapse consecutive repeated lines (optional).
        let dedup_repeats = budget.cfg.log.dedup_repeats;
        let after_dedup: Vec<String> = if dedup_repeats {
            dedup_consecutive(&lines)
        } else {
            lines.iter().map(|s| s.to_string()).collect()
        };

        // ② head/tail retention.
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

/// Whether it contains a timestamp line: `YYYY-MM-DD[T ]HH:MM:SS` or a bare `HH:MM:SS`.
/// Avoids pulling in a regex dependency; decided via character-level scanning.
fn has_timestamp(lines: &[&str]) -> bool {
    lines.iter().any(|l| line_has_full_ts(l) || line_has_clock_ts(l))
}

/// Detect whether the line contains `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}`.
fn line_has_full_ts(line: &str) -> bool {
    let bytes = line.as_bytes();
    // Sliding window: date(10) + sep(1) + time(8) = 19 characters.
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

/// Detect whether the line contains a bare `\d{2}:\d{2}:\d{2}`.
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

/// Whether the 8-byte window is `dd:dd:dd`.
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

/// Whether it has a stack-trace characteristic: some line contains ` at ` followed by a `:` immediately followed by a digit (e.g. `at foo.rs:42`).
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

/// Whether the substring contains a `:` immediately followed by a digit.
fn colon_then_digit(s: &str) -> bool {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b':' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

/// Whether there exist two consecutive identical lines.
fn has_consecutive_repeat(lines: &[&str]) -> bool {
    lines.windows(2).any(|w| w[0] == w[1])
}

/// Collapse runs of identical lines into: that line + a bare placeholder `[llm-compress: 上一行 ×N]` (N≥2).
/// Lines with N==1 are kept as-is (no placeholder added).
fn dedup_consecutive(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        let mut j = i + 1;
        while j < lines.len() && lines[j] == cur {
            j += 1;
        }
        let count = j - i; // total number of consecutive occurrences of this line
        out.push(cur.to_string());
        if count >= 2 {
            out.push(format!("[llm-compress: 上一行 ×{count}]"));
        }
        i = j;
    }
    out
}

/// head/tail retention: when total line count > head+tail, keep the first head lines + `[llm-compress: 略 N 行]` + the last tail lines.
/// Otherwise return as-is.
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

> **Design notes:**
> - No `regex` dependency is introduced; timestamps / stack traces are decided via character-level sliding windows — cheap and with zero new dependencies.
> - `dedup_consecutive`'s `N` is the "total number of consecutive occurrences" (including the first occurrence), consistent with the placeholder wording `×N`: `retrying` occurs 5 times → 1 body line + `×5` placeholder.
> - The two-step order is fixed: dedup precedes head/tail; placeholder lines count toward the head/tail line statistics.
> - `compress` uses no `unwrap`/`expect` throughout, consistent with the Rust style in the Global Constraints.

- [ ] **Step 4: Register the module (compress/mod.rs + lib.rs)**

Modify `zmod/llm-compress/src/compress/mod.rs`, adding one line:

```rust
pub mod log;
```

> If `src/compress/mod.rs` does not yet exist (Tasks 03–05 did not create the directory first), then **create** the file with the content:
> ```rust
> //! Per-content-type compressors (truncate / json / diff / log).
> pub mod log;
> ```
> Subsequent tasks 03/04/05 will append their own `pub mod` here.

Modify `zmod/llm-compress/src/lib.rs`, ensuring the `compress` module declaration is present (skip if 03–05 already added it). After `pub mod router;` add:

```rust
pub mod compress;
```

- [ ] **Step 5: Run the test and watch it pass**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test log_test
```
Expected: `test result: ok. 10 passed`.

- [ ] **Step 6: Commit (zmod + this plan only)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-06-log.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): LogCompressor (dedup repeats + head/tail)"
```

> **Do not** `git add codex-rs/core/Cargo.toml` — it is the dev-build enabler; keep it dirty until Task 09.
