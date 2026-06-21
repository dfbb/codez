//! csv-schema 内表达公共模块(JSON §5① + Tabular §5④ 共用)。
//! 对象数组(各项同构)→ {"_schema":[...],"_rows":[[...]]}。纯格式重构、内容全保留 → lossy=false。

use serde_json::{Map, Value};

/// 对象数组(各项同构)→ {"_schema":[...],"_rows":[[...]]}。非同构/非数组/空 → None。
/// 同构判定:所有元素都是 object 且键集合相同(键顺序以首元素为准)。
/// 标量与嵌套值都按 schema 列顺序放进 _rows 的行(嵌套值保留原样,不扁平)。
pub fn to_schema_form(value: &Value) -> Option<Value> {
    let arr = value.as_array()?;
    if arr.len() < 2 {
        return None; // 单元素无收益
    }
    let first = arr.first()?.as_object()?;
    if first.is_empty() {
        return None;
    }
    // schema = 首元素键序
    let schema: Vec<String> = first.keys().cloned().collect();
    let key_set: std::collections::BTreeSet<&String> = first.keys().collect();

    let mut rows: Vec<Value> = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object()?; // 任一非 object → None
        // 键集合必须完全一致
        let this_set: std::collections::BTreeSet<&String> = obj.keys().collect();
        if this_set != key_set {
            return None;
        }
        let row: Vec<Value> = schema.iter().map(|k| obj.get(k).cloned().unwrap_or(Value::Null)).collect();
        rows.push(Value::Array(row));
    }

    let mut out = Map::new();
    out.insert("_schema".to_string(), Value::Array(schema.into_iter().map(Value::String).collect()));
    out.insert("_rows".to_string(), Value::Array(rows));
    Some(Value::Object(out))
}
