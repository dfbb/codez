//! LogCompressor: Text-based log compressor.
//! Level-based scoring and retention (line deletion): Keep head/tail + keep_levels + high-scoring lines + stack frames, delete low-value lines.
//! Placeholder as raw text marker [llm-compress: …].

use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};

/// Detect multi-line logs / stack traces and perform conservative compression.
pub struct LogCompressor;

/// Threshold for "multi-line" detection: consider taking ownership if line count ≥ this value.
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

        // Level-based scoring and retention (line deletion): Keep head/tail + keep_levels + high-scoring lines + stack frames
        let head = budget.cfg.truncate.head_lines;
        let tail = budget.cfg.truncate.tail_lines;
        let lines_owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let keep_levels = &budget.cfg.log.keep_levels;
        let (after_score, dropped) = score_keep(&lines_owned, head, tail, keep_levels, budget);

        // No lines deleted → Unchanged
        if !dropped {
            return CompressOutcome::Unchanged;
        }
        let new_text = after_score.join("\n");
        let saved = text.len().saturating_sub(new_text.len());
        // Defensive: after line deletion the size should theoretically shrink, but keep the guard
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

/// Whether the text contains timestamp lines: `YYYY-MM-DD[T ]HH:MM:SS` or bare `HH:MM:SS`.
/// No regex dependency, use character-level scanning for detection.
fn has_timestamp(lines: &[&str]) -> bool {
    lines.iter().any(|l| line_has_full_ts(l) || line_has_clock_ts(l))
}

/// Detect if line contains `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}`.
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

/// Detect if line contains bare `\d{2}:\d{2}:\d{2}`.
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

/// 8-byte window is `dd:dd:dd`.
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

/// Whether the text contains stack trace features: a line contains ` at ` and is followed by `:` then a digit (e.g., `at foo.rs:42`).
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

/// Whether the substring contains `:` followed immediately by a digit.
fn colon_then_digit(s: &str) -> bool {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b':' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

/// Whether there are two consecutive identical lines.
fn has_consecutive_repeat(lines: &[&str]) -> bool {
    lines.windows(2).any(|w| w[0] == w[1])
}

/// Whether the line belongs to one of the levels in keep_levels (case-insensitive substring matching).
fn line_has_keep_level(line: &str, keep_levels: &[String]) -> bool {
    let lower = line.to_lowercase();
    keep_levels.iter().any(|lvl| lower.contains(lvl.as_str()))
}

/// Level-based scoring and retention: Keep head/tail + keep_levels lines + high-scoring lines (≥1.0) + stack frames;
/// Collapse dropped consecutive segments into [llm-compress: 略 N 行]. Returns (new lines, whether lines were deleted).
fn score_keep(
    lines: &[String],
    head: usize,
    tail: usize,
    keep_levels: &[String],
    _budget: &Budget,
) -> (Vec<String>, bool) {
    let n = lines.len();
    if n <= head + tail {
        return (lines.to_vec(), false);
    }
    let mut keep = vec![false; n];
    // head/tail must be kept
    keep[..head.min(n)].fill(true);
    keep[n.saturating_sub(tail)..].fill(true);
    // Must keep: placeholder lines starting with [llm-compress: + high-scoring lines + keep_levels lines + stack frames
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("[llm-compress: ")
            || crate::score::line_score(line) >= 1.0
            || line_has_keep_level(line, keep_levels)
            || is_stack_frame(line)
        {
            keep[i] = true;
        }
    }
    // Output, collapse dropped consecutive segments
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

/// Stack frame line features: contains ` at ` and is followed by :digit (reuses colon_then_digit).
fn is_stack_frame(line: &str) -> bool {
    if let Some(pos) = line.find(" at ") {
        colon_then_digit(&line[pos + 4..])
    } else {
        false
    }
}
