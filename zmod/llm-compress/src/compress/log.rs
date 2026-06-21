//! LogCompressor:文本型日志压缩器。
//! 两步保守压缩:① 连续重复行折叠 ② head/tail 保留。占位为裸文本标记 [llm-compress: …]。

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

        // ① 连续重复行折叠(可选)。
        let dedup_repeats = budget.cfg.log.dedup_repeats;
        let after_dedup: Vec<String> = if dedup_repeats {
            dedup_consecutive(&lines)
        } else {
            lines.iter().map(|s| s.to_string()).collect()
        };

        // dedup 是否产生实质变化:内容比较(行数判据对 count=2 失效)。
        let dedup_changed = dedup_repeats
            && (after_dedup.len() != lines.len()
                || after_dedup.iter().zip(lines.iter()).any(|(a, b)| a.as_str() != *b));

        // ② head/tail 保留。
        let head = budget.cfg.truncate.head_lines;
        let tail = budget.cfg.truncate.tail_lines;
        let final_lines = head_tail(&after_dedup, head, tail);

        // head/tail 是否产生了实质截断。
        let head_tail_changed = final_lines.len() < after_dedup.len();

        // 仅当有实质折叠时才视为压缩,避免尾换行副作用产生假阳性。
        if dedup_changed || head_tail_changed {
            let new_text = final_lines.join("\n");
            let saved = text.len().saturating_sub(new_text.len());
            if saved > 0 {
                CompressOutcome::Compressed { text: new_text, saved_bytes: saved, lossy: true, kind: ContentKind::Text }
            } else {
                CompressOutcome::Unchanged
            }
        } else {
            CompressOutcome::Unchanged
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

/// 把连续完全相同的行折叠为:该行 + 裸占位 `[llm-compress: 上一行 ×N]`(N≥2)。
/// N==1 的行原样保留(不加占位)。
fn dedup_consecutive(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        let mut j = i + 1;
        while j < lines.len() && lines[j] == cur {
            j += 1;
        }
        let count = j - i; // 该行连续出现的总次数
        out.push(cur.to_string());
        if count >= 2 {
            out.push(format!("[llm-compress: 上一行 ×{count}]"));
        }
        i = j;
    }
    out
}

/// head/tail 保留:总行数 > head+tail 时,保留前 head 行 + `[llm-compress: 略 N 行]` + 后 tail 行。
/// 否则原样返回。
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
