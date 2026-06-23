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

/// 核心:按文件分组,组内保首尾+评分选中,文件数超限折叠。
fn compress_search(text: &str, budget: &Budget) -> Option<String> {
    let max_per_file = budget.cfg.search.max_per_file.max(2);
    let max_files = budget.cfg.search.max_files.max(1);

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
            let best = groups[k].iter().map(|l| line_score(l)).fold(0.0_f32, f32::max);
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
        out.extend(select_in_file(matches, max_per_file));
    }
    if folded_files > 0 {
        out.push(format!("[llm-compress: 略 {folded_files} 个文件]"));
    }
    Some(out.join("\n"))
}

/// 组内选取:必留首+末;中间按分选 top-(K-2);丢弃段折叠计数;回原序。
fn select_in_file(matches: &[&str], max_per_file: usize) -> Vec<String> {
    if matches.len() <= max_per_file {
        return matches.iter().map(|s| s.to_string()).collect();
    }
    let n = matches.len();
    let mut keep_idx: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    keep_idx.insert(0);
    keep_idx.insert(n - 1);
    // 中间按分排序取 top
    let mut mids: Vec<(usize, f32)> = (1..n - 1).map(|i| (i, line_score(matches[i]))).collect();
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
