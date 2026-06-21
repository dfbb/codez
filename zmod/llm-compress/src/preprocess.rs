//! rtk 风格通用预处理层(spec §4.6/D1)。返回 (处理后文本, 是否删了实质内容)。
//! 顺序:strip_progress → blob_fold → collapse_blank → truncate_line_bytes → dedup_consecutive。
//! 删内容段(strip_progress/blob_fold/truncate_line_bytes)置 lossy=true;格式重构段不置。
//! base64/blob 折叠唯一执行位置(#6),Truncate 不再折叠。

use crate::config::PreprocessCfg;

const MARKER_PREFIX: &str = "[llm-compress: ";

/// 主入口:按顺序跑各段。返回 (文本, 是否删实质内容)。
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
        s = collapse_blank(&s); // 格式重构,不置 lossy
    }
    if cfg.truncate_line_bytes > 0 {
        let (ns, changed) = truncate_lines(&s, cfg.truncate_line_bytes);
        s = ns;
        lossy |= changed;
    }
    if cfg.dedup_consecutive {
        s = dedup_consecutive(&s); // 格式重构,不置 lossy
    }
    (s, lossy)
}

/// 删进度条/下载行(删内容)。返回 (文本, 是否删了行)。
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
    // 含回车覆写(\r)或百分比进度
    if line.contains('\r') {
        return true;
    }
    // 形如 " 45%" / "[####    ] 80%"
    let has_pct = t.split_whitespace().any(|w| w.ends_with('%') && w.trim_end_matches('%').parse::<f64>().is_ok());
    has_pct && (t.contains('[') || t.contains('#') || t.contains('='))
}

/// 折叠超长 base64/data-uri 段(删内容,#6 唯一位置)。返回 (文本, 是否折叠)。
fn blob_fold(text: &str, min_bytes: usize) -> (String, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut folded = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.len() >= min_bytes && is_blobish(trimmed) {
            out.push(format!("[llm-compress: base64 {} 字节]", trimmed.len()));
            folded = true;
        } else {
            out.push(line.to_string());
        }
    }
    (out.join("\n"), folded)
}

/// 判定一行是否像 base64/data-uri:data: 前缀,或长串且字符集限于 base64 字母表。
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

/// 连续空行归一为一个空行(格式重构,不删实质内容)。
fn collapse_blank(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in text.split('\n') {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue; // 跳过多余空行
        }
        out.push(line);
        prev_blank = blank;
    }
    out.join("\n")
}

/// 超长单行按字节截断(UTF-8 边界安全,删内容)。返回 (文本, 是否截断)。
fn truncate_lines(text: &str, max_bytes: usize) -> (String, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut truncated = false;
    for line in text.split('\n') {
        if line.len() > max_bytes {
            // 在 ≤ max_bytes 的最大字符边界截断
            let mut cut = 0;
            for (idx, ch) in line.char_indices() {
                let end = idx + ch.len_utf8();
                if end <= max_bytes {
                    cut = end;
                } else {
                    break;
                }
            }
            out.push(format!("{}[llm-compress: 行截断]", &line[..cut]));
            truncated = true;
        } else {
            out.push(line.to_string());
        }
    }
    (out.join("\n"), truncated)
}

/// 连续完全相同行折叠为 行 + [llm-compress: 上一行 ×N](格式重构,不删内容)。
/// #6:本身即 [llm-compress: 前缀的行不参与折叠(原样保留),避免占位混淆。
fn dedup_consecutive(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        if cur.starts_with(MARKER_PREFIX) {
            out.push(cur.to_string()); // 占位行不折叠
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
            out.push(format!("[llm-compress: 上一行 ×{count}]"));
        }
        i = j;
    }
    out.join("\n")
}
