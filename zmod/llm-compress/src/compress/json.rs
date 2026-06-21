//! JsonCompressor —— 只做不删内容步骤(连续 RLE 去重 + csv-schema),kind=Json 恒 lossy=false。
//! detect 内预判:无损压缩后是否仍超 truncate.max_bytes;超则让位 Truncate(spec §5①)。

use crate::compress::schema::to_schema_form;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::Value;

const DUP_KEY: &str = "_llm_dup_prev";

pub struct JsonCompressor;

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    /// 让位判据:能 parse 为对象/数组,且无损压缩后体积 ≤ truncate.max_bytes 才认领。
    /// 否则(含 parse 失败 / 无损压不下来)返回 false,让 Truncate 兜底。
    fn detect(&self, text: &str, budget: &Budget) -> bool {
        let value: Value = match serde_json::from_str(text) {
            Ok(v @ Value::Object(_)) | Ok(v @ Value::Array(_)) => v,
            _ => return false,
        };
        let compressed = lossless_compress(value, budget);
        match serde_json::to_string(&compressed) {
            Ok(s) => s.len() <= budget.cfg.truncate.max_bytes,
            Err(_) => false,
        }
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return CompressOutcome::Unchanged,
        };
        let compressed = lossless_compress(value, budget);
        let new = match serde_json::to_string(&compressed) {
            Ok(s) => s,
            Err(_) => return CompressOutcome::Unchanged,
        };
        // 校验产物可 parse(现有不变量)
        if serde_json::from_str::<Value>(&new).is_err() {
            return CompressOutcome::Unchanged;
        }
        let saved = text.len().saturating_sub(new.len());
        if saved > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes: saved, lossy: false, kind: ContentKind::Json }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// 无损压缩:递归对每个数组先做连续 RLE 去重,再尝试 csv-schema。均不删数据。
fn lossless_compress(mut value: Value, budget: &Budget) -> Value {
    transform_value(&mut value, budget);
    value
}

fn transform_value(v: &mut Value, budget: &Budget) {
    match v {
        Value::Array(items) => {
            // 先递归子元素
            for child in items.iter_mut() {
                transform_value(child, budget);
            }
            // 连续 RLE 去重(就地)
            rle_dedup(items);
            // csv-schema:整段数组同构 → 替换为 schema 形态
            if budget.cfg.json.csv_schema {
                let snapshot = Value::Array(items.clone());
                if let Some(schema_form) = to_schema_form(&snapshot) {
                    *v = schema_form;
                }
            }
        }
        Value::Object(map) => {
            for (_k, child) in map.iter_mut() {
                transform_value(child, budget);
            }
        }
        _ => {}
    }
}

/// 连续相邻相等项折叠为:首项 + {"_llm_dup_prev": N}(N=额外重复次数)。
/// 本身即含 _llm_dup_prev 键的对象不参与折叠(避免占位混淆)。
fn rle_dedup(items: &mut Vec<Value>) {
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() {
        let cur = &items[i];
        if is_dup_marker(cur) {
            out.push(cur.clone());
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < items.len() && items[j] == *cur && !is_dup_marker(&items[j]) {
            j += 1;
        }
        let extra = j - i - 1; // 额外重复次数
        out.push(cur.clone());
        if extra >= 1 {
            let mut m = serde_json::Map::new();
            m.insert(DUP_KEY.to_string(), Value::from(extra));
            out.push(Value::Object(m));
        }
        i = j;
    }
    *items = out;
}

fn is_dup_marker(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.contains_key(DUP_KEY))
}
