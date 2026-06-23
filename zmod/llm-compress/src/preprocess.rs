//! Generic preprocessing layer in rtk style (spec §4.6/D1). Returns (processed text, whether substantial content was deleted).
//! Order: strip_progress → blob_fold → collapse_blank → truncate_line_bytes → dedup_consecutive.
//! Content deletion steps (strip_progress/blob_fold/truncate_line_bytes) set lossy=true; format reconstruction steps do not.
//! base64/blob folding is the only execution location (#6); Truncate no longer folds.

use crate::config::PreprocessCfg;

const MARKER_PREFIX: &str = "[llm-compress: ";

/// Main entry point: runs each stage in order. Returns (text, whether substantial content was deleted).
pub fn run(text: &str, cfg: &PreprocessCfg) -> (String, bool) {
    let mut s = text.to_string();
    let mut lossy = false;

    if cfg.strip_progress {
        let (ns, changed) = strip_progress(&s);
        s = ns;
        lossy |= changed;
    }
    if cfg.blob_min_bytes > 0 {
        let (ns, changed) = blob_fold(&s, cfg.blob_min_bytes);
        s = ns;
        lossy |= changed;
    }
    if cfg.collapse_blank {
        s = collapse_blank(&s); // format reconstruction, do not set lossy
    }
    if cfg.truncate_line_bytes > 0 {
        let (ns, changed) = truncate_lines(&s, cfg.truncate_line_bytes);
        s = ns;
        lossy |= changed;
    }
    if cfg.dedup_consecutive {
        s = dedup_consecutive(&s); // format reconstruction, do not set lossy
    }
    (s, lossy)
}

/// Delete progress bars/download lines (deletes content). Returns (text, whether lines were deleted).
fn strip_progress(text: &str) -> (String, bool) {
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = false;
    for line in text.split('\n') {
        if is_progress_line(line) {
            removed = true;
        } else {
            kept.push(line);
        }
    }
    (kept.join("\n"), removed)
}

fn is_progress_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("Downloading") || t.starts_with("Downloaded") || t.starts_with("Fetching") {
        return true;
    }
    // contains carriage return (\r) or percentage progress
    if line.contains('\r') {
        return true;
    }
    // like " 45%" / "[####    ] 80%"
    let has_pct = t.split_whitespace().any(|w| w.ends_with('%') && w.trim_end_matches('%').parse::<f64>().is_ok());
    has_pct && (t.contains('[') || t.contains('#') || t.contains('='))
}

/// Fold long base64/data-uri segments (deletes content, #6 only location). Returns (text, whether folded).
fn blob_fold(text: &str, min_bytes: usize) -> (String, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut folded = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.len() >= min_bytes && is_blobish(trimmed) {
            out.push(format!("[llm-compress: base64 {} bytes]", trimmed.len()));
            folded = true;
        } else {
            out.push(line.to_string());
        }
    }
    (out.join("\n"), folded)
}

/// Determine if a line looks like base64/data-uri: has "data:" prefix, or is a long string with characters limited to base64 alphabet.
fn is_blobish(s: &str) -> bool {
    if s.starts_with("data:") {
        return true;
    }
    let body = s.strip_prefix("data:").unwrap_or(s);
    let b64_ratio = body.chars().filter(|c| {
        c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=' || *c == '-' || *c == '_'
    }).count();
    !body.is_empty() && b64_ratio == body.len()
}

/// Normalize consecutive blank lines to a single blank line (format reconstruction, does not delete substantial content).
fn collapse_blank(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in text.split('\n') {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue; // skip extra blank lines
        }
        out.push(line);
        prev_blank = blank;
    }
    out.join("\n")
}

/// Truncate long single lines by byte count (UTF-8 boundary safe, deletes content). Returns (text, whether truncated).
fn truncate_lines(text: &str, max_bytes: usize) -> (String, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut truncated = false;
    for line in text.split('\n') {
        if line.len() > max_bytes {
            // truncate at the maximum character boundary within max_bytes
            let mut cut = 0;
            for (idx, ch) in line.char_indices() {
                let end = idx + ch.len_utf8();
                if end <= max_bytes {
                    cut = end;
                } else {
                    break;
                }
            }
            out.push(format!("{}[llm-compress: line truncated]", &line[..cut]));
            truncated = true;
        } else {
            out.push(line.to_string());
        }
    }
    (out.join("\n"), truncated)
}

/// Fold consecutive identical lines into line + [llm-compress: previous line ×N] (format reconstruction, does not delete content).
/// #6: lines that already start with [llm-compress: prefix do not participate in folding (kept as-is) to avoid position confusion.
fn dedup_consecutive(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        if cur.starts_with(MARKER_PREFIX) {
            out.push(cur.to_string()); // placeholder lines do not fold
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && lines[j] == cur && !lines[j].starts_with(MARKER_PREFIX) {
            j += 1;
        }
        let count = j - i;
        out.push(cur.to_string());
        if count >= 2 {
            out.push(format!("[llm-compress: previous line ×{count}]"));
        }
        i = j;
    }
    out.join("\n")
}
