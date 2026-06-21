# Task 08 — Log Rewrite: Template Mining + Level-Score Retention

> Belongs to `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §5⑤. Depends on Task 01 (signatures), Task 02 (score). Can run in parallel with 05/06/07/09.

**Goal:** Change the Log compressor from v1's "fold consecutive duplicate lines + positional head/tail truncation" to: **① template mining (RLE, no content removal)** — fold consecutive same-template lines into a template header + variable table; **② level-score retention (line removal)** — use `score::line_score` to keep error/warn/stack frames, and discard excess DEBUG/INFO. This solves the "mid-stream ERROR/stack frames being folded indiscriminately" problem. Template folding is `lossy=false`; line removal is `lossy=true`.

## Files
- Modify: `zmod/llm-compress/src/compress/log.rs` (rewrite compress; keep detect + add budget — Task 01 already synced the signature)
- Test: `zmod/llm-compress/tests/log_test.rs` (rewrite; Task 01 already synced the old signature)

**Interfaces:**
- Consumes: Task 01's `Budget`/`CompressOutcome`/`ContentKind`, `config.log.{dedup_repeats,template_min_run,keep_levels}`, `config.truncate.{head_lines,tail_lines}`; Task 02's `score::line_score`.
- Produces: the upgraded `LogCompressor` (detect keeps v1's multiline/timestamp/stack/repeat detection + adds the budget parameter).

> **Algorithm (spec §5⑤)**:
> - **Template mining (no removal)**: for a run of ≥ `template_min_run` (default 3) lines, if they are "same template" (equal after replacing in-line numbers/hex with placeholders) → fold into `[llm-compress: 模板] <template>` + variable table `[llm-compress: 变量] v1, v2, ...`. All variables are kept → lossy=false.
> - **Level-score retention (line removal)**: for the remaining lines, error/warn (`keep_levels`) + stack frames (`at ...:digit`) + lines with `score::line_score ≥ 1.0` are **always kept**; the rest are selected by "keep head/tail + high-scoring middle", and discarded segments are folded into `[llm-compress: 略 N 行]` → lossy=true.
> - lossy: line removal occurred → true; template folding only → false. kind is always Text.

---

- [ ] **Step 1: Write failing tests (rewrite log_test.rs)**

Most of the existing v1 cases in `zmod/llm-compress/tests/log_test.rs` (testing consecutive repeats + head/tail) no longer apply. **Keep the detect-style cases whose signatures Task 01 synced**, remove the cases testing v1 positional truncation, and **append** the new Task 08 cases:

```rust
// ========== New for Task 08 (place at end of file) ==========
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
    // A single mid-stream ERROR surrounded by lots of INFO
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
        assert!(lossy, "INFO lines were removed");
        assert_eq!(kind, ContentKind::Text);
        assert!(new.contains("ERROR critical failure"), "the mid-stream ERROR must be kept");
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn template_mining_folds_similar_lines_lossless() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100; // don't trigger score-based removal; only test template folding
    cfg.log.template_min_run = 3;
    let c = LogCompressor;
    // Consecutive same-template lines (only the number differs)
    let text = "worker 1 done\nworker 2 done\nworker 3 done\nworker 4 done\nworker 5 done";
    if let CompressOutcome::Compressed { text: new, lossy, .. } = c.compress(text, &budget_t08(&cfg)) {
        // Template folding removes no content
        assert!(!lossy, "pure template folding → lossy=false");
        assert!(new.contains("[llm-compress: 模板]") || new.contains("模板"));
    } else {
        // It could also return Unchanged if there's no gain; but 5 same-template lines should gain
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

- [ ] **Step 2: Run and confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test log_test 2>&1 | head -20`
Expected: FAIL (new behavior not yet implemented)

- [ ] **Step 3: Rewrite log.rs's Compressor impl (detect gains budget + new compress)**

`zmod/llm-compress/src/compress/log.rs`: **keep** the existing helper functions `has_timestamp` / `line_has_full_ts` / `line_has_clock_ts` / `is_clock` / `has_stacktrace` / `colon_then_digit` / `has_consecutive_repeat` (still used by detect). **Replace** the entire `impl Compressor for LogCompressor` block with:

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

        // ① Template mining (no content removal)
        let min_run = budget.cfg.log.template_min_run.max(2);
        let (after_tpl, _tpl_changed) = template_mine(&lines, min_run);

        // ② Level-score retention (line removal)
        let head = budget.cfg.truncate.head_lines;
        let tail = budget.cfg.truncate.tail_lines;
        let (final_lines, dropped) = score_keep(&after_tpl, head, tail, budget);

        let new_text = final_lines.join("\n");
        let saved = text.len().saturating_sub(new_text.len());
        if saved == 0 {
            return CompressOutcome::Unchanged;
        }
        // line removal → lossy=true; template folding only (no removal) → lossy=false
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

> Note: remove the v1 `dedup_consecutive` / `head_tail` calls inside the existing compress and the related old helper functions (`dedup_consecutive`, `head_tail`) — they are replaced by `template_mine` / `score_keep`. Keep the `MIN_LINES` constant if it already exists.

- [ ] **Step 4: Append the template_mine implementation**

Append at the end of `log.rs`:

```rust
/// Template mining: for a run of ≥ min_run lines that are equal after normalization
/// (numbers/hex → placeholder), fold into a template header + variable table.
/// No content removal (all variables kept). Returns (new line sequence, whether folding happened).
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

/// Replace in-line number runs and hex runs with placeholders, used for "same template" matching.
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

- [ ] **Step 5: Append the score_keep implementation**

Append at the end of `log.rs`:

```rust
/// Level-score retention: keep head/tail + must-keep lines (error/warn/stack frame/high score) + middle by score;
/// discarded consecutive segments are folded into [llm-compress: 略 N 行]. Returns (new lines, whether lines were removed).
fn score_keep(lines: &[String], head: usize, tail: usize, budget: &Budget) -> (Vec<String>, bool) {
    let n = lines.len();
    if n <= head + tail {
        return (lines.to_vec(), false);
    }
    let query = budget.query;
    let mut keep = vec![false; n];
    // head/tail are always kept
    for i in 0..head.min(n) {
        keep[i] = true;
    }
    for i in n.saturating_sub(tail)..n {
        keep[i] = true;
    }
    // Must-keep: template/variable lines (starting with [llm-compress:) + high-score lines + stack frames
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("[llm-compress: ") {
            keep[i] = true;
        } else if crate::score::line_score(line, query) >= 1.0 {
            keep[i] = true;
        } else if is_stack_frame(line) {
            keep[i] = true;
        }
    }
    // Emit, folding discarded consecutive segments
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

/// Stack-frame line signature: contains " at " followed by :digit (reuses colon_then_digit).
fn is_stack_frame(line: &str) -> bool {
    if let Some(pos) = line.find(" at ") {
        colon_then_digit(&line[pos + 4..])
    } else {
        false
    }
}
```

> Note: make sure the top of `log.rs` includes `use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};` (added by Task 01). `colon_then_digit` is an existing function in the file; `is_stack_frame` reuses it.

- [ ] **Step 6: Run tests to pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test log_test`
Expected: PASS (the retained detect cases + the 3 new Task 08 cases)

> If existing v1 cases (e.g. testing `dedup_consecutive`/`head_tail` old output) fail to compile or fail assertions: delete them — they test the v1 behavior superseded by this task.

- [ ] **Step 7: clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/compress/log.rs zmod/llm-compress/tests/log_test.rs
git commit -m "feat(llm-compress-v2): Task08 Log rewrite — template mining (no removal) + level-score retention (line removal)"
```
