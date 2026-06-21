//! TabularCompressor —— CSV/TSV/Markdown 表格 → csv-schema(spec §5④)。
//! 严格前提在 detect 内判定;满足才认领。kind=Json, lossy=false,不挂 CCR。
//! detect 内同时预判 schema-form 是否比原文小,不小则让位给 Truncate。

use crate::compress::schema::to_schema_form;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::{Map, Value};

pub struct TabularCompressor;

impl Compressor for TabularCompressor {
    fn name(&self) -> &'static str {
        "tabular"
    }

    fn detect(&self, text: &str, budget: &Budget) -> bool {
        if !budget.cfg.tabular.enabled {
            return false;
        }
        matches!(try_build(text), Some(s) if s.len() < text.len())
    }

    fn compress(&self, text: &str, _budget: &Budget) -> CompressOutcome {
        match try_build(text) {
            Some(s) if s.len() < text.len() => CompressOutcome::Compressed {
                saved_bytes: text.len() - s.len(),
                text: s,
                lossy: false,
                kind: ContentKind::Json,
            },
            _ => CompressOutcome::Unchanged,
        }
    }
}

/// 解析表格、构建 schema-form JSON 字符串;任一步骤失败返回 None。
/// detect 与 compress 共用此函数,保证判定一致。
fn try_build(text: &str) -> Option<String> {
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
    let snapshot = Value::Array(arr);
    let schema_form = to_schema_form(&snapshot)?;
    serde_json::to_string(&schema_form).ok()
}

/// 解析 CSV/TSV/Markdown 表格为 Vec<行>(行 = Vec<单元格>),含 header。
/// 严格:有 header、列名唯一非空、所有行列数一致、无引号转义/单元格换行;否则 None。
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
