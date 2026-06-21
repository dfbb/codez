# Task 06 — search.rs:SearchCompressor

> Belongs to `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §5②. Depends on Task 01 (signature/Budget), Task 02 (score). Can run in parallel with 05/07/08/09.

**Goal:** Add `SearchCompressor`: recognize grep/ripgrep output (`path:line:content` or `path:line:col:content`), group by file, keep the first and last match per file plus score-selected middle segments, and fold files when the count exceeds the limit. `lossy=true, kind=Text`, attaches CCR.

## Files
- Create: `zmod/llm-compress/src/compress/search.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs` (add `pub mod search;`)
- Test: `zmod/llm-compress/tests/search_test.rs`

**Interfaces:**
- Consumes: Task 01's `Budget`/`CompressOutcome`/`ContentKind`/`Compressor`/`config.search.{max_per_file,max_files}`, `CommandHint::is_grep`; Task 02's `score::line_score`.
- Produces: `pub struct SearchCompressor;` (impl Compressor).

> **Algorithm (spec §5②, see search_compressor.rs)**: ① Parse each line as `path:line:col?:content` and group by path; non-match lines go into a "preamble/misc" bucket kept verbatim. ② Per group: always keep the first + last match; for the middle, select top-K by `score::line_score(content, query)` (`max_per_file`, default 5), folding dropped lines into `[llm-compress: omitted N matches]`; restore the original line order within the group. ③ When the file count exceeds `max_files` (default 15): sort by "highest score within group" and keep the high-scoring files, folding the rest of each whole group into `[llm-compress: omitted N files]`.

---

- [ ] **Step 1: Write the failing test**

Create `zmod/llm-compress/tests/search_test.rs`:

```rust
use codez_llm_compress::compress::search::SearchCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn detect_recognizes_grep_lines() {
    let cfg = Config::disabled();
    let c = SearchCompressor;
    let text = "src/a.rs:10:fn foo()\nsrc/a.rs:20:fn bar()\nsrc/b.rs:5:struct X\nsrc/b.rs:8:impl X\nsrc/c.rs:1:use std\nsrc/c.rs:2:use core\nsrc/c.rs:3:mod m";
    assert!(c.detect(text, &budget(&cfg)));
}

#[test]
fn detect_rejects_non_grep() {
    let cfg = Config::disabled();
    let c = SearchCompressor;
    assert!(!c.detect("just\nplain\ntext\nlines\nhere", &budget(&cfg)));
}

#[test]
fn keeps_first_and_last_match_per_file() {
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    // One file with 5 matches
    let text = "f.rs:1:one\nf.rs:2:two\nf.rs:3:three\nf.rs:4:four\nf.rs:5:five";
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &budget(&cfg)) {
        assert!(lossy);
        assert_eq!(kind, ContentKind::Text);
        assert!(new.contains("f.rs:1:one"), "keep first match");
        assert!(new.contains("f.rs:5:five"), "keep last match");
        assert!(new.contains("[llm-compress: 略"));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn files_over_limit_are_folded() {
    let mut cfg = Config::disabled();
    cfg.search.max_files = 2;
    cfg.search.max_per_file = 5;
    let c = SearchCompressor;
    let mut lines = Vec::new();
    for f in 0..5 {
        for l in 0..3 {
            lines.push(format!("file{f}.rs:{l}:content {f} {l}"));
        }
    }
    let text = lines.join("\n");
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(&text, &budget(&cfg)) {
        assert!(new.contains("个文件"), "fold files over the limit");
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn is_grep_command_forces_detect() {
    // Even when the line format is atypical, an is_grep hit claims it (provided by budget.cmd). This test only verifies detect reads budget.cmd without panicking.
    let cfg = Config::disabled();
    let c = SearchCompressor;
    let hint = codez_llm_compress::command::CommandHint { program: "rg".to_string(), argv: vec![] };
    let b = Budget { cfg: &cfg, cmd: Some(&hint), query: &[] };
    // Multiple lines but not standard grep format; is_grep hit → detect true
    assert!(c.detect("matchy line 1\nmatchy line 2\nmatchy line 3", &b));
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test search_test 2>&1 | head`
Expected: FAIL (the `search` module does not exist)

- [ ] **Step 3: Implement search.rs (parsing + detect)**

Create `zmod/llm-compress/src/compress/search.rs`, starting with the top through detect:

```rust
//! SearchCompressor —— grep/ripgrep output: group by file, keep first/last match, score-select the middle (spec §5②).
//! lossy=true, kind=Text, attaches CCR.

use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use crate::score::line_score;

pub struct SearchCompressor;

/// Parse `path:line:content` or `path:line:col:content`; return path or None.
fn parse_match(line: &str) -> Option<&str> {
    // At least two ':', with the first ':' immediately followed by digits (line number)
    let first = line.find(':')?;
    let rest = &line[first + 1..];
    let second_rel = rest.find(':')?;
    let num = &rest[..second_rel];
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(&line[..first]) // path
}

impl Compressor for SearchCompressor {
    fn name(&self) -> &'static str {
        "search"
    }

    fn detect(&self, text: &str, budget: &Budget) -> bool {
        // Command hint matches grep → claim it directly
        if budget.cmd.is_some_and(|c| c.is_grep()) {
            return text.lines().count() >= 3;
        }
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < 4 {
            return false;
        }
        let matched = lines.iter().filter(|l| parse_match(l).is_some()).count();
        // The majority of lines are in match format
        matched * 2 >= lines.len() && matched >= 3
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let result = compress_search(text, budget);
        match result {
            Some(new) => {
                let saved = text.len().saturating_sub(new.len());
                if saved > 0 {
                    CompressOutcome::Compressed { text: new, saved_bytes: saved, lossy: true, kind: ContentKind::Text }
                } else {
                    CompressOutcome::Unchanged
                }
            }
            None => CompressOutcome::Unchanged,
        }
    }
}
```

- [ ] **Step 4: Continue search.rs (compress_search grouping and selection)**

Append at the end of `search.rs`:

```rust
/// Core: group by file, keep first/last + score-select within each group, fold files over the limit.
fn compress_search(text: &str, budget: &Budget) -> Option<String> {
    let max_per_file = budget.cfg.search.max_per_file.max(2);
    let max_files = budget.cfg.search.max_files.max(1);
    let query = budget.query;

    let lines: Vec<&str> = text.lines().collect();
    // Group: preserve the order in which files first appear
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&str>> = std::collections::HashMap::new();
    let mut preamble: Vec<&str> = Vec::new();
    for line in &lines {
        match parse_match(line) {
            Some(path) => {
                let key = path.to_string();
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                }
                groups.entry(key).or_default().push(line);
            }
            None => preamble.push(line),
        }
    }
    if order.is_empty() {
        return None;
    }

    // Per-group selection: first + last + top-K in the middle
    let mut out: Vec<String> = preamble.iter().map(|s| s.to_string()).collect();
    // File-level score = highest line_score within the group
    let mut scored_files: Vec<(String, f32)> = order
        .iter()
        .map(|k| {
            let best = groups[k].iter().map(|l| line_score(l, query)).fold(0.0_f32, f32::max);
            (k.clone(), best)
        })
        .collect();
    // Select max_files high-scoring files (output keeps the original order, but determine the keep set first)
    let mut keep: std::collections::HashSet<String> = scored_files.iter().map(|(k, _)| k.clone()).collect();
    if order.len() > max_files {
        scored_files.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        keep = scored_files.iter().take(max_files).map(|(k, _)| k.clone()).collect();
    }

    let mut folded_files = 0;
    for key in &order {
        if !keep.contains(key) {
            folded_files += 1;
            continue;
        }
        let matches = &groups[key];
        out.extend(select_in_file(matches, max_per_file, query));
    }
    if folded_files > 0 {
        out.push(format!("[llm-compress: 略 {folded_files} 个文件]"));
    }
    Some(out.join("\n"))
}

/// In-group selection: always keep first + last; select top-(K-2) in the middle by score; fold and count dropped segments; restore the original order.
fn select_in_file(matches: &[&str], max_per_file: usize, query: &[String]) -> Vec<String> {
    if matches.len() <= max_per_file {
        return matches.iter().map(|s| s.to_string()).collect();
    }
    let n = matches.len();
    let mut keep_idx: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    keep_idx.insert(0);
    keep_idx.insert(n - 1);
    // Sort the middle by score and take the top
    let mut mids: Vec<(usize, f32)> = (1..n - 1).map(|i| (i, line_score(matches[i], query))).collect();
    mids.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (i, _) in mids.into_iter().take(max_per_file.saturating_sub(2)) {
        keep_idx.insert(i);
    }
    // Output in the original order, folding and counting dropped contiguous segments
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < n {
        if keep_idx.contains(&i) {
            out.push(matches[i].to_string());
            i += 1;
        } else {
            let start = i;
            while i < n && !keep_idx.contains(&i) {
                i += 1;
            }
            out.push(format!("[llm-compress: 略 {} 个匹配]", i - start));
        }
    }
    out
}
```

Add `pub mod search;` to `zmod/llm-compress/src/compress/mod.rs`.

- [ ] **Step 5: Run the tests to pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test search_test`
Expected: PASS (5 tests)

- [ ] **Step 6: clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/compress/search.rs zmod/llm-compress/src/compress/mod.rs \
  zmod/llm-compress/tests/search_test.rs
git commit -m "feat(llm-compress-v2): Task06 search.rs SearchCompressor(分组+保首尾+评分选中)"
```
