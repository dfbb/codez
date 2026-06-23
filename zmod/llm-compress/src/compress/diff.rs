//! DiffCompressor: Identifies unified diff, preserves all change lines and structural headers,
//! and collapses only excess context lines within hunks.
//!
//! Collapsing rules (spec §6):
//! - Change lines (`+`/`-` prefix, but not file headers `+++`/`---`), hunk headers (`@@`),
//!   and file headers (`diff --git`/`index`/`--- `/`+++ `) are all preserved.
//! - Context lines within hunks (lines starting with a single space) are preserved only for
//!   `context_lines` lines before and after change lines. Middle lines are collapsed into
//!   a single placeholder line `[llm-compress: 略 N 行上下文]`.

use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};

/// Unified diff compressor.
pub struct DiffCompressor;

impl DiffCompressor {
    /// Check if a line is a hunk header `@@ -a,b +c,d @@` (b, d are optional).
    ///
    /// No regex dependency; manual parsing equivalent to `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`.
    fn is_hunk_header(line: &str) -> bool {
        // Must start with "@@ -".
        let rest = match line.strip_prefix("@@ -") {
            Some(r) => r,
            None => return false,
        };
        // Parse "\d+(,\d+)? " — old range.
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // Followed by a space.
        let rest = match rest.strip_prefix(' ') {
            Some(r) => r,
            None => return false,
        };
        // Followed by "+".
        let rest = match rest.strip_prefix('+') {
            Some(r) => r,
            None => return false,
        };
        // Parse "\d+(,\d+)?" — new range.
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // Followed by " @@".
        rest.starts_with(" @@")
    }

    /// Consume a range in the form `\d+(,\d+)?`, returning the remaining slice; returns None on failure.
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

    /// Check if a line is a change line (`+`/`-` prefix, excluding file headers `+++ `/`--- `).
    fn is_change_line(line: &str) -> bool {
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            return false;
        }
        line.starts_with('+') || line.starts_with('-')
    }

    /// Check if a line is a context line (an unchanged line within a hunk starting with a single space).
    fn is_context_line(line: &str) -> bool {
        line.starts_with(' ')
    }
}

impl Compressor for DiffCompressor {
    fn name(&self) -> &'static str {
        "diff"
    }

    fn detect(&self, text: &str, _budget: &Budget) -> bool {
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

        // Step 1: Collect all lines into a Vec for convenient "window before and after" checks.
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.len();

        // Mark whether each line is a context line.
        let is_ctx: Vec<bool> = lines.iter().map(|l| Self::is_context_line(l)).collect();
        // Mark whether each line is an "anchor" (change line / hunk header / file header) —
        // context lines need to be retained around change lines.
        // The basis for the collapsing window is the distance to the nearest change line,
        // so mark the positions of change lines first.
        let is_change: Vec<bool> = lines.iter().map(|l| Self::is_change_line(l)).collect();

        // Compute the distance from each context line to the "nearest change line"
        // (only meaningful within the same continuous context segment,
        // but using the global nearest change line distance correctly implements the semantics:
        // a context line is retained if there is a change line within `ctx` lines above or below it).
        let mut keep: Vec<bool> = vec![true; n];

        for i in 0..n {
            if !is_ctx[i] {
                // Non-context lines (change lines / hunk headers / file headers / other) are all retained.
                continue;
            }
            // Context line: check if there is a change line within `ctx` lines above.
            let mut near_change = false;
            // Look up `ctx` lines.
            let lo = i.saturating_sub(ctx);
            for j in lo..i {
                if is_change[j] {
                    near_change = true;
                    break;
                }
            }
            // Look down `ctx` lines.
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

        // Step 2: Output in order. When encountering consecutive discarded context lines,
        // collapse them into a single placeholder line.
        let mut out_lines: Vec<String> = Vec::with_capacity(n);
        let mut i = 0;
        let mut any_folded = false;
        while i < n {
            if keep[i] {
                out_lines.push(lines[i].to_string());
                i += 1;
            } else {
                // Collect a segment of consecutive discarded context lines.
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

        // Rebuild the text: preserve the original newline convention at the end.
        // If the original ends with '\n', add one.
        let mut result = out_lines.join("\n");
        if text.ends_with('\n') {
            result.push('\n');
        }

        // If the collapsed text is not smaller (placeholder is even longer), treat as Unchanged.
        if result.len() >= text.len() {
            return CompressOutcome::Unchanged;
        }

        let saved_bytes = text.len() - result.len();
        CompressOutcome::Compressed { text: result, saved_bytes, lossy: true, kind: ContentKind::Text }
    }
}
