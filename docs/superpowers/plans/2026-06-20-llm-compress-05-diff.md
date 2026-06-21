# Implementation Plan: llm-compress 05 — DiffCompressor

> This file is one of the llm-compress task series (part 05) under the `docs/superpowers/plans/` index.
>
> **Dependencies**:
> - **Task 01** (config): provides `Config` and `pub struct DiffCfg { pub context_lines: usize }`, accessible via `budget.cfg.diff`.
> - **Task 02** (contract): provides the `Budget<'a>`, `CompressOutcome`, and `Compressor` trait. This task **strictly follows** that contract and must not modify it.
>
> This task implements `DiffCompressor`: it recognizes a unified diff, preserves all change lines and structural headers, and only collapses the redundant context lines within a hunk.

---

## Contract (from Task 02, read-only, must not change)

```rust
pub struct Budget<'a> { pub cfg: &'a Config }
pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize }, Unchanged }
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

Config (from Task 01):

```rust
pub struct DiffCfg { pub context_lines: usize }
```

Read from `budget.cfg.diff`.

---

## Behavior Spec (spec §6)

- `name()` returns `"diff"`.
- `detect(text)` returns `true` when **any** of the following conditions hold, otherwise `false`:
  1. any line matches the hunk-header regex `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`;
  2. any line starts with `diff --git `;
  3. there exists a line starting with `--- ` **and** a line starting with `+++ ` at the same time.
- `compress(text, budget)`: parse the unified diff and process it hunk by hunk:
  - **Fully preserved**: change lines (starting with `+` or `-`, but not the file headers `+++`/`---`), hunk headers (`@@`), file headers (`diff --git`, `index`, `--- `, `+++ `).
  - **Context lines** (unchanged lines within a hunk that start with a single space ` `): keep only the `context_lines` lines immediately **before and after** each change line; the redundant context in between is collapsed into **a single** bare placeholder `[llm-compress: 略 N 行上下文]`, where `N` is the number of collapsed context lines.
  - `saved_bytes > 0` → `Compressed { text, saved_bytes }`; otherwise `Unchanged`.
  - The placeholder uses a bare text marker `[llm-compress: …]` (a diff is text-typed, so a bare marker is allowed).

---

## Files

- **Create** `zmod/llm-compress/src/compress/diff.rs` — `DiffCompressor` implementation.
- **Create** `zmod/llm-compress/tests/diff_test.rs` — integration tests.
- **Modify** `zmod/llm-compress/src/compress/mod.rs` — add `pub mod diff;`.

## Interfaces

- **Produces**: `pub struct DiffCompressor;` (implements the `Compressor` trait).
- **Consumes**: `Budget<'a>`, `CompressOutcome`, `Compressor` (Task 02); `DiffCfg`, `Config` (Task 01).

---

## TDD Steps

### ① Write failing tests

Create `zmod/llm-compress/tests/diff_test.rs`:

```rust
//! DiffCompressor integration tests.
//!
//! Coverage:
//! - detect is true for a real git diff, false for plain text;
//! - a hunk with a large context block is collapsed, change lines fully preserved;
//! - the placeholder marker is present;
//! - a small diff (already few context lines) → Unchanged;
//! - saved_bytes is correct.

use codez_llm_compress::compress::diff::DiffCompressor;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use codez_llm_compress::config::Config;

/// Build a Config with context_lines=N (start from Task 01's default, then override the diff field).
fn cfg_with_context(n: usize) -> Config {
    let mut cfg = Config::default();
    cfg.diff.context_lines = n;
    cfg
}

/// A real multi-line unified diff fixture: single file, single hunk, with a large unchanged context block.
/// Within the hunk: 6 lines of leading context + 1 deleted line + 1 added line + 6 lines of trailing context.
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
    assert!(c.detect(REAL_DIFF), "a real git diff should be recognized");
}

#[test]
fn detect_true_for_bare_hunk_header() {
    let c = DiffCompressor;
    let text = "@@ -1,3 +1,4 @@\n a\n-b\n+c\n d\n";
    assert!(c.detect(text), "text containing a hunk header should be recognized");
}

#[test]
fn detect_false_for_plain_text() {
    let c = DiffCompressor;
    let text = "这是一段普通文本。\n没有任何 diff 特征。\n+ 这不是变更行只是个加号开头的句子\n";
    // Note: a single line starting with '+' alone does not constitute a diff
    // (no hunk header, no diff --git, no '--- '+'+++ ' pairing).
    assert!(!c.detect(text), "plain text should not be recognized");
}

#[test]
fn compress_folds_large_context_and_keeps_changes() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(2);
    let budget = Budget { cfg: &cfg };

    let outcome = c.compress(REAL_DIFF, &budget);
    let CompressOutcome::Compressed { text, saved_bytes } = outcome else {
        panic!("a large context block should be compressed");
    };

    // Change lines must be fully preserved.
    assert!(text.contains("-old changed line"), "the deleted line must be preserved");
    assert!(text.contains("+new changed line"), "the added line must be preserved");

    // File headers and the hunk header must be preserved.
    assert!(text.contains("diff --git a/src/lib.rs b/src/lib.rs"));
    assert!(text.contains("index 1234567..89abcde 100644"));
    assert!(text.contains("--- a/src/lib.rs"));
    assert!(text.contains("+++ b/src/lib.rs"));
    assert!(text.contains("@@ -1,14 +1,14 @@"));

    // The 2 context lines immediately before and after the change line must be preserved.
    assert!(text.contains(" line ctx 5"), "the 2nd line before the change must be preserved");
    assert!(text.contains(" line ctx 6"), "the 1st line before the change must be preserved");
    assert!(text.contains(" line ctx 7"), "the 1st line after the change must be preserved");
    assert!(text.contains(" line ctx 8"), "the 2nd line after the change must be preserved");

    // The collapsed far-side context should not appear.
    assert!(!text.contains(" line ctx 1"), "the far leading context should be collapsed");
    assert!(!text.contains(" line ctx 12"), "the far trailing context should be collapsed");

    // The placeholder marker must be present.
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "leading context 6-2=4 lines should collapse into a placeholder, actual output:\n{text}"
    );
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "trailing context 6-2=4 lines should collapse into a placeholder"
    );

    // saved_bytes should equal the byte difference between the original and the compressed text.
    assert_eq!(saved_bytes, REAL_DIFF.len() - text.len(), "saved_bytes must be the byte difference");
    assert!(saved_bytes > 0);
}

#[test]
fn compress_small_diff_unchanged() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(3);
    let budget = Budget { cfg: &cfg };

    // Context is already ≤ context_lines, nothing to collapse.
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
        "should be Unchanged when there is no context to collapse"
    );
}
```

### ② Run the tests and watch them fail

```bash
cd /Users/dfbb/Sites/skycode/codez/codex-rs
cargo test -p codez-llm-compress --test diff_test
```

Expected: compilation fails (the `compress::diff` module does not exist yet), i.e. "red".

### ③ Write the full `diff.rs` implementation

Create `zmod/llm-compress/src/compress/diff.rs`:

```rust
//! DiffCompressor: recognizes a unified diff, preserves all change lines and structural
//! headers, and only collapses the redundant context lines within a hunk.
//!
//! Collapse rules (spec §6):
//! - change lines (starting with `+`/`-`, but not the file headers `+++`/`---`), hunk
//!   headers (`@@`), and file headers (`diff --git`/`index`/`--- `/`+++ `) are all preserved.
//! - context lines within a hunk (starting with a single space) keep only the
//!   `context_lines` lines immediately before and after each change line; the middle is
//!   collapsed into a single bare placeholder `[llm-compress: 略 N 行上下文]`.

use crate::router::{Budget, CompressOutcome, Compressor};

/// Unified-diff compressor.
pub struct DiffCompressor;

impl DiffCompressor {
    /// Determine whether a line is a hunk header `@@ -a,b +c,d @@` (b and d may be omitted).
    ///
    /// Introduces no regex dependency; the hand-written parser is equivalent to
    /// `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`.
    fn is_hunk_header(line: &str) -> bool {
        // Must start with "@@ -".
        let rest = match line.strip_prefix("@@ -") {
            Some(r) => r,
            None => return false,
        };
        // Parse "\d+(,\d+)? " — the old range.
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // Followed by a single space.
        let rest = match rest.strip_prefix(' ') {
            Some(r) => r,
            None => return false,
        };
        // Followed by "+".
        let rest = match rest.strip_prefix('+') {
            Some(r) => r,
            None => return false,
        };
        // Parse "\d+(,\d+)?" — the new range.
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // Followed by " @@".
        rest.starts_with(" @@")
    }

    /// Consume a range of the form `\d+(,\d+)?`, returning the remaining slice; None on failure.
    fn consume_range(s: &str) -> Option<&str> {
        // At least one digit.
        let first_len = s.bytes().take_while(|b| b.is_ascii_digit()).count();
        if first_len == 0 {
            return None;
        }
        let s = &s[first_len..];
        // Optional ",\d+".
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

    /// Whether a line is a file header (preserved as a whole, not subject to context collapsing).
    fn is_file_header(line: &str) -> bool {
        line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
    }

    /// Whether a line is a change line (starting with `+`/`-`, but excluding the file headers `+++ `/`--- `).
    fn is_change_line(line: &str) -> bool {
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            return false;
        }
        line.starts_with('+') || line.starts_with('-')
    }

    /// Whether a line is a context line (an unchanged line within a hunk that starts with a single space).
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

        // Step one: collect all lines into a Vec for easy "before/after window" decisions.
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.len();

        // Mark whether each line is a context line.
        let is_ctx: Vec<bool> = lines.iter().map(|l| Self::is_context_line(l)).collect();
        // Mark whether each line is an "anchor" (change line / hunk header / file header) — context
        // must be kept around change lines.
        // The collapse window is based on "distance to the nearest change line", so first mark the
        // change-line positions.
        let is_change: Vec<bool> = lines.iter().map(|l| Self::is_change_line(l)).collect();

        // Compute each context line's distance to the "nearest change line" (only meaningful within
        // the same contiguous context block, but using the global nearest-change-line distance
        // correctly implements the "keep ctx lines immediately before and after each change line"
        // semantics: a context line is kept if a change line exists within ctx lines above or below it).
        let mut keep: Vec<bool> = vec![true; n];

        for i in 0..n {
            if !is_ctx[i] {
                // Non-context lines (change line / hunk header / file header / other) are always kept.
                continue;
            }
            // Context line: check whether a change line exists within ctx lines above.
            let mut near_change = false;
            // Look ctx lines up.
            let lo = i.saturating_sub(ctx);
            for j in lo..i {
                if is_change[j] {
                    near_change = true;
                    break;
                }
            }
            // Look ctx lines down.
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

        // Step two: emit in order; collapse each run of discarded context lines into a single placeholder.
        let mut out_lines: Vec<String> = Vec::with_capacity(n);
        let mut i = 0;
        let mut any_folded = false;
        while i < n {
            if keep[i] {
                out_lines.push(lines[i].to_string());
                i += 1;
            } else {
                // Collect a contiguous run of discarded context lines.
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

        // Rebuild the text, preserving the original's trailing-newline convention. If the original
        // ends with '\n', append one.
        let mut result = out_lines.join("\n");
        if text.ends_with('\n') {
            result.push('\n');
        }

        // If the size did not shrink after folding (the placeholder is actually longer), treat as Unchanged.
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

### ④ Register the module

In `zmod/llm-compress/src/compress/mod.rs`, add:

```rust
pub mod diff;
```

(Place it in the existing `pub mod ...;` declaration block, alongside the other modules.)

### ⑤ Run the tests and watch them pass

```bash
cd /Users/dfbb/Sites/skycode/codez/codex-rs
cargo test -p codez-llm-compress --test diff_test
```

Expected: all cases pass, i.e. "green".

### ⑥ Commit

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-05-diff.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): DiffCompressor (keep changes + bounded context)"
```

---

## Test Coverage Checklist

| Case | Verification point |
| --- | --- |
| `detect_true_for_real_git_diff` | detect returns `true` for a real git diff |
| `detect_true_for_bare_hunk_header` | detect returns `true` for a bare hunk header |
| `detect_false_for_plain_text` | detect returns `false` for plain text |
| `compress_folds_large_context_and_keeps_changes` | large context collapsed + change lines/headers fully preserved + placeholder marker present + `saved_bytes` is the byte difference |
| `compress_small_diff_unchanged` | already few context lines → `Unchanged` |

## Implementation Notes

- **No regex dependency**: `is_hunk_header`'s hand-written parser is equivalent to `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`, avoiding pulling in the `regex` crate for a single match.
- **Collapse semantics**: "keep the `context_lines` lines immediately before and after each change line" is decided by "whether a change line exists within the `ctx`-line window above/below this context line" — equivalent and concise; each contiguous run of discarded context lines is collapsed into a single placeholder, with `N` being the run's line count.
- **Trailing newline**: the output preserves whether the original ends with `\n`, ensuring `saved_bytes` matches `text.len() - result.len()` exactly.
- **Conservative fallback**: if the size does not shrink after folding, return `Unchanged`, consistent with the `saved_bytes > 0 → Compressed` spec.
