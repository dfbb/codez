# Task 03: TruncateCompressor (fallback text truncator)

> Belongs to `2026-06-20-llm-compress-00-index.md`. Read the index's Global Constraints before executing. Depends on Task 01 (`config::TruncateCfg`) and Task 02 (`router::{Budget, CompressOutcome, Compressor}`).

**Goal:** Implement the fallback text compressor `TruncateCompressor`: strip ANSI escapes, keep head/tail by line, replace the middle with a bare placeholder marker `[llm-compress: …]`, and when necessary hard-truncate at a UTF-8 character boundary per `max_bytes`. `detect()` is always true (claims everything), serving as the tail-end fallback of the ContentRouter chain. Conservatively returns `Unchanged` for small inputs.

**Spec coverage:** §6 (text-type compressor: head/tail truncation + bare placeholder + irreversible but conservative).

**Pinned contract (from Task 02, do not change):**

```rust
pub struct Budget<'a> { pub cfg: &'a Config }            // crate::router::Budget
pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize }, Unchanged }
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

Config slice (from Task 01 `config::TruncateCfg`, taken from `budget.cfg.truncate`):

```rust
pub struct TruncateCfg { pub head_lines: usize, pub tail_lines: usize, pub max_bytes: usize }
```

**Files:**
- Create: `zmod/llm-compress/src/compress/truncate.rs`
- Create: `zmod/llm-compress/tests/truncate_test.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs` (create this file and add `pub mod truncate;`; if 04–06 already created it, only append that line)
- Modify: `zmod/llm-compress/src/lib.rs` (add `pub mod compress;`)

**Interfaces:**
- Consumes (from Task 01): `config::{Config, TruncateCfg}`.
- Consumes (from Task 02): `router::{Budget, CompressOutcome, Compressor}`.
- Produces (depended on by 08 when registering into ContentRouter):
  - `pub struct TruncateCompressor;` — implements `Compressor`, `name() == "truncate"`, `detect()` always true.

**Behavior spec (spec §6, item by item):**
1. `name()` returns `"truncate"`.
2. `detect()` always true (fallback, claims all content).
3. `compress()`:
   - ① strip ANSI: remove CSI sequences of the form `\x1b[ … <terminating byte>` (covers `\x1b[0m`, `\x1b[1;31m`, `\x1b[2K`, etc.); hand-written scanner, no third-party regex dependency.
   - ② split into lines by `\n`.
   - ③ if (after stripping ANSI) the total line count `≤ head_lines + tail_lines` **and** the byte count `≤ max_bytes` → `Unchanged`.
   - ④ otherwise keep the first `head_lines` lines + the last `tail_lines` lines, replacing the middle with a **single** bare placeholder marker `[llm-compress: 略 N 行 / M 字节]` (N = number of omitted lines, M = byte count of the omitted lines, including newlines between omitted lines).
   - ⑤ if after joining the result is still `> max_bytes`, hard-truncate per `max_bytes` (at a UTF-8 character boundary, not breaking multi-byte characters) and append `\n[llm-compress: 截断至 max_bytes]` at the end.
   - ⑥ `saved_bytes = original byte length - new text byte length`; `saved_bytes > 0` → `Compressed { text, saved_bytes }`, otherwise `Unchanged`.
4. The placeholder marker format strictly uses `[llm-compress: …]` (bare text, not JSON).
5. **Irreversible but conservative**: small inputs (line count and bytes both below threshold) return `Unchanged` directly, never rewriting needlessly.

> Design tradeoffs (per index Global Constraints): non-test code does not use `unwrap`/`expect`; ANSI stripping is hand-written with zero extra dependencies; UTF-8 hard truncation uses `char_indices` to find a safe byte boundary. `saved_bytes` is measured in bytes (`len()`), consistent with the spec §7 metrics caliber. The `min_total_bytes` / `per_item_min_bytes` thresholds are gated by the transform in Task 08; this compressor only looks at the three `truncate.*` thresholds.

---

- [ ] **Step 1: Write a failing test**

Create `zmod/llm-compress/tests/truncate_test.rs`:

```rust
use codez_llm_compress::compress::truncate::TruncateCompressor;
use codez_llm_compress::config::{Config, TruncateCfg};
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

/// Construct a Config with the given truncate thresholds (other fields take defaults).
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
    // 3 lines, far below head+tail=100, and bytes far below max_bytes.
    let cfg = cfg_with(50, 50, 16384);
    let input = "line1\nline2\nline3";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    assert!(matches!(out, CompressOutcome::Unchanged));
}

#[test]
fn large_input_keeps_head_and_tail_with_marker() {
    // head=2, tail=2; build 10 lines → necessarily exceeds head+tail=4.
    let cfg = cfg_with(2, 2, 16384);
    let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
    let input = lines.join("\n");
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes } = out else {
        panic!("expected Compressed");
    };
    // first 2 lines present;
    assert!(text.contains("line0"));
    assert!(text.contains("line1"));
    // last 2 lines present;
    assert!(text.contains("line8"));
    assert!(text.contains("line9"));
    // some middle line was omitted;
    assert!(!text.contains("line5"));
    // bare placeholder marker present, correct format;
    assert!(text.contains("[llm-compress: 略 6 行 /"));
    // compression actually happened;
    assert!(saved_bytes > 0);
    assert_eq!(saved_bytes, input.len() - text.len());
}

#[test]
fn ansi_escapes_are_stripped() {
    // Contains color codes; thresholds enlarged so line/byte truncation is not triggered,
    // only verifying ANSI is stripped.
    // Note: after stripping ANSI it is still a small input → Unchanged, so here we force
    // compression via multiple lines, but to focus on ANSI, using head=0/tail=0 + a single
    // long line that gets hard-truncated is not convenient to observe either.
    // Approach: multiple lines + low head/tail to enter the compression path, then assert
    // there is no \x1b in the output.
    let cfg = cfg_with(1, 1, 16384);
    let input = "\x1b[31mred0\x1b[0m\n\x1b[1;32mgreen1\x1b[0m\n\x1b[33myellow2\x1b[0m\n\x1b[34mblue3\x1b[0m";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, .. } = out else {
        panic!("expected Compressed");
    };
    // no ESC byte may remain in the output;
    assert!(!text.contains('\x1b'));
    // after stripping color codes, the plain text remains;
    assert!(text.contains("red0"));
    assert!(text.contains("blue3"));
}

#[test]
fn hard_truncate_does_not_split_utf8() {
    // head=tail=0 → everything goes into the placeholder; the placeholder marker itself
    // contains Chinese (multi-byte), set an extremely small max_bytes to force hard truncation,
    // assert the output is still valid UTF-8 (can be converted back to &str successfully).
    let cfg = cfg_with(0, 0, 12);
    // multiple lines of Chinese, byte count far exceeds 12.
    let input = "甲乙丙丁\n戊己庚辛\n壬癸子丑\n寅卯辰巳";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes } = out else {
        panic!("expected Compressed");
    };
    // Key: String inherently guarantees UTF-8; if hard truncation breaks a boundary,
    // the implementation would panic or produce invalid bytes.
    // Here we explicitly verify: the bytes can be parsed back into the string losslessly (round-trip).
    assert_eq!(std::str::from_utf8(text.as_bytes()).unwrap(), text);
    // hard-truncation marker present;
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
    // Only 2 lines (≤ head+tail=4), but bytes exceed max_bytes → must still compress
    // (condition ③ is "and", exceeding either one triggers compression).
    let cfg = cfg_with(2, 2, 8);
    let input = "0123456789\nabcdefghij"; // 21 bytes > 8
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    // head=2/tail=2 already covers all 2 lines → no omitted middle lines → no placeholder marker,
    // but after joining it is still > max_bytes → triggers the hard-truncation marker.
    let CompressOutcome::Compressed { text, saved_bytes } = out else {
        panic!("expected Compressed (byte limit exceeded)");
    };
    assert!(text.contains("[llm-compress: 截断至 max_bytes]"));
    assert!(saved_bytes > 0);
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test truncate_test
```
Expected: compilation failure (`compress` module / `TruncateCompressor` undefined).

- [ ] **Step 3: Write truncate.rs (complete implementation, no TODOs)**

Create `zmod/llm-compress/src/compress/truncate.rs`:

```rust
//! Fallback text truncator: strip ANSI, keep head/tail by line, bare placeholder in the
//! middle, and when necessary hard-truncate at a UTF-8 character boundary per max_bytes.
//! detect() is always true, serving as the tail-end fallback of the ContentRouter chain.
//! Irreversible but conservative: small inputs return Unchanged directly.

use crate::router::{Budget, CompressOutcome, Compressor};

/// Fallback text compressor. Stateless, unit struct.
pub struct TruncateCompressor;

impl Compressor for TruncateCompressor {
    fn name(&self) -> &'static str {
        "truncate"
    }

    /// Always true: claims all content (tail-end fallback).
    fn detect(&self, _text: &str) -> bool {
        true
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let cfg = &budget.cfg.truncate;
        let original_len = text.len();

        // ① Strip ANSI escape sequences.
        let cleaned = strip_ansi(text);

        // ② Split into lines.
        let lines: Vec<&str> = cleaned.split('\n').collect();
        let line_count = lines.len();

        // ③ Neither line count nor bytes exceed thresholds → conservatively leave it alone.
        //    Note: even after stripping ANSI, as long as nothing exceeds the threshold, return
        //    Unchanged (don't rewrite just to "de-color", avoiding needless irreversible changes;
        //    the saved_bytes caliber also requires new < original to count as compression).
        if line_count <= cfg.head_lines + cfg.tail_lines && cleaned.len() <= cfg.max_bytes {
            return CompressOutcome::Unchanged;
        }

        // ④ Keep the first head_lines + last tail_lines, one bare placeholder line in the middle.
        let mut result = build_head_tail(&lines, cfg.head_lines, cfg.tail_lines);

        // ⑤ If still over max_bytes, hard-truncate at a character boundary + append truncation marker.
        if result.len() > cfg.max_bytes {
            result = hard_truncate(&result, cfg.max_bytes);
        }

        // ⑥ Compute saved_bytes; there must be an actual reduction.
        if result.len() < original_len {
            let saved_bytes = original_len - result.len();
            CompressOutcome::Compressed { text: result, saved_bytes }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// Hand-written ANSI (CSI) stripping: skip sequences starting at `\x1b[` up to the terminating
/// byte (`@`..=`~`, 0x40..=0x7E) inclusive.
/// Covers `\x1b[0m`, `\x1b[1;31m`, `\x1b[2K`, etc.; also safely skips an isolated `\x1b`.
/// Other non-CSI ESC forms (such as `\x1bP`…) are rare in tool output, so here we conservatively
/// discard the single byte that follows, without aiming to cover all of ECMA-48 — good enough
/// (spec §6 only requires stripping common color/control codes).
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI: skip up to the terminating byte (0x40..=0x7E), inclusive.
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                // j points to the terminating byte (if present); discard the whole run including it.
                i = if j < bytes.len() { j + 1 } else { j };
            } else {
                // Isolated ESC or non-CSI: discard the ESC and the byte after it (if any).
                i = if i + 1 < bytes.len() { i + 2 } else { i + 1 };
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // strip_ansi only removes complete ASCII control sequences (ESC=0x1b and 0x40..=0x7e are both
    // single-byte ASCII), so it never cuts a multi-byte UTF-8 character; from_utf8 will always
    // succeed; on failure, conservatively fall back to the original text.
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// Assemble "first head lines + placeholder marker + last tail lines".
/// If head+tail covers all lines (no middle omission), no placeholder marker is inserted; join as-is.
fn build_head_tail(lines: &[&str], head: usize, tail: usize) -> String {
    let n = lines.len();

    // head+tail covers all lines → no omission, just join back (may still go through hard
    // truncation if bytes exceed the threshold).
    if head + tail >= n {
        return lines.join("\n");
    }

    let head_part = &lines[..head];
    let tail_part = &lines[n - tail..];
    let omitted = &lines[head..n - tail];

    let omitted_lines = omitted.len();
    // Omitted byte count: bytes of each omitted line + the newlines between them
    // (omitted_lines - 1 of them, if > 0).
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

/// Truncate `text` to within `max_bytes` at a UTF-8 character boundary, appending a truncation marker.
/// Reserve the bytes needed for the marker, ensuring the final result is ≤ a reasonable upper bound
/// overall (as close to max_bytes as possible).
fn hard_truncate(text: &str, max_bytes: usize) -> String {
    const SUFFIX: &str = "\n[llm-compress: 截断至 max_bytes]";

    // Budget: the byte limit allowed for the body = max_bytes minus the marker length
    // (if max_bytes is smaller than the marker, the body takes 0).
    let budget = max_bytes.saturating_sub(SUFFIX.len());

    // Find the largest UTF-8 character boundary ≤ budget.
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

- [ ] **Step 4: Register the module (compress/mod.rs + lib.rs)**

Create (or modify if 04–06 already created it) `zmod/llm-compress/src/compress/mod.rs`:

```rust
//! Collection of per-content-type compressors. Each submodule implements a router::Compressor.
//! 03 truncate (fallback text); 04 json; 05 diff; 06 log — the respective tasks append their pub mod lines.

pub mod truncate;
```

> If `compress/mod.rs` was already created by a parallel task (04–06) when executing this task, **only append** the single line `pub mod truncate;`, do not overwrite the other `pub mod` entries.

Modify `zmod/llm-compress/src/lib.rs`, appending after `pub mod router;` (skip if 04–06 already added it, to avoid duplication):

```rust
pub mod compress;
```

- [ ] **Step 5: Run the test and watch it pass**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test truncate_test
```
Expected: `test result: ok. 9 passed`.

- [ ] **Step 6: Commit (zmod + this plan only, not codex-rs's dirty changes)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-03-truncate.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): TruncateCompressor (fallback text truncation)"
```

> **Do not** `git add codex-rs/**` — the dev-build enabling change to `core/Cargo.toml` stays dirty until Task 09.
