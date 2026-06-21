//! LogCompressor:文本型日志压缩器。
//! 级别评分保留(删行):保 head/tail + keep_levels + 高分行 + 栈帧,删低价值行。
//! 占位为裸文本标记 [llm-compress: …]。

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

        // 级别评分保留(删行):保 head/tail + keep_levels + 高分行 + 栈帧
        let head = budget.cfg.truncate.head_lines;
        let tail = budget.cfg.truncate.tail_lines;
        let lines_owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let keep_levels = &budget.cfg.log.keep_levels;
        let (after_score, dropped) = score_keep(&lines_owned, head, tail, keep_levels, budget);

        // 无删行 → Unchanged
        if !dropped {
            return CompressOutcome::Unchanged;
        }
        let new_text = after_score.join("\n");
        let saved = text.len().saturating_sub(new_text.len());
        // 防御性:删行后体积理论上必缩小,但保留守卫
        if saved == 0 {
            return CompressOutcome::Unchanged;
        }
        CompressOutcome::Compressed {
            text: new_text,
            saved_bytes: saved,
            lossy: true,
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

/// 行是否属于 keep_levels 中的某个级别(大小写不敏感的子串匹配)。
fn line_has_keep_level(line: &str, keep_levels: &[String]) -> bool {
    let lower = line.to_lowercase();
    keep_levels.iter().any(|lvl| lower.contains(lvl.as_str()))
}

/// 级别评分保留:保 head/tail + keep_levels行 + 高分行(≥1.0) + 栈帧;
/// 丢弃的连续段折叠为 [llm-compress: 略 N 行]。返回 (新行, 是否删了行)。
fn score_keep(
    lines: &[String],
    head: usize,
    tail: usize,
    keep_levels: &[String],
    budget: &Budget,
) -> (Vec<String>, bool) {
    let n = lines.len();
    if n <= head + tail {
        return (lines.to_vec(), false);
    }
    let query = budget.query;
    let mut keep = vec![false; n];
    // head/tail 必留
    keep[..head.min(n)].fill(true);
    keep[n.saturating_sub(tail)..].fill(true);
    // 必留:[llm-compress: 开头的占位行 + 高分行 + keep_levels行 + 栈帧
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("[llm-compress: ")
            || crate::score::line_score(line, query) >= 1.0
            || line_has_keep_level(line, keep_levels)
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
