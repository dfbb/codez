//! LogCompressor:文本型日志压缩器。
//! 两步压缩:① 模板挖掘(不删内容)② 级别评分保留(删行)。占位为裸文本标记 [llm-compress: …]。

use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};

/// 识别多行日志 / 栈跟踪文本并做保守压缩。
pub struct LogCompressor;

/// detect 的"多行"门槛:行数 ≥ 此值才考虑认领。
const MIN_LINES: usize = 8;

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

        // ① 级别评分保留(删行):先裁掉低价值行,中段 ERROR/栈帧/高分行必留
        let head = budget.cfg.truncate.head_lines;
        let tail = budget.cfg.truncate.tail_lines;
        let lines_owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let (after_score, dropped) = score_keep(&lines_owned, head, tail, budget);

        // ② 模板挖掘(不删内容):对留下的行做连续同模板折叠
        let min_run = budget.cfg.log.template_min_run.max(2);
        let after_score_refs: Vec<&str> = after_score.iter().map(String::as_str).collect();
        let (final_lines, tpl_changed) = template_mine(&after_score_refs, min_run);

        // 无实质变化:既无删行也无模板折叠 → Unchanged
        if !dropped && !tpl_changed {
            return CompressOutcome::Unchanged;
        }
        let new_text = final_lines.join("\n");
        let saved = text.len().saturating_sub(new_text.len());
        // 删行 → lossy=true;仅模板折叠(无删行)→ lossy=false
        let lossy = dropped;
        CompressOutcome::Compressed {
            text: new_text,
            saved_bytes: saved,
            lossy,
            kind: ContentKind::Text,
        }
    }
}

/// 是否含时间戳行:`YYYY-MM-DD[T ]HH:MM:SS` 或裸 `HH:MM:SS`。
/// 不引入正则依赖,用字符级扫描判定。
fn has_timestamp(lines: &[&str]) -> bool {
    lines.iter().any(|l| line_has_full_ts(l) || line_has_clock_ts(l))
}

/// 检测行内是否出现 `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}`。
fn line_has_full_ts(line: &str) -> bool {
    let bytes = line.as_bytes();
    // 滑动窗口:date(10) + sep(1) + time(8) = 19 个字符。
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

/// 检测行内是否出现裸 `\d{2}:\d{2}:\d{2}`。
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

/// 8 字节窗口是否为 `dd:dd:dd`。
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

/// 是否含栈跟踪特征:某行含 ` at ` 且其后存在 `:` 紧跟数字(如 `at foo.rs:42`)。
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

/// 子串里是否存在 `:` 紧跟一个数字。
fn colon_then_digit(s: &str) -> bool {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b':' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

/// 是否存在连续两行完全相同。
fn has_consecutive_repeat(lines: &[&str]) -> bool {
    lines.windows(2).any(|w| w[0] == w[1])
}

/// 模板挖掘:连续 ≥ min_run 行,规范化(数字/hex→占位)后相等则折叠为模板头 + 变量表。
/// 不删内容(变量全保留)。返回 (新行序列, 是否折叠过)。
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
            for line in &lines[i..j] {
                out.push(line.to_string());
            }
        }
        i = j;
    }
    (out, changed)
}

/// 把行内数字串、十六进制串替换成占位,用于"同模板"判定。
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

/// 级别评分保留:保 head/tail + 必留行(error/warn/栈帧/高分)+ 中段按分;
/// 丢弃的连续段折叠为 [llm-compress: 略 N 行]。返回 (新行, 是否删了行)。
fn score_keep(lines: &[String], head: usize, tail: usize, budget: &Budget) -> (Vec<String>, bool) {
    let n = lines.len();
    if n <= head + tail {
        return (lines.to_vec(), false);
    }
    let query = budget.query;
    let mut keep = vec![false; n];
    // head/tail 必留
    keep[..head.min(n)].fill(true);
    keep[n.saturating_sub(tail)..].fill(true);
    // 必留:模板行/变量行(以 [llm-compress: 开头)+ 高分行 + 栈帧
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("[llm-compress: ")
            || crate::score::line_score(line, query) >= 1.0
            || is_stack_frame(line)
        {
            keep[i] = true;
        }
    }
    // 输出,丢弃连续段折叠
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

/// 栈帧行特征:含 " at " 且其后有 :数字(复用 colon_then_digit)。
fn is_stack_frame(line: &str) -> bool {
    if let Some(pos) = line.find(" at ") {
        colon_then_digit(&line[pos + 4..])
    } else {
        false
    }
}
