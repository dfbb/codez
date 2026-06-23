//! TabularCompressor — CSV/TSV/Markdown table → TOON.
//! Parses a strict table (header, unique non-empty columns, equal column
//! counts, no quoted cells), builds an array-of-objects Value, and encodes it
//! to TOON. Product is kind=Toon, lossy=false. Gated by json.use_toon; claims
//! only when TOON round-trips, is strictly smaller, and fits truncate.max_bytes.

use crate::compress::toon::encode_checked;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::{Map, Value};

pub struct TabularCompressor;

/// Parse + encode + claim pipeline shared by detect and compress.
fn try_toon(text: &str, budget: &Budget) -> Option<String> {
    if !budget.cfg.json.use_toon {
        return None;
    }
    let table = parse_table(text)?;
    let header = &table[0];
    let mut arr: Vec<Value> = Vec::with_capacity(table.len() - 1);
    for row in &table[1..] {
        let mut m = Map::new();
        for (i, key) in header.iter().enumerate() {
            m.insert(key.clone(), Value::String(row[i].clone()));
        }
        arr.push(Value::Object(m));
    }
    let value = Value::Array(arr);
    let toon = encode_checked(&value)?;
    if toon.len() < text.len() && toon.len() <= budget.cfg.truncate.max_bytes {
        Some(toon)
    } else {
        None
    }
}

impl Compressor for TabularCompressor {
    fn name(&self) -> &'static str {
        "tabular"
    }

    fn detect(&self, text: &str, budget: &Budget) -> bool {
        try_toon(text, budget).is_some()
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        match try_toon(text, budget) {
            Some(toon) => {
                let saved = text.len().saturating_sub(toon.len());
                if saved == 0 {
                    return CompressOutcome::Unchanged;
                }
                CompressOutcome::Compressed {
                    text: toon,
                    saved_bytes: saved,
                    lossy: false,
                    kind: ContentKind::Toon,
                }
            }
            None => CompressOutcome::Unchanged,
        }
    }
}

/// Parse CSV/TSV/Markdown table into Vec<row> (row = Vec<cell>), including header.
/// Strict: requires header, unique non-empty column names, equal column counts
/// across all rows, no quoted cells or escaped content. Returns None on failure.
fn parse_table(text: &str) -> Option<Vec<Vec<String>>> {
    let raw_lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if raw_lines.len() < 2 {
        return None;
    }
    let is_md = raw_lines[0].trim_start().starts_with('|');
    // 含引号的单元格 → 放弃(简单解析器无法稳定还原)
    if text.contains('"') {
        return None;
    }
    let split = |line: &str| -> Vec<String> {
        if is_md {
            let t = line.trim().trim_start_matches('|').trim_end_matches('|');
            t.split('|').map(|c| c.trim().to_string()).collect()
        } else if line.contains('\t') {
            line.split('\t').map(|c| c.trim().to_string()).collect()
        } else {
            line.split(',').map(|c| c.trim().to_string()).collect()
        }
    };

    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in raw_lines.iter() {
        // Markdown 分隔行 |---|---| 跳过
        if is_md && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            continue;
        }
        let cells = split(line);
        rows.push(cells);
    }
    if rows.len() < 2 {
        return None;
    }
    let ncol = rows[0].len();
    if ncol == 0 {
        return None;
    }
    // header 唯一非空
    let header = &rows[0];
    if header.iter().any(|h| h.is_empty()) {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for h in header {
        if !seen.insert(h) {
            return None; // 重复列名
        }
    }
    // 列数一致
    if rows.iter().any(|r| r.len() != ncol) {
        return None;
    }
    Some(rows)
}
