//! Fallback text truncator: strip ANSI, preserve head/tail lines by line, bare placeholder in middle, hard truncate by max_bytes if needed.
//! Truncates at UTF-8 character boundaries. detect() always returns true, serves as the end-of-chain fallback in ContentRouter.
//! Irreversible but conservative: small inputs remain unchanged.

use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};

/// Fallback text compressor. Stateless, unit struct.
pub struct TruncateCompressor;

impl Compressor for TruncateCompressor {
    fn name(&self) -> &'static str {
        "truncate"
    }

    /// Always true: claims all content (end-of-chain fallback).
    fn detect(&self, _text: &str, _budget: &Budget) -> bool {
        true
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let cfg = &budget.cfg.truncate;
        let original_len = text.len();

        // ① Strip ANSI escape sequences.
        let cleaned = strip_ansi(text);

        // ② Split by lines.
        let lines: Vec<&str> = cleaned.split('\n').collect();
        let line_count = lines.len();

        // ③ Neither line count nor byte count exceeds threshold → conservatively keep unchanged.
        //    Note: even if ANSI is stripped, if thresholds are not exceeded, return Unchanged
        //    (we don't rewrite just to remove color; avoid unnecessary irreversible changes;
        //    saved_bytes accounting also requires new < original to count as compression).
        if line_count <= cfg.head_lines + cfg.tail_lines && cleaned.len() <= cfg.max_bytes {
            return CompressOutcome::Unchanged;
        }

        // ④ Keep first head_lines + last tail_lines, bare placeholder line in middle.
        let mut result = build_head_tail(&lines, cfg.head_lines, cfg.tail_lines);

        // ⑤ If still exceeds max_bytes, hard truncate at character boundary + append truncation marker.
        if result.len() > cfg.max_bytes {
            result = hard_truncate(&result, cfg.max_bytes);
        }

        // ⑥ Calculate saved_bytes; must confirm actual reduction.
        if result.len() < original_len {
            let saved_bytes = original_len - result.len();
            CompressOutcome::Compressed { text: result, saved_bytes, lossy: true, kind: ContentKind::Text }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// Manual ANSI (CSI) stripping: skip sequences from `\x1b[` to terminator byte (`@`..=`~`, 0x40..=0x7E).
/// Covers `\x1b[0m`, `\x1b[1;31m`, `\x1b[2K`, etc.; safely skips isolated `\x1b` as well.
/// Other non-CSI ESC forms (e.g., `\x1bP`…) are rare in tool output; conservatively discard the following single byte,
/// not aiming for full ECMA-48 coverage—sufficient for spec §6 requirement to strip common color/control codes.
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI: skip to terminator byte (0x40..=0x7E) inclusive.
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                // j points to terminator byte (if exists); discard the entire segment including terminator.
                i = if j < bytes.len() { j + 1 } else { j };
            } else {
                // Isolated ESC or non-CSI: discard ESC and following byte (if any).
                i = if i + 1 < bytes.len() { i + 2 } else { i + 1 };
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // strip_ansi only removes complete ASCII control sequences (ESC=0x1b and 0x40..=0x7e are single-byte ASCII),
    // never cuts multi-byte UTF-8 chars, so from_utf8 must succeed; on failure, conservatively fall back to original.
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// Assemble "first head lines + placeholder marker + last tail lines".
/// If head+tail covers all lines (no middle omission), don't insert placeholder; reassemble as-is.
fn build_head_tail(lines: &[&str], head: usize, tail: usize) -> String {
    let n = lines.len();

    // head+tail covers all lines → no omission, reassemble directly (may still undergo hard truncation due to byte threshold).
    if head + tail >= n {
        return lines.join("\n");
    }

    let head_part = &lines[..head];
    let tail_part = &lines[n - tail..];
    let omitted = &lines[head..n - tail];

    let omitted_lines = omitted.len();
    // Omitted bytes: sum of bytes in omitted lines + newlines between them (omitted_lines - 1 if > 0).
    let mut omitted_bytes: usize = omitted.iter().map(|l| l.len()).sum();
    if omitted_lines > 0 {
        omitted_bytes += omitted_lines - 1;
    }

    let marker = format!("[llm-compress: 略 {omitted_lines} 行 / {omitted_bytes} 字节]");

    let mut parts: Vec<String> = Vec::with_capacity(head + 1 + tail);
    for line in head_part {
        parts.push(line.to_string());
    }
    parts.push(marker);
    for line in tail_part {
        parts.push(line.to_string());
    }
    parts.join("\n")
}

/// Truncate `text` to within `max_bytes` at UTF-8 character boundary, append truncation marker.
/// Reserve bytes for marker, ensure final result overall ≤ reasonable upper bound (as close to max_bytes as possible).
fn hard_truncate(text: &str, max_bytes: usize) -> String {
    const SUFFIX: &str = "\n[llm-compress: 截断至 max_bytes]";

    // Budget: bytes allowed for main content = max_bytes minus marker length (if max_bytes < marker, main content gets 0).
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
