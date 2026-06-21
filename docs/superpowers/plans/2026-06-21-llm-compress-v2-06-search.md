# Task 06 — search.rs:SearchCompressor

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §5②。依赖 Task 01(签名/Budget)、Task 02(score)。可与 05/07/08/09 并行。

**Goal:** 新增 `SearchCompressor`:识别 grep/ripgrep 输出(`路径:行号:内容` 或 `路径:行号:列:内容`),按文件分组,每文件保首尾匹配 + 评分选中段,文件数超限折叠。`lossy=true, kind=Text`,挂 CCR。

## Files
- Create: `zmod/llm-compress/src/compress/search.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs`(加 `pub mod search;`)
- Test: `zmod/llm-compress/tests/search_test.rs`

**Interfaces:**
- Consumes: Task 01 的 `Budget`/`CompressOutcome`/`ContentKind`/`Compressor`/`config.search.{max_per_file,max_files}`、`CommandHint::is_grep`;Task 02 的 `score::line_score`。
- Produces: `pub struct SearchCompressor;`(impl Compressor)。

> **算法(spec §5②,参考 search_compressor.rs)**:① 解析每行 `path:line:col?:content`,按 path 分组;非匹配行归入"前导/杂项"原样保留。② 每组:必留首+末匹配;中间按 `score::line_score(content, query)` 选 top-K(`max_per_file`,默认 5),被丢的折叠为 `[llm-compress: 略 N 个匹配]`;组内回原行号顺序。③ 文件数超 `max_files`(默认 15):按"组内最高分"排序留高分文件,其余整组折叠为 `[llm-compress: 略 N 个文件]`。

---

- [ ] **Step 1: 写失败测试**

创建 `zmod/llm-compress/tests/search_test.rs`:

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
    // 一个文件 5 个匹配
    let text = "f.rs:1:one\nf.rs:2:two\nf.rs:3:three\nf.rs:4:four\nf.rs:5:five";
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &budget(&cfg)) {
        assert!(lossy);
        assert_eq!(kind, ContentKind::Text);
        assert!(new.contains("f.rs:1:one"), "保留首匹配");
        assert!(new.contains("f.rs:5:five"), "保留末匹配");
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
        assert!(new.contains("个文件"), "文件数超限折叠");
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn is_grep_command_forces_detect() {
    // 即使行格式不典型,is_grep 命中也认领(由 budget.cmd 提供)。本测试只验证 detect 读 budget.cmd 不 panic。
    let cfg = Config::disabled();
    let c = SearchCompressor;
    let hint = codez_llm_compress::command::CommandHint { program: "rg".to_string(), argv: vec![] };
    let b = Budget { cfg: &cfg, cmd: Some(&hint), query: &[] };
    // 多行但非标准 grep 格式;is_grep 命中 → detect true
    assert!(c.detect("matchy line 1\nmatchy line 2\nmatchy line 3", &b));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test search_test 2>&1 | head`
Expected: FAIL(`search` 模块不存在)

- [ ] **Step 3: 实现 search.rs(解析 + detect)**

创建 `zmod/llm-compress/src/compress/search.rs`,先写顶部到 detect:

```rust
//! SearchCompressor —— grep/ripgrep 输出:按文件分组、保首尾匹配、评分选中段(spec §5②)。
//! lossy=true, kind=Text,挂 CCR。

use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use crate::score::line_score;

pub struct SearchCompressor;

/// 解析 `path:line:content` 或 `path:line:col:content`;返回 path 或 None。
fn parse_match(line: &str) -> Option<&str> {
    // 至少两个 ':',第一个 ':' 后紧跟数字(行号)
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
        // 命令提示命中 grep → 直接认领
        if budget.cmd.is_some_and(|c| c.is_grep()) {
            return text.lines().count() >= 3;
        }
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < 4 {
            return false;
        }
        let matched = lines.iter().filter(|l| parse_match(l).is_some()).count();
        // 多数行是匹配格式
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

- [ ] **Step 4: 续写 search.rs(compress_search 分组与选取)**

在 `search.rs` 末尾追加:

```rust
/// 核心:按文件分组,组内保首尾+评分选中,文件数超限折叠。
fn compress_search(text: &str, budget: &Budget) -> Option<String> {
    let max_per_file = budget.cfg.search.max_per_file.max(2);
    let max_files = budget.cfg.search.max_files.max(1);
    let query = budget.query;

    let lines: Vec<&str> = text.lines().collect();
    // 分组:保持文件首次出现顺序
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

    // 每组选取:首 + 末 + 中间 top-K
    let mut out: Vec<String> = preamble.iter().map(|s| s.to_string()).collect();
    // 文件级评分 = 组内最高 line_score
    let mut scored_files: Vec<(String, f32)> = order
        .iter()
        .map(|k| {
            let best = groups[k].iter().map(|l| line_score(l, query)).fold(0.0_f32, f32::max);
            (k.clone(), best)
        })
        .collect();
    // 选 max_files 个高分文件(保持原序输出,但先确定保留集合)
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

/// 组内选取:必留首+末;中间按分选 top-(K-2);丢弃段折叠计数;回原序。
fn select_in_file(matches: &[&str], max_per_file: usize, query: &[String]) -> Vec<String> {
    if matches.len() <= max_per_file {
        return matches.iter().map(|s| s.to_string()).collect();
    }
    let n = matches.len();
    let mut keep_idx: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    keep_idx.insert(0);
    keep_idx.insert(n - 1);
    // 中间按分排序取 top
    let mut mids: Vec<(usize, f32)> = (1..n - 1).map(|i| (i, line_score(matches[i], query))).collect();
    mids.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (i, _) in mids.into_iter().take(max_per_file.saturating_sub(2)) {
        keep_idx.insert(i);
    }
    // 回原序输出,被丢的连续段折叠计数
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

在 `zmod/llm-compress/src/compress/mod.rs` 加 `pub mod search;`。

- [ ] **Step 5: 运行测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test search_test`
Expected: PASS(5 个)

- [ ] **Step 6: clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/compress/search.rs zmod/llm-compress/src/compress/mod.rs \
  zmod/llm-compress/tests/search_test.rs
git commit -m "feat(llm-compress-v2): Task06 search.rs SearchCompressor(分组+保首尾+评分选中)"
```

