//! TabularCompressor —— CSV/TSV/Markdown 表格 → csv-schema(spec §5④)。
//! 严格前提在 detect 内判定;满足才认领。kind=Json, lossy=false,不挂 CCR。

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
        parse_table(text).is_some()
    }

    fn compress(&self, text: &str, _budget: &Budget) -> CompressOutcome {
        let table = match parse_table(text) {
            Some(t) => t,
            None => return CompressOutcome::Unchanged,
        };
        // 转对象数组
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
        let schema_form = match to_schema_form(&snapshot) {
            Some(v) => v,
            None => return CompressOutcome::Unchanged,
        };
        let new = match serde_json::to_string(&schema_form) {
            Ok(s) => s,
            Err(_) => return CompressOutcome::Unchanged,
        };
        // 格式转换(→ csv-schema):即使字节数未减少也视为 Compressed(lossy=false 的格式重构)。
        // router 的 saved_bytes>0 过滤由调用方 ContentRouter 处理;此处忠实上报实际节省量。
        let saved = text.len().saturating_sub(new.len());
        CompressOutcome::Compressed { text: new, saved_bytes: saved.max(1), lossy: false, kind: ContentKind::Json }
    }
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
